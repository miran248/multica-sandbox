//! The scope envelope: the per-role env files, the secret files, the harness
//! env and the host exports, rendered outside every checkout.
//!
//! Values come from the three layers ([`Layers`]), the manifest lists only
//! names, and every value is rendered for one scope in one context:
//!
//! - `env/<role>.env` and `env/harness.env` carry the **container** ports,
//!   because their consumer is on the scope's Compose network.
//! - `env/host.env` carries the **assigned loopback** ports, because that is
//!   what a host shell can reach. `msbx env export` prints this file.
//!
//! `{{host:<role>}}`, `{{port:<role>}}`, `{{scope}}` and `{{project}}` are
//! substituted, and an unknown token is an error. `{{epic}}` is retired: it is
//! an error naming `{{scope}}`, never a silent alias. The rendered material
//! lives under `$XDG_STATE_HOME/msbx/<project>/<scope>/`, mode 0600, outside the
//! checkout.
//!
//! No error message and no file header here carries a value: errors name the
//! role, the name, the token or the file.

use crate::manifest::Manifest;
use crate::scope::{Key, Published};
use crate::state;
use crate::store::{parse_env, Values};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The rendered env files, under the scope's directory.
pub const ENV_DIR: &str = "env";
/// The rendered secret files, under the scope's directory.
pub const SECRETS_DIR: &str = "secrets";
/// The harness env, under [`ENV_DIR`].
pub const HARNESS_FILE: &str = "harness.env";
/// The host-context exports, under [`ENV_DIR`].
pub const HOST_FILE: &str = "host.env";
/// Where a service finds a secret, inside the box.
pub const SECRET_MOUNT: &str = "/run/msbx/secrets";

/// Which port a `{{port:<role>}}` token renders in this context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    /// A service or the harness container on the scope network.
    Container,
    /// A host shell, which reaches the assigned loopback port.
    Host,
}

impl Context {
    fn port(self, port: &Published) -> u16 {
        match self {
            Context::Container => port.container_port,
            Context::Host => port.host_port,
        }
    }
}

/// One scope's rendered material.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Envelope {
    dir: PathBuf,
    /// Role → rendered `NAME=value` entries for its service, in manifest order.
    env: BTreeMap<String, Vec<(String, String)>>,
    /// Role → secret names it receives, in manifest order.
    secrets: BTreeMap<String, Vec<String>>,
    /// Secret name → rendered value (the service and the harness share it).
    secret_values: BTreeMap<String, String>,
    /// The harness env, sorted by name (container ports).
    harness: BTreeMap<String, String>,
    /// The host envelope, sorted by name (assigned loopback ports).
    host: BTreeMap<String, String>,
}

/// A refused render. The message names what is wrong, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvError(String);

impl EnvError {
    fn new(message: impl Into<String>) -> Self {
        EnvError(message.into())
    }
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EnvError {}

impl Envelope {
    /// Render one scope. Every listed name must already be resolved (see
    /// [`Layers::values`]); every token must be known.
    pub fn render(
        key: &Key,
        manifest: &Manifest,
        published: &[Published],
        values: &Values,
    ) -> Result<Envelope, EnvError> {
        Self::render_in(key.dir(), key, manifest, published, values)
    }

    /// The empty envelope for a directory. Used where nothing is declared.
    pub fn empty(dir: impl Into<PathBuf>) -> Envelope {
        Envelope {
            dir: dir.into(),
            ..Envelope::default()
        }
    }

    fn render_in(
        dir: PathBuf,
        key: &Key,
        manifest: &Manifest,
        published: &[Published],
        values: &Values,
    ) -> Result<Envelope, EnvError> {
        check_reserved(manifest, values)?;

        let mut env = BTreeMap::new();
        for (role, names) in &manifest.env {
            let mut entries = Vec::with_capacity(names.len());
            for name in names {
                let raw = values.env.get(name).ok_or_else(|| {
                    EnvError::new(format!(
                        "`x-msbx.env.{role}` lists `{name}`, which has no value"
                    ))
                })?;
                let what = format!("env `{name}` for {role}");
                entries.push((
                    name.clone(),
                    substitute(raw, key, manifest, published, Context::Container, &what)?,
                ));
            }
            if !entries.is_empty() {
                env.insert(role.clone(), entries);
            }
        }

        let mut secrets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut secret_values = BTreeMap::new();
        for (role, names) in &manifest.secrets {
            for name in names {
                let raw = values.secrets.get(name).ok_or_else(|| {
                    EnvError::new(format!(
                        "`x-msbx.secrets.{role}` lists `{name}`, which has no value"
                    ))
                })?;
                if !secret_values.contains_key(name) {
                    let what = format!("secret `{name}` for {role}");
                    secret_values.insert(
                        name.clone(),
                        substitute(raw, key, manifest, published, Context::Container, &what)?,
                    );
                }
                secrets.entry(role.clone()).or_default().push(name.clone());
            }
        }

        // The harness gets container ports and the values of every declared
        // secret (it cannot see the services' secret mounts). The harness layer
        // wins over a declared value; it may not shadow a generated MSBX_<ROLE>_*
        // name.
        let mut harness = BTreeMap::new();
        let generated: BTreeSet<String> = published_vars(key, published)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        for (name, value) in published_vars(key, published) {
            insert_unique(&mut harness, name, value)?;
        }
        for (role, names) in &manifest.env {
            for name in names {
                let raw = values.env.get(name).ok_or_else(|| {
                    EnvError::new(format!(
                        "`x-msbx.env.{role}` lists `{name}`, which has no value"
                    ))
                })?;
                let value = substitute(raw, key, manifest, published, Context::Container, name)?;
                harness.insert(name.clone(), value);
            }
        }
        for (name, value) in &secret_values {
            if generated.contains(name) {
                return Err(EnvError::new(format!(
                    "`{name}` is rendered twice: an `x-msbx.secrets` name must not collide with `MSBX_<ROLE>_*`"
                )));
            }
            harness.insert(name.clone(), value.clone());
        }
        for (name, raw) in &values.harness {
            if generated.contains(name) {
                return Err(EnvError::new(format!(
                    "the harness store sets `{name}`, which is generated for every scope; rename it"
                )));
            }
            let value = substitute(
                raw,
                key,
                manifest,
                published,
                Context::Container,
                &format!("harness `{name}`"),
            )?;
            harness.insert(name.clone(), value);
        }

        // The host context: the same names with the assigned loopback ports.
        // A declared secret is a path to the rendered file, because the host can
        // read it; the harness gets the value instead.
        let mut host = BTreeMap::new();
        let mut generated_files = BTreeSet::new();
        for (name, value) in host_vars(key, published) {
            insert_unique(&mut host, name, value)?;
        }
        for (role, names) in &manifest.env {
            for name in names {
                let raw = values.env.get(name).ok_or_else(|| {
                    EnvError::new(format!(
                        "`x-msbx.env.{role}` lists `{name}`, which has no value"
                    ))
                })?;
                if generated.contains(name) {
                    return Err(EnvError::new(format!(
                        "`{name}` is rendered twice: an `x-msbx.env` name must not collide with `MSBX_<ROLE>_*` or `<NAME>_FILE`"
                    )));
                }
                let value = substitute(raw, key, manifest, published, Context::Host, name)?;
                host.insert(name.clone(), value);
            }
        }
        for name in secret_values.keys() {
            let file = format!("{name}_FILE");
            let path = dir.join(SECRETS_DIR).join(name);
            insert_unique(&mut host, file.clone(), path.to_string_lossy().to_string())?;
            generated_files.insert(file);
        }
        for (name, raw) in &values.harness {
            if generated.contains(name) || generated_files.contains(name) {
                return Err(EnvError::new(format!(
                    "the harness store sets `{name}`, which is generated for every scope; rename it"
                )));
            }
            let value = substitute(
                raw,
                key,
                manifest,
                published,
                Context::Host,
                &format!("harness `{name}`"),
            )?;
            host.insert(name.clone(), value);
        }

        Ok(Envelope {
            dir,
            env,
            secrets,
            secret_values,
            harness,
            host,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The harness env, sorted by name.
    pub fn harness(&self) -> &BTreeMap<String, String> {
        &self.harness
    }

    /// The host env, sorted by name.
    pub fn host(&self) -> &BTreeMap<String, String> {
        &self.host
    }

    /// The role's rendered env file, when it has entries.
    pub fn role_env_file(&self, role: &str) -> Option<PathBuf> {
        self.env
            .contains_key(role)
            .then(|| self.dir.join(ENV_DIR).join(format!("{role}.env")))
    }

    /// Role → the env names it receives, without values.
    pub fn role_env_names(&self) -> impl Iterator<Item = (&str, Vec<&str>)> {
        self.env.iter().map(|(role, entries)| {
            (
                role.as_str(),
                entries.iter().map(|(name, _)| name.as_str()).collect(),
            )
        })
    }

    /// Role → the secret names it receives.
    pub fn role_secret_names(&self) -> impl Iterator<Item = (&str, Vec<&str>)> {
        self.secrets
            .iter()
            .map(|(role, names)| (role.as_str(), names.iter().map(String::as_str).collect()))
    }

    /// A secret's rendered file, the bind-mount source.
    pub fn secret_source(&self, name: &str) -> PathBuf {
        self.dir.join(SECRETS_DIR).join(name)
    }

    /// The secrets one role mounts: each name and its source file.
    pub fn secret_mounts(&self, role: &str) -> Vec<(String, PathBuf)> {
        self.secrets
            .get(role)
            .map(|names| {
                names
                    .iter()
                    .map(|name| (name.clone(), self.secret_source(name)))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn harness_file(&self) -> PathBuf {
        self.dir.join(ENV_DIR).join(HARNESS_FILE)
    }

    pub fn host_file(&self) -> PathBuf {
        self.dir.join(ENV_DIR).join(HOST_FILE)
    }

    /// Write the envelope: `env/<role>.env` and `env/harness.env` (container
    /// ports), `env/host.env` (export lines with the assigned loopback ports)
    /// and `secrets/<NAME>`, all 0600 under 0700 directories. Files from an
    /// earlier render that this one no longer produces are removed.
    pub fn write(&self) -> io::Result<()> {
        let env_dir = self.dir.join(ENV_DIR);
        let secret_dir = self.dir.join(SECRETS_DIR);
        state::create_private_dir(&env_dir)?;
        state::create_private_dir(&secret_dir)?;

        let mut kept_env = BTreeSet::new();
        for (role, entries) in &self.env {
            let mut body = String::from("# Generated by msbx; rewritten by every `msbx up`.\n");
            for (name, value) in entries {
                body.push_str(name);
                body.push('=');
                body.push_str(value);
                body.push('\n');
            }
            let file = format!("{role}.env");
            state::write_private(&env_dir.join(&file), &body)?;
            kept_env.insert(file);
        }

        let mut body = String::from("# Generated by msbx; the harness envelope for this scope.\n");
        for (name, value) in &self.harness {
            body.push_str(name);
            body.push('=');
            body.push_str(value);
            body.push('\n');
        }
        state::write_private(&env_dir.join(HARNESS_FILE), &body)?;
        kept_env.insert(HARNESS_FILE.to_string());

        // The host file is shell exports: `msbx env export` prints it verbatim.
        let mut host = String::new();
        for (name, value) in &self.host {
            host.push_str(&export_line(name, value));
            host.push('\n');
        }
        state::write_private(&env_dir.join(HOST_FILE), &host)?;
        kept_env.insert(HOST_FILE.to_string());

        let mut kept_secrets = BTreeSet::new();
        for (name, value) in &self.secret_values {
            state::write_private(&secret_dir.join(name), &format!("{value}\n"))?;
            kept_secrets.insert(name.clone());
        }

        prune(&env_dir, &kept_env)?;
        prune(&secret_dir, &kept_secrets)?;
        Ok(())
    }

    /// Read a written `env/harness.env`. The file is our own dotenv, so it is
    /// parsed with the store's line rules.
    pub fn read_harness(dir: &Path) -> Result<BTreeMap<String, String>, EnvError> {
        let path = dir.join(ENV_DIR).join(HARNESS_FILE);
        let text = read_rendered(&path)?;
        parse_env(&text).map_err(|error| EnvError::new(format!("{}: {error}", path.display())))
    }

    /// Read a written `env/host.env` and hand it back, so `env export` prints
    /// exactly what the last `up` rendered.
    pub fn read_host(dir: &Path) -> Result<String, EnvError> {
        read_rendered(&dir.join(ENV_DIR).join(HOST_FILE))
    }
}

fn read_rendered(path: &Path) -> Result<String, EnvError> {
    fs::read_to_string(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            EnvError::new(format!(
                "no rendered envelope at {}; run `msbx up` first",
                path.display()
            ))
        } else {
            EnvError::new(format!("cannot read {}: {error}", path.display()))
        }
    })
}

/// `MSBX_<ROLE>_HOST` and `MSBX_<ROLE>_PORT` for every published role, in the
/// container context: the alias and the role's container port, what a service
/// and the harness container see.
pub fn published_vars(key: &Key, published: &[Published]) -> Vec<(String, String)> {
    vars(key, published, Context::Container)
}

/// The same pair in the host context: the alias and the assigned loopback port,
/// what a host shell can reach.
pub fn host_vars(key: &Key, published: &[Published]) -> Vec<(String, String)> {
    vars(key, published, Context::Host)
}

fn vars(key: &Key, published: &[Published], context: Context) -> Vec<(String, String)> {
    let mut vars = Vec::with_capacity(published.len() * 2);
    for port in published {
        let base = msbx_var(&port.role);
        vars.push((format!("{base}_HOST"), key.alias(&port.role)));
        vars.push((format!("{base}_PORT"), context.port(port).to_string()));
    }
    vars
}

/// The env-name form of a role: uppercased, hyphens become underscores.
pub fn msbx_var(role: &str) -> String {
    format!("MSBX_{}", role.to_uppercase().replace('-', "_"))
}

/// One shell-safe `export` line. A value never needs to be inspected by the
/// shell: single quotes keep it literal and `'\''` closes and reopens them.
pub fn export_line(name: &str, value: &str) -> String {
    format!("export {name}='{}'", value.replace('\'', "'\\''"))
}

/// Read back [`export_line`] output: the wrapper consumes `msbx env export`
/// without handing the values to a shell.
pub fn parse_exports(text: &str) -> Result<Vec<(String, String)>, EnvError> {
    let mut seen = BTreeSet::new();
    let mut exports = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let (name, value) = parse_export_line(line).map_err(|error| {
            EnvError::new(format!("`msbx env export` line {}: {error}", index + 1))
        })?;
        if !seen.insert(name.clone()) {
            return Err(EnvError::new(format!(
                "`msbx env export` exports `{name}` twice"
            )));
        }
        exports.push((name, value));
    }
    Ok(exports)
}

fn parse_export_line(line: &str) -> Result<(String, String), String> {
    let rest = line
        .strip_prefix("export ")
        .ok_or_else(|| "not an `export NAME='value'` line".to_string())?;
    let (name, quoted) = rest
        .split_once('=')
        .ok_or_else(|| "not an `export NAME='value'` line".to_string())?;
    if !crate::store::valid_name(name) {
        return Err(format!("`{name}` is not a valid env name"));
    }
    let quoted = quoted
        .strip_prefix('\'')
        .ok_or_else(|| format!("`{name}` is not single-quoted"))?;

    let bytes = quoted.as_bytes();
    let mut value = String::new();
    let mut index = 0;
    loop {
        match bytes.get(index) {
            None => return Err(format!("`{name}` has no closing quote")),
            Some(b'\'') => {
                if index + 1 == bytes.len() {
                    break;
                }
                // [`export_line`] writes a quote inside the value as `'\''`.
                if bytes.get(index + 1) == Some(&b'\\')
                    && bytes.get(index + 2) == Some(&b'\'')
                    && bytes.get(index + 3) == Some(&b'\'')
                {
                    value.push('\'');
                    index += 4;
                    continue;
                }
                return Err(format!("`{name}` has text after its closing quote"));
            }
            Some(_) => {
                let character = quoted[index..]
                    .chars()
                    .next()
                    .expect("a byte index always starts a char");
                value.push(character);
                index += character.len_utf8();
            }
        }
    }
    Ok((name.to_string(), value))
}

fn insert_unique(
    map: &mut BTreeMap<String, String>,
    name: String,
    value: String,
) -> Result<(), EnvError> {
    if map.insert(name.clone(), value).is_some() {
        return Err(EnvError::new(format!(
            "`{name}` is rendered twice: an `x-msbx.env` name must not collide with `MSBX_<ROLE>_*` or `<NAME>_FILE`"
        )));
    }
    Ok(())
}

/// Substitute every `{{...}}` token in one store value.
fn substitute(
    text: &str,
    key: &Key,
    manifest: &Manifest,
    published: &[Published],
    context: Context,
    what: &str,
) -> Result<String, EnvError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(EnvError::new(format!(
                "{what}: a `{{` is never closed with `}}`"
            )));
        };
        out.push_str(&expand(
            after[..end].trim(),
            key,
            manifest,
            published,
            context,
            what,
        )?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn expand(
    token: &str,
    key: &Key,
    manifest: &Manifest,
    published: &[Published],
    context: Context,
    what: &str,
) -> Result<String, EnvError> {
    match token {
        "scope" => return Ok(key.scope().to_string()),
        "epic" => {
            return Err(EnvError::new(format!(
                "{what}: `{{{{epic}}}}` is retired; use `{{{{scope}}}}`"
            )))
        }
        "project" => return Ok(key.project().to_string()),
        _ => {}
    }
    // A malformed token may be a fragment of a store value, so errors name the
    // env and the role they belong to and never repeat the token's text.
    if let Some(role) = token.strip_prefix("host:").map(str::trim) {
        if !role_exists(manifest, role) {
            return Err(EnvError::new(format!(
                "{what}: a `{{{{host:<role>}}}}` token names no service or tunnel"
            )));
        }
        return Ok(key.alias(role));
    }
    if let Some(role) = token.strip_prefix("port:").map(str::trim) {
        if !role_exists(manifest, role) {
            return Err(EnvError::new(format!(
                "{what}: a `{{{{port:<role>}}}}` token names no service or tunnel"
            )));
        }
        return published
            .iter()
            .find(|port| port.role == role)
            .map(|port| context.port(port).to_string())
            .ok_or_else(|| {
                EnvError::new(format!(
                    "{what}: a `{{{{port:<role>}}}}` token names a role with no host port (not in `x-msbx.publish`)"
                ))
            });
    }
    Err(EnvError::new(format!(
        "{what}: a `{{{{…}}}}` token is not known (`host:<role>`, `port:<role>`, `scope`, `project`)"
    )))
}

fn role_exists(manifest: &Manifest, role: &str) -> bool {
    manifest.services.contains(role) || manifest.tunnels.contains_key(role)
}

/// Reserved names are refused at `up` (in the store layer) and again here at
/// render, so a caller that renders without `Layers::values` still cannot put
/// one in an envelope. The message names the name, the layer and the role.
fn check_reserved(manifest: &Manifest, values: &Values) -> Result<(), EnvError> {
    for (role, names) in &manifest.env {
        for name in names {
            refuse_reserved(name, "project", role)?;
        }
    }
    for (role, names) in &manifest.secrets {
        for name in names {
            refuse_reserved(name, "project", role)?;
        }
    }
    for name in values.harness.keys() {
        refuse_reserved(name, "harness", "harness")?;
    }
    Ok(())
}

fn refuse_reserved(name: &str, layer: &str, role: &str) -> Result<(), EnvError> {
    if crate::store::reserved(name) {
        return Err(EnvError::new(format!(
            "reserved name `{name}` in layer `{layer}` for role `{role}` is refused; the harness sets it"
        )));
    }
    Ok(())
}

fn prune(dir: &Path, keep: &BTreeSet<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if keep.contains(name) || !entry.file_type()?.is_file() {
            continue;
        }
        fs::remove_file(entry.path())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"x-msbx:
  project: signals
  publish:
    pg: 5432
  env:
    pg: [DATABASE_URL]
    web: [STRIPE_KEY]
  secrets:
    web: [SMTP_PASSWORD]
services:
  pg:
    image: postgres:17
  web:
    image: nginx:1.27
  client:
    image: alpine:3.20
"#;

    fn key() -> Key {
        Key::new("signals", "MIR-42").unwrap()
    }

    fn published() -> Vec<Published> {
        vec![Published {
            role: "pg".into(),
            container_port: 5432,
            host_port: 23000,
        }]
    }

    fn values() -> Values {
        let mut values = Values::default();
        values.env.insert(
            "DATABASE_URL".into(),
            "postgres://{{host:pg}}:{{port:pg}}/{{scope}}/{{project}}".into(),
        );
        values
            .env
            .insert("STRIPE_KEY".into(), "sk_live_{{scope}}".into());
        values
            .secrets
            .insert("SMTP_PASSWORD".into(), "hunter2-{{project}}".into());
        values
            .harness
            .insert("GH_TOKEN".into(), "ghp_marker".into());
        values
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("envelope-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn render(tag: &str) -> Envelope {
        Envelope::render_in(
            scratch(tag),
            &key(),
            &Manifest::parse(MANIFEST).unwrap(),
            &published(),
            &values(),
        )
        .unwrap()
    }

    #[test]
    fn every_token_is_substituted_per_scope() {
        let envelope = render("tokens");
        let pg = &envelope.env["pg"];
        assert_eq!(
            pg[0].1,
            "postgres://pg.mir-42.signals.msbx.test:5432/mir-42/signals"
        );
        assert_eq!(envelope.env["web"][0].1, "sk_live_mir-42");
        assert_eq!(envelope.secret_values["SMTP_PASSWORD"], "hunter2-signals");
    }

    #[test]
    fn the_service_and_harness_contexts_carry_container_ports() {
        let envelope = render("harness");
        assert_eq!(
            envelope.harness()["DATABASE_URL"],
            "postgres://pg.mir-42.signals.msbx.test:5432/mir-42/signals"
        );
        assert_eq!(envelope.harness()["STRIPE_KEY"], "sk_live_mir-42");
        assert_eq!(
            envelope.harness()["MSBX_PG_HOST"],
            "pg.mir-42.signals.msbx.test"
        );
        assert_eq!(envelope.harness()["MSBX_PG_PORT"], "5432");
        assert_eq!(envelope.harness()["SMTP_PASSWORD"], "hunter2-signals");
        assert!(!envelope.harness().contains_key("SMTP_PASSWORD_FILE"));
        assert_eq!(envelope.harness()["GH_TOKEN"], "ghp_marker");
        assert!(!envelope.harness().contains_key("MSBX_CLIENT_PORT"));
    }

    #[test]
    fn the_host_context_carries_the_assigned_loopback_ports() {
        let envelope = render("host");
        assert_eq!(
            envelope.host()["DATABASE_URL"],
            "postgres://pg.mir-42.signals.msbx.test:23000/mir-42/signals"
        );
        assert_eq!(envelope.host()["MSBX_PG_PORT"], "23000");
        assert_eq!(envelope.host()["GH_TOKEN"], "ghp_marker");
        assert_eq!(
            envelope.host()["SMTP_PASSWORD_FILE"],
            envelope
                .secret_source("SMTP_PASSWORD")
                .to_string_lossy()
                .to_string()
        );
        assert!(!envelope.host().contains_key("SMTP_PASSWORD"));
    }

    #[test]
    fn the_harness_value_wins_over_a_declared_one() {
        let mut values = values();
        values
            .harness
            .insert("STRIPE_KEY".into(), "from-harness".into());
        let envelope = Envelope::render_in(
            scratch("harness-wins"),
            &key(),
            &Manifest::parse(MANIFEST).unwrap(),
            &published(),
            &values,
        )
        .unwrap();
        assert_eq!(envelope.harness()["STRIPE_KEY"], "from-harness");
        assert_eq!(envelope.env["web"][0].1, "sk_live_mir-42");
    }

    #[test]
    fn the_harness_store_may_not_shadow_a_generated_name() {
        let mut values = values();
        values.harness.insert("MSBX_PG_PORT".into(), "1".into());
        let error = Envelope::render_in(
            scratch("harness-generated"),
            &key(),
            &Manifest::parse(MANIFEST).unwrap(),
            &published(),
            &values,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("`MSBX_PG_PORT`"), "{error}");
        assert!(error.contains("generated"), "{error}");
    }

    #[test]
    fn the_retired_epic_token_names_its_replacement() {
        let mut values = values();
        values.env.insert("STRIPE_KEY".into(), "sk_{{epic}}".into());
        let error = Envelope::render_in(
            scratch("epic-token"),
            &key(),
            &Manifest::parse(MANIFEST).unwrap(),
            &published(),
            &values,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("`{{epic}}` is retired"), "{error}");
        assert!(error.contains("`{{scope}}`"), "{error}");
    }

    #[test]
    fn unknown_tokens_and_roles_are_errors() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let error = |text: &str| {
            Envelope::render_in(
                scratch("errors"),
                &key(),
                &manifest,
                &published(),
                &Values {
                    env: BTreeMap::from([
                        ("DATABASE_URL".to_string(), "plain".to_string()),
                        ("STRIPE_KEY".to_string(), text.to_string()),
                    ]),
                    secrets: BTreeMap::from([("SMTP_PASSWORD".to_string(), "plain".to_string())]),
                    harness: BTreeMap::new(),
                },
            )
            .unwrap_err()
            .to_string()
        };
        assert!(
            error("{{nope}}").contains("token is not known"),
            "{}",
            error("{{nope}}")
        );
        assert!(
            error("{{host:cache}}").contains("names no service or tunnel"),
            "{}",
            error("{{host:cache}}")
        );
        assert!(
            error("{{port:client}}").contains("no host port"),
            "{}",
            error("{{port:client}}")
        );
        assert!(
            error("{{port:cache}}").contains("names no service or tunnel"),
            "{}",
            error("{{port:cache}}")
        );
        assert!(
            error("x{{port:pg").contains("never closed"),
            "{}",
            error("x{{port:pg")
        );

        // A malformed token may be a fragment of a store value, so the message
        // names the env and the role, never the token's text.
        let message = error("{{sk_live_fragment}}");
        assert!(message.contains("token is not known"), "{message}");
        assert!(
            !message.contains("sk_live_fragment"),
            "the token text is not echoed: {message}"
        );
        let message = error("{{host:sk_live_fragment}}");
        assert!(message.contains("names no service or tunnel"), "{message}");
        assert!(
            !message.contains("sk_live_fragment"),
            "the role text is not echoed: {message}"
        );
    }

    #[test]
    fn a_declared_env_name_that_collides_with_an_exported_one_is_an_error() {
        let manifest = Manifest::parse(
            "x-msbx:\n  project: signals\n  publish:\n    pg: 5432\n  env:\n    web: [MSBX_PG_PORT]\nservices:\n  pg: {}\n  web: {}\n",
        )
        .unwrap();
        let mut values = Values::default();
        values.env.insert("MSBX_PG_PORT".into(), "1".into());
        let error = Envelope::render_in(
            scratch("collision"),
            &key(),
            &manifest,
            &published(),
            &values,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("rendered twice"), "{error}");
    }

    #[test]
    fn writing_is_private_and_prunes_stale_files() {
        let envelope = render("write");
        let dir = envelope.dir().to_path_buf();
        state::create_private_dir(&dir.join(ENV_DIR)).unwrap();
        state::write_private(&dir.join(ENV_DIR).join("old.env"), "GONE=1\n").unwrap();
        state::write_private(&dir.join("secrets").join("OLD_SECRET"), "gone\n").unwrap();
        envelope.write().unwrap();

        let harness = Envelope::read_harness(&dir).unwrap();
        assert_eq!(harness["MSBX_PG_PORT"], "5432");
        let host = Envelope::read_host(&dir).unwrap();
        assert!(host.contains("export MSBX_PG_PORT='23000'"), "{host}");
        assert!(!dir.join(ENV_DIR).join("old.env").exists());
        assert!(!dir.join("secrets").join("OLD_SECRET").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                dir.join(ENV_DIR).join("pg.env"),
                dir.join(ENV_DIR).join(HARNESS_FILE),
                dir.join(ENV_DIR).join(HOST_FILE),
                dir.join(SECRETS_DIR).join("SMTP_PASSWORD"),
            ] {
                let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{}", path.display());
            }
            let dir_mode = fs::metadata(dir.join(SECRETS_DIR))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_harness_file_says_to_run_up() {
        let dir = scratch("no-harness");
        let error = Envelope::read_harness(&dir).unwrap_err().to_string();
        assert!(error.contains("run `msbx up` first"), "{error}");
        let error = Envelope::read_host(&dir).unwrap_err().to_string();
        assert!(error.contains("run `msbx up` first"), "{error}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn export_lines_are_shell_safe() {
        assert_eq!(export_line("A", "plain"), "export A='plain'");
        assert_eq!(export_line("A", "it's"), r#"export A='it'\''s'"#);
        assert_eq!(export_line("A", "$HOME `x`"), "export A='$HOME `x`'");
    }

    #[test]
    fn exported_lines_round_trip_through_the_parser() {
        let values = [
            ("PLAIN", "plain"),
            ("QUOTED", "it's"),
            ("SHELL", "$HOME `x` \"y\" \\"),
            ("EMPTY", ""),
            ("URL", "postgres://pg.mir-42.signals.msbx.test:23000/mir-42"),
            ("UNICODE", "héllo"),
            ("EQUALS", "a=b=c"),
        ];
        let text = values
            .iter()
            .map(|(name, value)| export_line(name, value))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = parse_exports(&text).unwrap();
        assert_eq!(
            parsed,
            values
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn export_parsing_refuses_what_export_line_cannot_write() {
        assert!(parse_exports("PLAIN=1\n").is_err());
        assert!(parse_exports("export BAD NAME='1'\n").is_err());
        assert!(parse_exports("export A=plain\n").is_err());
        assert!(parse_exports("export A='open\n").is_err());
        assert!(parse_exports("export A='x'y'\n").is_err());
        assert!(parse_exports("export A='1'\nexport A='2'\n").is_err());
    }

    #[test]
    fn roles_with_hyphens_get_underscored_variable_names() {
        assert_eq!(msbx_var("pg-analytics"), "MSBX_PG_ANALYTICS");
    }
}
