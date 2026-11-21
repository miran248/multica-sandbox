//! The three value layers, outside every checkout.
//!
//! The manifest lists names; the values live in:
//!
//! | Layer | Path |
//! | --- | --- |
//! | host | `$XDG_CONFIG_HOME/msbx/host.env` |
//! | project | `$XDG_CONFIG_HOME/msbx/projects/<project>/env` and `secrets/<NAME>` |
//! | harness | `$XDG_CONFIG_HOME/msbx/harness/env` |
//!
//! The project value wins over the host value for the same name, and a project
//! may read a host name only when `host-allow`, beside the host store, lists it:
//! the manifest is repo-controlled, so without the allowlist a repository could
//! name and exfiltrate any host value. The harness layer is not
//! manifest-driven; it carries the model credential, `GH_TOKEN` and the git
//! identity, wins in the harness context, and never enters a service envelope.
//!
//! Reserved names are refused here, before `up` allocates a block, naming the
//! name, the layer and the role. Nothing in this module echoes a value: errors
//! name the name, the layer, the role or the file, never the content.

use crate::manifest::Manifest;
use crate::state;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One project's store directory.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

/// The resolved values one render needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Values {
    /// Declared `x-msbx.env` names.
    pub env: BTreeMap<String, String>,
    /// Declared `x-msbx.secrets` names.
    pub secrets: BTreeMap<String, String>,
    /// The harness-only store, untouched by the manifest.
    pub harness: BTreeMap<String, String>,
}

/// Which layer a value or a refusal belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Host,
    Project,
    Harness,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Host => "host",
            Layer::Project => "project",
            Layer::Harness => "harness",
        }
    }
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
    /// The project store of the installed binary.
    pub fn for_project(project: &str) -> Store {
        Store {
            dir: state::store_dir(project),
        }
    }

    /// A store over an explicit directory. Tests use it.
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
        read_env(&self.env_path())
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
}

/// The three layers for one project, read once.
#[derive(Debug, Clone)]
pub struct Layers {
    project: Store,
    host: BTreeMap<String, String>,
    host_path: PathBuf,
    allow: BTreeSet<String>,
    allow_path: PathBuf,
    harness: BTreeMap<String, String>,
}

impl Layers {
    /// The layers the installed binary uses for `project`.
    pub fn for_project(project: &str) -> Result<Layers, StoreError> {
        Layers::at(
            state::store_dir(project),
            state::host_env_file(),
            state::host_allow_file(),
            state::harness_env_file(),
        )
    }

    /// Layers over explicit paths. Tests use it.
    pub fn at(
        project: impl Into<PathBuf>,
        host_env: impl Into<PathBuf>,
        host_allow: impl Into<PathBuf>,
        harness_env: impl Into<PathBuf>,
    ) -> Result<Layers, StoreError> {
        let host_path = host_env.into();
        let allow_path = host_allow.into();
        let harness_path = harness_env.into();
        Ok(Layers {
            project: Store::at(project),
            host: read_env(&host_path)?,
            host_path,
            allow: read_allow(&allow_path)?,
            allow_path,
            harness: read_env(&harness_path)?,
        })
    }

    pub fn project(&self) -> &Store {
        &self.project
    }

    pub fn host(&self) -> &BTreeMap<String, String> {
        &self.host
    }

    pub fn allow(&self) -> &BTreeSet<String> {
        &self.allow
    }

    pub fn harness(&self) -> &BTreeMap<String, String> {
        &self.harness
    }

    /// Resolve every name `manifest` lists. The first name with no value, or a
    /// reserved one, is an error that names it, so `up` stops before any
    /// container starts.
    pub fn values(&self, manifest: &Manifest) -> Result<Values, StoreError> {
        let env = self.project.env()?;

        for (role, names) in &manifest.env {
            for name in names {
                check_reserved(name, Layer::Project, role)?;
            }
        }
        for (role, names) in &manifest.secrets {
            for name in names {
                check_reserved(name, Layer::Project, role)?;
            }
        }
        for name in self.harness.keys() {
            check_reserved(name, Layer::Harness, "harness")?;
        }

        let mut declared_env = BTreeMap::new();
        for (role, names) in &manifest.env {
            for name in names {
                let value = self.lookup(name, role, "env", &env)?;
                declared_env.insert(name.clone(), value);
            }
        }

        let mut declared_secrets = BTreeMap::new();
        for (role, names) in &manifest.secrets {
            for name in names {
                if declared_secrets.contains_key(name) {
                    continue;
                }
                let value = self.lookup_secret(name, role, &env)?;
                declared_secrets.insert(name.clone(), value);
            }
        }

        Ok(Values {
            env: declared_env,
            secrets: declared_secrets,
            harness: self.harness.clone(),
        })
    }

    /// The project value, else an allowed host value. A host name that is not
    /// allowed is an error naming the name and the allowlist file.
    fn lookup(
        &self,
        name: &str,
        role: &str,
        block: &str,
        project: &BTreeMap<String, String>,
    ) -> Result<String, StoreError> {
        if let Some(value) = project.get(name) {
            return Ok(value.clone());
        }
        if let Some(value) = self.host.get(name) {
            if self.allow.contains(name) {
                return Ok(value.clone());
            }
            return Err(StoreError::new(format!(
                "`x-msbx.{block}.{role}` lists `{name}`, which is set only in the host store {} and is not listed in the allowlist {}; add `{name}` to the allowlist or set it in the project store",
                self.host_path.display(),
                self.allow_path.display()
            )));
        }
        Err(StoreError::new(format!(
            "`x-msbx.{block}.{role}` lists `{name}`, which has no value in {} or {}",
            self.project.env_path().display(),
            self.host_path.display()
        )))
    }

    /// A declared secret: the project secret file first, then an allowed host
    /// value, then a project `env` entry (a secret is a kind of value, not a
    /// second namespace).
    fn lookup_secret(
        &self,
        name: &str,
        role: &str,
        project_env: &BTreeMap<String, String>,
    ) -> Result<String, StoreError> {
        if let Some(value) = self.project.secret(name)? {
            return Ok(value);
        }
        self.lookup(name, role, "secrets", project_env)
    }
}

/// Is this name reserved for the harness? The harness sets these and a store
/// value carrying them would shadow or break the launch.
pub fn reserved(name: &str) -> bool {
    name.starts_with("MULTICA_")
        || name.starts_with("PI_")
        || matches!(name, "HOME" | "PATH" | "HOSTNAME")
}

fn check_reserved(name: &str, layer: Layer, role: &str) -> Result<(), StoreError> {
    if reserved(name) {
        return Err(StoreError::new(format!(
            "reserved name `{name}` in layer `{}` for role `{role}` is refused; the harness sets it",
            layer.as_str()
        )));
    }
    Ok(())
}

/// Read a dotenv file. A missing file is an empty store, not an error.
fn read_env(path: &Path) -> Result<BTreeMap<String, String>, StoreError> {
    match fs::read_to_string(path) {
        Ok(text) => parse_env(&text)
            .map_err(|error| StoreError::new(format!("{}: {error}", path.display()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(StoreError::new(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

/// Read the allowlist: one name per line, `#` comments, blanks ignored. A
/// missing file allows nothing.
fn read_allow(path: &Path) -> Result<BTreeSet<String>, StoreError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => {
            return Err(StoreError::new(format!(
                "cannot read the allowlist {}: {error}",
                path.display()
            )))
        }
    };
    let mut allowed = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !valid_name(line) {
            return Err(StoreError::new(format!(
                "{} line {}: `{line}` is not a valid env name",
                path.display(),
                index + 1
            )));
        }
        allowed.insert(line.to_string());
    }
    Ok(allowed)
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
            return Err(format!("line {number} redefines `{name}`"));
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

    fn layers(root: &Path) -> Layers {
        Layers::at(
            root.join("project"),
            root.join("host.env"),
            root.join("host-allow"),
            root.join("harness.env"),
        )
        .unwrap()
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
        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("`A`"), "{error}");
    }

    #[test]
    fn a_missing_env_file_is_an_empty_store() {
        let dir = scratch("empty");
        assert!(Store::at(&dir).env().unwrap().is_empty());
        assert!(read_allow(&dir.join("none")).unwrap().is_empty());
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
        let root = scratch("values");
        write(
            &root.join("project").join("env"),
            "DATABASE_URL=postgres://db\nSTRIPE_KEY=sk_1\n",
        );
        write(
            &root.join("project").join("secrets").join("SMTP_PASSWORD"),
            "hunter2",
        );
        let values = layers(&root).values(&manifest()).unwrap();
        assert_eq!(values.env["DATABASE_URL"], "postgres://db");
        assert_eq!(values.secrets["SMTP_PASSWORD"], "hunter2");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_project_value_wins_over_the_host_value() {
        let root = scratch("precedence");
        write(&root.join("project").join("env"), "DATABASE_URL=project\n");
        write(
            &root.join("project").join("secrets").join("SMTP_PASSWORD"),
            "hunter2",
        );
        write(
            &root.join("host.env"),
            "DATABASE_URL=host\nSTRIPE_KEY=allowed\n",
        );
        write(&root.join("host-allow"), "STRIPE_KEY\n");
        let values = layers(&root).values(&manifest()).unwrap();
        assert_eq!(values.env["DATABASE_URL"], "project");
        assert_eq!(values.env["STRIPE_KEY"], "allowed");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_host_name_that_is_not_allowed_is_refused_naming_the_name_and_file() {
        let root = scratch("allowlist");
        write(&root.join("project").join("env"), "DATABASE_URL=db\n");
        write(&root.join("host.env"), "STRIPE_KEY=host-only\n");
        let error = layers(&root).values(&manifest()).unwrap_err().to_string();
        assert!(error.contains("`STRIPE_KEY`"), "{error}");
        assert!(error.contains("host-allow"), "{error}");
        assert!(error.contains("host.env"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_declared_name_with_no_value_fails_naming_it() {
        let root = scratch("missing");
        write(&root.join("project").join("env"), "DATABASE_URL=db\n");
        let error = layers(&root).values(&manifest()).unwrap_err().to_string();
        assert!(error.contains("`x-msbx.env.web`"), "{error}");
        assert!(error.contains("`STRIPE_KEY`"), "{error}");
        assert!(error.contains("has no value"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_harness_store_is_read_but_does_not_replace_a_declared_value() {
        let root = scratch("harness");
        write(
            &root.join("project").join("env"),
            "DATABASE_URL=db\nSTRIPE_KEY=sk_1\n",
        );
        write(
            &root.join("project").join("secrets").join("SMTP_PASSWORD"),
            "hunter2",
        );
        write(
            &root.join("harness.env"),
            "GH_TOKEN=ghp_marker\nDATABASE_URL=harness\n",
        );
        let values = layers(&root).values(&manifest()).unwrap();
        assert_eq!(values.env["DATABASE_URL"], "db");
        assert_eq!(values.secrets["SMTP_PASSWORD"], "hunter2");
        assert_eq!(values.harness["GH_TOKEN"], "ghp_marker");
        assert_eq!(values.harness["DATABASE_URL"], "harness");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reserved_names_are_refused_naming_the_name_layer_and_role() {
        let root = scratch("reserved");
        let error = layers(&root)
            .values(
                &Manifest::parse(
                    "x-msbx:\n  project: signals\n  env:\n    web: [PATH]\nservices:\n  web: {}\n",
                )
                .unwrap(),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("`PATH`"), "{error}");
        assert!(error.contains("layer `project`"), "{error}");
        assert!(error.contains("role `web`"), "{error}");

        let root2 = scratch("reserved-harness");
        write(&root2.join("harness.env"), "MULTICA_TOKEN=secret\n");
        let error = layers(&root2).values(&manifest()).unwrap_err().to_string();
        assert!(error.contains("`MULTICA_TOKEN`"), "{error}");
        assert!(error.contains("layer `harness`"), "{error}");
        assert!(error.contains("role `harness`"), "{error}");
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(root2);
    }

    #[test]
    fn undeclared_store_entries_are_not_read() {
        let root = scratch("undeclared");
        write(
            &root.join("project").join("env"),
            "DATABASE_URL=db\nSTRIPE_KEY=sk_1\nEXTRA=not-declared\n",
        );
        write(
            &root.join("project").join("secrets").join("SMTP_PASSWORD"),
            "hunter2",
        );
        let values = layers(&root).values(&manifest()).unwrap();
        assert!(!values.env.contains_key("EXTRA"));
        let _ = fs::remove_dir_all(root);
    }
}
