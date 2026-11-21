//! The per-project value store, outside every checkout.
//!
//! The manifest lists names; the values live in
//! `$XDG_CONFIG_HOME/msbx/projects/<project>/`:
//!
//! ```text
//! env              NAME=value lines, 0600
//! secrets/<NAME>   one value per file, 0600
//! ```
//!
//! [`Store::values`] resolves every name the manifest lists for one epic and
//! fails on the first one with no value, naming it. Nothing here echoes a value:
//! errors name the block, the role, the name or the file, never the content.

use crate::manifest::Manifest;
use crate::state;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One project's store directory.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

/// The values one render needs: the declared env names and secret names,
/// resolved from the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Values {
    pub env: BTreeMap<String, String>,
    pub secrets: BTreeMap<String, String>,
}

/// A store lookup problem. Every message names what is missing, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(String);

impl StoreError {
    fn new(message: impl Into<String>) -> Self {
        StoreError(message.into())
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

impl Store {
    /// The store of the installed binary for `project`.
    pub fn for_project(project: &str) -> Store {
        Store {
            dir: state::store_dir(project),
        }
    }

    /// A store over an explicit directory. Tests and future stores use it.
    pub fn at(dir: impl Into<PathBuf>) -> Store {
        Store { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn env_path(&self) -> PathBuf {
        self.dir.join("env")
    }

    fn secret_path(&self, name: &str) -> PathBuf {
        self.dir.join("secrets").join(name)
    }

    /// Read the project's `env` file. A missing file is an empty store, not an
    /// error; a malformed line names its line number and never its content.
    pub fn env(&self) -> Result<BTreeMap<String, String>, StoreError> {
        let path = self.env_path();
        match fs::read_to_string(&path) {
            Ok(text) => parse_env(&text)
                .map_err(|error| StoreError::new(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(StoreError::new(format!(
                "cannot read {}: {error}",
                path.display()
            ))),
        }
    }

    /// One secret file's value, without its trailing newline.
    pub fn secret(&self, name: &str) -> Result<Option<String>, StoreError> {
        let path = self.secret_path(name);
        match fs::read_to_string(&path) {
            Ok(text) => single_value(name, &text).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StoreError::new(format!(
                "cannot read the secret `{name}` at {}: {error}",
                path.display()
            ))),
        }
    }

    /// Resolve every name `manifest` lists. The first name with no value is an
    /// error that names it, so `up` stops before any container starts.
    pub fn values(&self, manifest: &Manifest) -> Result<Values, StoreError> {
        let env = self.env()?;
        let mut declared = BTreeMap::new();
        for (role, names) in &manifest.env {
            for name in names {
                let value = env.get(name).ok_or_else(|| {
                    StoreError::new(format!(
                        "`x-msbx.env.{role}` lists `{name}`, which has no value in {}",
                        self.env_path().display()
                    ))
                })?;
                declared.insert(name.clone(), value.clone());
            }
        }

        let mut secrets = BTreeMap::new();
        for (role, names) in &manifest.secrets {
            for name in names {
                if secrets.contains_key(name) {
                    continue;
                }
                let value = self.secret(name)?.ok_or_else(|| {
                    StoreError::new(format!(
                        "`x-msbx.secrets.{role}` lists `{name}`, which has no value at {}",
                        self.secret_path(name).display()
                    ))
                })?;
                secrets.insert(name.clone(), value);
            }
        }

        Ok(Values {
            env: declared,
            secrets,
        })
    }
}

/// Parse `NAME=value` lines. Blank lines and lines whose first non-blank
/// character is `#` are skipped; the value is everything after the first `=`.
/// Errors quote the line number and the name, never the value.
pub(crate) fn parse_env(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!("line {number} is not NAME=value"));
        };
        if !valid_name(name) {
            return Err(format!(
                "line {number} does not start with a valid NAME ([A-Za-z_][A-Za-z0-9_]*)"
            ));
        }
        if values.insert(name.to_string(), value.to_string()).is_some() {
            return Err(format!("`{name}` is defined twice"));
        }
    }
    Ok(values)
}

/// A name becomes an environment variable, a file name and a mount path, so it
/// stays one shell identifier.
pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A secret file holds one value. A trailing newline is a file convention, not
/// part of the value; a newline anywhere else makes it several values.
fn single_value(name: &str, text: &str) -> Result<String, StoreError> {
    let value = text.strip_suffix('\n').unwrap_or(text);
    let value = value.strip_suffix('\r').unwrap_or(value);
    if value.contains('\n') {
        return Err(StoreError::new(format!(
            "the secret `{name}` must be one value, not several lines"
        )));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("store-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest() -> Manifest {
        Manifest::parse(
            "x-msbx:\n  project: signals\n  env:\n    web: [DATABASE_URL, STRIPE_KEY]\n  secrets:\n    web: [SMTP_PASSWORD]\nservices:\n  web: {}\n",
        )
        .unwrap()
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn parses_name_value_lines_and_ignores_comments_and_blanks() {
        let values =
            parse_env("# a comment\nDATABASE_URL=postgres://x/y?a=1\n\nSTRIPE_KEY=sk_1\nEMPTY=\n")
                .unwrap();
        assert_eq!(values["DATABASE_URL"], "postgres://x/y?a=1");
        assert_eq!(values["EMPTY"], "");
    }

    #[test]
    fn malformed_env_lines_name_the_line_not_the_value() {
        let error = parse_env("GOOD=1\nsecret-value-without-a-name\n").unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        assert!(!error.contains("secret-value-without-a-name"), "{error}");
        let error = parse_env("BAD NAME=1\n").unwrap_err();
        assert!(error.contains("valid NAME"), "{error}");
        let error = parse_env("A=1\nA=2\n").unwrap_err();
        assert!(error.contains("`A` is defined twice"), "{error}");
    }

    #[test]
    fn a_missing_env_file_is_an_empty_store() {
        let dir = scratch("empty");
        assert!(Store::at(&dir).env().unwrap().is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_secret_drops_its_trailing_newline_and_refuses_several_lines() {
        let dir = scratch("secret");
        write(&dir.join("secrets").join("SMTP_PASSWORD"), "hunter2\n");
        assert_eq!(
            Store::at(&dir).secret("SMTP_PASSWORD").unwrap(),
            Some("hunter2".to_string())
        );
        write(&dir.join("secrets").join("SMTP_PASSWORD"), "one\ntwo\n");
        assert!(Store::at(&dir)
            .secret("SMTP_PASSWORD")
            .unwrap_err()
            .to_string()
            .contains("one value"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn values_are_resolved_for_every_declared_name() {
        let dir = scratch("values");
        write(
            &dir.join("env"),
            "DATABASE_URL=postgres://db\nSTRIPE_KEY=sk_1\n",
        );
        write(&dir.join("secrets").join("SMTP_PASSWORD"), "hunter2");
        let values = Store::at(&dir).values(&manifest()).unwrap();
        assert_eq!(values.env["DATABASE_URL"], "postgres://db");
        assert_eq!(values.secrets["SMTP_PASSWORD"], "hunter2");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_declared_name_with_no_value_fails_naming_it() {
        let dir = scratch("missing-env");
        write(&dir.join("env"), "DATABASE_URL=postgres://db\n");
        let error = Store::at(&dir).values(&manifest()).unwrap_err().to_string();
        assert!(error.contains("`x-msbx.env.web`"), "{error}");
        assert!(error.contains("`STRIPE_KEY`"), "{error}");
        assert!(error.contains("has no value"), "{error}");
        let _ = fs::remove_dir_all(dir);

        let dir = scratch("missing-secret");
        write(&dir.join("env"), "DATABASE_URL=db\nSTRIPE_KEY=sk_1\n");
        let error = Store::at(&dir).values(&manifest()).unwrap_err().to_string();
        assert!(error.contains("`x-msbx.secrets.web`"), "{error}");
        assert!(error.contains("`SMTP_PASSWORD`"), "{error}");
        assert!(error.contains("has no value"), "{error}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn undeclared_store_entries_are_not_read() {
        let dir = scratch("undeclared");
        write(
            &dir.join("env"),
            "DATABASE_URL=db\nSTRIPE_KEY=sk_1\nEXTRA=not-declared\n",
        );
        write(&dir.join("secrets").join("SMTP_PASSWORD"), "hunter2");
        let values = Store::at(&dir).values(&manifest()).unwrap();
        assert!(!values.env.contains_key("EXTRA"));
        let _ = fs::remove_dir_all(dir);
    }
}
