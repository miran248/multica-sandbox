//! `msbx-run`: the harness wrapper the Multica runtime profile launches.
//!
//! The harness still runs on the host in this stage. The wrapper only decides
//! what the task's scope is, starts that scope's box for a manifest-bearing
//! checkout, exports the host envelope and then hands the process to the real
//! harness with the original arguments and stdin.
//!
//! - A checkout without a sandbox manifest (no `.multica/compose.yaml`, or a
//!   Compose file without `x-msbx`) is passed through untouched.
//! - `--probe` and `--list-models` are harness launches, not task launches:
//!   they never resolve a scope, start a box or touch Docker.
//! - The scope is `MSBX_SCOPE` (refused if invalid), else the daemon task
//!   context (the issue id, or the chat session), else the worktree branch.
//!   There is no parent lookup and no silent default.
//! - A failed sandbox step fails the launch; `MSBX_RUN_SOFT=1` prints the
//!   failure and passes the prompt through without the envelope.
//!
//! The harness command is `MSBX_HARNESS`, defaulting to `dsh` on `PATH`. The
//! `msbx` binary is `MSBX_BIN`, else the binary next to this one, else `msbx`
//! on `PATH`. The Multica CLI is `MULTICA_CLI_PATH`, else `multica` on `PATH`.
//! No store value is ever printed, cached or passed in argv.

use crate::env;
use crate::manifest::Manifest;
use crate::resolve;
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

/// Why the sandbox step did not finish. The message names the scope, the key or
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

/// Resolve the scope, start the box, and return the host envelope the last `up`
/// rendered. `None` means the checkout's Compose file has no `x-msbx` block: it
/// is not a sandbox manifest and the launch passes through.
fn sandbox() -> Result<Option<Vec<(String, String)>>, Failure> {
    let root = process_env::current_dir()
        .map_err(|error| Failure::new(format!("cannot read the checkout directory: {error}")))?;
    let manifest = match Manifest::from_repo(&root) {
        Ok(manifest) => manifest,
        Err(error) if error.is_missing_manifest() => return Ok(None),
        Err(error) => return Err(Failure::new(error.to_string())),
    };
    let scope =
        resolve::resolve_in(None, &root).map_err(|error| Failure::new(error.to_string()))?;
    let msbx = msbx_bin();

    let started = capture(
        &msbx,
        &[
            "up",
            "--scope",
            &scope,
            "--project",
            manifest.project.as_str(),
        ],
        &root,
    )?;
    if !started.status.success() {
        return Err(Failure::new(format!(
            "`msbx up` failed for scope `{scope}`: {}",
            one_line(&started)
        )));
    }

    let exported = capture(
        &msbx,
        &[
            "env",
            "export",
            "--scope",
            &scope,
            "--project",
            &manifest.project,
        ],
        &root,
    )?;
    if !exported.status.success() {
        return Err(Failure::new(format!(
            "`msbx env export` failed for scope `{scope}`: {}",
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
            resolve::scope_from_branch("agent/implementer/mir-261-eebfc37c0c33").as_deref(),
            Some("mir-261")
        );
    }
}
