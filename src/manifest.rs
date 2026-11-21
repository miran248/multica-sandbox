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
    /// `x-msbx.project`: `[a-z0-9-]+`, never containing the epic.
    pub project: String,
    /// Compose service names. Not validated as roles until referenced.
    pub services: BTreeSet<String>,
    /// Role → container port, in declaration order. The order fixes host ports.
    pub publish: Vec<(String, u16)>,
    /// Role → value names from the store.
    pub env: BTreeMap<String, Vec<String>>,
    /// Role → secret names mounted at `/run/msbx/secrets/<NAME>`.
    pub secrets: BTreeMap<String, Vec<String>>,
    /// Role → remote service, in declaration order.
    pub tunnels: BTreeMap<String, Tunnel>,
}

/// A remote service served by an in-epic tunnel sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunnel {
    pub host: String,
    pub port: u16,
    pub via: Via,
}

/// The tunnel transport. Both are generated as sidecars on the epic network.
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
}

impl ManifestError {
    fn new(message: impl Into<String>) -> Self {
        ManifestError {
            message: message.into(),
        }
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
        Self::parse(&text)
            .map_err(|error| ManifestError::new(format!("{}: {error}", path.display())))
    }

    /// Validate a manifest document.
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let root: Value = serde_yaml_ng::from_str(text)
            .map_err(|error| ManifestError::new(format!("invalid YAML: {error}")))?;
        let root = root
            .as_mapping()
            .ok_or_else(|| ManifestError::new("the manifest must be a YAML mapping"))?;

        let services = parse_services(root)?;

        let block = root
            .get("x-msbx")
            .ok_or_else(|| ManifestError::new("not a sandbox manifest: `x-msbx` is missing"))?;
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

    fn validate_roles(&self) -> Result<(), ManifestError> {
        for role in self.tunnels.keys() {
            if self.services.contains(role) {
                return Err(ManifestError::new(format!(
                    "`x-msbx.tunnels.{role}` is also a service; a role is either local or tunnelled, never both"
                )));
            }
        }

        for (role, _) in &self.publish {
            self.require_role(role, "publish")?;
        }
        for role in self.env.keys() {
            self.require_role(role, "env")?;
        }
        for role in self.secrets.keys() {
            self.require_role(role, "secrets")?;
        }
        Ok(())
    }

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

fn parse_services(root: &Mapping) -> Result<BTreeSet<String>, ManifestError> {
    let Some(services) = root.get("services") else {
        return Ok(BTreeSet::new());
    };
    let services = services
        .as_mapping()
        .ok_or_else(|| ManifestError::new("`services` must be a mapping"))?;
    let mut names = BTreeSet::new();
    for key in services.keys() {
        let name = key
            .as_str()
            .ok_or_else(|| ManifestError::new("service names must be strings"))?;
        names.insert(name.to_string());
    }
    Ok(names)
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
    fn a_manifest_with_only_a_project_is_valid() {
        let manifest = Manifest::parse("x-msbx:\n  project: signals\nservices: {}\n").unwrap();
        assert!(manifest.publish.is_empty());
        assert!(manifest.services.is_empty());
    }
}
