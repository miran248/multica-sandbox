//! The Compose lifecycle: `up`, `down`, `rm`, `status` and `gc`.
//!
//! One epic is one Compose project (`<project>-<epic>`). `up` validates the
//! manifest, gives the epic its port block, renders the override (container
//! names, dotted aliases, loopback ports, network name) into the epic's envelope
//! directory outside the checkout, runs `docker compose up -d --wait` and
//! records what it did. `down` stops the containers and keeps the volumes; `rm`
//! destroys the project, its volumes and its block; `status` reports services,
//! health and ports; `gc` destroys boxes idle for 14 days and frees their
//! blocks.
//!
//! Nothing here renders the env store or tunnels: those are later stages
//! ([MIR-260], [MIR-262]). The override is still the only place names, ports
//! and the Compose project are decided.
//!
//! [MIR-260]: https://multica.ai/miran248/issues/MIR-260
//! [MIR-262]: https://multica.ai/miran248/issues/MIR-262

use crate::driver;
use crate::epic::{self, Key};
use crate::manifest::Manifest;
use crate::ports::{Allocator, Block, EpicMeta, IDLE_SECS};
use crate::state::{self, Entry};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// How long `up` waits for the project to be running or healthy.
const WAIT_SECS: u64 = 240;

const UP_USAGE: &str = "\
usage: msbx up [--epic E] [--project P] [--file F]

Start or reconcile the epic's Compose project. The manifest is
`.multica/compose.yaml` in the current checkout unless `--file` names it.
The epic is `--epic`, else `MSBX_EPIC`; it is required.
";
const DOWN_USAGE: &str = "\
usage: msbx down [--epic E] [--project P]

Stop the epic's containers and keep its volumes and state. With no `--epic`,
every recorded box is stopped. An absent box is a no-op.
";
const RM_USAGE: &str = "\
usage: msbx rm [--epic E] [--project P] [--yes]

Destroy the epic's project, its volumes and its port block. With no `--epic`,
every recorded box is destroyed. `--yes` skips the confirmation prompt; a
non-interactive run requires it.
";
const STATUS_USAGE: &str = "\
usage: msbx status [--epic E] [--project P] [--json]

Show each recorded box, its services, health and assigned loopback ports.
`--json` is the machine-readable form.
";
const GC_USAGE: &str = "\
usage: msbx gc [--dry-run]

Remove boxes idle for 14 days and release their port blocks. `--dry-run` only
lists them.
";

/// Why a verb did not finish. Usage errors exit 64, runtime errors 1.
enum Failure {
    Usage(String),
    Run(String),
}

impl Failure {
    fn run(message: impl Into<String>) -> Self {
        Failure::Run(message.into())
    }
}

fn finish(verb: &str, result: Result<(), Failure>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("msbx {verb}: {message}");
            ExitCode::from(64)
        }
        Err(Failure::Run(message)) => {
            eprintln!("msbx {verb}: {message}");
            ExitCode::from(1)
        }
    }
}

fn wants_help(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "-h" || arg == "--help")
}

#[derive(Debug, Default)]
struct Options {
    epic: Option<String>,
    project: Option<String>,
    file: Option<PathBuf>,
    json: bool,
    yes: bool,
    dry_run: bool,
}

/// Parse a verb's flags. Every flag an unknown-to-this-verb option is a usage
/// error instead of being silently ignored.
fn parse(args: &[String], allowed: &[&str], usage: &str) -> Result<Options, Failure> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if !allowed.contains(&arg) {
            return Err(Failure::Usage(format!(
                "`{arg}` is not an option here\n\n{usage}"
            )));
        }
        match arg {
            "--epic" => options.epic = Some(value(args, &mut index, arg)?),
            "--project" => options.project = Some(value(args, &mut index, arg)?),
            "--file" => options.file = Some(PathBuf::from(value(args, &mut index, arg)?)),
            "--json" => options.json = true,
            "--yes" => options.yes = true,
            "--dry-run" => options.dry_run = true,
            other => unreachable!("`{other}` passed the allowed list but has no arm"),
        }
        index += 1;
    }
    Ok(options)
}

fn value(args: &[String], index: &mut usize, flag: &str) -> Result<String, Failure> {
    *index += 1;
    args.get(*index)
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Failure::Usage(format!("{flag} needs a value")))
}

pub fn run_up(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{UP_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result =
        parse(args, &["--epic", "--project", "--file"], UP_USAGE).and_then(|options| up(&options));
    finish("up", result)
}

pub fn run_down(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{DOWN_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result =
        parse(args, &["--epic", "--project"], DOWN_USAGE).and_then(|options| down(&options));
    finish("down", result)
}

pub fn run_rm(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{RM_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result =
        parse(args, &["--epic", "--project", "--yes"], RM_USAGE).and_then(|options| rm(&options));
    finish("rm", result)
}

pub fn run_status(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{STATUS_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = parse(args, &["--epic", "--project", "--json"], STATUS_USAGE)
        .and_then(|options| status(&options));
    finish("status", result)
}

pub fn run_gc(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{GC_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = parse(args, &["--dry-run"], GC_USAGE).and_then(|options| gc(&options));
    finish("gc", result)
}

fn up(options: &Options) -> Result<(), Failure> {
    let (manifest_path, root) = locate(options.file.as_deref())?;
    within_home(&root)?;
    let manifest =
        Manifest::from_path(&manifest_path).map_err(|error| Failure::run(error.to_string()))?;
    let project = resolve_project(&manifest, options.project.as_deref())?;
    let epic = resolve_epic(options.epic.as_deref())?;
    let key = Key::new(&project, &epic).map_err(|error| Failure::run(error.to_string()))?;
    epic::check_publish(&manifest).map_err(|error| Failure::run(error.to_string()))?;

    let allocator = Allocator::default_path();
    let state_key = key.compose_project();
    let at = state::now();
    // The mapping recorded by the last `up`, if this box has been up before:
    // a first `up` has nothing to compare against.
    let previous = allocator
        .entry(&state_key)
        .map_err(|error| Failure::run(error.to_string()))?
        .map(|entry| entry.published);
    let block = allocator
        .allocate(&state_key, at)
        .map_err(|error| Failure::run(format!("cannot allocate a port block: {error}")))?;

    let published =
        epic::published(&manifest, block).map_err(|error| Failure::run(error.to_string()))?;
    let override_path = key.override_path();
    let override_text = epic::override_yaml(&key, &manifest, &published);
    state::write_private(&override_path, &override_text).map_err(|error| {
        Failure::run(format!(
            "cannot write the override {}: {error}",
            override_path.display()
        ))
    })?;

    let wait = WAIT_SECS.to_string();
    let started = compose(
        &state_key,
        Some((&root, &manifest_path, &override_path)),
        &["up", "-d", "--wait", "--wait-timeout", &wait],
    );
    if !started.ok {
        return Err(Failure::run(format!(
            "docker compose up failed: {}",
            one_line(&started.text())
        )));
    }

    allocator
        .record(
            &state_key,
            &EpicMeta {
                project: project.clone(),
                epic: key.epic().to_string(),
                dir: root.clone(),
                published: epic::recorded(&published),
            },
            at,
        )
        .map_err(|error| Failure::run(format!("cannot record the box: {error}")))?;

    println!("up {state_key} (project {project}, epic {})", key.epic());
    if let Some(previous) = &previous {
        for change in epic::publish_changes(previous, &published) {
            println!("publish changed: {change}");
        }
    }
    if published.is_empty() {
        println!("  no published roles; `x-msbx.publish` makes a role reachable from the host");
    }
    for port in &published {
        println!(
            "  {:<12} {} → 127.0.0.1:{} → {}  {}",
            port.role,
            key.alias(&port.role),
            port.host_port,
            port.container_port,
            key.container(&port.role)
        );
    }
    Ok(())
}

fn down(options: &Options) -> Result<(), Failure> {
    let selected = select(options)?;
    if selected.is_empty() {
        println!("no box to stop");
        return Ok(());
    }
    let allocator = Allocator::default_path();
    let mut failures = Vec::new();
    for (key, _) in &selected {
        let stopped = compose(key, None, &["stop"]);
        if !stopped.ok {
            failures.push(format!("{key}: {}", one_line(&stopped.text())));
            continue;
        }
        match allocator.touch(key, state::now()) {
            Ok(_) => println!("down {key} (containers stopped; volumes kept)"),
            Err(error) => failures.push(format!("{key}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Failure::run(format!(
            "could not stop: {}",
            failures.join("; ")
        )))
    }
}

fn rm(options: &Options) -> Result<(), Failure> {
    let selected = select(options)?;
    if selected.is_empty() {
        println!("no box to remove");
        return Ok(());
    }
    if !options.yes && !confirm(&selected)? {
        println!("cancelled");
        return Ok(());
    }
    let allocator = Allocator::default_path();
    let mut failures = Vec::new();
    for (key, entry) in &selected {
        let removed = compose(key, None, &["down", "-v", "--remove-orphans"]);
        if !removed.ok {
            failures.push(format!("{key}: {}", one_line(&removed.text())));
            continue;
        }
        match allocator.release(key) {
            Ok(Some(block)) => println!("removed {key} ({block} freed)"),
            Ok(None) => println!(
                "removed {key} (block {} already free)",
                Block { start: entry.start }
            ),
            Err(error) => failures.push(format!("{key}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Failure::run(format!(
            "could not remove: {}",
            failures.join("; ")
        )))
    }
}

/// Ask before destroying boxes. A non-interactive run must pass `--yes`.
fn confirm(selected: &[(String, Entry)]) -> Result<bool, Failure> {
    if !driver::is_interactive() {
        return Err(Failure::Usage(
            "refusing to destroy a box without confirmation; pass --yes".to_string(),
        ));
    }
    for (key, _) in selected {
        eprintln!("remove {key} and its volumes?");
    }
    eprint!("[y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| Failure::run(format!("cannot read the answer: {error}")))?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

fn gc(options: &Options) -> Result<(), Failure> {
    let allocator = Allocator::default_path();
    let at = state::now();
    let state = allocator
        .store()
        .load()
        .map_err(|error| Failure::run(format!("cannot read the state: {error}")))?;
    let stale = allocator
        .release_idle(IDLE_SECS, at, true)
        .map_err(|error| Failure::run(error.to_string()))?;
    if stale.is_empty() {
        println!("nothing to release");
        return Ok(());
    }

    let mut failures = Vec::new();
    for (key, block) in &stale {
        let since = state
            .blocks
            .get(key)
            .map(|entry| age(at.saturating_sub(entry.last_used)))
            .unwrap_or_else(|| "unknown".to_string());
        if options.dry_run {
            println!("would remove {key} ({block}) idle {since}");
            continue;
        }
        let removed = compose(key, None, &["down", "-v", "--remove-orphans"]);
        if !removed.ok {
            failures.push(format!("{key}: {}", one_line(&removed.text())));
            continue;
        }
        match allocator.release_if_idle(key, IDLE_SECS, at) {
            Ok(Some(freed)) => println!("removed {key} ({freed} freed), idle {since}"),
            Ok(None) => println!("kept {key}: touched during gc"),
            Err(error) => failures.push(format!("{key}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Failure::run(format!(
            "could not sweep: {}",
            failures.join("; ")
        )))
    }
}

/// The boxes a verb addresses: every recorded entry, or the one named by
/// `--epic`/`--project`. An epic repeated across projects is an error, because
/// acting on "the" box would be a guess.
fn select(options: &Options) -> Result<Vec<(String, Entry)>, Failure> {
    let state = Allocator::default_path()
        .store()
        .load()
        .map_err(|error| Failure::run(format!("cannot read the state: {error}")))?;
    let epic = options
        .epic
        .as_deref()
        .map(|raw| raw.trim().to_ascii_lowercase());
    let project = options.project.as_deref();

    let mut selected = Vec::new();
    for (key, entry) in state.blocks {
        if let Some(project) = project {
            if box_project(&key, &entry) != Some(project) {
                continue;
            }
        }
        let matches = match &epic {
            None => true,
            Some(epic) => key == epic.as_str() || entry.epic.as_deref() == Some(epic.as_str()),
        };
        if matches {
            selected.push((key, entry));
        }
    }

    if epic.is_some() && selected.len() > 1 {
        let keys: Vec<&str> = selected.iter().map(|(key, _)| key.as_str()).collect();
        return Err(Failure::run(format!(
            "`{}` matches several boxes ({}); pass --project or the full `<project>-<epic>` name",
            options.epic.as_deref().unwrap_or_default(),
            keys.join(", ")
        )));
    }
    Ok(selected)
}

fn box_project<'a>(key: &'a str, entry: &'a Entry) -> Option<&'a str> {
    entry
        .project
        .as_deref()
        .or_else(|| key.rsplit_once('-').map(|(project, _)| project))
}

/// Resolve the manifest and the checkout root. The default is
/// `./.multica/compose.yaml`; the root is the directory above `.multica`, so a
/// relative `build: .` and `.env` resolve in the checkout, not in `.multica`.
fn locate(file: Option<&Path>) -> Result<(PathBuf, PathBuf), Failure> {
    let manifest = match file {
        Some(path) => path.to_path_buf(),
        None => Manifest::path_in(Path::new(".")),
    };
    let manifest = std::fs::canonicalize(&manifest).map_err(|error| {
        Failure::run(format!(
            "cannot read the manifest {}: {error}",
            manifest.display()
        ))
    })?;
    let root = checkout_root(&manifest);
    Ok((manifest, root))
}

fn checkout_root(manifest: &Path) -> PathBuf {
    let dir = manifest.parent().unwrap_or_else(|| Path::new("."));
    if dir.file_name().and_then(|name| name.to_str()) == Some(".multica") {
        dir.parent().unwrap_or(dir).to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

/// A sandbox checkout must live under `$HOME`, and `~/projects` is off limits.
fn within_home(root: &Path) -> Result<(), Failure> {
    let home = state::home();
    let home = std::fs::canonicalize(&home).unwrap_or(home);
    if !root.starts_with(&home) {
        return Err(Failure::run(format!(
            "refusing {}: a sandbox checkout must be under $HOME ({})",
            root.display(),
            home.display()
        )));
    }
    if root.starts_with(home.join("projects")) {
        return Err(Failure::run(format!(
            "refusing {}: ~/projects is off limits",
            root.display()
        )));
    }
    Ok(())
}

fn resolve_project(manifest: &Manifest, explicit: Option<&str>) -> Result<String, Failure> {
    let given = explicit
        .map(str::to_string)
        .or_else(|| std::env::var("MSBX_PROJECT").ok())
        .filter(|project| !project.is_empty());
    match given {
        Some(project) if project != manifest.project => Err(Failure::run(format!(
            "`x-msbx.project` is `{}` but `{project}` was selected; the manifest decides the project",
            manifest.project
        ))),
        _ => Ok(manifest.project.clone()),
    }
}

fn resolve_epic(explicit: Option<&str>) -> Result<String, Failure> {
    let raw = explicit
        .map(str::to_string)
        .or_else(|| std::env::var("MSBX_EPIC").ok())
        .filter(|epic| !epic.is_empty())
        .ok_or_else(|| {
            Failure::run("no epic: pass --epic <E> or set MSBX_EPIC (the wrapper supplies it)")
        })?;
    epic::normalize_epic(&raw).map_err(|error| Failure::run(error.to_string()))
}

/// Run `docker compose` for one project. `files` adds the manifest and the
/// generated override; `down`, `stop` and `ps` work from the project name alone,
/// so they also work after the checkout is gone.
fn compose(key: &str, files: Option<(&Path, &Path, &Path)>, args: &[&str]) -> driver::Output {
    let mut all = vec![
        "compose".to_string(),
        "--ansi".to_string(),
        "never".to_string(),
        "-p".to_string(),
        key.to_string(),
    ];
    if let Some((root, manifest, generated)) = files {
        all.push("--project-directory".to_string());
        all.push(root.to_string_lossy().to_string());
        all.push("-f".to_string());
        all.push(manifest.to_string_lossy().to_string());
        all.push("-f".to_string());
        all.push(generated.to_string_lossy().to_string());
    }
    all.extend(args.iter().map(|arg| arg.to_string()));
    driver::run_owned("docker", &all)
}

#[derive(Debug, Deserialize)]
struct PsContainer {
    #[serde(rename = "Service", default)]
    service: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "State", default)]
    state: String,
    #[serde(rename = "Health", default)]
    health: String,
    #[serde(rename = "ExitCode", default)]
    exit_code: Option<i64>,
    #[serde(rename = "Publishers", default)]
    publishers: Vec<PsPublisher>,
}

#[derive(Debug, Deserialize)]
struct PsPublisher {
    #[serde(rename = "URL", default)]
    url: String,
    #[serde(rename = "TargetPort", default)]
    target_port: u16,
    #[serde(rename = "PublishedPort", default)]
    published_port: u16,
    #[serde(rename = "Protocol", default)]
    protocol: String,
}

#[derive(Debug, Serialize)]
struct StatusReport {
    boxes: Vec<BoxStatus>,
}

#[derive(Debug, Serialize)]
struct BoxStatus {
    project: String,
    epic: String,
    compose_project: String,
    block: BlockStatus,
    last_used: u64,
    dir: Option<String>,
    services: Vec<ServiceStatus>,
}

#[derive(Debug, Serialize)]
struct BlockStatus {
    start: u16,
    end: u16,
}

#[derive(Debug, Serialize)]
struct ServiceStatus {
    role: String,
    container: String,
    state: String,
    health: String,
    exit_code: Option<i64>,
    ports: Vec<PortStatus>,
}

#[derive(Debug, Serialize)]
struct PortStatus {
    name: String,
    host: String,
    published: u16,
    target: Option<u16>,
    protocol: String,
}

fn status(options: &Options) -> Result<(), Failure> {
    let selected = select(options)?;
    let mut boxes = Vec::new();
    for (key, entry) in &selected {
        boxes.push(box_status(key, entry)?);
    }
    if options.json {
        let report = StatusReport { boxes };
        let text = serde_json::to_string_pretty(&report)
            .map_err(|error| Failure::run(format!("cannot render status JSON: {error}")))?;
        println!("{text}");
        return Ok(());
    }
    if boxes.is_empty() {
        println!("no boxes");
        return Ok(());
    }
    let at = state::now();
    for report in &boxes {
        let since = age(at.saturating_sub(report.last_used));
        println!(
            "{}  {}/{}  block {}..={}  idle {}",
            report.compose_project,
            report.project,
            report.epic,
            report.block.start,
            report.block.end,
            since
        );
        if report.services.is_empty() {
            println!("  no containers");
        }
        for service in &report.services {
            let health = if service.health.is_empty() {
                "-"
            } else {
                service.health.as_str()
            };
            let ports = if service.ports.is_empty() {
                "-".to_string()
            } else {
                service
                    .ports
                    .iter()
                    .map(|port| match port.target {
                        Some(target) => format!("{}:{} → {target}", port.host, port.published),
                        None => format!("{}:{}", port.host, port.published),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            println!(
                "  {:<12} {:<9} {:<8} {:<22} {}",
                service.role, service.state, health, ports, service.container
            );
        }
    }
    Ok(())
}

fn box_status(key: &str, entry: &Entry) -> Result<BoxStatus, Failure> {
    let containers = ps_containers(key)?;
    let project = entry.project.clone().unwrap_or_else(|| {
        key.rsplit_once('-')
            .map(|(project, _)| project.to_string())
            .unwrap_or_else(|| key.to_string())
    });
    let epic_key = entry.epic.clone().unwrap_or_default();
    let names = Key::new(&project, &epic_key).ok();

    let mut services: Vec<ServiceStatus> = containers
        .into_iter()
        .map(|container| {
            let role = container.service.clone();
            let mut ports: Vec<PortStatus> = container
                .publishers
                .iter()
                .filter(|publisher| publisher.published_port != 0)
                .map(|publisher| PortStatus {
                    name: names
                        .as_ref()
                        .map(|names| names.alias(&role))
                        .unwrap_or_default(),
                    host: if publisher.url.is_empty() {
                        "127.0.0.1".to_string()
                    } else {
                        publisher.url.clone()
                    },
                    published: publisher.published_port,
                    target: Some(publisher.target_port),
                    protocol: publisher.protocol.clone(),
                })
                .collect();
            if ports.is_empty() {
                ports = recorded_ports(&role, entry, names.as_ref());
            }
            ServiceStatus {
                role,
                container: if container.name.is_empty() {
                    format!("{key}-{}", container.service)
                } else {
                    container.name
                },
                state: if container.state.is_empty() {
                    "unknown".to_string()
                } else {
                    container.state
                },
                health: container.health,
                exit_code: container.exit_code,
                ports,
            }
        })
        .collect();

    // A role recorded as published with no container left still has a port.
    for port in &entry.published {
        if !services.iter().any(|service| service.role == port.role) {
            services.push(ServiceStatus {
                role: port.role.clone(),
                container: format!("{key}-{}", port.role),
                state: "absent".to_string(),
                health: String::new(),
                exit_code: None,
                ports: recorded_ports(&port.role, entry, names.as_ref()),
            });
        }
    }
    services.sort_by(|left, right| left.role.cmp(&right.role));

    Ok(BoxStatus {
        project,
        epic: epic_key,
        compose_project: key.to_string(),
        block: BlockStatus {
            start: entry.start,
            end: Block { start: entry.start }.end(),
        },
        last_used: entry.last_used,
        dir: entry.dir.clone(),
        services,
    })
}

fn recorded_ports(role: &str, entry: &Entry, names: Option<&Key>) -> Vec<PortStatus> {
    entry
        .published
        .iter()
        .filter(|port| port.role == role)
        .map(|port| PortStatus {
            name: names.map(|names| names.alias(role)).unwrap_or_default(),
            host: "127.0.0.1".to_string(),
            published: port.port,
            target: None,
            protocol: "tcp".to_string(),
        })
        .collect()
}

fn ps_containers(key: &str) -> Result<Vec<PsContainer>, Failure> {
    let out = compose(key, None, &["ps", "--all", "--format", "json"]);
    if !out.ok {
        return Err(Failure::run(format!(
            "docker compose ps failed for {key}: {}",
            one_line(&out.text())
        )));
    }
    parse_ps(&out.stdout).map_err(Failure::run)
}

/// `docker compose ps --format json` is a JSON array in current Compose and one
/// JSON object per line in older releases. Accept both.
fn parse_ps(text: &str) -> Result<Vec<PsContainer>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.starts_with('[') {
        return serde_json::from_str(text)
            .map_err(|error| format!("cannot parse `docker compose ps`: {error}"));
    }
    let mut containers = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        containers.push(
            serde_json::from_str(line)
                .map_err(|error| format!("cannot parse `docker compose ps`: {error}"))?,
        );
    }
    Ok(containers)
}

/// `14d 6h`, `3h 12m`, `4m` — how long an entry has been idle.
fn age(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn one_line(text: &str) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    if joined.is_empty() {
        "no output".to_string()
    } else if joined.chars().count() > 240 {
        let mut truncated: String = joined.chars().take(240).collect();
        truncated.push('…');
        truncated
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_accepts_an_array_and_one_object_per_line() {
        let array = r#"[{"Service":"pg","Name":"signals-mir42-pg","State":"running","Health":"healthy","ExitCode":0,"Publishers":[{"URL":"127.0.0.1","TargetPort":5432,"PublishedPort":23000,"Protocol":"tcp"}]}]"#;
        let parsed = parse_ps(array).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].service, "pg");
        assert_eq!(parsed[0].publishers[0].published_port, 23000);

        let lines = concat!(
            r#"{"Service":"pg","State":"running"}"#,
            "\n",
            r#"{"Service":"web","State":"exited"}"#,
            "\n"
        );
        let parsed = parse_ps(lines).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].service, "web");
        assert!(parse_ps("").unwrap().is_empty());
    }

    #[test]
    fn the_checkout_root_is_above_dot_multica() {
        assert_eq!(
            checkout_root(Path::new("/home/me/signals/.multica/compose.yaml")),
            PathBuf::from("/home/me/signals")
        );
        assert_eq!(
            checkout_root(Path::new("/home/me/elsewhere/compose.yaml")),
            PathBuf::from("/home/me/elsewhere")
        );
    }

    #[test]
    fn ages_are_compact() {
        assert_eq!(age(30), "0m");
        assert_eq!(age(4 * 60), "4m");
        assert_eq!(age(3 * 3_600 + 12 * 60), "3h 12m");
        assert_eq!(age(14 * 86_400 + 6 * 3_600), "14d 6h");
    }

    #[test]
    fn values_are_required() {
        let args = vec!["--epic".to_string()];
        assert!(matches!(
            parse(&args, &["--epic"], UP_USAGE),
            Err(Failure::Usage(_))
        ));
        let args = vec!["--json".to_string()];
        assert!(matches!(
            parse(&args, &["--epic"], UP_USAGE),
            Err(Failure::Usage(_))
        ));
    }
}
