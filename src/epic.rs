//! The epic namespace: keys, names, paths, published ports, tunnel sidecars and
//! the generated Compose override.
//!
//! Everything here is pure: the same [`Manifest`], [`Block`] and [`Envelope`]
//! produce the same override text, so a second `up` regenerates an identical
//! file and Compose changes nothing. The override is the only place container
//! names, dotted aliases, loopback port mappings, env files, secret mounts,
//! tunnel sidecars and the epic's network name are decided.

use crate::dns;
use crate::env::{Envelope, SECRET_MOUNT};
use crate::manifest::{Manifest, Tunnel, Via};
use crate::ports::{Block, BLOCK_SIZE};
use crate::state::{self, PublishedPort, TunnelRecord};
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
/// `/run/msbx/secrets/<NAME>` with `<NAME>_FILE` in its environment. A
/// `x-msbx.tunnels` role gets a generated sidecar instead of a Compose service:
/// it listens on the role's port inside the epic, carries the same alias and
/// labels, and is never given a host port unless `publish` lists it. The
/// default network is named after the Compose project.
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
        push_ports(&mut out, published, role);
        push_env_and_secrets(&mut out, envelope, role);
        push_labels(&mut out, key, role, None);
        push_alias(&mut out, key, role);
    }
    for (role, tunnel) in &manifest.tunnels {
        out.push_str(&format!("  {role}:\n"));
        out.push_str(&format!("    container_name: {}\n", key.container(role)));
        push_transport(&mut out, role, tunnel);
        push_ports(&mut out, published, role);
        push_env_and_secrets(&mut out, envelope, role);
        push_labels(&mut out, key, role, Some(tunnel));
        push_alias(&mut out, key, role);
    }
    out.push_str("networks:\n  default:\n");
    out.push_str(&format!("    name: {}\n", key.network()));
    out
}

/// The loopback mapping for a published role. A tunnelled role spends its
/// block port on the sidecar's listening port, exactly like a local service.
fn push_ports(out: &mut String, published: &[Published], role: &str) {
    if let Some(port) = published.iter().find(|port| port.role == role) {
        out.push_str("    ports:\n");
        out.push_str(&format!(
            "      - \"127.0.0.1:{}:{}\"\n",
            port.host_port, port.container_port
        ));
    }
}

/// The rendered env file and the read-only secret mounts. A tunnel sidecar
/// receives them too, so a project's configuration is the same whether the
/// role is local or remote.
fn push_env_and_secrets(out: &mut String, envelope: &Envelope, role: &str) {
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
}

fn push_labels(out: &mut String, key: &Key, role: &str, tunnel: Option<&Tunnel>) {
    out.push_str("    labels:\n");
    out.push_str(&format!("      com.msbx.project: {}\n", key.project()));
    out.push_str(&format!("      com.msbx.epic: {}\n", key.epic()));
    out.push_str(&format!("      com.msbx.role: {role}\n"));
    if let Some(tunnel) = tunnel {
        out.push_str(&format!("      com.msbx.tunnel: {}\n", tunnel.via.as_str()));
    }
}

fn push_alias(out: &mut String, key: &Key, role: &str) {
    out.push_str("    networks:\n      default:\n        aliases:\n");
    out.push_str(&format!("          - {}\n", key.alias(role)));
}

/// How the generated sidecar terminates the tunnel. The `ssh` transport is a
/// shell that installs the client, materializes any declared key/known-hosts
/// secret and execs `ssh -N -L`; the `cloudflared` transport runs the
/// transport binary with the role's Access credentials from its envelope.
fn push_transport(out: &mut String, role: &str, tunnel: &Tunnel) {
    match tunnel.via {
        Via::Ssh => {
            out.push_str("    image: alpine:3.20\n");
            out.push_str("    command:\n      - sh\n      - -c\n      - |\n");
            for line in ssh_command(role, tunnel).lines() {
                out.push_str("        ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("    healthcheck:\n");
            out.push_str(&format!(
                "      test: [\"CMD-SHELL\", \"nc -z 127.0.0.1 {}\"]\n",
                tunnel.port
            ));
            out.push_str("      interval: 5s\n      timeout: 3s\n      retries: 12\n");
            out.push_str("      start_period: 30s\n");
        }
        Via::Cloudflared => {
            out.push_str("    image: cloudflare/cloudflared:2026.11.1\n");
            out.push_str("    command:\n      - access\n      - tcp\n      - --hostname\n");
            out.push_str(&format!("      - {}\n", yaml_string(&tunnel.host)));
            out.push_str("      - --url\n");
            out.push_str(&format!("      - 0.0.0.0:{}\n", tunnel.port));
        }
    }
}

/// The `ssh` sidecar script. The role's envelope supplies the login and any
/// transport settings: `MSBX_TUNNEL_SSH_USER` (required), `MSBX_TUNNEL_SSH_HOST`
/// (the ssh endpoint, defaulting to the tunnel host), `MSBX_TUNNEL_SSH_PORT`
/// (default 22), and the secrets `MSBX_TUNNEL_SSH_KEY` and
/// `MSBX_TUNNEL_SSH_KNOWN_HOSTS`, each base64 (one line, as the store requires)
/// and decoded to a private file before use.
///
/// Compose interpolates `$` in every string it reads, including this one, so a
/// shell variable is written `$$VAR`: Compose hands the container a literal
/// `$VAR` and the shell, not the host, expands it.
fn ssh_command(role: &str, tunnel: &Tunnel) -> String {
    SSH_COMMAND
        .replace("@ROLE@", role)
        .replace("@HOST@", &tunnel.host)
        .replace("@PORT@", &tunnel.port.to_string())
}

const SSH_COMMAND: &str = r#"set -eu
apk add --no-cache openssh-client >/dev/null
: "$${MSBX_TUNNEL_SSH_USER:?x-msbx.env.@ROLE@ must declare MSBX_TUNNEL_SSH_USER}"
set -- -N -T \
  -o BatchMode=yes \
  -o ExitOnForwardFailure=yes \
  -o ConnectTimeout=10 \
  -o ServerAliveInterval=15 \
  -o ServerAliveCountMax=3 \
  -p "$${MSBX_TUNNEL_SSH_PORT:-22}" \
  -L "0.0.0.0:@PORT@:@HOST@:@PORT@"
if [ -n "$${MSBX_TUNNEL_SSH_KEY_FILE:-}" ]; then
  base64 -d "$$MSBX_TUNNEL_SSH_KEY_FILE" > /tmp/msbx-ssh-key
  chmod 600 /tmp/msbx-ssh-key
  set -- "$$@" -i /tmp/msbx-ssh-key
fi
if [ -n "$${MSBX_TUNNEL_SSH_KNOWN_HOSTS_FILE:-}" ]; then
  base64 -d "$$MSBX_TUNNEL_SSH_KNOWN_HOSTS_FILE" > /tmp/msbx-known-hosts
  set -- "$$@" -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=/tmp/msbx-known-hosts"
else
  mkdir -p /root/.ssh
  set -- "$$@" -o StrictHostKeyChecking=accept-new
fi
exec ssh "$$@" "$${MSBX_TUNNEL_SSH_USER}@$${MSBX_TUNNEL_SSH_HOST:-@HOST@}"
"#;

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

/// The generated tunnels, in the shape the state file keeps, so `status` can
/// mark a role as remote after the checkout is gone.
pub fn recorded_tunnels(manifest: &Manifest) -> Vec<TunnelRecord> {
    manifest
        .tunnels
        .iter()
        .map(|(role, tunnel)| TunnelRecord {
            role: role.clone(),
            via: tunnel.via.as_str().to_string(),
            upstream: format!("{}:{}", tunnel.host, tunnel.port),
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

    fn tunnel_manifest(via: &str, published: &str) -> Manifest {
        Manifest::parse(&format!(
            "x-msbx:\n  project: signals\n{published}  env:\n    pg: [MSBX_TUNNEL_SSH_USER]\n  secrets:\n    pg: [MSBX_TUNNEL_SSH_KEY, MSBX_TUNNEL_SSH_KNOWN_HOSTS]\n  tunnels:\n    pg:\n      host: db.internal.example.com\n      port: 5432\n      via: {via}\nservices:\n  client: {{}}\n"
        ))
        .unwrap()
    }

    fn tunnel_envelope(manifest: &Manifest) -> Envelope {
        let mut values = crate::store::Values::default();
        values
            .env
            .insert("MSBX_TUNNEL_SSH_USER".to_string(), "tunnel".to_string());
        values.secrets.insert(
            "MSBX_TUNNEL_SSH_KEY".to_string(),
            "a2V5LW1hcmtlcg==".to_string(),
        );
        values.secrets.insert(
            "MSBX_TUNNEL_SSH_KNOWN_HOSTS".to_string(),
            "a25vd24tbWFya2Vy".to_string(),
        );
        Envelope::render(&key(), manifest, &[], &values).unwrap()
    }

    #[test]
    fn a_tunnelled_role_gets_a_generated_ssh_sidecar() {
        let manifest = tunnel_manifest("ssh", "");
        let envelope = tunnel_envelope(&manifest);
        let text = override_yaml(&key(), &manifest, &[], &envelope);
        assert!(text.contains("  pg:\n"), "{text}");
        assert!(text.contains("container_name: signals-mir-42-pg"), "{text}");
        assert!(text.contains("image: alpine:3.20"), "{text}");
        assert!(
            text.contains("-L \"0.0.0.0:5432:db.internal.example.com:5432\""),
            "the sidecar listens on the role's port and terminates the tunnel: {text}"
        );
        assert!(text.contains(" -L \"0.0.0.0:5432:"), "{text}");
        assert!(
            text.contains("ExitOnForwardFailure=yes") && text.contains("ConnectTimeout=10"),
            "an unreachable endpoint fails instead of hanging: {text}"
        );
        assert!(
            text.contains("$${MSBX_TUNNEL_SSH_USER"),
            "Compose must not interpolate the shell's variables: {text}"
        );
        assert!(
            text.contains("-o StrictHostKeyChecking=accept-new"),
            "{text}"
        );
        assert!(
            text.contains("test: [\"CMD-SHELL\", \"nc -z 127.0.0.1 5432\"]"),
            "the sidecar is unhealthy when the listener is gone: {text}"
        );
        assert!(
            text.contains("MSBX_TUNNEL_SSH_KEY_FILE: /run/msbx/secrets/MSBX_TUNNEL_SSH_KEY"),
            "{text}"
        );
        assert!(
            text.contains("com.msbx.tunnel: ssh"),
            "a tunnel is labelled: {text}"
        );
        assert!(
            text.contains("- pg.mir-42.signals.msbx.test"),
            "the sidecar carries the dotted alias: {text}"
        );
        assert!(
            !text.contains("127.0.0.1:23000:5432"),
            "an unpublished tunnel has no host port: {text}"
        );
        assert!(
            !text.contains("a2V5LW1hcmtlcg==") && !text.contains("a25vd24tbWFya2Vy"),
            "no secret value enters the override: {text}"
        );
    }

    #[test]
    fn a_tunnelled_role_can_be_published_through_the_sidecar() {
        let manifest = tunnel_manifest("ssh", "  publish:\n    pg: 5432\n");
        let ports = published(&manifest, Block { start: 23000 }).unwrap();
        let envelope = tunnel_envelope(&manifest);
        let text = override_yaml(&key(), &manifest, &ports, &envelope);
        assert!(text.contains("- \"127.0.0.1:23000:5432\""), "{text}");
    }

    #[test]
    fn a_cloudflared_tunnel_uses_the_transport_image() {
        let manifest = tunnel_manifest("cloudflared", "");
        let envelope = tunnel_envelope(&manifest);
        let text = override_yaml(&key(), &manifest, &[], &envelope);
        assert!(
            text.contains("image: cloudflare/cloudflared:2026.11.1"),
            "{text}"
        );
        assert!(
            text.contains("      - access\n      - tcp\n      - --hostname\n      - \"db.internal.example.com\"\n      - --url\n      - 0.0.0.0:5432\n"),
            "{text}"
        );
        assert!(!text.contains("127.0.0.1:23000:5432"), "{text}");
    }

    #[test]
    fn recorded_tunnels_name_the_role_transport_and_upstream() {
        let manifest = tunnel_manifest("cloudflared", "");
        assert_eq!(
            recorded_tunnels(&manifest),
            vec![TunnelRecord {
                role: "pg".to_string(),
                via: "cloudflared".to_string(),
                upstream: "db.internal.example.com:5432".to_string(),
            }]
        );
    }
}
