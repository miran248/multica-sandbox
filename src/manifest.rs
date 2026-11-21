//! The project manifest: `<repo>/.multica/compose.yaml`, a standard Compose
//! file plus the `x-msbx` block.
//!
//! `docker compose` reads the file unchanged; `msbx` reads only what Compose
//! cannot express. Validation is strict and returns an error for every
//! malformed block, before anything is started: an unknown `x-msbx` key, a role
//! that is not a service, a role both local and tunnelled, or a missing
//! `project`.

use crate::ports::Block;
use serde_yaml_ng::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

/// A validated manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// `x-msbx.project`: `[a-z0-9-]+`, never containing the scope.
    pub project: String,
    /// Compose service names. Not validated as roles until referenced.
    pub services: BTreeSet<String>,
    /// Service → bind-mount sources, as written. `up` refuses the checkout root.
    pub volume_sources: BTreeMap<String, Vec<String>>,
    /// Service → build context, as written. `up` refuses one inside the checkout.
    pub build_contexts: BTreeMap<String, String>,
    /// Role → container port, in declaration order. The order fixes host ports.
    pub publish: Vec<(String, u16)>,
    /// Role → value names from the store.
    pub env: BTreeMap<String, Vec<String>>,
    /// Role → secret names mounted at `/run/msbx/secrets/<NAME>`.
    pub secrets: BTreeMap<String, Vec<String>>,
    /// Role → remote service, in declaration order.
    pub tunnels: BTreeMap<String, Tunnel>,
}

/// A remote service served by an in-scope tunnel sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunnel {
    pub host: String,
    pub port: u16,
    pub via: Via,
}

/// The tunnel transport. Both are generated as sidecars on the scope network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Ssh,
    Cloudflared,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Ssh => "ssh",
            Via::Cloudflared => "cloudflared",
        }
    }
}

/// A manifest problem. Every message names the offending key; callers print it
/// and stop before starting a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    message: String,
    no_manifest: bool,
}

impl ManifestError {
    fn new(message: impl Into<String>) -> Self {
        ManifestError {
            message: message.into(),
            no_manifest: false,
        }
    }

    /// A Compose file that carries no `x-msbx` block. The file is a normal
    /// Compose file, not a sandbox manifest, so callers pass it through.
    fn no_manifest(message: impl Into<String>) -> Self {
        ManifestError {
            message: message.into(),
            no_manifest: true,
        }
    }

    /// Whether the error means "this is not a sandbox manifest" rather than a
    /// broken one.
    pub fn is_missing_manifest(&self) -> bool {
        self.no_manifest
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ManifestError {}

impl Manifest {
    /// `<root>/.multica/compose.yaml`.
    pub fn path_in(root: &Path) -> PathBuf {
        root.join(".multica").join("compose.yaml")
    }

    /// Read and validate a manifest from `<root>/.multica/compose.yaml`.
    pub fn from_repo(root: &Path) -> Result<Manifest, ManifestError> {
        Self::from_path(&Self::path_in(root))
    }

    /// Read and validate a manifest from `path`.
    pub fn from_path(path: &Path) -> Result<Manifest, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            ManifestError::new(format!("cannot read {}: {error}", path.display()))
        })?;
        Self::parse(&text).map_err(|error| {
            let message = format!("{}: {error}", path.display());
            if error.is_missing_manifest() {
                ManifestError::no_manifest(message)
            } else {
                ManifestError::new(message)
            }
        })
    }

    /// Validate a manifest document.
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let root: Value = serde_yaml_ng::from_str(text)
            .map_err(|error| ManifestError::new(format!("invalid YAML: {error}")))?;
        let root = root
            .as_mapping()
            .ok_or_else(|| ManifestError::new("the manifest must be a YAML mapping"))?;

        let Services {
            names: services,
            volume_sources,
            build_contexts,
        } = parse_services(root)?;

        let block = root.get("x-msbx").ok_or_else(|| {
            ManifestError::no_manifest("not a sandbox manifest: `x-msbx` is missing")
        })?;
        let block = block
            .as_mapping()
            .ok_or_else(|| ManifestError::new("`x-msbx` must be a mapping"))?;

        let mut project = None;
        let mut publish = None;
        let mut env = None;
        let mut secrets = None;
        let mut tunnels = None;

        for (key, value) in block {
            let key = key.as_str().ok_or_else(|| {
                ManifestError::new(
                    "`x-msbx` keys must be strings (project, publish, env, secrets, tunnels)",
                )
            })?;
            match key {
                "project" => project = Some(parse_project(value)?),
                "publish" => publish = Some(parse_publish(value)?),
                "env" => env = Some(parse_names(value, "env")?),
                "secrets" => secrets = Some(parse_names(value, "secrets")?),
                "tunnels" => tunnels = Some(parse_tunnels(value)?),
                unknown => {
                    return Err(ManifestError::new(format!(
                        "`x-msbx.{unknown}` is not a known key (project, publish, env, secrets, tunnels)"
                    )))
                }
            }
        }

        let project = project.ok_or_else(|| ManifestError::new("`x-msbx.project` is required"))?;
        let publish = publish.unwrap_or_default();
        let env = env.unwrap_or_default();
        let secrets = secrets.unwrap_or_default();
        let tunnels = tunnels.unwrap_or_default();

        let manifest = Manifest {
            project,
            services,
            volume_sources,
            build_contexts,
            publish,
            env,
            secrets,
            tunnels,
        };
        manifest.validate_roles()?;
        Ok(manifest)
    }

    /// The host port a published role gets from `block`, by `publish` order.
    pub fn host_port(&self, block: Block, role: &str) -> Option<u16> {
        self.publish
            .iter()
            .position(|(name, _)| name == role)
            .and_then(|index| block.port(index))
    }

    /// The box is state, not code. `up` refuses a service volume whose source is
    /// the checkout root and a service `build:` whose context is inside the
    /// checkout, before it allocates a block or starts a container.
    pub fn check_state_only(&self, root: &Path) -> Result<(), ManifestError> {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| lexical(root));
        for (role, sources) in &self.volume_sources {
            for source in sources {
                if resolve_path(&root, source) == root {
                    return Err(ManifestError::new(format!(
                        "`services.{role}.volumes` binds the checkout root {}; the box is state only, and mounting the checkout would make two scopes share code (mount a subpath instead)",
                        root.display()
                    )));
                }
            }
        }
        for (role, context) in &self.build_contexts {
            let context = resolve_path(&root, context);
            if context == root || context.starts_with(&root) {
                return Err(ManifestError::new(format!(
                    "`services.{role}.build` uses the build context {}, inside the checkout; a live-code service is out of scope, and the app process runs in the harness container (publish an image instead)",
                    context.display()
                )));
            }
        }
        Ok(())
    }

    fn validate_roles(&self) -> Result<(), ManifestError> {
        for role in self.tunnels.keys() {
            if self.services.contains(role) {
                return Err(ManifestError::new(format!(
                    "`x-msbx.tunnels.{role}` is also a service; a role is either local or tunnelled, never both"
                )));
            }
        }

        for (role, port) in &self.publish {
            self.require_role(role, "publish")?;
            if let Some(tunnel) = self.tunnels.get(role) {
                if *port != tunnel.port {
                    return Err(ManifestError::new(format!(
                        "`x-msbx.publish.{role}` is {port} but `x-msbx.tunnels.{role}.port` is {}; a tunnelled role listens on its tunnel port",
                        tunnel.port
                    )));
                }
            }
        }
        for role in self.env.keys() {
            self.require_role(role, "env")?;
        }
        for role in self.secrets.keys() {
            self.require_role(role, "secrets")?;
        }
        Ok(())
    }

    /// A role named in `publish`, `env` or `secrets` is either a Compose
    /// service or a role `msbx` generates a tunnel sidecar for.
    fn require_role(&self, role: &str, block: &str) -> Result<(), ManifestError> {
        if self.services.contains(role) || self.tunnels.contains_key(role) {
            Ok(())
        } else {
            Err(ManifestError::new(format!(
                "`x-msbx.{block}.{role}` names no service or tunnel"
            )))
        }
    }
}

struct Services {
    names: BTreeSet<String>,
    volume_sources: BTreeMap<String, Vec<String>>,
    build_contexts: BTreeMap<String, String>,
}

fn parse_services(root: &Mapping) -> Result<Services, ManifestError> {
    let Some(services) = root.get("services") else {
        return Ok(Services {
            names: BTreeSet::new(),
            volume_sources: BTreeMap::new(),
            build_contexts: BTreeMap::new(),
        });
    };
    let services = services
        .as_mapping()
        .ok_or_else(|| ManifestError::new("`services` must be a mapping"))?;
    let mut parsed = Services {
        names: BTreeSet::new(),
        volume_sources: BTreeMap::new(),
        build_contexts: BTreeMap::new(),
    };
    for (key, value) in services {
        let name = key
            .as_str()
            .ok_or_else(|| ManifestError::new("service names must be strings"))?;
        parsed.names.insert(name.to_string());
        let Some(service) = value.as_mapping() else {
            continue;
        };
        if let Some(volumes) = service.get("volumes") {
            let sources = parse_volume_sources(name, volumes)?;
            if !sources.is_empty() {
                parsed.volume_sources.insert(name.to_string(), sources);
            }
        }
        if let Some(build) = service.get("build") {
            let context = parse_build_context(name, build)?;
            parsed.build_contexts.insert(name.to_string(), context);
        }
    }
    Ok(parsed)
}

/// The path-shaped sources of a service's `volumes`. A named volume has no
/// source path and is not a bind. Both the short `source:target[:mode]` form and
/// the long `{type: bind, source: …}` form are read.
fn parse_volume_sources(role: &str, volumes: &Value) -> Result<Vec<String>, ManifestError> {
    let list = volumes
        .as_sequence()
        .ok_or_else(|| ManifestError::new(format!("`services.{role}.volumes` must be a list")))?;
    let mut sources = Vec::new();
    for item in list {
        let source = match item {
            Value::String(text) => {
                // One field is an anonymous volume target, not a bind source.
                let mut fields = text.split(':');
                let source = fields.next().unwrap_or_default();
                fields
                    .next()
                    .and_then(|_| is_path_source(source).then(|| source.to_string()))
            }
            Value::Mapping(entry) => {
                let bind = entry
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind == "bind");
                bind.then(|| {
                    entry
                        .get("source")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .flatten()
            }
            _ => None,
        };
        if let Some(source) = source {
            sources.push(source);
        }
    }
    Ok(sources)
}

/// A source is a path when Compose would treat it as one: an absolute path, or
/// a relative path explicitly rooted at `.` or `..`. Anything else is a named
/// volume.
fn is_path_source(source: &str) -> bool {
    source.starts_with('/')
        || source == "."
        || source == ".."
        || source.starts_with("./")
        || source.starts_with("../")
}

/// The build context of a service: `build: .` or `build: {context: …}`.
fn parse_build_context(role: &str, build: &Value) -> Result<String, ManifestError> {
    match build {
        Value::String(context) => Ok(context.clone()),
        Value::Mapping(entry) => entry
            .get("context")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                ManifestError::new(format!("`services.{role}.build.context` must be a string"))
            }),
        _ => Err(ManifestError::new(format!(
            "`services.{role}.build` must be a path or a mapping"
        ))),
    }
}

/// Resolve a manifest path against the checkout root and remove `.` and `..`
/// segments. A path that exists is canonicalized, so a symlinked checkout root
/// is still refused.
fn resolve_path(root: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let lexical = lexical(&joined);
    std::fs::canonicalize(&lexical).unwrap_or(lexical)
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn parse_project(value: &Value) -> Result<String, ManifestError> {
    let project = value
        .as_str()
        .ok_or_else(|| ManifestError::new("`x-msbx.project` must be a string"))?;
    if project.is_empty() {
        return Err(ManifestError::new("`x-msbx.project` must not be empty"));
    }
    if !project
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ManifestError::new(format!(
            "`x-msbx.project` `{project}` must match [a-z0-9-]+"
        )));
    }
    Ok(project.to_string())
}

fn parse_publish(value: &Value) -> Result<Vec<(String, u16)>, ManifestError> {
    let mapping = value
        .as_mapping()
        .ok_or_else(|| ManifestError::new("`x-msbx.publish` must be a role → port mapping"))?;
    let mut publish = Vec::with_capacity(mapping.len());
    for (key, value) in mapping {
        let role = role_name(key, "publish")?;
        let port = value
            .as_u64()
            .filter(|port| (1..=65535).contains(port))
            .ok_or_else(|| {
                ManifestError::new(format!("`x-msbx.publish.{role}` must be a port 1-65535"))
            })?;
        publish.push((role, port as u16));
    }
    Ok(publish)
}

fn parse_names(value: &Value, block: &str) -> Result<BTreeMap<String, Vec<String>>, ManifestError> {
    let mapping = value.as_mapping().ok_or_else(|| {
        ManifestError::new(format!("`x-msbx.{block}` must be a role → names mapping"))
    })?;
    let mut out = BTreeMap::new();
    for (key, value) in mapping {
        let role = role_name(key, block)?;
        if block == "env" && role == "harness" {
            return Err(ManifestError::new(format!(
                "`x-msbx.env.{role}` would overwrite the harness envelope `env/harness.env`; name the role something else"
            )));
        }
        let list = value.as_sequence().ok_or_else(|| {
            ManifestError::new(format!("`x-msbx.{block}.{role}` must be a list of names"))
        })?;
        let mut names = Vec::with_capacity(list.len());
        for item in list {
            let name = item.as_str().ok_or_else(|| {
                ManifestError::new(format!("`x-msbx.{block}.{role}` names must be strings"))
            })?;
            if name.is_empty() || name.chars().any(char::is_whitespace) {
                return Err(ManifestError::new(format!(
                    "`x-msbx.{block}.{role}` contains an empty or whitespace name"
                )));
            }
            if !valid_env_name(name) {
                return Err(ManifestError::new(format!(
                    "`x-msbx.{block}.{role}` name `{name}` must match [A-Za-z_][A-Za-z0-9_]* (it becomes an environment variable, a file name and a mount path)"
                )));
            }
            if names.iter().any(|seen| seen == name) {
                return Err(ManifestError::new(format!(
                    "`x-msbx.{block}.{role}` lists `{name}` twice"
                )));
            }
            names.push(name.to_string());
        }
        out.insert(role, names);
    }
    Ok(out)
}

fn parse_tunnels(value: &Value) -> Result<BTreeMap<String, Tunnel>, ManifestError> {
    let mapping = value
        .as_mapping()
        .ok_or_else(|| ManifestError::new("`x-msbx.tunnels` must be a role → tunnel mapping"))?;
    let mut out = BTreeMap::new();
    for (key, value) in mapping {
        let role = role_name(key, "tunnels")?;
        let tunnel = value.as_mapping().ok_or_else(|| {
            ManifestError::new(format!("`x-msbx.tunnels.{role}` must be a mapping"))
        })?;
        let mut host = None;
        let mut port = None;
        let mut via = None;
        for (key, value) in tunnel {
            let key = key.as_str().ok_or_else(|| {
                ManifestError::new(format!("`x-msbx.tunnels.{role}` keys must be strings"))
            })?;
            match key {
                "host" => {
                    let name = value.as_str().ok_or_else(|| {
                        ManifestError::new(format!("`x-msbx.tunnels.{role}.host` must be a string"))
                    })?;
                    if name.is_empty() {
                        return Err(ManifestError::new(format!(
                            "`x-msbx.tunnels.{role}.host` must not be empty"
                        )));
                    }
                    // The host is interpolated into the generated sidecar's
                    // command, so it stays a host name or address: no shell
                    // metacharacters and no YAML-significant braces.
                    if !name.chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']')
                    }) {
                        return Err(ManifestError::new(format!(
                            "`x-msbx.tunnels.{role}.host` `{name}` must be a host name or address ([A-Za-z0-9._:-], brackets for IPv6)"
                        )));
                    }
                    host = Some(name.to_string());
                }
                "port" => {
                    let number = value
                        .as_u64()
                        .filter(|port| (1..=65535).contains(port))
                        .ok_or_else(|| {
                            ManifestError::new(format!(
                                "`x-msbx.tunnels.{role}.port` must be a port 1-65535"
                            ))
                        })?;
                    port = Some(number as u16);
                }
                "via" => {
                    let transport = value.as_str().ok_or_else(|| {
                        ManifestError::new(format!("`x-msbx.tunnels.{role}.via` must be a string"))
                    })?;
                    via = Some(match transport {
                        "ssh" => Via::Ssh,
                        "cloudflared" => Via::Cloudflared,
                        other => {
                            return Err(ManifestError::new(format!(
                            "`x-msbx.tunnels.{role}.via` is `{other}`, expected ssh or cloudflared"
                        )))
                        }
                    });
                }
                unknown => {
                    return Err(ManifestError::new(format!(
                        "`x-msbx.tunnels.{role}.{unknown}` is not a known key (host, port, via)"
                    )))
                }
            }
        }
        let host = host.ok_or_else(|| {
            ManifestError::new(format!("`x-msbx.tunnels.{role}.host` is required"))
        })?;
        let port = port.ok_or_else(|| {
            ManifestError::new(format!("`x-msbx.tunnels.{role}.port` is required"))
        })?;
        let via = via.ok_or_else(|| {
            ManifestError::new(format!(
                "`x-msbx.tunnels.{role}.via` is required (ssh or cloudflared)"
            ))
        })?;
        out.insert(role, Tunnel { host, port, via });
    }
    Ok(out)
}

/// A role is one DNS label and one container-name suffix, so it stays a single
/// alphanumeric/hyphen label.
fn role_name(value: &Value, block: &str) -> Result<String, ManifestError> {
    let role = value.as_str().ok_or_else(|| {
        ManifestError::new(format!("`x-msbx.{block}` role names must be strings"))
    })?;
    let valid = !role.is_empty()
        && role.len() <= 63
        && role.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !role.starts_with('-')
        && !role.ends_with('-');
    if !valid {
        return Err(ManifestError::new(format!(
            "`x-msbx.{block}` role `{role}` is not a valid role name ([A-Za-z0-9-], no leading or trailing hyphen, at most 63 bytes)"
        )));
    }
    Ok(role.to_string())
}

/// An env or secret name becomes an environment variable, a file name and a
/// mount path, so it stays one shell identifier.
fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
x-msbx:
  project: signals
  publish:
    pg: 5432
    web: 3000
  env:
    web: [DATABASE_URL, STRIPE_KEY]
  secrets:
    web: [SMTP_PASSWORD]
  tunnels:
    search:
      host: db.internal.example.com
      port: 5432
      via: ssh
services:
  pg:
    image: postgres:17
  web:
    build: .
    depends_on: [pg]
"#;

    fn error(text: &str) -> String {
        Manifest::parse(text).unwrap_err().to_string()
    }

    #[test]
    fn reads_a_valid_manifest_including_tunnels() {
        let manifest = Manifest::parse(VALID).unwrap();
        assert_eq!(manifest.project, "signals");
        assert_eq!(
            manifest.publish,
            vec![("pg".into(), 5432), ("web".into(), 3000)]
        );
        assert_eq!(manifest.env["web"], vec!["DATABASE_URL", "STRIPE_KEY"]);
        assert_eq!(manifest.secrets["web"], vec!["SMTP_PASSWORD"]);
        assert_eq!(manifest.tunnels["search"].via, Via::Ssh);
    }

    #[test]
    fn publish_order_fixes_host_ports() {
        let manifest = Manifest::parse(VALID).unwrap();
        let block = Block { start: 23000 };
        assert_eq!(manifest.host_port(block, "pg"), Some(23000));
        assert_eq!(manifest.host_port(block, "web"), Some(23001));
        assert_eq!(manifest.host_port(block, "search"), None);
    }

    #[test]
    fn a_compose_file_without_x_msbx_is_not_a_manifest() {
        assert!(error("services:\n  pg:\n    image: postgres:17\n").contains("`x-msbx` is missing"));
    }

    #[test]
    fn project_is_required() {
        assert!(
            error("x-msbx:\n  publish:\n    pg: 5432\nservices:\n  pg: {}\n")
                .contains("`x-msbx.project` is required")
        );
    }

    #[test]
    fn project_must_be_lowercase_digits_and_hyphens() {
        assert!(error("x-msbx:\n  project: Signals\nservices: {}\n").contains("[a-z0-9-]+"));
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(error(
            "x-msbx:\n  project: signals\n  publishh:\n    pg: 5432\nservices:\n  pg: {}\n"
        )
        .contains("`x-msbx.publishh` is not a known key"));
    }

    #[test]
    fn a_published_role_must_be_a_service_or_tunnel() {
        assert!(error(
            "x-msbx:\n  project: signals\n  publish:\n    db: 5432\nservices:\n  pg: {}\n"
        )
        .contains("`x-msbx.publish.db` names no service or tunnel"));
    }

    #[test]
    fn an_env_role_must_be_a_service_or_tunnel() {
        assert!(error(
            "x-msbx:\n  project: signals\n  env:\n    cache: [REDIS_URL]\nservices:\n  pg: {}\n"
        )
        .contains("`x-msbx.env.cache` names no service or tunnel"));
    }

    #[test]
    fn a_secret_role_must_be_a_service_or_tunnel() {
        assert!(error(
            "x-msbx:\n  project: signals\n  secrets:\n    cache: [TOKEN]\nservices:\n  pg: {}\n"
        )
        .contains("`x-msbx.secrets.cache` names no service or tunnel"));
    }

    #[test]
    fn a_tunnelled_role_must_not_also_be_a_service() {
        let text = "x-msbx:\n  project: signals\n  tunnels:\n    pg:\n      host: db.example.com\n      port: 5432\n      via: ssh\nservices:\n  pg: {}\n";
        assert!(error(text).contains("either local or tunnelled, never both"));
    }

    #[test]
    fn a_tunnel_may_be_published() {
        let text = "x-msbx:\n  project: signals\n  publish:\n    pg: 5432\n  tunnels:\n    pg:\n      host: db.example.com\n      port: 5432\n      via: cloudflared\nservices:\n  web: {}\n";
        assert!(Manifest::parse(text).is_ok());
    }

    #[test]
    fn tunnel_requires_host_port_and_via() {
        assert!(error("x-msbx:\n  project: signals\n  tunnels:\n    pg:\n      port: 5432\n      via: ssh\nservices: {}\n")
            .contains("`x-msbx.tunnels.pg.host` is required"));
        assert!(error("x-msbx:\n  project: signals\n  tunnels:\n    pg:\n      host: db.example.com\n      via: ssh\nservices: {}\n")
            .contains("`x-msbx.tunnels.pg.port` is required"));
        assert!(error("x-msbx:\n  project: signals\n  tunnels:\n    pg:\n      host: db.example.com\n      port: 5432\nservices: {}\n")
            .contains("`x-msbx.tunnels.pg.via` is required"));
    }

    #[test]
    fn unknown_tunnel_keys_and_transports_are_errors() {
        assert!(error("x-msbx:\n  project: signals\n  tunnels:\n    pg:\n      host: db.example.com\n      port: 5432\n      via: ssh\n      extra: 1\nservices: {}\n")
            .contains("`x-msbx.tunnels.pg.extra` is not a known key"));
        assert!(error("x-msbx:\n  project: signals\n  tunnels:\n    pg:\n      host: db.example.com\n      port: 5432\n      via: tcp\nservices: {}\n")
            .contains("expected ssh or cloudflared"));
    }

    #[test]
    fn a_tunnel_host_stays_a_host_name_or_address() {
        // The host is interpolated into the generated sidecar's command.
        for host in [
            "db.example.com;rm -rf /",
            "db$(whoami)",
            "db example",
            "db\"quoted",
        ] {
            let text = format!(
                "x-msbx:\n  project: signals\n  tunnels:\n    pg:\n      host: {host}\n      port: 5432\n      via: ssh\nservices: {{}}\n"
            );
            let message = error(&text);
            assert!(
                message.contains("must be a host name or address"),
                "{host}: {message}"
            );
        }
        assert!(Manifest::parse(
            "x-msbx:\n  project: signals\n  tunnels:\n    pg:\n      host: \"[::1]\"\n      port: 5432\n      via: ssh\nservices: {}\n"
        )
        .is_ok());
    }

    #[test]
    fn publish_ports_must_be_ports() {
        assert!(
            error("x-msbx:\n  project: signals\n  publish:\n    pg: 0\nservices:\n  pg: {}\n")
                .contains("must be a port 1-65535")
        );
        assert!(error(
            "x-msbx:\n  project: signals\n  publish:\n    pg: http\nservices:\n  pg: {}\n"
        )
        .contains("must be a port 1-65535"));
    }

    #[test]
    fn role_names_stay_one_dns_label() {
        assert!(error("x-msbx:\n  project: signals\n  publish:\n    pg.prod: 5432\nservices:\n  pg.prod: {}\n")
            .contains("not a valid role name"));
    }

    #[test]
    fn env_and_secret_names_stay_shell_identifiers() {
        let text =
            "x-msbx:\n  project: signals\n  env:\n    web: [BAD-NAME]\nservices:\n  web: {}\n";
        assert!(error(text).contains("must match [A-Za-z_][A-Za-z0-9_]*"));
        let text =
            "x-msbx:\n  project: signals\n  secrets:\n    web: [../escape]\nservices:\n  web: {}\n";
        assert!(error(text).contains("must match [A-Za-z_][A-Za-z0-9_]*"));
    }

    #[test]
    fn a_role_named_harness_cannot_declare_env_entries() {
        let text = "x-msbx:\n  project: signals\n  env:\n    harness: [OTHER]\nservices:\n  harness: {}\n  web: {}\n";
        let message = error(text);
        assert!(message.contains("`x-msbx.env.harness`"), "{message}");
        assert!(
            message.contains("env/harness.env"),
            "the reserved file is named: {message}"
        );
    }

    #[test]
    fn env_and_secrets_for_a_tunnelled_role_are_accepted() {
        let text = "x-msbx:\n  project: signals\n  env:\n    pg: [TUNNEL_USER]\n  secrets:\n    pg: [TUNNEL_KEY]\n  tunnels:\n    pg:\n      host: db.example.com\n      port: 5432\n      via: ssh\nservices:\n  web: {}\n";
        let manifest = Manifest::parse(text).unwrap();
        assert_eq!(manifest.env["pg"], vec!["TUNNEL_USER"]);
        assert_eq!(manifest.secrets["pg"], vec!["TUNNEL_KEY"]);
    }

    #[test]
    fn a_published_tunnel_must_publish_its_tunnel_port() {
        let text = "x-msbx:\n  project: signals\n  publish:\n    pg: 5433\n  tunnels:\n    pg:\n      host: db.example.com\n      port: 5432\n      via: ssh\nservices:\n  web: {}\n";
        let message = error(text);
        assert!(message.contains("`x-msbx.publish.pg` is 5433"), "{message}");
        assert!(
            message.contains("`x-msbx.tunnels.pg.port` is 5432"),
            "{message}"
        );
    }

    #[test]
    fn a_manifest_with_only_a_project_is_valid() {
        let manifest = Manifest::parse("x-msbx:\n  project: signals\nservices: {}\n").unwrap();
        assert!(manifest.publish.is_empty());
        assert!(manifest.services.is_empty());
    }

    #[test]
    fn a_bind_of_the_checkout_root_is_refused() {
        let root = Path::new("/home/me/signals");
        for volumes in [
            "      - ./:/work\n",
            "      - .:/work\n",
            "      - /home/me/signals:/work\n",
        ] {
            let text = format!(
                "x-msbx:\n  project: signals\nservices:\n  web:\n    image: alpine\n    volumes:\n{volumes}"
            );
            let error = Manifest::parse(&text)
                .unwrap()
                .check_state_only(root)
                .unwrap_err()
                .to_string();
            assert!(error.contains("checkout root"), "{text}: {error}");
        }

        let long = "x-msbx:\n  project: signals\nservices:\n  web:\n    image: alpine\n    volumes:\n      - type: bind\n        source: .\n        target: /work\n";
        assert!(Manifest::parse(long)
            .unwrap()
            .check_state_only(root)
            .is_err());
    }

    #[test]
    fn a_named_volume_and_a_subpath_are_allowed() {
        let root = Path::new("/home/me/signals");
        let text = "x-msbx:\n  project: signals\nservices:\n  web:\n    image: alpine\n    volumes:\n      - data:/var/lib/data\n      - ./init.sql:/docker-entrypoint-initdb.d/init.sql:ro\n";
        assert!(Manifest::parse(text)
            .unwrap()
            .check_state_only(root)
            .is_ok());
    }

    #[test]
    fn a_build_context_inside_the_checkout_is_refused() {
        let root = Path::new("/home/me/signals");
        for context in [".", "./", "./sub"] {
            let text =
                format!("x-msbx:\n  project: signals\nservices:\n  web:\n    build: {context}\n");
            let error = Manifest::parse(&text)
                .unwrap()
                .check_state_only(root)
                .unwrap_err()
                .to_string();
            assert!(error.contains("inside the checkout"), "{text}: {error}");
        }
        let outside = "x-msbx:\n  project: signals\nservices:\n  web:\n    build:\n      context: ../shared\n";
        assert!(Manifest::parse(outside)
            .unwrap()
            .check_state_only(root)
            .is_ok());
    }
}
