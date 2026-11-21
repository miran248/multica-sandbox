//! The state file: what `msbx` has allocated on this host.
//!
//! The file lives outside every checkout, under
//! `$XDG_STATE_HOME/msbx/state.json` (default `~/.local/state/msbx/`), and is
//! written atomically under a lock file. An entry records the scope's port block
//! and, once `up` has run, the keys, checkout and published ports it was
//! started with; `down` refreshes it and `rm` and `gc` remove it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Schema version of the state file.
pub const VERSION: u32 = 1;

/// A lock older than this is treated as a crash, not a live writer.
const STALE_LOCK: Duration = Duration::from_secs(60);
/// How long a writer waits for another one before giving up.
const LOCK_WAIT: Duration = Duration::from_secs(3);

/// The on-disk state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub blocks: BTreeMap<String, Entry>,
}

/// One scope's allocation. `assigned` is when the block was first recorded;
/// `last_used` is when `msbx` last touched the scope and is what `gc` reads.
///
/// The remaining fields are written by `up`: the two keys, the checkout it was
/// started from, the role → host-port mapping the block was spent on and the
/// generated tunnel sidecars. They are optional so a state file from before the
/// lifecycle verbs still loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub start: u16,
    pub assigned: u64,
    pub last_used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(default)]
    pub published: Vec<PublishedPort>,
    #[serde(default)]
    pub tunnels: Vec<TunnelRecord>,
    /// Whether a lease started this box. A manual `up` leaves it false, so the
    /// last shim lease does not stop a box a human started.
    #[serde(default)]
    pub lease_managed: bool,
}

/// A role published from the host and the block port it was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedPort {
    pub role: String,
    pub port: u16,
}

/// A role `msbx` serves with a generated tunnel sidecar, as `up` recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelRecord {
    pub role: String,
    pub via: String,
    pub upstream: String,
}

impl State {
    fn new() -> Self {
        State {
            version: VERSION,
            blocks: BTreeMap::new(),
        }
    }
}

/// `$HOME`, or `.` when the process has no home. Every path this crate writes
/// is derived from it; the store and the state are deliberately outside the
/// checkout.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `$XDG_STATE_HOME/msbx`, defaulting to `~/.local/state/msbx`.
pub fn state_dir() -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("msbx"),
        _ => home().join(".local").join("state").join("msbx"),
    }
}

/// The default state file.
pub fn state_file() -> PathBuf {
    state_dir().join("state.json")
}

/// `$XDG_STATE_HOME/msbx/<project>/<scope>` — the scope's envelope directory,
/// outside every checkout. The generated Compose override lives here.
pub fn scope_dir(project: &str, scope: &str) -> PathBuf {
    state_dir().join(project).join(scope)
}

/// `$XDG_CONFIG_HOME/msbx`, defaulting to `~/.config/msbx`. Configuration, not
/// state: it holds the project value stores and survives `rm` and `gc`.
pub fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("msbx"),
        _ => home().join(".config").join("msbx"),
    }
}

/// `$XDG_CONFIG_HOME/msbx/projects/<project>` — the project's value store,
/// outside every checkout. The manifest lists names; the values live here.
pub fn store_dir(project: &str) -> PathBuf {
    config_dir().join("projects").join(project)
}

/// `$XDG_CONFIG_HOME/msbx/host.env` — the host value store. It sits below the
/// project store: the project wins for the same name, and a project may read a
/// host name only when [`host_allow_file`] lists it.
pub fn host_env_file() -> PathBuf {
    config_dir().join("host.env")
}

/// `$XDG_CONFIG_HOME/msbx/host-allow` — the allowlist beside the host store,
/// one name per line, `#` comments.
pub fn host_allow_file() -> PathBuf {
    config_dir().join("host-allow")
}

/// `$XDG_CONFIG_HOME/msbx/harness/env` — the harness-only store. It carries the
/// model credential, `GH_TOKEN` and the git identity, and never enters a
/// service envelope.
pub fn harness_env_file() -> PathBuf {
    config_dir().join("harness").join("env")
}

/// `$XDG_STATE_HOME/msbx/leases/<project>/<scope>` — the leases one box holds.
/// `gc` and `rm` remove the directory with the box.
pub fn leases_dir(project: &str, scope: &str) -> PathBuf {
    state_dir().join("leases").join(project).join(scope)
}

/// `$XDG_STATE_HOME/msbx/agents/<agent-id>/pi` — the persistent per-agent Pi
/// home, mounted at `/home/agent/.pi`. Never the host's real `~/.pi`.
pub fn agent_dir(agent_id: &str) -> PathBuf {
    state_dir().join("agents").join(agent_id).join("pi")
}

/// `$XDG_STATE_HOME/msbx/agents/<agent-id>` — the parent of [`agent_dir`], so a
/// seed can be staged and renamed into place.
pub fn agents_dir(agent_id: &str) -> PathBuf {
    state_dir().join("agents").join(agent_id)
}

/// `$XDG_STATE_HOME/msbx/cache` — probe output cached by image digest.
pub fn cache_dir() -> PathBuf {
    state_dir().join("cache")
}

/// The scratch directory one `init` run owns. It is under the state directory,
/// never under a checkout, and is removed when the run finishes.
pub fn init_dir() -> PathBuf {
    state_dir()
        .join("init")
        .join(std::process::id().to_string())
}

/// Seconds since the Unix epoch. The state stores whole seconds, which is all
/// `gc`'s 14-day window needs and keeps the format readable.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// One state file.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Store { path: path.into() }
    }

    /// The state file the installed binary uses.
    pub fn default_path() -> Self {
        Store::new(state_file())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the state. A missing file is an empty state, not an error; a
    /// corrupt or future-version file is an error, never silently discarded.
    pub fn load(&self) -> io::Result<State> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(State::new()),
            Err(error) => return Err(error),
        };
        let state: State = serde_json::from_str(&text).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {error}", self.path.display()),
            )
        })?;
        if state.version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: state version {} is not supported (expected {VERSION})",
                    self.path.display(),
                    state.version
                ),
            ));
        }
        Ok(state)
    }

    /// Write the state atomically: a sibling temporary file, then a rename.
    /// The directory is created 0700 and the file 0600 because the state may
    /// name secrets-adjacent paths later.
    pub fn save(&self, state: &State) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            create_private_dir(parent)?;
        }
        let temp = self
            .path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        let mut file = File::create(&temp)?;
        set_private_file(&file)?;
        let json = serde_json::to_string_pretty(state)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        file.write_all(json.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &self.path)
    }

    /// Take the writer lock. The guard removes the lock file when dropped.
    pub fn lock(&self) -> io::Result<Lock> {
        if let Some(parent) = self.path.parent() {
            create_private_dir(parent)?;
        }
        Lock::acquire(&lock_path(&self.path))
    }
}

/// An exclusive writer lock beside the state file.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    fn acquire(path: &Path) -> io::Result<Lock> {
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Lock {
                        path: path.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if is_stale(path) {
                        let _ = fs::remove_file(path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!("{} is locked by another msbx process", path.display()),
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

fn lock_path(state: &Path) -> PathBuf {
    state.with_extension("lock")
}

fn is_stale(lock: &Path) -> bool {
    fs::metadata(lock)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age > STALE_LOCK)
        .unwrap_or(false)
}

/// Create `dir` and its parents mode 0700.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// Write `body` to `path` mode 0600, replacing any existing file. A new file is
/// created private in the same open, so a rendered value is never briefly
/// readable by another user.
pub fn write_private(path: &Path, body: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    set_private_file(&file)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()
}

fn set_private_file(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("state-{}-{tag}", std::process::id()))
    }

    #[test]
    fn missing_state_is_empty() {
        let dir = scratch("missing");
        let _ = fs::remove_dir_all(&dir);
        let store = Store::new(dir.join("state.json"));
        let state = store.load().unwrap();
        assert_eq!(state.version, VERSION);
        assert!(state.blocks.is_empty());
    }

    #[test]
    fn round_trips_an_entry() {
        let dir = scratch("roundtrip");
        let _ = fs::remove_dir_all(&dir);
        let store = Store::new(dir.join("state.json"));
        let mut state = State::new();
        state.blocks.insert(
            "mir-901".to_string(),
            Entry {
                start: 23000,
                assigned: 10,
                last_used: 20,
                project: Some("demo".to_string()),
                scope: Some("mir-901".to_string()),
                dir: Some("/home/demo/mir-901".to_string()),
                published: vec![PublishedPort {
                    role: "pg".to_string(),
                    port: 23000,
                }],
                tunnels: vec![TunnelRecord {
                    role: "search".to_string(),
                    via: "cloudflared".to_string(),
                    upstream: "search.internal.example.com:9200".to_string(),
                }],
                lease_managed: true,
            },
        );
        store.save(&state).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.blocks["mir-901"].start, 23000);
        assert_eq!(loaded.blocks["mir-901"].published[0].role, "pg");
        assert!(loaded.blocks["mir-901"].lease_managed);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_entry_from_before_the_lifecycle_verbs_still_loads() {
        let dir = scratch("legacy");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("state.json");
        create_private_dir(&dir).unwrap();
        fs::write(
            &path,
            r#"{"version": 1, "blocks": {"mir-901": {"start": 23000, "assigned": 1, "last_used": 2}}}"#,
        )
        .unwrap();
        let loaded = Store::new(&path).load().unwrap();
        let entry = &loaded.blocks["mir-901"];
        assert_eq!(entry.project, None);
        assert!(entry.published.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_an_unknown_version() {
        let dir = scratch("version");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("state.json");
        create_private_dir(&dir).unwrap();
        fs::write(&path, r#"{"version": 99, "blocks": {}}"#).unwrap();
        let error = Store::new(&path).load().unwrap_err();
        assert!(error.to_string().contains("not supported"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_lock_is_exclusive_and_released() {
        let dir = scratch("lock");
        let _ = fs::remove_dir_all(&dir);
        let store = Store::new(dir.join("state.json"));
        let first = store.lock().unwrap();
        assert!(store.lock().is_err());
        drop(first);
        assert!(store.lock().is_ok());
        let _ = fs::remove_dir_all(&dir);
    }
}
