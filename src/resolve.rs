//! Scope resolution: what the box belongs to.
//!
//! A scope is a ticket (`mir-42`) or a chat session (`chat-1a2b3c4d`), and it
//! comes from, in order:
//!
//! 1. `--scope` or `MSBX_SCOPE`, normalized (`MIR-42` → `mir-42`); an invalid
//!    key is a loud error.
//! 2. The daemon's task-context marker `.multica/daemon_task_context.json`,
//!    beside or above the workdir: `issue_id` names the ticket and
//!    `chat_session_id` names the chat scope. Both present means the issue wins.
//! 3. The worktree branch, when it matches `agent/<role>/mir-<n>[-<slug>]`.
//! 4. Anything else is an error. There is no silent default and no parent
//!    lookup: retries, follow-ups and stage moves of one ticket share a scope;
//!    sibling tickets do not.
//!
//! The retired `MSBX_EPIC` is refused here, naming `MSBX_SCOPE`, rather than
//! quietly acting as an alias.

use crate::scope::{normalize_scope, ScopeError};
use serde::Deserialize;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The scope's environment variable.
pub const SCOPE_ENV: &str = "MSBX_SCOPE";
/// The retired epic variable; setting it is an error naming [`SCOPE_ENV`].
pub const LEGACY_ENV: &str = "MSBX_EPIC";
/// The daemon's per-task marker.
const TASK_CONTEXT: &str = "daemon_task_context.json";
/// How far above the workdir the marker may live.
const ANCESTORS: usize = 4;

/// A scope that could not be resolved. The message names what was tried and
/// never a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError(String);

impl ResolveError {
    fn new(message: impl Into<String>) -> Self {
        ResolveError(message.into())
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ResolveError {}

/// Resolve the scope for the current workdir. The absolute path matters: the
/// task marker lives beside or above the workdir.
pub fn resolve(requested: Option<&str>) -> Result<String, ResolveError> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_in(requested, &root)
}

/// Resolve the scope for `root`, in the documented order.
pub fn resolve_in(requested: Option<&str>, root: &Path) -> Result<String, ResolveError> {
    refuse_legacy_env()?;

    if let Some(raw) = requested {
        return normalize(raw, "--scope");
    }
    if let Some(raw) = std::env::var_os(SCOPE_ENV).filter(|value| !value.is_empty()) {
        let raw = raw
            .to_str()
            .ok_or_else(|| ResolveError::new("MSBX_SCOPE is not valid UTF-8"))?;
        return normalize(raw, "MSBX_SCOPE");
    }

    if let Some(scope) = task_context(root)? {
        return Ok(scope);
    }

    if let Some(branch) = branch(root) {
        if let Some(scope) = scope_from_branch(&branch) {
            return Ok(scope);
        }
    }

    let branch = branch(root).unwrap_or_else(|| "<detached>".to_string());
    Err(ResolveError::new(format!(
        "cannot resolve the scope: pass --scope <S>, set MSBX_SCOPE, or run on an `agent/*/mir-<n>-*` branch (the worktree is on `{branch}`)"
    )))
}

/// `MSBX_EPIC` is retired, not aliased: a stale value would silently key a
/// different box, so it is a loud error naming the replacement.
pub fn refuse_legacy_env() -> Result<(), ResolveError> {
    if std::env::var_os(LEGACY_ENV).is_some_and(|value| !value.is_empty()) {
        return Err(ResolveError::new(
            "MSBX_EPIC is retired; set MSBX_SCOPE instead (a scope is a ticket like `mir-42` or a chat like `chat-1a2b3c4d`)"
                .to_string(),
        ));
    }
    Ok(())
}

fn normalize(raw: &str, source: &str) -> Result<String, ResolveError> {
    normalize_scope(raw).map_err(|error: ScopeError| {
        ResolveError::new(format!("{source} is not a valid scope: {error}"))
    })
}

/// The daemon's marker, if there is one. It names the issue (a ticket scope) or
/// the chat session (a chat scope); the issue wins when both are present.
#[derive(Debug, Deserialize)]
struct TaskContext {
    #[serde(default)]
    managed_by: String,
    #[serde(default)]
    issue_id: Option<String>,
    #[serde(default)]
    chat_session_id: Option<String>,
}

fn task_context(root: &Path) -> Result<Option<String>, ResolveError> {
    for dir in root.ancestors().take(ANCESTORS) {
        let path = dir.join(".multica").join(TASK_CONTEXT);
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
            return issue_scope(&issue).map(Some);
        }
        if let Some(chat) = context
            .chat_session_id
            .filter(|session| !session.is_empty())
        {
            return chat_scope(&chat).map(Some);
        }
    }
    Ok(None)
}

/// A ticket scope from the issue the marker names. The issue's own identifier
/// is the scope; there is no parent lookup.
fn issue_scope(issue_id: &str) -> Result<String, ResolveError> {
    let multica = multica_bin();
    let output = Command::new(&multica)
        .args(["issue", "get", issue_id, "--output", "json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            ResolveError::new(format!(
                "cannot resolve the scope: cannot run `{}`: {error}",
                multica.to_string_lossy()
            ))
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        return Err(ResolveError::new(format!(
            "cannot resolve the scope: `multica issue get {issue_id}` failed{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        )));
    }
    let issue: Issue = serde_json::from_slice(&output.stdout).map_err(|error| {
        ResolveError::new(format!(
            "cannot resolve the scope: `multica issue get {issue_id}` returned no usable issue: {error}"
        ))
    })?;
    normalize(&issue.identifier, "the task-context issue")
}

#[derive(Debug, Deserialize)]
struct Issue {
    identifier: String,
}

/// `chat-` plus the first eight characters of the session id.
fn chat_scope(session: &str) -> Result<String, ResolveError> {
    let prefix: String = session.chars().take(8).collect();
    normalize(&format!("chat-{prefix}"), "the task-context chat session")
}

/// The checkout's branch, when it is on one. A detached HEAD is not an error:
/// the marker still names the scope.
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

/// `agent/<role>/mir-<n>[-<slug>]` → `mir-<n>`. The branch names the issue; the
/// issue is the scope.
pub fn scope_from_branch(branch: &str) -> Option<String> {
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
    normalize_scope(&format!("mir-{digits}")).ok()
}

fn multica_bin() -> PathBuf {
    std::env::var_os("MULTICA_CLI_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("multica"))
}

/// The scope key from an explicit `--scope`-style value, without the environment
/// and marker fallbacks. The CLI uses it for its own flag.
pub fn explicit(raw: &str) -> Result<String, ResolveError> {
    normalize(raw, "--scope")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_issue_number_from_a_task_branch() {
        assert_eq!(
            scope_from_branch("agent/implementer/mir-261-eebfc37c0c33").as_deref(),
            Some("mir-261")
        );
        assert_eq!(
            scope_from_branch("agent/implementer/mir-7").as_deref(),
            Some("mir-7")
        );
    }

    #[test]
    fn refuses_branches_that_do_not_name_an_issue() {
        assert_eq!(scope_from_branch("main"), None);
        assert_eq!(scope_from_branch("agent/implementer/eebfc37c0c33"), None);
        assert_eq!(scope_from_branch("agent/implementer/mir-x-abcd"), None);
        assert_eq!(scope_from_branch("agent/implementer/mir-12x-abcd"), None);
        assert_eq!(scope_from_branch("agent/implementer/mir-12/extra"), None);
        assert_eq!(
            scope_from_branch("feature/agent/implementer/mir-12-x"),
            None
        );
    }

    #[test]
    fn a_chat_session_becomes_chat_and_eight_characters() {
        assert_eq!(chat_scope("1a2b3c4d5e6f").unwrap(), "chat-1a2b3c4d");
        assert_eq!(chat_scope("ABCDEFGH").unwrap(), "chat-abcdefgh");
        assert!(chat_scope("not/valid").is_err());
    }
}
