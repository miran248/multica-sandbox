//! The epic envelope: the per-role env files, the secret files and the harness
//! env, rendered outside every checkout.
//!
//! Values come from the project [`Store`], the manifest lists only names, and
//! every value is rendered for one epic: `{{host:<role>}}`, `{{port:<role>}}`,
//! `{{epic}}` and `{{project}}` are substituted, and an unknown token is an
//! error. The result lives in
//! `$XDG_STATE_HOME/msbx/<project>/<epic>/` — `env/<role>.env`, `env/harness.env`
//! and `secrets/<NAME>` — mode 0600, outside the checkout.
//!
//! No error message and no file header here carries a value: errors name the
//! role, the name, the token or the file.

use crate::epic::{Key, Published};
use crate::manifest::Manifest;
use crate::state;
use crate::store::{parse_env, Values};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The rendered env files, under the epic's directory.
pub const ENV_DIR: &str = "env";
/// The rendered secret files, under the epic's directory.
pub const SECRETS_DIR: &str = "secrets";
/// The harness env, under [`ENV_DIR`].
pub const HARNESS_FILE: &str = "harness.env";
/// Where a service and the harness find a secret, inside the box.
pub const SECRET_MOUNT: &str = "/run/msbx/secrets";

/// One epic's rendered material.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Envelope {
    dir: PathBuf,
    /// Role → rendered `NAME=value` entries, in manifest order.
    env: BTreeMap<String, Vec<(String, String)>>,
    /// Role → secret names it receives, in manifest order.
    secrets: BTreeMap<String, Vec<String>>,
    /// Secret name → rendered value.
    secret_values: BTreeMap<String, String>,
    /// The harness env, sorted by name.
    harness: BTreeMap<String, String>,
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
    /// Render one epic. Every listed name must already be in `values` (see
    /// [`Store::values`]); every token must be known.
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
                    substitute(raw, key, manifest, published, &what)?,
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
                        substitute(raw, key, manifest, published, &what)?,
                    );
                }
                secrets.entry(role.clone()).or_default().push(name.clone());
            }
        }

        let mut harness = BTreeMap::new();
        for (name, value) in published_vars(key, published) {
            insert_harness(&mut harness, name, value)?;
        }
        let mut declared = BTreeSet::new();
        for (role, names) in &manifest.env {
            for name in names {
                if !declared.insert(name.clone()) {
                    continue;
                }
                let value = env
                    .get(role)
                    .and_then(|entries| entries.iter().find(|(seen, _)| seen == name))
                    .map(|(_, value)| value.clone())
                    .ok_or_else(|| {
                        EnvError::new(format!(
                            "`x-msbx.env.{role}` lists `{name}`, which has no value"
                        ))
                    })?;
                insert_harness(&mut harness, name.clone(), value)?;
            }
        }
        for name in secret_values.keys() {
            let path = dir.join(SECRETS_DIR).join(name);
            insert_harness(
                &mut harness,
                format!("{name}_FILE"),
                path.to_string_lossy().to_string(),
            )?;
        }

        Ok(Envelope {
            dir,
            env,
            secrets,
            secret_values,
            harness,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The harness env, sorted by name.
    pub fn harness(&self) -> &BTreeMap<String, String> {
        &self.harness
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

    /// Write the envelope: `env/<role>.env`, `env/harness.env` and
    /// `secrets/<NAME>`, all 0600 under 0700 directories. Files from an earlier
    /// render that this one no longer produces are removed.
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

        let mut body = String::from("# Generated by msbx; the harness envelope for this epic.\n");
        for (name, value) in &self.harness {
            body.push_str(name);
            body.push('=');
            body.push_str(value);
            body.push('\n');
        }
        state::write_private(&env_dir.join(HARNESS_FILE), &body)?;
        kept_env.insert(HARNESS_FILE.to_string());

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
        let text = fs::read_to_string(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                EnvError::new(format!(
                    "no harness envelope at {}; run `msbx up` first",
                    path.display()
                ))
            } else {
                EnvError::new(format!("cannot read {}: {error}", path.display()))
            }
        })?;
        parse_env(&text).map_err(|error| EnvError::new(format!("{}: {error}", path.display())))
    }
}

/// `MSBX_<ROLE>_HOST` and `MSBX_<ROLE>_PORT` for every published role: what the
/// harness and the Compose process see instead of a canonical host port.
pub fn published_vars(key: &Key, published: &[Published]) -> Vec<(String, String)> {
    let mut vars = Vec::with_capacity(published.len() * 2);
    for port in published {
        let base = msbx_var(&port.role);
        vars.push((format!("{base}_HOST"), key.alias(&port.role)));
        vars.push((format!("{base}_PORT"), port.host_port.to_string()));
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

fn insert_harness(
    harness: &mut BTreeMap<String, String>,
    name: String,
    value: String,
) -> Result<(), EnvError> {
    if harness.insert(name.clone(), value).is_some() {
        return Err(EnvError::new(format!(
            "`{name}` is exported twice: an `x-msbx.env` name must not collide with `MSBX_<ROLE>_*` or `<NAME>_FILE`"
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
    what: &str,
) -> Result<String, EnvError> {
    match token {
        "epic" => return Ok(key.epic().to_string()),
        "project" => return Ok(key.project().to_string()),
        _ => {}
    }
    if let Some(role) = token.strip_prefix("host:").map(str::trim) {
        if !role_exists(manifest, role) {
            return Err(EnvError::new(format!(
                "{what}: `{{{{host:{role}}}}}` names no service or tunnel"
            )));
        }
        return Ok(key.alias(role));
    }
    if let Some(role) = token.strip_prefix("port:").map(str::trim) {
        if !role_exists(manifest, role) {
            return Err(EnvError::new(format!(
                "{what}: `{{{{port:{role}}}}}` names no service or tunnel"
            )));
        }
        return published
            .iter()
            .find(|port| port.role == role)
            .map(|port| port.host_port.to_string())
            .ok_or_else(|| {
                EnvError::new(format!(
                    "{what}: `{{{{port:{role}}}}}` names a role with no host port (not in `x-msbx.publish`)"
                ))
            });
    }
    Err(EnvError::new(format!(
        "{what}: `{{{{{token}}}}}` is not a known token (`host:<role>`, `port:<role>`, `epic`, `project`)"
    )))
}

fn role_exists(manifest: &Manifest, role: &str) -> bool {
    manifest.services.contains(role) || manifest.tunnels.contains_key(role)
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
    build: .
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
            "postgres://{{host:pg}}:{{port:pg}}/{{epic}}/{{project}}".into(),
        );
        values
            .env
            .insert("STRIPE_KEY".into(), "sk_live_{{epic}}".into());
        values
            .secrets
            .insert("SMTP_PASSWORD".into(), "hunter2-{{project}}".into());
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
    fn every_token_is_substituted_per_epic() {
        let envelope = render("tokens");
        let pg = &envelope.env["pg"];
        assert_eq!(
            pg[0].1,
            "postgres://pg.mir-42.signals.msbx.test:23000/mir-42/signals"
        );
        assert_eq!(envelope.env["web"][0].1, "sk_live_mir-42");
        assert_eq!(envelope.secret_values["SMTP_PASSWORD"], "hunter2-signals");
    }

    #[test]
    fn the_harness_gets_the_same_values_plus_names_and_ports() {
        let envelope = render("harness");
        assert_eq!(
            envelope.harness()["DATABASE_URL"],
            "postgres://pg.mir-42.signals.msbx.test:23000/mir-42/signals"
        );
        assert_eq!(envelope.harness()["STRIPE_KEY"], "sk_live_mir-42");
        assert_eq!(
            envelope.harness()["MSBX_PG_HOST"],
            "pg.mir-42.signals.msbx.test"
        );
        assert_eq!(envelope.harness()["MSBX_PG_PORT"], "23000");
        assert_eq!(
            envelope.harness()["SMTP_PASSWORD_FILE"],
            envelope
                .secret_source("SMTP_PASSWORD")
                .to_string_lossy()
                .to_string()
        );
        assert!(!envelope.harness().contains_key("MSBX_CLIENT_PORT"));
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
                    secrets: BTreeMap::new(),
                },
            )
            .unwrap_err()
            .to_string()
        };
        assert!(
            error("{{nope}}").contains("is not a known token"),
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
        assert!(error.contains("exported twice"), "{error}");
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
        assert_eq!(harness["MSBX_PG_PORT"], "23000");
        assert!(!dir.join(ENV_DIR).join("old.env").exists());
        assert!(!dir.join("secrets").join("OLD_SECRET").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                dir.join(ENV_DIR).join("pg.env"),
                dir.join(ENV_DIR).join(HARNESS_FILE),
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
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn export_lines_are_shell_safe() {
        assert_eq!(export_line("A", "plain"), "export A='plain'");
        assert_eq!(export_line("A", "it's"), r#"export A='it'\''s'"#);
        assert_eq!(export_line("A", "$HOME `x`"), "export A='$HOME `x`'");
    }

    #[test]
    fn roles_with_hyphens_get_underscored_variable_names() {
        assert_eq!(msbx_var("pg-analytics"), "MSBX_PG_ANALYTICS");
    }
}
