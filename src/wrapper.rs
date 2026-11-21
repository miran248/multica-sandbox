//! `msbx-run`: the harness wrapper the Multica runtime profile launches.
//!
//! The harness still runs on the host. The wrapper only decides what the task
//! is about, starts the epic's box for a manifest-bearing checkout, exports the
//! harness envelope and then hands the process to the real harness with the
//! original arguments and stdin.
//!
//! - A checkout without a sandbox manifest (no `.multica/compose.yaml`, or a
//!   Compose file without `x-msbx`) is passed through untouched.
//! - `--probe` and `--list-models` are harness launches, not task launches:
//!   they never resolve an epic, start a box or touch Docker.
//! - The epic is `MSBX_EPIC`, else the task's issue — from the checkout branch
//!   (`agent/*/mir-<n>-*`) or the daemon task-context marker — and then the
//!   issue's parent epic. The result is cached per task under the wrapper's
//!   private directory in the state tree. A resolution failure is a loud error,
//!   never a default epic.
//! - A failed sandbox step fails the launch; `MSBX_RUN_SOFT=1` prints the
//!   failure and passes the prompt through without the envelope.
//!
//! The harness command is `MSBX_HARNESS`, defaulting to `dsh` on `PATH`. The
//! `msbx` binary is `MSBX_BIN`, else the binary next to this one, else `msbx`
//! on `PATH`. The Multica CLI is `MULTICA_CLI_PATH`, else `multica` on `PATH`.
//! No store value is ever printed, cached or passed in argv.

use crate::env;
use crate::epic;
use crate::manifest::Manifest;
use crate::state;
use serde::Deserialize;
use std::env as process_env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};

/// The wrapper's name in every message it writes.
const NAME: &str = "msbx-run";
/// Harness launches that must never wait on a sandbox.
const PROBE_FLAGS: [&str; 2] = ["--probe", "--list-models"];
/// The harness a profile without `MSBX_HARNESS` gets.
const DEFAULT_HARNESS: &str = "dsh";
/// The private cache directory, under `$XDG_STATE_HOME/msbx`.
const CACHE_DIR: &str = ".wrapper";

/// Why the sandbox step did not finish. The message names the epic, the key or
/// the command, never a value.
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

/// What `multica issue get --output json` gives the resolver.
#[derive(Debug, Deserialize)]
struct Issue {
    identifier: String,
    #[serde(default)]
    parent_issue_id: Option<String>,
}

/// The wrapper entry point. Returns only when the harness could not be
/// launched; a successful launch is an `exec` and never comes back.
pub fn run(args: &[OsString]) -> ExitCode {
    let harness = harness_command();
    if let Some(problem) = self_reference(&harness) {
        eprintln!("{NAME}: {problem}");
        return ExitCode::FAILURE;
    }

    if is_probe(args) {
        return exec_harness(&harness, args, &[]);
    }
    if !Path::new(".multica").join("compose.yaml").is_file() {
        return exec_harness(&harness, args, &[]);
    }

    match sandbox() {
        Ok(Some(exports)) => exec_harness(&harness, args, &exports),
        Ok(None) => exec_harness(&harness, args, &[]),
        Err(error) if soft() => {
            eprintln!("{NAME}: {error}");
            eprintln!("{NAME}: MSBX_RUN_SOFT=1: launching the harness without the sandbox");
            exec_harness(&harness, args, &[])
        }
        Err(error) => {
            eprintln!("{NAME}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `MSBX_RUN_SOFT=1` turns a sandbox failure into a warning. Anything else is a
/// hard failure so a broken launch is visible.
fn soft() -> bool {
    matches!(process_env::var("MSBX_RUN_SOFT"), Ok(value) if value == "1")
}

/// A harness launch that never starts a box: the flag is a harness question,
/// not a task.
fn is_probe(args: &[OsString]) -> bool {
    args.iter()
        .filter_map(|arg| arg.to_str())
        .any(|arg| PROBE_FLAGS.contains(&arg))
}

fn harness_command() -> OsString {
    process_env::var_os("MSBX_HARNESS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from(DEFAULT_HARNESS))
}

/// Refuse to exec ourselves: a profile that points both the command and the
/// harness at the wrapper would recurse until the runtime gives up.
fn self_reference(harness: &OsStr) -> Option<String> {
    let me = process_env::current_exe().ok()?;
    let me = fs::canonicalize(&me).unwrap_or(me);
    let found = find_on_path(harness)?;
    let found = fs::canonicalize(&found).unwrap_or(found);
    (found == me).then(|| {
        format!(
            "refusing to launch itself as the harness (`{}`); set MSBX_HARNESS to the real harness",
            harness.to_string_lossy()
        )
    })
}

fn find_on_path(program: &OsStr) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    let dirs = process_env::var_os("PATH")?;
    process_env::split_paths(&dirs)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Resolve the project and the epic, start the box, and return the harness
/// envelope the last `up` rendered. `None` means the checkout's Compose file
/// has no `x-msbx` block: it is not a sandbox manifest and the launch passes
/// through.
fn sandbox() -> Result<Option<Vec<(String, String)>>, Failure> {
    let root = process_env::current_dir()
        .map_err(|error| Failure::new(format!("cannot read the checkout directory: {error}")))?;
    let manifest = match Manifest::from_repo(&root) {
        Ok(manifest) => manifest,
        Err(error) if error.is_missing_manifest() => return Ok(None),
        Err(error) => return Err(Failure::new(error.to_string())),
    };
    let epic = resolve_epic(&root)?;
    let msbx = msbx_bin();

    let started = capture(
        &msbx,
        &[
            "up",
            "--epic",
            &epic,
            "--project",
            manifest.project.as_str(),
        ],
        &root,
    )?;
    if !started.status.success() {
        return Err(Failure::new(format!(
            "`msbx up` failed for epic `{epic}`: {}",
            one_line(&started)
        )));
    }

    let exported = capture(
        &msbx,
        &[
            "env",
            "export",
            "--epic",
            &epic,
            "--project",
            &manifest.project,
        ],
        &root,
    )?;
    if !exported.status.success() {
        return Err(Failure::new(format!(
            "`msbx env export` failed for epic `{epic}`: {}",
            one_line(&exported)
        )));
    }
    let stdout = String::from_utf8_lossy(&exported.stdout);
    let exports = env::parse_exports(&stdout).map_err(|error| {
        Failure::new(format!(
            "`msbx env export` printed an envelope this wrapper cannot read: {error}"
        ))
    })?;
    Ok(Some(exports))
}

/// Run `msbx` with a closed stdin, so the prompt the harness will read stays
/// the harness's. Output is captured: the wrapper's stdout belongs to the
/// harness protocol and must not carry `up`'s summary.
fn capture(program: &Path, args: &[&str], root: &Path) -> Result<Output, Failure> {
    Command::new(program)
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

fn one_line(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    if text.is_empty() {
        "no output".to_string()
    } else {
        text
    }
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

/// The epic this task belongs to. `MSBX_EPIC` is taken as given (and refused if
/// it is not a valid key); otherwise the task names the issue — from the
/// checkout branch or the daemon's task-context marker — and the issue's parent
/// names the epic.
fn resolve_epic(root: &Path) -> Result<String, Failure> {
    if let Some(raw) = process_env::var_os("MSBX_EPIC").filter(|value| !value.is_empty()) {
        let raw = raw.to_str().ok_or_else(|| {
            Failure::new("MSBX_EPIC is not valid UTF-8; set it to a `[a-z0-9-]+` epic key")
        })?;
        return epic::normalize_epic(raw)
            .map_err(|error| Failure::new(format!("MSBX_EPIC is not a valid epic: {error}")));
    }

    let branch = branch(root);
    let cache = cache_key(branch.as_deref());
    if let Some(cached) = cache.as_deref().and_then(read_cache) {
        return Ok(cached);
    }

    let reference = branch
        .as_deref()
        .and_then(issue_number)
        .map(|number| format!("mir-{number}"))
        .or_else(|| task_context_issue(root))
        .ok_or_else(|| {
            Failure::new(format!(
                "cannot resolve the epic: branch `{}` is not an `agent/*/mir-<n>-*` branch and no task-context issue marker was found; set MSBX_EPIC",
                branch.as_deref().unwrap_or("<detached>")
            ))
        })?;

    let resolved = epic_from_issue(&reference)?;
    if let Some(path) = cache.as_deref() {
        // The cache is an optimization; a read-only state directory must not
        // stop the launch.
        let _ = state::write_private(path, &format!("{resolved}\n"));
    }
    Ok(resolved)
}

/// The checkout's branch, if it is on one. A detached HEAD is not an error on
/// its own: the daemon's task-context marker still names the issue.
fn branch(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// `agent/<role>/mir-<n>-<slug>` → `n`. The branch is how the runtime records
/// the task, so it names the issue, not the epic.
fn issue_number(branch: &str) -> Option<u64> {
    let mut parts = branch.split('/');
    if parts.next()? != "agent" {
        return None;
    }
    if parts.next()?.is_empty() {
        return None;
    }
    let task = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let rest = task.strip_prefix("mir-")?;
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    let tail = &rest[digits.len()..];
    if !(tail.is_empty() || tail.starts_with('-')) {
        return None;
    }
    digits.parse().ok()
}

/// The daemon's task-context marker, beside or above the checkout. It carries
/// the issue id when the checkout branch is the task's own
/// `agent/<role>/<task-slot>` branch rather than an `agent/*/mir-<n>-*` one.
#[derive(Debug, Deserialize)]
struct TaskContext {
    #[serde(default)]
    managed_by: String,
    #[serde(default)]
    issue_id: Option<String>,
}

fn task_context_issue(root: &Path) -> Option<String> {
    for dir in root.ancestors().take(4) {
        let path = dir.join(".multica").join("daemon_task_context.json");
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(context) = serde_json::from_str::<TaskContext>(&text) else {
            continue;
        };
        if context.managed_by != "multica-daemon-task" {
            continue;
        }
        if let Some(issue) = context.issue_id.filter(|issue| !issue.is_empty()) {
            return Some(issue);
        }
    }
    None
}

/// The resolution cache is per task, so a model round never re-queries the
/// issue tree. Without a task id, the branch stands in for the task.
fn cache_key(branch: Option<&str>) -> Option<PathBuf> {
    let task = process_env::var("MULTICA_TASK_ID")
        .ok()
        .filter(|task| !task.is_empty());
    cache_key_for(task.as_deref(), branch)
}

fn cache_key_for(task: Option<&str>, branch: Option<&str>) -> Option<PathBuf> {
    let raw = task.or(branch)?;
    let safe = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        return None;
    }
    Some(
        state::state_dir()
            .join(CACHE_DIR)
            .join(format!("{safe}.epic")),
    )
}

fn read_cache(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    epic::normalize_epic(text.trim()).ok()
}

fn epic_from_issue(reference: &str) -> Result<String, Failure> {
    let multica = multica_bin();
    let issue = issue_get(&multica, reference)?;
    let epic_issue = match issue.parent_issue_id.as_deref() {
        Some(parent) if !parent.is_empty() => issue_get(&multica, parent)?,
        // A task on the epic itself has no parent; the issue is the epic.
        _ => issue,
    };
    epic::normalize_epic(&epic_issue.identifier).map_err(|error| {
        Failure::new(format!(
            "cannot resolve the epic: `{}` is not a valid epic key: {error}; set MSBX_EPIC",
            epic_issue.identifier
        ))
    })
}

fn multica_bin() -> PathBuf {
    process_env::var_os("MULTICA_CLI_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("multica"))
}

fn issue_get(multica: &Path, reference: &str) -> Result<Issue, Failure> {
    let output = Command::new(multica)
        .args(["issue", "get", reference, "--output", "json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Failure::new(format!(
                "cannot resolve the epic: cannot run `{}`: {error}; set MSBX_EPIC",
                multica.to_string_lossy()
            ))
        })?;
    if !output.status.success() {
        return Err(Failure::new(format!(
            "cannot resolve the epic: `multica issue get {reference}` failed: {}; set MSBX_EPIC",
            one_line(&output)
        )));
    }
    serde_json::from_slice::<Issue>(&output.stdout).map_err(|error| {
        Failure::new(format!(
            "cannot resolve the epic: `multica issue get {reference}` returned no usable issue: {error}; set MSBX_EPIC"
        ))
    })
}

/// Replace the wrapper process with the harness. Arguments, stdin, stdout,
/// stderr and the exit status are the harness's, not the wrapper's.
fn exec_harness(harness: &OsStr, args: &[OsString], exports: &[(String, String)]) -> ExitCode {
    let mut command = Command::new(harness);
    command.args(args);
    for (name, value) in exports {
        command.env(name, value);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!(
            "{NAME}: cannot launch the harness `{}`: {error}; set MSBX_HARNESS to the real harness",
            harness.to_string_lossy()
        );
        ExitCode::FAILURE
    }

    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => match status.code() {
                Some(code) => ExitCode::from(code as u8),
                None => ExitCode::FAILURE,
            },
            Err(error) => {
                eprintln!(
                    "{NAME}: cannot launch the harness `{}`: {error}; set MSBX_HARNESS to the real harness",
                    harness.to_string_lossy()
                );
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_issue_number_from_a_task_branch() {
        assert_eq!(
            issue_number("agent/implementer/mir-261-eebfc37c0c33"),
            Some(261)
        );
        assert_eq!(issue_number("agent/implementer/mir-7"), Some(7));
        assert_eq!(issue_number("agent/implementer/mir-7-x"), Some(7));
    }

    #[test]
    fn refuses_branches_that_not_name_an_issue() {
        assert_eq!(issue_number("main"), None);
        assert_eq!(issue_number("agent/implementer/eebfc37c0c33"), None);
        assert_eq!(issue_number("agent/implementer/mir-x-abcd"), None);
        assert_eq!(issue_number("agent/implementer/mir-12x-abcd"), None);
        assert_eq!(issue_number("agent/implementer/mir-12/extra"), None);
        assert_eq!(issue_number("feature/agent/implementer/mir-12-x"), None);
    }

    #[test]
    fn prefers_the_task_id_and_falls_back_to_the_branch() {
        assert_eq!(
            cache_key_for(Some("task-abc"), Some("agent/implementer/mir-261-x"))
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "task-abc.epic"
        );
        assert_eq!(
            cache_key_for(None, Some("agent/implementer/mir-261-eebfc37c0c33"))
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "agent-implementer-mir-261-eebfc37c0c33.epic"
        );
        assert!(cache_key_for(None, None).is_none());
    }
}
