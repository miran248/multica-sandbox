//! The Compose lifecycle: `up`, `down`, `rm`, `status`, `gc` and the env verbs.
//!
//! One scope is one Compose project (`<project>-<scope>`). `up` validates the
//! manifest, refuses code in the box, resolves the project's env and secret
//! names from the three value layers, gives the scope its port block, renders
//! the envelope (per-role env files, secret files, the harness env and the host
//! exports) and the override (container names, dotted aliases, loopback ports,
//! env files, secret mounts, tunnel sidecars, network name) into the scope's
//! directory outside the checkout, runs `docker compose up -d --wait` and
//! records what it did. `down` stops the containers and keeps the volumes; `rm`
//! destroys the project, its volumes, envelope, leases and block; `status`
//! reports services, health, ports and tunnelled roles; `gc` is the lifecycle
//! sweep: a final ticket, an idle backstop and `--dry-run`. `env ls` reads the
//! rendered harness envelope and `env export` prints `env/host.env`.
//!
//! Nothing here prints a value from the store: names, roles and paths only.
//! The override is still the only place names, ports and the Compose project
//! are decided.

use crate::driver;
use crate::env::{self, Envelope};
use crate::manifest::Manifest;
use crate::ports::{Allocator, Block, ScopeMeta, CHAT_IDLE_SECS, FIRST_PORT, IDLE_SECS};
use crate::resolve;
use crate::scope::{self, Key};
use crate::state::{self, Entry};
use crate::store::Layers;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// How long `up` waits for the project to be running or healthy.
const WAIT_SECS: u64 = 240;
/// The Multica CLI, when the daemon did not provide one.
const MULTICA: &str = "multica";

const UP_USAGE: &str = "\
usage: msbx up [--scope S] [--project P] [--file F]

Start or reconcile the scope's Compose project. The manifest is
`.multica/compose.yaml` in the current checkout unless `--file` names it.
The scope is `--scope`, else `MSBX_SCOPE`, else the daemon task context, else
the worktree branch; a resolution failure is an error, never a default.
";
const DOWN_USAGE: &str = "\
usage: msbx down [--scope S] [--project P]

Stop the scope's containers and keep its volumes and state. With no `--scope`,
every recorded box is stopped. An absent box is a no-op.
";
const RM_USAGE: &str = "\
usage: msbx rm [--scope S] [--project P] [--yes]

Destroy the scope's project, its volumes, envelope, leases and port block. With
no `--scope`, every recorded box is destroyed. `--yes` skips the confirmation
prompt; a non-interactive run requires it.
";
const STATUS_USAGE: &str = "\
usage: msbx status [--scope S] [--project P] [--json]

Show each recorded box, its services, health and assigned loopback ports.
`--json` is the machine-readable form.
";
const GC_USAGE: &str = "\
usage: msbx gc [--dry-run]

The lifecycle sweep: remove a final ticket box (the issue is done, cancelled,
archived or gone), a chat box idle for 2 days and a ticket box idle for 14 days.
`--dry-run` only lists them.
";
const ENV_USAGE: &str = "\
usage: msbx env ls|export [--scope S] [--project P]

`ls` prints the harness envelope's names without values; `export` prints
`env/host.env`, the host context with the assigned loopback ports. The scope is
`--scope`, else `MSBX_SCOPE`, else the daemon task context, else the worktree
branch; `msbx up` must have rendered the scope first.
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

/// `MSBX_EPIC` is retired; every verb refuses it, naming `MSBX_SCOPE`.
fn no_legacy() -> Result<(), Failure> {
    resolve::refuse_legacy_env().map_err(|error| Failure::Usage(error.to_string()))
}

fn wants_help(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "-h" || arg == "--help")
}

#[derive(Debug, Default)]
struct Options {
    scope: Option<String>,
    project: Option<String>,
    file: Option<PathBuf>,
    json: bool,
    yes: bool,
    dry_run: bool,
}

/// Parse a verb's flags. Every flag that is not allowed for this verb is a
/// usage error instead of being silently ignored; the retired `--epic` names
/// its replacement.
fn parse(args: &[String], allowed: &[&str], usage: &str) -> Result<Options, Failure> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--epic" {
            return Err(Failure::Usage(format!(
                "`--epic` is retired; use `--scope` (a scope is a ticket like `mir-42` or a chat like `chat-1a2b3c4d`)\n\n{usage}"
            )));
        }
        if !allowed.contains(&arg) {
            return Err(Failure::Usage(format!(
                "`{arg}` is not an option here\n\n{usage}"
            )));
        }
        match arg {
            "--scope" => options.scope = Some(value(args, &mut index, arg)?),
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
    let result = no_legacy()
        .and_then(|_| parse(args, &["--scope", "--project", "--file"], UP_USAGE))
        .and_then(|options| up(&options));
    finish("up", result)
}

pub fn run_down(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{DOWN_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = no_legacy()
        .and_then(|_| parse(args, &["--scope", "--project"], DOWN_USAGE))
        .and_then(|options| down(&options));
    finish("down", result)
}

pub fn run_rm(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{RM_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = no_legacy()
        .and_then(|_| parse(args, &["--scope", "--project", "--yes"], RM_USAGE))
        .and_then(|options| rm(&options));
    finish("rm", result)
}

pub fn run_status(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{STATUS_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = no_legacy()
        .and_then(|_| parse(args, &["--scope", "--project", "--json"], STATUS_USAGE))
        .and_then(|options| status(&options));
    finish("status", result)
}

pub fn run_gc(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{GC_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = no_legacy()
        .and_then(|_| parse(args, &["--dry-run"], GC_USAGE))
        .and_then(|options| gc(&options));
    finish("gc", result)
}

pub fn run_env(args: &[String]) -> ExitCode {
    if wants_help(args) {
        print!("{ENV_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = no_legacy().and_then(|_| env_command(args));
    finish("env", result)
}

/// `env ls|export`: read what `up` rendered for one box.
fn env_command(args: &[String]) -> Result<(), Failure> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| Failure::Usage(format!("`env` needs `ls` or `export`\n\n{ENV_USAGE}")))?;
    let export = match verb.as_str() {
        "ls" => false,
        "export" => true,
        other => {
            return Err(Failure::Usage(format!(
                "`{other}` is not an `env` verb (`ls` or `export`)\n\n{ENV_USAGE}"
            )))
        }
    };
    let options = parse(rest, &["--scope", "--project"], ENV_USAGE)?;
    let scope = resolve_scope(options.scope.as_deref())?;
    let selected = select(&Options {
        scope: Some(scope.clone()),
        project: options.project,
        ..Options::default()
    })?;
    let (key, entry) = match selected.as_slice() {
        [only] => only,
        [] => {
            return Err(Failure::run(format!(
                "no box for `{scope}`; run `msbx up` first"
            )))
        }
        _ => unreachable!("select refuses several boxes for one scope"),
    };
    let project = entry.project.as_deref().ok_or_else(|| {
        Failure::run(format!(
            "{key} has no recorded project; run `msbx up` again"
        ))
    })?;
    let scope = entry
        .scope
        .as_deref()
        .ok_or_else(|| Failure::run(format!("{key} has no recorded scope; run `msbx up` again")))?;
    let dir = state::scope_dir(project, scope);

    // `env` activity counts against the idle backstop.
    let _ = Allocator::default_path().touch(key, state::now());

    if export {
        let text = Envelope::read_host(&dir).map_err(|error| Failure::run(error.to_string()))?;
        print!("{text}");
    } else {
        let harness =
            Envelope::read_harness(&dir).map_err(|error| Failure::run(error.to_string()))?;
        for name in harness.keys() {
            println!("{name}");
        }
    }
    Ok(())
}

fn up(options: &Options) -> Result<(), Failure> {
    let (manifest_path, root) = locate(options.file.as_deref())?;
    within_home(&root)?;
    let manifest =
        Manifest::from_path(&manifest_path).map_err(|error| Failure::run(error.to_string()))?;
    manifest
        .check_state_only(&root)
        .map_err(|error| Failure::run(error.to_string()))?;
    let project = resolve_project(&manifest, options.project.as_deref())?;
    let scope = resolve_scope(options.scope.as_deref())?;
    let key = Key::new(&project, &scope).map_err(|error| Failure::run(error.to_string()))?;
    scope::check_publish(&manifest).map_err(|error| Failure::run(error.to_string()))?;

    let allocator = Allocator::default_path();
    let state_key = key.compose_project();
    let at = state::now();

    // A final scope is not restarted: the sweep it would have had happens here.
    if let Finality::Final(reason) = finality(&scope)? {
        let removed = destroy(
            &allocator, &state_key, &project, &scope, true, IDLE_SECS, at,
        );
        let detail = match removed {
            Ok(()) => "its box, volumes, envelope, leases and block were removed",
            Err(error) => return Err(Failure::run(format!(
                "refusing to start `{state_key}`: the scope is final ({reason}); the cleanup failed: {error}"
            ))),
        };
        return Err(Failure::run(format!(
            "refusing to start `{state_key}`: the scope is final ({reason}); {detail}"
        )));
    }

    // Resolve every declared name before a block is spent or a container
    // starts: a missing value, a disallowed host name or a reserved name names
    // the name, the layer and the role and stops `up` here.
    let layers = Layers::for_project(&project).map_err(|error| Failure::run(error.to_string()))?;
    let values = layers
        .values(&manifest)
        .map_err(|error| Failure::run(error.to_string()))?;

    // Render once against a provisional block so an unknown or retired token is
    // refused before a block is spent. The real render follows the allocation.
    let provisional = scope::published(&manifest, Block { start: FIRST_PORT })
        .map_err(|error| Failure::run(error.to_string()))?;
    Envelope::render(&key, &manifest, &provisional, &values)
        .map_err(|error| Failure::run(error.to_string()))?;

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
        scope::published(&manifest, block).map_err(|error| Failure::run(error.to_string()))?;
    let envelope = Envelope::render(&key, &manifest, &published, &values)
        .map_err(|error| Failure::run(error.to_string()))?;
    envelope.write().map_err(|error| {
        Failure::run(format!(
            "cannot write the envelope {}: {error}",
            envelope.dir().display()
        ))
    })?;

    let override_path = key.override_path();
    let override_text = scope::override_yaml(&key, &manifest, &published, &envelope);
    state::write_private(&override_path, &override_text).map_err(|error| {
        Failure::run(format!(
            "cannot write the override {}: {error}",
            override_path.display()
        ))
    })?;

    let wait = WAIT_SECS.to_string();
    let compose_env = env::published_vars(&key, &published);
    let started = compose(
        &state_key,
        Some((&root, &manifest_path, &override_path)),
        &compose_env,
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
            &ScopeMeta {
                project: project.clone(),
                scope: key.scope().to_string(),
                dir: root.clone(),
                published: scope::recorded(&published),
                tunnels: scope::recorded_tunnels(&manifest),
            },
            at,
        )
        .map_err(|error| Failure::run(format!("cannot record the box: {error}")))?;

    println!("up {state_key} (project {project}, scope {})", key.scope());
    if let Some(previous) = &previous {
        for change in scope::publish_changes(previous, &published) {
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
    for (role, tunnel) in &manifest.tunnels {
        let published_note = if published.iter().any(|port| &port.role == role) {
            ""
        } else {
            " (no host port)"
        };
        println!(
            "  tunnel {role:<5} via {} → {}:{}  {}{published_note}",
            tunnel.via.as_str(),
            tunnel.host,
            tunnel.port,
            key.alias(role)
        );
    }
    for (role, names) in envelope.role_env_names() {
        println!("  env {role:<9} {}", names.join(" "));
    }
    for (role, names) in envelope.role_secret_names() {
        println!(
            "  secret {role:<6} {} (read-only at {}/)",
            names.join(" "),
            env::SECRET_MOUNT
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
        let stopped = compose(key, None, &[], &["stop"]);
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
    if options.selected_is_all() && selected.len() > 1 {
        return Err(Failure::Usage(format!(
            "refusing to remove {} boxes without --scope; name the box to destroy",
            selected.len()
        )));
    }
    if !options.yes && !confirm(&selected)? {
        println!("cancelled");
        return Ok(());
    }
    let allocator = Allocator::default_path();
    let mut failures = Vec::new();
    for (key, entry) in &selected {
        let removed = compose(key, None, &[], &["down", "-v", "--remove-orphans"]);
        if !removed.ok {
            failures.push(format!("{key}: {}", one_line(&removed.text())));
            continue;
        }
        if let (Some(project), Some(scope)) = (entry.project.as_deref(), entry.scope.as_deref()) {
            if let Err(error) = remove_artifacts(project, scope) {
                failures.push(format!("{key}: {error}"));
                continue;
            }
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

impl Options {
    /// The verb addresses every box because neither `--scope` nor `MSBX_SCOPE`
    /// names one.
    fn selected_is_all(&self) -> bool {
        self.scope.is_none()
            && std::env::var_os(resolve::SCOPE_ENV).is_none_or(|value| value.is_empty())
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

/// Why a ticket box is final, as far as the tracker is concerned.
enum Finality {
    Live,
    Final(String),
}

/// A chat scope is never final on the tracker; a ticket scope is final when the
/// issue is done, cancelled, archived or gone. "Gone" is only a not-found
/// answer: any other failure of the lookup is a loud error, so a transient CLI
/// or network problem never destroys a box.
fn finality(scope: &str) -> Result<Finality, Failure> {
    if scope.starts_with("chat-") {
        return Ok(Finality::Live);
    }
    let output = Command::new(multica_bin())
        .args(["issue", "get", scope, "--output", "json"])
        .stdin(Stdio::null())
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return Err(Failure::run(format!(
                "cannot check the issue for scope `{scope}`: cannot run `{}`: {error}",
                multica_bin().to_string_lossy()
            )))
        }
    };
    if !output.status.success() {
        if is_not_found(&output) {
            return Ok(Finality::Final("the issue is gone".to_string()));
        }
        return Err(Failure::run(format!(
            "cannot check the issue for scope `{scope}`: `multica issue get` failed: {}",
            one_line(&output_text(&output))
        )));
    }
    let issue: Issue = match serde_json::from_slice(&output.stdout) {
        Ok(issue) => issue,
        // A live but unreadable answer is not a reason to destroy state.
        Err(_) => return Ok(Finality::Live),
    };
    if issue.archived_at.is_some() {
        return Ok(Finality::Final("the issue is archived".to_string()));
    }
    match issue.status_category.as_str() {
        "done" => Ok(Finality::Final("the issue is done".to_string())),
        "cancelled" => Ok(Finality::Final("the issue is cancelled".to_string())),
        _ => Ok(Finality::Live),
    }
}

/// The not-found answer is the only failure that means "the issue is gone".
/// The CLI exits 4 for it; the text is not consulted, because a configuration or
/// auth error can also say "not found" and must not destroy a box.
fn is_not_found(output: &std::process::Output) -> bool {
    output.status.code() == Some(4)
}

fn output_text(output: &std::process::Output) -> String {
    format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

#[derive(Debug, Deserialize)]
struct Issue {
    #[serde(default)]
    status_category: String,
    #[serde(default)]
    archived_at: Option<serde_json::Value>,
}

fn multica_bin() -> PathBuf {
    std::env::var_os("MULTICA_CLI_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(MULTICA))
}

fn gc(options: &Options) -> Result<(), Failure> {
    let allocator = Allocator::default_path();
    let at = state::now();
    let state = allocator
        .store()
        .load()
        .map_err(|error| Failure::run(format!("cannot read the state: {error}")))?;

    let mut plans = Vec::new();
    for (key, entry) in &state.blocks {
        let Some(scope) = entry.scope.as_deref() else {
            // A state file from before the lifecycle verbs: idle only.
            let idle = at.saturating_sub(entry.last_used);
            if idle >= IDLE_SECS {
                plans.push(Plan {
                    key: key.clone(),
                    project: entry.project.clone(),
                    scope: None,
                    block: Block { start: entry.start },
                    idle,
                    final_state: false,
                    reason: format!("idle {}", age(idle)),
                });
            }
            continue;
        };
        let idle = at.saturating_sub(entry.last_used);
        let finality = finality(scope)?;
        let window = if scope.starts_with("chat-") {
            CHAT_IDLE_SECS
        } else {
            IDLE_SECS
        };
        let (final_state, reason) = match finality {
            Finality::Final(reason) => (true, reason),
            Finality::Live if idle >= window => (false, format!("idle {}", age(idle))),
            Finality::Live => continue,
        };
        plans.push(Plan {
            key: key.clone(),
            project: entry.project.clone(),
            scope: Some(scope.to_string()),
            block: Block { start: entry.start },
            idle,
            final_state,
            reason,
        });
    }

    if plans.is_empty() {
        println!("nothing to remove");
        return Ok(());
    }

    let mut failures = Vec::new();
    for plan in &plans {
        if options.dry_run {
            println!("would remove {} ({}) {}", plan.key, plan.block, plan.reason);
            continue;
        }
        let window = if plan
            .scope
            .as_deref()
            .is_some_and(|scope| scope.starts_with("chat-"))
        {
            CHAT_IDLE_SECS
        } else {
            IDLE_SECS
        };
        let project = plan.project.as_deref().unwrap_or_default();
        let scope = plan.scope.as_deref().unwrap_or_default();
        match destroy(
            &allocator,
            &plan.key,
            project,
            scope,
            plan.final_state,
            window,
            at,
        ) {
            Ok(()) => println!("removed {} ({}) {}", plan.key, plan.block, plan.reason),
            Err(error) => failures.push(format!("{}: {error}", plan.key)),
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

struct Plan {
    key: String,
    project: Option<String>,
    scope: Option<String>,
    block: Block,
    #[allow(dead_code)]
    idle: u64,
    final_state: bool,
    reason: String,
}

/// Stop the box, remove its volumes, envelope and leases, and release its block.
/// A final scope releases unconditionally; an idle one re-checks the window so a
/// concurrent `up` keeps its block.
fn destroy(
    allocator: &Allocator,
    key: &str,
    project: &str,
    scope: &str,
    final_state: bool,
    window: u64,
    at: u64,
) -> Result<(), String> {
    let removed = compose(key, None, &[], &["down", "-v", "--remove-orphans"]);
    if !removed.ok {
        return Err(one_line(&removed.text()));
    }
    if !project.is_empty() && !scope.is_empty() {
        remove_artifacts(project, scope)?;
    }
    if final_state {
        allocator
            .release(key)
            .map(|_| ())
            .map_err(|e| e.to_string())
    } else {
        allocator
            .release_if_idle(key, window, at)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// The envelope and the leases live outside the checkout and belong to the box.
fn remove_artifacts(project: &str, scope: &str) -> Result<(), String> {
    for dir in [
        state::scope_dir(project, scope),
        state::leases_dir(project, scope),
    ] {
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot remove {}: {error}", dir.display())),
        }
    }
    Ok(())
}

/// The boxes a verb addresses: every recorded entry, or the one named by
/// `--scope`/`MSBX_SCOPE` and `--project`. A scope repeated across projects is
/// an error, because acting on "the" box would be a guess.
fn select(options: &Options) -> Result<Vec<(String, Entry)>, Failure> {
    let state = Allocator::default_path()
        .store()
        .load()
        .map_err(|error| Failure::run(format!("cannot read the state: {error}")))?;
    let raw = options
        .scope
        .clone()
        .or_else(|| std::env::var(resolve::SCOPE_ENV).ok())
        .filter(|scope| !scope.is_empty());
    let scope = match raw.as_deref() {
        Some(raw) => {
            Some(scope::normalize_scope(raw).map_err(|error| Failure::run(error.to_string()))?)
        }
        None => None,
    };
    let project = options.project.as_deref();

    let mut selected = Vec::new();
    for (key, entry) in state.blocks {
        if let Some(project) = project {
            if entry.project.as_deref() != Some(project) {
                continue;
            }
        }
        let matches = match &scope {
            None => true,
            Some(scope) => key == scope.as_str() || entry.scope.as_deref() == Some(scope.as_str()),
        };
        if matches {
            selected.push((key, entry));
        }
    }

    if scope.is_some() && selected.len() > 1 {
        let keys: Vec<&str> = selected.iter().map(|(key, _)| key.as_str()).collect();
        return Err(Failure::run(format!(
            "`{}` matches several boxes ({}); pass --project or the full `<project>-<scope>` name",
            raw.unwrap_or_default(),
            keys.join(", ")
        )));
    }
    Ok(selected)
}

/// The scope a verb that reads one box addresses: the full resolution order.
fn resolve_scope(explicit: Option<&str>) -> Result<String, Failure> {
    resolve::resolve(explicit).map_err(|error| Failure::run(error.to_string()))
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

/// Run `docker compose` for one project. `files` adds the manifest and the
/// generated override; `envs` is the `MSBX_<ROLE>_HOST`/`_PORT` set a project's
/// Compose file may interpolate, never a store value. `down`, `stop` and `ps`
/// work from the project name alone, so they also work after the checkout is
/// gone.
fn compose(
    key: &str,
    files: Option<(&Path, &Path, &Path)>,
    envs: &[(String, String)],
    args: &[&str],
) -> driver::Output {
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
    driver::run_owned_env("docker", &all, envs)
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
    scope: String,
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
    /// Present for a role served by a generated tunnel sidecar; absent for a
    /// local Compose service.
    #[serde(skip_serializing_if = "Option::is_none")]
    tunnel: Option<TunnelStatus>,
}

#[derive(Debug, Serialize)]
struct TunnelStatus {
    via: String,
    upstream: String,
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
            report.scope,
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
                "  {:<12} {:<11} {:<9} {:<8} {:<22} {}",
                service.role,
                match &service.tunnel {
                    Some(tunnel) => format!("tunnel:{}", tunnel.via),
                    None => "local".to_string(),
                },
                service.state,
                health,
                ports,
                service.container
            );
        }
    }
    Ok(())
}

fn box_status(key: &str, entry: &Entry) -> Result<BoxStatus, Failure> {
    let containers = ps_containers(key)?;
    let project = entry.project.clone().unwrap_or_default();
    let scope_key = entry.scope.clone().unwrap_or_default();
    let names = Key::new(&project, &scope_key).ok();

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
            let tunnel = tunnel_status(entry, &role);
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
                tunnel,
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
                tunnel: tunnel_status(entry, &port.role),
            });
        }
    }
    // A recorded tunnel with no container left is listed, so a sidecar that
    // never started is visible instead of silently missing.
    for tunnel in &entry.tunnels {
        if !services.iter().any(|service| service.role == tunnel.role) {
            services.push(ServiceStatus {
                role: tunnel.role.clone(),
                container: format!("{key}-{}", tunnel.role),
                state: "absent".to_string(),
                health: String::new(),
                exit_code: None,
                ports: recorded_ports(&tunnel.role, entry, names.as_ref()),
                tunnel: tunnel_status(entry, &tunnel.role),
            });
        }
    }
    services.sort_by(|left, right| left.role.cmp(&right.role));

    Ok(BoxStatus {
        project,
        scope: scope_key,
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

/// The recorded tunnel for `role`, when `up` generated one.
fn tunnel_status(entry: &Entry, role: &str) -> Option<TunnelStatus> {
    entry
        .tunnels
        .iter()
        .find(|tunnel| tunnel.role == role)
        .map(|tunnel| TunnelStatus {
            via: tunnel.via.clone(),
            upstream: tunnel.upstream.clone(),
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
    let out = compose(key, None, &[], &["ps", "--all", "--format", "json"]);
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
        let array = r#"[{"Service":"pg","Name":"signals-mir-42-pg","State":"running","Health":"healthy","ExitCode":0,"Publishers":[{"URL":"127.0.0.1","TargetPort":5432,"PublishedPort":23000,"Protocol":"tcp"}]}]"#;
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
        let args = vec!["--scope".to_string()];
        assert!(matches!(
            parse(&args, &["--scope"], UP_USAGE),
            Err(Failure::Usage(_))
        ));
        let args = vec!["--json".to_string()];
        assert!(matches!(
            parse(&args, &["--scope"], UP_USAGE),
            Err(Failure::Usage(_))
        ));
    }

    #[test]
    fn the_retired_flag_names_its_replacement() {
        let args = vec!["--epic".to_string(), "mir-42".to_string()];
        let error = match parse(&args, &["--scope"], UP_USAGE) {
            Err(Failure::Usage(message)) => message,
            _ => panic!("--epic must be refused"),
        };
        assert!(error.contains("`--epic` is retired"), "{error}");
        assert!(error.contains("`--scope`"), "{error}");
    }
}
