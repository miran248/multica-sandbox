//! The epic namespace: keys, names, paths, published ports and the generated
//! Compose override.
//!
//! Everything here is pure: the same [`Manifest`], [`Block`] and [`Envelope`]
//! produce the same override text, so a second `up` regenerates an identical
//! file and Compose changes nothing. The override is the only place container
//! names, dotted aliases, loopback port mappings, env files and secret mounts
//! and the epic's network name are decided.

use crate::dns;
use crate::env::{Envelope, SECRET_MOUNT};
use crate::manifest::Manifest;
use crate::ports::{Block, BLOCK_SIZE};
use crate::state::{self, PublishedPort};
use std::fmt;
use std::path::PathBuf;

/// The generated Compose file, under the epic's state directory.
pub const OVERRIDE_FILE: &str = "override.yaml";

/// One epic: the project key and the epic key. Together they name the Compose
/// project, every container and every dotted alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    project: String,
    epic: String,
}

/// A refused key or an impossible override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpicError(String);

impl EpicError {
    fn new(message: impl Into<String>) -> Self {
        EpicError(message.into())
    }
}

impl fmt::Display for EpicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EpicError {}

impl Key {
    /// Validate the two keys. The project comes from `x-msbx.project` (already
    /// validated there); the epic is lowercased, so `MIR-178` is `mir-178`.
    pub fn new(project: &str, epic: &str) -> Result<Key, EpicError> {
        if !valid_key(project) {
            return Err(EpicError::new(format!(
                "project `{project}` must match [a-z0-9-]+"
            )));
        }
        Ok(Key {
            project: project.to_string(),
            epic: normalize_epic(epic)?,
        })
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn epic(&self) -> &str {
        &self.epic
    }

    /// The Compose project, `<project>-<epic>`.
    pub fn compose_project(&self) -> String {
        format!("{}-{}", self.project, self.epic)
    }

    /// The epic's network, named after its Compose project so two epics never
    /// share one.
    pub fn network(&self) -> String {
        self.compose_project()
    }

    /// The container name, `<project>-<epic>-<role>`.
    pub fn container(&self, role: &str) -> String {
        format!("{}-{}-{role}", self.project, self.epic)
    }

    /// The dotted alias, `<role>.<epic>.<project>.msbx.test`.
    pub fn alias(&self, role: &str) -> String {
        format!("{role}.{}.{}.{}", self.epic, self.project, dns::ZONE)
    }

    /// The epic's envelope directory, outside every checkout.
    pub fn dir(&self) -> PathBuf {
        state::epic_dir(&self.project, &self.epic)
    }

    /// The generated override, under [`Key::dir`].
    pub fn override_path(&self) -> PathBuf {
        self.dir().join(OVERRIDE_FILE)
    }
}

/// Lowercase and validate an epic key.
pub fn normalize_epic(raw: &str) -> Result<String, EpicError> {
    let epic = raw.trim().to_ascii_lowercase();
    if !valid_key(&epic) {
        return Err(EpicError::new(format!(
            "epic `{raw}` must match [a-z0-9-]+"
        )));
    }
    Ok(epic)
}

/// A key becomes a Compose project, a container prefix and DNS labels, so it
/// stays lowercase alphanumerics and hyphens.
fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

/// One role reachable from the host: its container port and the loopback port
/// the epic's block assigns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub role: String,
    pub container_port: u16,
    pub host_port: u16,
}

/// Map `x-msbx.publish` onto `block`, in declaration order.
pub fn published(manifest: &Manifest, block: Block) -> Result<Vec<Published>, EpicError> {
    check_publish(manifest)?;
    Ok(manifest
        .publish
        .iter()
        .enumerate()
        .filter_map(|(index, (role, container_port))| {
            block.port(index).map(|host_port| Published {
                role: role.clone(),
                container_port: *container_port,
                host_port,
            })
        })
        .collect())
}

/// `x-msbx.publish` must fit the block before a block is allocated for it.
pub fn check_publish(manifest: &Manifest) -> Result<(), EpicError> {
    if manifest.publish.len() > usize::from(BLOCK_SIZE) {
        return Err(EpicError::new(format!(
            "`x-msbx.publish` lists {} roles but a block holds {BLOCK_SIZE}",
            manifest.publish.len()
        )));
    }
    Ok(())
}

/// Render the override Compose file for `key`.
///
/// It carries, for every service: the container name, the dotted network alias,
/// the `com.msbx.*` labels, a published role's loopback port mapping, the role's
/// rendered `env_file` and each secret mounted read-only at
/// `/run/msbx/secrets/<NAME>` with `<NAME>_FILE` in its environment. The default
/// network is named after the Compose project.
pub fn override_yaml(
    key: &Key,
    manifest: &Manifest,
    published: &[Published],
    envelope: &Envelope,
) -> String {
    let mut out = format!(
        "# Generated by msbx for {}; rewritten by every `msbx up`.\n\
         # Do not edit: container names, aliases, host ports, env files and\n\
         # secret mounts are decided here.\n\
         services:\n",
        key.compose_project()
    );
    for role in &manifest.services {
        out.push_str(&format!("  {role}:\n"));
        out.push_str(&format!("    container_name: {}\n", key.container(role)));
        if let Some(port) = published.iter().find(|port| &port.role == role) {
            out.push_str("    ports:\n");
            out.push_str(&format!(
                "      - \"127.0.0.1:{}:{}\"\n",
                port.host_port, port.container_port
            ));
        }
        if let Some(path) = envelope.role_env_file(role) {
            out.push_str("    env_file:\n");
            out.push_str(&format!(
                "      - {}\n",
                yaml_string(&path.to_string_lossy())
            ));
        }
        let secrets = envelope.secret_mounts(role);
        if !secrets.is_empty() {
            out.push_str("    environment:\n");
            for (name, _) in &secrets {
                out.push_str(&format!("      {name}_FILE: {SECRET_MOUNT}/{name}\n"));
            }
            out.push_str("    volumes:\n");
            for (name, source) in &secrets {
                let mount = format!("{}:{SECRET_MOUNT}/{name}:ro", source.display());
                out.push_str(&format!("      - {}\n", yaml_string(&mount)));
            }
        }
        out.push_str("    labels:\n");
        out.push_str(&format!("      com.msbx.project: {}\n", key.project()));
        out.push_str(&format!("      com.msbx.epic: {}\n", key.epic()));
        out.push_str(&format!("      com.msbx.role: {role}\n"));
        out.push_str("    networks:\n      default:\n        aliases:\n");
        out.push_str(&format!("          - {}\n", key.alias(role)));
    }
    out.push_str("networks:\n  default:\n");
    out.push_str(&format!("    name: {}\n", key.network()));
    out
}

/// A YAML double-quoted string. Paths under `$HOME` may carry spaces, and the
/// rendered file lives outside the checkout, so they are always quoted.
fn yaml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// What a render recorded, in the shape the state file keeps.
pub fn recorded(published: &[Published]) -> Vec<PublishedPort> {
    published
        .iter()
        .map(|port| PublishedPort {
            role: port.role.clone(),
            port: port.host_port,
        })
        .collect()
}

/// Human-readable changes between the mapping recorded by the last `up` and
/// the one this `up` will apply. Empty when the published set is unchanged.
pub fn publish_changes(previous: &[PublishedPort], next: &[Published]) -> Vec<String> {
    let mut changes = Vec::new();
    for port in next {
        match previous.iter().find(|old| old.role == port.role) {
            Some(old) if old.port != port.host_port => {
                changes.push(format!("{} {} → {}", port.role, old.port, port.host_port));
            }
            None => changes.push(format!("{} new on {}", port.role, port.host_port)),
            Some(_) => {}
        }
    }
    for old in previous {
        if !next.iter().any(|port| port.role == old.role) {
            changes.push(format!(
                "{} no longer published (was {})",
                old.role, old.port
            ));
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"x-msbx:
  project: signals
  publish:
    pg: 5432
    web: 3000
services:
  pg:
    image: postgres:17
  web:
    build: .
"#;

    fn key() -> Key {
        Key::new("signals", "MIR-42").unwrap()
    }

    #[test]
    fn names_are_project_epic_role_in_both_directions() {
        let key = key();
        assert_eq!(key.epic(), "mir-42");
        assert_eq!(key.compose_project(), "signals-mir-42");
        assert_eq!(key.container("pg"), "signals-mir-42-pg");
        assert_eq!(key.alias("pg"), "pg.mir-42.signals.msbx.test");
        assert_eq!(key.dir(), state::epic_dir("signals", "mir-42"));
    }

    #[test]
    fn keys_stay_one_label() {
        assert!(Key::new("Signals", "mir-1").is_err());
        assert!(Key::new("signals", "mir 1").is_err());
        assert!(Key::new("signals", "-mir").is_err());
        assert_eq!(normalize_epic(" MIR-178 ").unwrap(), "mir-178");
    }

    #[test]
    fn publish_order_fixes_host_ports_from_the_block() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let ports = published(&manifest, Block { start: 23000 }).unwrap();
        assert_eq!(
            ports,
            vec![
                Published {
                    role: "pg".into(),
                    container_port: 5432,
                    host_port: 23000,
                },
                Published {
                    role: "web".into(),
                    container_port: 3000,
                    host_port: 23001,
                },
            ]
        );
    }

    #[test]
    fn more_published_roles_than_the_block_holds_is_an_error() {
        let mut manifest = Manifest::parse(MANIFEST).unwrap();
        manifest.services.insert("extra".into());
        for index in 0..BLOCK_SIZE {
            manifest
                .publish
                .push((format!("role-{index}"), 1000 + index));
        }
        assert!(published(&manifest, Block { start: 23000 })
            .unwrap_err()
            .to_string()
            .contains("a block holds"));
    }

    #[test]
    fn the_override_carries_names_aliases_ports_and_the_network() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let ports = published(&manifest, Block { start: 23000 }).unwrap();
        let text = override_yaml(&key(), &manifest, &ports, &Envelope::empty(key().dir()));
        assert!(text.contains("container_name: signals-mir-42-pg"), "{text}");
        assert!(
            text.contains("- pg.mir-42.signals.msbx.test"),
            "the dotted name is an alias: {text}"
        );
        assert!(
            text.contains("- \"127.0.0.1:23000:5432\""),
            "a published role gets its block port: {text}"
        );
        assert!(text.contains("com.msbx.role: web"), "{text}");
        assert!(text.contains("name: signals-mir-42"), "{text}");
        assert_eq!(
            text,
            override_yaml(&key(), &manifest, &ports, &Envelope::empty(key().dir()))
        );
    }

    #[test]
    fn an_unpublished_role_has_an_alias_and_no_host_port() {
        let text = override_yaml(
            &key(),
            &Manifest::parse("x-msbx:\n  project: signals\nservices:\n  client: {}\n").unwrap(),
            &[],
            &Envelope::empty(key().dir()),
        );
        assert!(text.contains("- client.mir-42.signals.msbx.test"));
        assert!(text.contains("container_name: signals-mir-42-client"));
        assert!(!text.contains("ports:"));
    }

    #[test]
    fn a_rendered_envelope_contributes_env_files_and_secret_mounts_not_values() {
        let manifest = Manifest::parse(
            "x-msbx:\n  project: signals\n  env:\n    web: [STRIPE_KEY]\n  secrets:\n    web: [SMTP_PASSWORD]\nservices:\n  web: {}\n",
        )
        .unwrap();
        let key = key();
        let mut values = crate::store::Values::default();
        values
            .env
            .insert("STRIPE_KEY".to_string(), "sk_live_marker".to_string());
        values
            .secrets
            .insert("SMTP_PASSWORD".to_string(), "hunter2-marker".to_string());
        let envelope = Envelope::render(&key, &manifest, &[], &values).unwrap();
        let text = override_yaml(&key, &manifest, &[], &envelope);
        assert!(
            text.contains(&format!(
                "      - \"{}\"",
                key.dir().join("env").join("web.env").display()
            )),
            "the env file is injected by path: {text}"
        );
        assert!(
            text.contains("SMTP_PASSWORD_FILE: /run/msbx/secrets/SMTP_PASSWORD"),
            "{text}"
        );
        assert!(text.contains(":ro\""), "the secret is read-only: {text}");
        assert!(
            !text.contains("sk_live_marker") && !text.contains("hunter2-marker"),
            "a value never enters the override: {text}"
        );
    }

    #[test]
    fn the_override_lives_outside_the_checkout() {
        let key = key();
        assert_eq!(
            key.override_path(),
            state::epic_dir("signals", "mir-42").join(OVERRIDE_FILE)
        );
    }

    #[test]
    fn publish_changes_report_old_and_new_ports() {
        let next = vec![
            Published {
                role: "pg".into(),
                container_port: 5432,
                host_port: 23001,
            },
            Published {
                role: "web".into(),
                container_port: 3000,
                host_port: 23000,
            },
        ];
        let previous = vec![PublishedPort {
            role: "pg".into(),
            port: 23000,
        }];
        let changes = publish_changes(&previous, &next);
        assert_eq!(changes, vec!["pg 23000 → 23001", "web new on 23000"]);
        assert!(publish_changes(&recorded(&next), &next).is_empty());
    }

    #[test]
    fn a_removed_publication_is_reported() {
        let previous = vec![PublishedPort {
            role: "pg".into(),
            port: 23000,
        }];
        assert_eq!(
            publish_changes(&previous, &[]),
            vec!["pg no longer published (was 23000)"]
        );
    }
}
