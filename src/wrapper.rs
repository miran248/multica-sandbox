//! `msbx-run`: the container shim the Multica runtime profile launches.
//!
//! A task launch resolves the scope and the project, takes a lease, starts or
//! reconciles the scope's box, seeds the agent dir, and runs Pi in one container
//! from the pinned image with stdin, stdout and stderr attached. The container
//! mounts exactly four host paths, each at its identical absolute path:
//!
//! - the task workdir (the shim's cwd), read-write;
//! - `MULTICA_TASK_CONFIG_ROOT`, when the daemon set it;
//! - the directory holding the argv's `--session`, when it names one;
//! - the agent dir at `/home/agent/.pi`.
//!
//! There is no `$HOME`, no `~/.ssh`, no `~/.config`, no Docker socket and no
//! ambient environment: only the daemon's `MULTICA_*` names pass through, with
//! `MULTICA_CLI_PATH` rewritten to the in-image CLI, plus the rendered
//! `env/harness.env` file. A checkout without a manifest still runs in the
//! container, on the default bridge and with no service pairs.
//!
//! The run's exit status is the container's. The lease is released afterwards;
//! the last lease of a lease-managed box stops it with `msbx down` (volumes
//! stay). A killed shim leaves a stale lease and an orphan container, which
//! `gc` (or the next `up` of that scope) sweeps.
//!
//! Discovery never starts a box. `--version` answers from the image pin without
//! Docker; `--probe`, `--list-models` and the Pi RPC discovery args run in a
//! throwaway container from the pinned digest, with `--list-models` cached by
//! digest.

use crate::image::Pin;
use crate::lease::{self, Lease};
use crate::manifest::Manifest;
use crate::resolve;
use crate::scope::Key;
use crate::state;
use std::env as process_env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Output, Stdio};

/// The shim's name in every message it writes.
const NAME: &str = "msbx-run";
/// The CLI baked into the image.
const IN_IMAGE_CLI: &str = "/usr/local/bin/multica";
/// Where the agent dir is mounted.
const AGENT_PI: &str = "/home/agent/.pi";
/// Where a seed container stages the image's baseline.
const SEED_DIR: &str = "/seed";
/// A project key when the checkout has no manifest and no `MSBX_PROJECT`.
const DEFAULT_PROJECT: &str = "workspace";

/// Why a launch did not finish. The message names the scope, the key or the
/// command, never a store value.
#[derive(Debug)]
pub struct Failure(String);

impl Failure {
    fn new(message: impl Into<String>) -> Self {
        Failure(message.into())
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Failure {}

/// The shim entry point.
pub fn run(args: &[OsString]) -> ExitCode {
    if is_version(args) {
        return version();
    }
    let root = match process_env::current_dir() {
        Ok(root) => root,
        Err(error) => return fail(&Failure::new(format!("cannot read the workdir: {error}"))),
    };
    if is_discovery(args) {
        return match probe(args, &root) {
            Ok(code) => code,
            Err(error) => fail(&error),
        };
    }
    match launch(args, &root) {
        Ok(code) => code,
        Err(error) => fail(&error),
    }
}

fn fail(error: &Failure) -> ExitCode {
    eprintln!("{NAME}: {error}");
    ExitCode::FAILURE
}

/// `--version` is the daemon's discovery call, not a task: it answers from the
/// pin and never needs Docker.
fn is_version(args: &[OsString]) -> bool {
    args.len() == 1 && matches!(args[0].to_str(), Some("--version" | "-V"))
}

fn version() -> ExitCode {
    match Pin::load() {
        Ok(pin) => {
            println!("{}", pin.pi);
            ExitCode::SUCCESS
        }
        Err(error) => fail(&Failure::new(error.to_string())),
    }
}

/// Discovery launches: the harness answers a question about itself, so they
/// run in a throwaway container and never resolve a scope or start a box.
fn is_discovery(args: &[OsString]) -> bool {
    let text: Vec<&str> = args.iter().filter_map(|arg| arg.to_str()).collect();
    if text
        .iter()
        .any(|arg| matches!(*arg, "--probe" | "--list-models"))
    {
        return true;
    }
    let rpc = text.iter().enumerate().any(|(index, arg)| {
        *arg == "--mode=rpc" || (*arg == "--mode" && text.get(index + 1).copied() == Some("rpc"))
    });
    rpc && text.contains(&"--no-session")
}

/// Run a discovery argument in a throwaway container from the pinned digest.
/// No scope, box, lease or mount; a throwaway home and the harness store.
fn probe(args: &[OsString], root: &Path) -> Result<ExitCode, Failure> {
    let pin = Pin::load().map_err(|error| Failure::new(error.to_string()))?;
    let list_models = args.iter().any(|arg| arg.to_str() == Some("--list-models"));
    let cache = list_models.then(|| cache_path(&pin));
    if let Some(cache) = &cache {
        if let Ok(text) = fs::read_to_string(cache) {
            if !text.is_empty() {
                print!("{text}");
                return Ok(ExitCode::SUCCESS);
            }
        }
    }

    let mut command = Command::new("docker");
    command.arg("run").arg("--rm").arg("-i");
    if let Some(file) = harness_store() {
        command.arg("--env-file").arg(file);
    }
    push_multica_env(&mut command);
    command.arg(pin.reference());
    command.args(args);
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command
        .output()
        .map_err(|error| Failure::new(format!("cannot run `docker run`: {error}")))?;
    if list_models && output.status.success() && !output.stdout.is_empty() {
        if let Some(cache) = &cache {
            let _ = state::write_private(cache, &String::from_utf8_lossy(&output.stdout));
        }
    }
    Ok(pass_through(&output))
}

fn cache_path(pin: &Pin) -> PathBuf {
    state::cache_dir().join(format!("list-models-{}.txt", pin.digest_slug()))
}

/// The host store the probes read, when it exists.
fn harness_store() -> Option<PathBuf> {
    let path = state::harness_env_file();
    path.is_file().then_some(path)
}

/// One task run.
fn launch(args: &[OsString], root: &Path) -> Result<ExitCode, Failure> {
    let manifest_path = Manifest::path_in(root);
    let manifest = if manifest_path.is_file() {
        Some(Manifest::from_path(&manifest_path).map_err(|error| Failure::new(error.to_string()))?)
    } else {
        None
    };
    let scope = resolve::resolve_in(None, root).map_err(|error| Failure::new(error.to_string()))?;
    let project = project_for(manifest.as_ref(), root)?;
    let key = Key::new(&project, &scope).map_err(|error| Failure::new(error.to_string()))?;
    let pin = Pin::load().map_err(|error| Failure::new(error.to_string()))?;

    let task = task_id();
    let task8 = task_short(&task);
    let container = format!("{}-harness-{task8}", key.compose_project());
    let agent = Agent::detect(root);
    let identity = HostIdentity::detect();
    let mounts = Mounts {
        workdir: root.to_path_buf(),
        config_root: config_root()?,
        session: session_dir(args, root)?,
        agent: state::agent_dir(&agent.id),
    };
    seed_agent_dir(&pin, &agent, identity.as_deref())?;

    // The lease is on disk before the box starts: a crash between the two
    // leaves a claim the next sweep can read.
    let lease = Lease {
        task: task.clone(),
        pid: std::process::id(),
        started_at: state::now(),
        container: container.clone(),
    };
    let mut claim = Claim::take(key.clone(), manifest.is_some(), lease)?;

    let mut network = None;
    let mut harness_env = None;
    if let Some(manifest) = &manifest {
        let up = run_msbx(
            root,
            &[
                "up",
                "--scope",
                &scope,
                "--project",
                &project,
                "--lease",
                &task,
            ],
        )?;
        if !up.status.success() {
            return Err(Failure::new(format!(
                "`msbx up` failed for scope `{scope}`: {}",
                one_line(&output_text(&up))
            )));
        }
        let dir = state::scope_dir(&project, &scope);
        let file = dir.join("env").join("harness.env");
        if !file.is_file() {
            return Err(Failure::new(format!(
                "`msbx up` for scope `{scope}` wrote no harness envelope at {}",
                file.display()
            )));
        }
        harness_env = Some(file);
        if !manifest.services.is_empty() || !manifest.tunnels.is_empty() {
            network = Some(key.network());
        }
    }

    // A container with this run's name can only be a launch that died before
    // it could clean up. Remove it so a retried run id is not a name conflict;
    // the orphan sweep covers every other stale container.
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let mut command = Command::new("docker");
    command
        .arg("run")
        .arg("--rm")
        .arg("-i")
        .arg("--init")
        .arg("--name")
        .arg(&container)
        .arg("--label")
        .arg("com.msbx.harness=true")
        .arg("--label")
        .arg(format!("com.msbx.project={project}"))
        .arg("--label")
        .arg(format!("com.msbx.scope={scope}"))
        .arg("--label")
        .arg(format!("com.msbx.task={task}"))
        .arg("--label")
        .arg(format!(
            "com.msbx.lease={}",
            lease::path(&project, &scope, &task).display()
        ))
        .arg("--workdir")
        .arg(root);
    if let Some(user) = identity.as_deref() {
        command.arg("--user").arg(user);
    }
    if let Some(network) = &network {
        command.arg("--network").arg(network);
    }
    if cfg!(target_os = "linux") {
        command
            .arg("--add-host")
            .arg("host.docker.internal:host-gateway");
    }
    if let Some(file) = &harness_env {
        command.arg("--env-file").arg(file);
    }
    for (source, target) in mounts.pairs().map_err(Failure::new)? {
        command.arg("--volume").arg(format!("{source}:{target}:rw"));
    }
    push_multica_env(&mut command);
    command.arg(pin.reference());
    command.args(args);
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = command.status();
    let code = match status {
        Ok(status) => exit_code(status),
        Err(error) => {
            let _ = claim.release();
            let _ = down_if_last(&key, manifest.is_some());
            return Err(Failure::new(format!(
                "cannot run the harness container: {error}"
            )));
        }
    };

    if let Err(error) = claim.release() {
        eprintln!("{NAME}: cannot release the lease for `{scope}`: {error}");
    }
    if let Err(error) = down_if_last(&key, manifest.is_some()) {
        eprintln!("{NAME}: {error}");
    }
    Ok(code)
}

/// Release the lease and, when it was the last one, stop a lease-managed box.
/// The harness's exit status is preserved even if the stop fails.
fn down_if_last(key: &Key, has_box: bool) -> Result<(), Failure> {
    if !has_box {
        return Ok(());
    }
    let entry = crate::ports::Allocator::default_path()
        .entry(&key.compose_project())
        .map_err(|error| Failure::new(format!("cannot read the box state: {error}")))?;
    if !entry.is_some_and(|entry| entry.lease_managed) {
        return Ok(());
    }
    let _lock = lease::Lock::acquire(key.project(), key.scope())
        .map_err(|error| Failure::new(format!("cannot lock the leases: {error}")))?;
    let live = lease::live(key.project(), key.scope(), state::now())
        .map_err(|error| Failure::new(format!("cannot read the leases: {error}")))?;
    if !live.is_empty() {
        return Ok(());
    }
    let down = run_msbx(
        Path::new("."),
        &["down", "--scope", key.scope(), "--project", key.project()],
    )?;
    if !down.status.success() {
        return Err(Failure::new(format!(
            "`msbx down` failed for scope `{}`: {}",
            key.scope(),
            one_line(&output_text(&down))
        )));
    }
    Ok(())
}

/// The run's claim on its box.
struct Claim {
    key: Key,
    has_box: bool,
    task: String,
    released: bool,
}

impl Claim {
    fn take(key: Key, has_box: bool, lease: Lease) -> Result<Claim, Failure> {
        lease::write(key.project(), key.scope(), &lease).map_err(|error| {
            Failure::new(format!(
                "cannot write the lease for scope `{}`: {error}",
                key.scope()
            ))
        })?;
        Ok(Claim {
            key,
            has_box,
            task: lease.task,
            released: false,
        })
    }

    fn release(&mut self) -> std::io::Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        lease::release(self.key.project(), self.key.scope(), &self.task)
    }
}

impl Drop for Claim {
    /// A launch that fails after taking the lease releases it and, when it was
    /// the last one, stops the box it may have started.
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let _ = lease::release(self.key.project(), self.key.scope(), &self.task);
        let _ = down_if_last(&self.key, self.has_box);
    }
}

/// The four mount sources and their targets.
struct Mounts {
    workdir: PathBuf,
    config_root: Option<PathBuf>,
    session: Option<PathBuf>,
    agent: PathBuf,
}

impl Mounts {
    fn pairs(&self) -> Result<Vec<(String, String)>, String> {
        let mut pairs = Vec::with_capacity(4);
        let workdir = self.workdir.to_string_lossy().to_string();
        pairs.push((workdir.clone(), workdir));
        if let Some(config) = &self.config_root {
            let source = config.to_string_lossy().to_string();
            pairs.push((source.clone(), source));
        }
        if let Some(session) = &self.session {
            let source = session.to_string_lossy().to_string();
            pairs.push((source.clone(), source));
        }
        pairs.push((
            self.agent.to_string_lossy().to_string(),
            AGENT_PI.to_string(),
        ));
        for (source, _) in &pairs {
            if source.contains(':') {
                return Err(format!(
                    "cannot mount `{source}`: a Docker bind source cannot contain `:`"
                ));
            }
        }
        Ok(pairs)
    }
}

/// `MULTICA_TASK_CONFIG_ROOT` when the daemon set it; a path that does not
/// exist is a loud error, because Docker would create a root-owned directory.
fn config_root() -> Result<Option<PathBuf>, Failure> {
    let Some(raw) =
        process_env::var_os("MULTICA_TASK_CONFIG_ROOT").filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| Failure::new(format!("cannot read the workdir: {error}")))?
            .join(path)
    };
    if !path.is_dir() {
        return Err(Failure::new(format!(
            "MULTICA_TASK_CONFIG_ROOT is not a directory: {}",
            path.display()
        )));
    }
    Ok(Some(path))
}

/// The directory holding the argv's `--session` path, mounted at the identical
/// absolute path so Pi's resume re-anchors to the same workdir.
fn session_dir(args: &[OsString], root: &Path) -> Result<Option<PathBuf>, Failure> {
    let mut session = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--session" {
            index += 1;
            session = args.get(index).map(PathBuf::from);
        } else if let Some(value) = arg.to_str().and_then(|arg| arg.strip_prefix("--session=")) {
            session = Some(PathBuf::from(value));
        }
        index += 1;
    }
    let Some(session) = session else {
        return Ok(None);
    };
    let session = if session.is_absolute() {
        session
    } else {
        root.join(session)
    };
    let dir = match session.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => {
            return Err(Failure::new(format!(
                "`--session` names no directory: {}",
                session.display()
            )))
        }
    };
    if !dir.is_dir() {
        return Err(Failure::new(format!(
            "the session directory does not exist: {}",
            dir.display()
        )));
    }
    Ok(Some(dir))
}

/// The project key: the manifest's, else `MSBX_PROJECT`, else a stable key from
/// the checkout's directory name. A `MSBX_PROJECT` that disagrees with the
/// manifest is refused, as `msbx up` refuses it.
fn project_for(manifest: Option<&Manifest>, root: &Path) -> Result<String, Failure> {
    let given = process_env::var("MSBX_PROJECT")
        .ok()
        .filter(|project| !project.is_empty());
    match manifest {
        Some(manifest) => {
            if let Some(given) = given.filter(|given| given != &manifest.project) {
                return Err(Failure::new(format!(
                    "`x-msbx.project` is `{}` but `{given}` was selected; the manifest decides the project",
                    manifest.project
                )));
            }
            Ok(manifest.project.clone())
        }
        None => Ok(given.unwrap_or_else(|| derived_project(root))),
    }
}

fn derived_project(root: &Path) -> String {
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let cleaned: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let cleaned = cleaned.trim_matches('-');
    let cleaned: String = cleaned.chars().take(63).collect();
    if cleaned.is_empty() {
        DEFAULT_PROJECT.to_string()
    } else {
        cleaned
    }
}

/// The run id. The daemon sets `MULTICA_TASK_ID`; a run outside a task gets a
/// PID-scoped id, which is enough for the lease and the container name.
fn task_id() -> String {
    if let Some(task) = process_env::var("MULTICA_TASK_ID")
        .ok()
        .filter(|task| !task.is_empty())
    {
        return task;
    }
    if let Some(task) = process_env::var("MULTICA_RUN_ID")
        .ok()
        .filter(|task| !task.is_empty())
    {
        return task;
    }
    format!("run-{}", std::process::id())
}

fn task_short(task: &str) -> String {
    let short: String = task
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if short.is_empty() {
        format!("{:08}", std::process::id())
    } else {
        short
    }
}

/// The per-agent Pi home. The id is the daemon's agent id when it sets one,
/// else the task-context marker's, else the sanitized agent name.
struct Agent {
    id: String,
    name: String,
}

impl Agent {
    fn detect(root: &Path) -> Agent {
        let context = resolve::task_context_fields(root);
        let name = process_env::var("MULTICA_AGENT_NAME")
            .ok()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "agent".to_string());
        let id = process_env::var("MULTICA_AGENT_ID")
            .ok()
            .filter(|id| !id.is_empty())
            .or_else(|| context.and_then(|context| context.agent_id))
            .unwrap_or_else(|| name.clone());
        let id = sanitize_agent(&id);
        let name = sanitize_agent(&name);
        Agent { id, name }
    }
}

fn sanitize_agent(value: &str) -> String {
    let cleaned: String = value
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        "agent".to_string()
    } else {
        cleaned.chars().take(48).collect()
    }
}

/// Seed `$XDG_STATE_HOME/msbx/agents/<agent-id>/pi` once from the image's
/// baseline plus the role overlay. The seed is staged and renamed, so a failed
/// copy never becomes the agent's permanent home. A baseline change is a manual
/// reset (remove the dir).
fn seed_agent_dir(pin: &Pin, agent: &Agent, identity: Option<&str>) -> Result<(), Failure> {
    let dir = state::agent_dir(&agent.id);
    if dir.is_dir() {
        return Ok(());
    }
    let parent = state::agents_dir(&agent.id);
    state::create_private_dir(&parent).map_err(|error| {
        Failure::new(format!(
            "cannot create the agent dir {}: {error}",
            parent.display()
        ))
    })?;
    let temp = parent.join(format!("pi.tmp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    state::create_private_dir(&temp)
        .map_err(|error| Failure::new(format!("cannot create {}: {error}", temp.display())))?;

    let script = format!(
        "set -e\ncp -a /opt/msbx/agent/. {SEED_DIR}/\nif [ -d '/opt/msbx/agent/roles/{name}' ]; then cp -a '/opt/msbx/agent/roles/{name}'/. {SEED_DIR}/; fi\n",
        name = agent.name
    );
    let mut command = Command::new("docker");
    command.arg("run").arg("--rm");
    if let Some(user) = identity {
        command.arg("--user").arg(user);
    }
    command
        .arg("--entrypoint")
        .arg("/bin/sh")
        .arg("--volume")
        .arg(format!("{}:{SEED_DIR}:rw", temp.display()))
        .arg(pin.reference())
        .arg("-c")
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command
        .output()
        .map_err(|error| Failure::new(format!("cannot seed the agent dir: {error}")))?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&temp);
        return Err(Failure::new(format!(
            "cannot seed the agent dir {}: {}",
            dir.display(),
            one_line(&output_text(&output))
        )));
    }
    let seed = format!("digest {}\nagent {}\n", pin.digest, agent.name);
    if let Err(error) = state::write_private(&temp.join(".msbx-seed"), &seed) {
        let _ = fs::remove_dir_all(&temp);
        return Err(Failure::new(format!(
            "cannot record the agent seed in {}: {error}",
            temp.display()
        )));
    }
    match fs::rename(&temp, &dir) {
        Ok(()) => {
            private_agent_dir(&dir);
            Ok(())
        }
        Err(_) if dir.is_dir() => {
            // Another run seeded it first; this run's copy is redundant.
            let _ = fs::remove_dir_all(&temp);
            private_agent_dir(&dir);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&temp);
            Err(Failure::new(format!(
                "cannot move the agent seed into {}: {error}",
                dir.display()
            )))
        }
    }
}

/// The agent dir is the agent's home: only its owner may read it.
fn private_agent_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// The host UID/GID, so bind-mounted files keep their owner.
struct HostIdentity(Option<String>);

impl HostIdentity {
    fn detect() -> HostIdentity {
        if !cfg!(unix) {
            return HostIdentity(None);
        }
        let uid = id_field("-u");
        let gid = id_field("-g");
        HostIdentity(match (uid, gid) {
            (Some(uid), Some(gid)) => Some(format!("{uid}:{gid}")),
            _ => None,
        })
    }

    fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

fn id_field(flag: &str) -> Option<String> {
    let out = Command::new("id")
        .arg(flag)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !value.is_empty()).then_some(value)
}

/// Pass the daemon's `MULTICA_*` names through without putting their values in
/// argv: `--env NAME` makes Docker copy the value from this process. The CLI
/// path is the image's, not the host's.
fn push_multica_env(command: &mut Command) {
    let mut names: Vec<String> = process_env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| name.starts_with("MULTICA_") && name != "MULTICA_CLI_PATH")
        .collect();
    names.sort();
    for name in names {
        command.arg("--env").arg(name);
    }
    command
        .arg("--env")
        .arg(format!("MULTICA_CLI_PATH={IN_IMAGE_CLI}"));
}

fn run_msbx(root: &Path, args: &[&str]) -> Result<Output, Failure> {
    let program = msbx_bin();
    Command::new(&program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Failure::new(format!(
                "cannot run `{}`: {error}",
                program.to_string_lossy()
            ))
        })
}

fn msbx_bin() -> PathBuf {
    if let Some(path) = process_env::var_os("MSBX_BIN").filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    if let Ok(exe) = process_env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("msbx");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from("msbx")
}

fn exit_code(status: ExitStatus) -> ExitCode {
    match status.code() {
        Some(code) => ExitCode::from(code.clamp(0, 255) as u8),
        None => ExitCode::FAILURE,
    }
}

fn pass_through(output: &Output) -> ExitCode {
    use std::io::Write;
    let _ = std::io::stdout().write_all(&output.stdout);
    let _ = std::io::stderr().write_all(&output.stderr);
    exit_code(output.status)
}

fn output_text(output: &Output) -> String {
    format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
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
    fn a_task_branch_names_its_issue_scope() {
        assert_eq!(
            resolve::scope_from_branch("agent/implementer/mir-261-eebfc37c0c33").as_deref(),
            Some("mir-261")
        );
    }

    #[test]
    fn a_derived_project_is_a_safe_key() {
        assert_eq!(
            derived_project(Path::new("/tmp/Signals API")),
            "signals-api"
        );
        assert_eq!(derived_project(Path::new("/tmp/---")), DEFAULT_PROJECT);
    }

    #[test]
    fn a_task_short_is_eight_safe_characters() {
        assert_eq!(task_short("01a1fe4b-7d52-716f"), "01a1fe4b");
        assert_eq!(task_short("!!!"), format!("{:08}", std::process::id()));
    }

    #[test]
    fn discovery_is_recognized_but_a_task_is_not() {
        let probe = [OsString::from("--list-models")];
        assert!(is_discovery(&probe));
        let rpc = [
            OsString::from("--mode"),
            OsString::from("rpc"),
            OsString::from("--no-session"),
        ];
        assert!(is_discovery(&rpc));
        let task = [
            OsString::from("--model"),
            OsString::from("flash"),
            OsString::from("--session"),
            OsString::from("/state/a.json"),
        ];
        assert!(!is_discovery(&task));
    }
}
