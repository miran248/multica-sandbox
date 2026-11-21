//! Leases and the orphan sweep.
//!
//! A lease is one run's claim on its scope's box. The shim writes
//! `$XDG_STATE_HOME/msbx/leases/<project>/<scope>/<task>.json` before it starts
//! the box, refreshes nothing while the container runs, and removes the file
//! when the container exits or the launch fails. A lease is stale when its host
//! PID is gone or it is older than 24 hours.
//!
//! `up --lease <task>` records that the box is lease-managed; the last lease to
//! leave stops the box with `msbx down` (volumes stay). Sibling runs of one
//! scope share the box, so only the last one stops it.
//!
//! `gc` (and `up` for its own scope) also removes harness containers labelled
//! `com.msbx.harness=true` whose lease is stale or gone — the container a
//! killed shim left behind. Probes carry no such label, so the sweep never
//! races a discovery call.

use crate::driver;
use crate::state;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// A lease older than this is stale even when its PID still exists (a reused
/// PID must not pin a box forever).
pub const STALE_AFTER: u64 = 24 * 60 * 60;
/// How long the last-lease check waits for another shim's lock.
const LOCK_WAIT: Duration = Duration::from_secs(3);
/// A lock older than this is treated as a crash, not a live writer.
const STALE_LOCK: Duration = Duration::from_secs(60);

/// The label every task harness container carries.
pub const HARNESS_LABEL: &str = "com.msbx.harness";
/// The label naming the lease file a harness container belongs to.
pub const LEASE_LABEL: &str = "com.msbx.lease";
/// The label naming the box's project.
pub const PROJECT_LABEL: &str = "com.msbx.project";
/// The label naming the box's scope.
pub const SCOPE_LABEL: &str = "com.msbx.scope";
/// The label naming the run.
pub const TASK_LABEL: &str = "com.msbx.task";

/// One run's claim on its box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub task: String,
    pub pid: u32,
    pub started_at: u64,
    pub container: String,
}

/// `$XDG_STATE_HOME/msbx/leases/<project>/<scope>`.
pub fn dir(project: &str, scope: &str) -> PathBuf {
    state::leases_dir(project, scope)
}

/// The lease file for one task.
pub fn path(project: &str, scope: &str, task: &str) -> PathBuf {
    dir(project, scope).join(format!("{}.json", file_stem(task)))
}

/// Write (or refresh) the lease for this run. Called before the box starts so a
/// crash between the two leaves a claim the next sweep can read.
pub fn write(project: &str, scope: &str, lease: &Lease) -> io::Result<PathBuf> {
    let path = path(project, scope, &lease.task);
    let body = serde_json::to_string_pretty(lease)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    state::write_private(&path, &format!("{body}\n"))?;
    Ok(path)
}

/// Remove one run's lease. An absent file is not an error.
pub fn release(project: &str, scope: &str, task: &str) -> io::Result<()> {
    match fs::remove_file(path(project, scope, task)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Live leases for one box, dropping stale claims from disk as a side effect.
pub fn live(project: &str, scope: &str, at: u64) -> io::Result<Vec<Lease>> {
    live_in(&dir(project, scope), at)
}

fn live_in(dir: &Path, at: u64) -> io::Result<Vec<Lease>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut leases = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        match read(&path) {
            Some(lease) if !stale(&lease, at) => leases.push(lease),
            // An unreadable file is a shim killed between create and write;
            // a stale lease is a dead run. Both are dropped.
            _ => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    leases.sort_by(|left, right| left.task.cmp(&right.task));
    Ok(leases)
}

/// Drop the stale claims for one box and name the tasks that were swept.
pub fn sweep(project: &str, scope: &str, at: u64) -> io::Result<Vec<String>> {
    sweep_in(&dir(project, scope), at)
}

fn sweep_in(dir: &Path, at: u64) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut dropped = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let keep = read(&path).is_some_and(|lease| !stale(&lease, at));
        if !keep {
            let task = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_default();
            let _ = fs::remove_file(&path);
            dropped.push(task);
        }
    }
    dropped.sort();
    Ok(dropped)
}

/// The task names with a stale claim, without removing anything. `gc
/// --dry-run` uses it to list what the sweep would drop.
pub fn stale_tasks(project: &str, scope: &str, at: u64) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir(project, scope)) else {
        return Vec::new();
    };
    let mut tasks: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .filter(|path| !read(path).is_some_and(|lease| !stale(&lease, at)))
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .collect();
    tasks.sort();
    tasks
}

/// Read a lease without dropping anything. `status` uses this: it reports, it
/// changes nothing.
pub fn list(project: &str, scope: &str, at: u64) -> Vec<Lease> {
    list_in(&dir(project, scope), at)
}

fn list_in(dir: &Path, at: u64) -> Vec<Lease> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut leases: Vec<Lease> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .filter_map(|path| read(&path))
        .filter(|lease| !stale(lease, at))
        .collect();
    leases.sort_by(|left, right| left.task.cmp(&right.task));
    leases
}

/// Is this claim stale? A gone host PID or a 24-hour-old file each suffice.
pub fn stale(lease: &Lease, at: u64) -> bool {
    at.saturating_sub(lease.started_at) > STALE_AFTER || !pid_alive(lease.pid)
}

fn read(path: &Path) -> Option<Lease> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // `kill -0` is the portable probe: it asks the kernel whether the process
    // exists without sending a signal. A missing `kill` is treated as alive so
    // a host without procps never loses a live claim.
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// Exclusive lock for the "is this the last lease?" decision. Two shims that
/// exit together otherwise both see one remaining lease and neither stops the
/// box (or both stop it).
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn acquire(project: &str, scope: &str) -> io::Result<Lock> {
        let dir = dir(project, scope);
        state::create_private_dir(&dir)?;
        let path = dir.join(".lock");
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Lock { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!("{} is locked by another shim", path.display()),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age > STALE_LOCK)
        .unwrap_or(false)
}

/// The file stem for a task id. A task id is a UUID, but the path is ours to
/// keep safe: anything outside `[A-Za-z0-9_-]` becomes `-`.
fn file_stem(task: &str) -> String {
    let cleaned: String = task
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "run".to_string()
    } else {
        cleaned
    }
}

/// Remove harness containers whose lease is gone or stale. `filter` limits the
/// sweep to one box, which is what `up` uses; `None` is the full `gc` sweep.
/// With `dry_run`, nothing is removed and the would-be names are returned.
pub fn sweep_containers(
    filter: Option<(&str, &str)>,
    dry_run: bool,
) -> Result<Vec<String>, String> {
    let mut args = vec![
        "ps".to_string(),
        "-aq".to_string(),
        "--filter".to_string(),
        format!("label={HARNESS_LABEL}=true"),
    ];
    if let Some((project, scope)) = filter {
        args.push("--filter".to_string());
        args.push(format!("label={PROJECT_LABEL}={project}"));
        args.push("--filter".to_string());
        args.push(format!("label={SCOPE_LABEL}={scope}"));
    }
    let listed = driver::run_owned("docker", &args);
    if !listed.ok {
        return Err(listed.text());
    }
    let at = state::now();
    let mut swept = Vec::new();
    for id in listed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        // An inspect failure means "unknown", not "orphan": never kill a
        // container because the daemon hiccuped.
        let Some(lease_path) = inspect(id, LEASE_LABEL) else {
            continue;
        };
        let live = if lease_path.is_empty() {
            false
        } else {
            match read(Path::new(&lease_path)) {
                Some(lease) => !stale(&lease, at),
                // A lease file that exists but cannot be read right now is
                // unknown too; do not race a live run.
                None if Path::new(&lease_path).exists() => continue,
                None => false,
            }
        };
        if live {
            continue;
        }
        let name = inspect(id, ".Name").unwrap_or_else(|| id.to_string());
        if dry_run {
            swept.push(name);
            continue;
        }
        let removed = driver::run("docker", &["rm", "-f", id]);
        if !removed.ok {
            return Err(format!("cannot remove {name}: {}", removed.text()));
        }
        swept.push(name);
    }
    Ok(swept)
}

fn inspect(id: &str, what: &str) -> Option<String> {
    let format = if what == ".Name" {
        "{{.Name}}".to_string()
    } else {
        format!("{{{{ index .Config.Labels \"{what}\" }}}}")
    };
    let out = driver::run("docker", &["inspect", "--format", &format, id]);
    if !out.ok {
        return None;
    }
    Some(out.stdout.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("lease-{}-{tag}", std::process::id()))
    }

    #[test]
    fn a_gone_pid_makes_a_lease_stale() {
        let lease = Lease {
            task: "t".into(),
            pid: 4_200_000,
            started_at: state::now(),
            container: "c".into(),
        };
        assert!(stale(&lease, state::now()));
    }

    #[test]
    fn a_24_hour_old_lease_is_stale_even_with_a_live_pid() {
        let lease = Lease {
            task: "t".into(),
            pid: std::process::id(),
            started_at: 1,
            container: "c".into(),
        };
        assert!(stale(&lease, 1 + STALE_AFTER + 1));
        assert!(!stale(&lease, 2));
    }

    #[test]
    fn live_drops_a_stale_claim_and_keeps_a_current_one() {
        let root = scratch("live");
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("project").join("mir-42");
        state::create_private_dir(&dir).unwrap();
        let at = state::now();
        let current = Lease {
            task: "here".into(),
            pid: std::process::id(),
            started_at: at,
            container: "c".into(),
        };
        let gone = Lease {
            task: "gone".into(),
            pid: 4_200_000,
            started_at: at,
            container: "c".into(),
        };
        for lease in [&current, &gone] {
            let body = serde_json::to_string(lease).unwrap();
            state::write_private(&dir.join(format!("{}.json", lease.task)), &body).unwrap();
        }
        let kept = live_in(&dir, at).unwrap();
        assert_eq!(kept, vec![current]);
        assert!(
            !dir.join("gone.json").exists(),
            "the stale claim is dropped"
        );
        assert!(dir.join("here.json").exists());
        assert_eq!(list_in(&dir, at).len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_task_id_stays_a_safe_file_stem() {
        assert_eq!(file_stem("01a1fe4b-7d52"), "01a1fe4b-7d52");
        assert_eq!(file_stem("../nope"), "---nope");
        assert_eq!(file_stem(""), "run");
    }
}
