//! The harness image: its manifest, its pin and the `msbx image` verbs.
//!
//! The one harness image is built locally from `images/` and pinned by digest:
//! `msbx image build` builds and records the built tag, `msbx image pin`
//! resolves the tag to its manifest digest and writes
//! `{image, digest, pi, built_at, versions, packages}` to
//! `$XDG_STATE_HOME/msbx/image.json`, and `msbx image show` prints it. The shim
//! names the image by digest (`tag@sha256:…`), never by a bare tag, so a moved
//! tag cannot silently change what a run executes.
//!
//! `images/manifest.json` is the single version source: the build passes every
//! version, digest and checksum to the Dockerfile, which asserts each one. The
//! image labels record the same values, so `msbx image pin` can write the pin
//! from the image alone.

use crate::driver;
use crate::state;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// The pin file, under the state directory.
pub const IMAGE_FILE: &str = "image.json";
/// The candidate a build records, promoted by `msbx image pin`.
pub const BUILD_FILE: &str = "image-build.json";
/// The local tag prefix. The full tag is `msbx-pi:<pi>-<n>`.
const TAG_PREFIX: &str = "msbx-pi";
/// The directory and file `msbx image build` reads.
const SOURCE_DIR: &str = "images";
const MANIFEST_FILE: &str = "manifest.json";

/// Why the image verbs or the pin failed. The message names the file and the
/// fix, never a value from a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageError(String);

impl ImageError {
    fn new(message: impl Into<String>) -> Self {
        ImageError(message.into())
    }
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ImageError {}

/// A pinned harness image: the local tag, the digest it resolved to and the Pi
/// version the image carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub image: String,
    pub digest: String,
    pub pi: String,
}

impl Pin {
    /// The default pin file.
    pub fn load() -> Result<Pin, ImageError> {
        Self::load_from(&path())
    }

    /// Read the pin at `path`. A missing file names the command that writes it.
    pub fn load_from(path: &Path) -> Result<Pin, ImageError> {
        let file = read_pin_file(path)?;
        let image = file.image.trim().to_string();
        if image.is_empty() || image.contains('@') || image.chars().any(char::is_whitespace) {
            return Err(ImageError::new(format!(
                "{} names an unusable image `{image}`",
                path.display()
            )));
        }
        let digest = file.digest.trim().to_ascii_lowercase();
        if !valid_digest(&digest) {
            return Err(ImageError::new(format!(
                "{} has no `sha256:` digest; run `msbx image pin` again",
                path.display()
            )));
        }
        let pi = file
            .pi
            .or(file.pi_version)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            // A pin that predates the explicit field still names the version
            // in its tag: `msbx-pi:<pi>-<n>`.
            .or_else(|| pi_from_tag(&image))
            .ok_or_else(|| {
                ImageError::new(format!(
                    "{} does not record the pinned Pi version; run `msbx image pin` again",
                    path.display()
                ))
            })?;
        Ok(Pin { image, digest, pi })
    }

    /// The image reference the shim names: `tag@sha256:…`.
    pub fn reference(&self) -> String {
        format!("{}@{}", self.image, self.digest)
    }

    /// The digest without the `sha256:` prefix, for a cache file name.
    pub fn digest_slug(&self) -> String {
        self.digest
            .strip_prefix("sha256:")
            .unwrap_or(&self.digest)
            .to_string()
    }
}

/// The full pin, as `msbx image pin` writes it and `msbx image show` prints it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub pi: String,
    #[serde(default)]
    pub built_at: String,
    /// base, node, pnpm, rust, go, python, multica
    #[serde(default)]
    pub versions: BTreeMap<String, String>,
    /// package name → pinned version
    #[serde(default)]
    pub packages: BTreeMap<String, String>,
}

/// `msbx image build|pin|show`.
pub fn run_command(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("build") => match build() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("msbx image build: {error}");
                ExitCode::FAILURE
            }
        },
        Some("pin") => match pin() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("msbx image pin: {error}");
                ExitCode::FAILURE
            }
        },
        Some("show") => match show() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("msbx image show: {error}");
                ExitCode::FAILURE
            }
        },
        Some("-h") | Some("--help") | Some("help") | None => {
            print!(
                "Usage: msbx image build|pin|show\n\n\
                 build  Build {TAG_PREFIX}:<pi>-<n> from images/manifest.json\n\
                 pin    Resolve the built tag and record its digest in {IMAGE_FILE}\n\
                 show   Print the pin and the versions the image records\n"
            );
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!(
                "msbx image: unknown subcommand `{other}` (usage: msbx image build|pin|show)"
            );
            ExitCode::from(64)
        }
    }
}

/// Build the harness image and record the tag and digest a later `pin` promotes.
fn build() -> Result<(), ImageError> {
    let source = source_dir()?;
    let manifest_path = source.join(MANIFEST_FILE);
    let text = fs::read_to_string(&manifest_path).map_err(|error| {
        ImageError::new(format!("cannot read {}: {error}", manifest_path.display()))
    })?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|error| {
        ImageError::new(format!(
            "{} is not an image manifest: {error}",
            manifest_path.display()
        ))
    })?;
    manifest.validate()?;
    if !source.join("Dockerfile").is_file() {
        return Err(ImageError::new(format!(
            "{} has no Dockerfile; run `msbx image build` from a checkout of multica-sandbox",
            source.display()
        )));
    }

    if !driver::has("docker") {
        return Err(ImageError::new(
            "docker is not on PATH; install Docker and start the daemon",
        ));
    }
    let tag = next_tag(&manifest.pi.version)?;
    let built_at = rfc3339(state::now());
    let build_args = manifest.build_args(&built_at)?;

    let buildx = state::state_dir().join("buildx");
    state::create_private_dir(&buildx)
        .map_err(|error| ImageError::new(format!("cannot create {}: {error}", buildx.display())))?;

    let mut command = Command::new("docker");
    command.args(["buildx", "build", "--load"]);
    for (name, value) in &build_args {
        command.arg("--build-arg").arg(format!("{name}={value}"));
    }
    command
        .arg("--tag")
        .arg(&tag)
        .arg("--file")
        .arg(source.join("Dockerfile"))
        .arg(&source)
        .env("BUILDX_CONFIG", &buildx)
        .env("DOCKER_BUILDKIT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = command
        .status()
        .map_err(|error| ImageError::new(format!("cannot run `docker buildx build`: {error}")))?;
    if !status.success() {
        return Err(ImageError::new(format!(
            "`docker buildx build` failed for {tag}; the image was not built"
        )));
    }

    let digest = tag_digest(&tag)?;
    let labels = image_labels(&tag)?;
    let record = record_from_labels(&tag, &digest, &labels)?;
    write_record(&build_path(), &record)?;
    println!("built {} ({})", record.image, record.digest);
    println!("record it with `msbx image pin`");
    Ok(())
}

/// Resolve the built tag and write the pin. A tag that no longer resolves to
/// the digest the build recorded is refused; the next build gets a new tag.
fn pin() -> Result<(), ImageError> {
    let build_path = build_path();
    let built = read_record(&build_path)?;
    if built.image.is_empty() || !valid_digest(&built.digest) {
        return Err(ImageError::new(format!(
            "{} is not a build record; run `msbx image build`",
            build_path.display()
        )));
    }
    let Some((_, tag)) = built.image.rsplit_once(':') else {
        return Err(ImageError::new(format!(
            "{} names no tag; run `msbx image build`",
            build_path.display()
        )));
    };
    if !tag.starts_with(&format!("{}-", built.pi)) {
        return Err(ImageError::new(format!(
            "{} names `{}` for Pi {}; run `msbx image build`",
            build_path.display(),
            built.image,
            built.pi
        )));
    }
    let current = tag_digest(&built.image)?;
    if current != built.digest {
        return Err(ImageError::new(format!(
            "tag `{}` moved after it was built (built {}, now {}); run `msbx image build` for a new tag, or remove the stale tag",
            built.image, built.digest, current
        )));
    }
    if let Ok(existing) = Pin::load() {
        if existing.image == built.image && existing.digest != current {
            return Err(ImageError::new(format!(
                "refusing to repin `{}` from {} to {}; the tag moved. Run `msbx image build` for a new tag",
                existing.image, existing.digest, current
            )));
        }
    }

    let labels = image_labels(&built.image)?;
    let mut record = record_from_labels(&built.image, &current, &labels)?;
    record.built_at = built.built_at.clone();
    record.pi = built.pi.clone();
    write_record(&path(), &record)?;
    println!("pinned {}", record.image);
    println!("digest {}", record.digest);
    println!("wrote  {}", path().display());
    Ok(())
}

/// Print the pin and the versions the image records. Never touches Docker.
fn show() -> Result<(), ImageError> {
    let pin = Pin::load_from(&path())?;
    let record = read_record(&path()).unwrap_or_default();
    println!("{:<9}{}", "image", pin.image);
    println!("{:<9}{}", "digest", pin.digest);
    println!("{:<9}{}", "pi", pin.pi);
    if !record.built_at.is_empty() {
        println!("{:<9}{}", "built", record.built_at);
    }
    for (name, version) in &record.versions {
        println!("{name:<9}{version}");
    }
    for (name, version) in &record.packages {
        println!("{:<9}{name} {version}", "package");
    }
    Ok(())
}

/// `$XDG_STATE_HOME/msbx/image-build.json`.
pub fn build_path() -> PathBuf {
    state::state_dir().join(BUILD_FILE)
}

/// The image source: `MSBX_IMAGE_DIR`, then `images/` under the current
/// directory, then the checkout this binary was built in.
fn source_dir() -> Result<PathBuf, ImageError> {
    if let Some(dir) = env::var_os("MSBX_IMAGE_DIR") {
        let dir = PathBuf::from(dir);
        if dir.join(MANIFEST_FILE).is_file() {
            return Ok(dir);
        }
        return Err(ImageError::new(format!(
            "MSBX_IMAGE_DIR is `{}`, which has no {MANIFEST_FILE}; run `msbx image build` from a checkout of multica-sandbox",
            dir.display()
        )));
    }
    let cwd = env::current_dir()
        .map_err(|error| ImageError::new(format!("cannot read the current directory: {error}")))?;
    for candidate in [cwd.join(SOURCE_DIR), cwd] {
        if candidate.join(MANIFEST_FILE).is_file() {
            return Ok(candidate);
        }
    }
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join(SOURCE_DIR);
    if built.join(MANIFEST_FILE).is_file() {
        return Ok(built);
    }
    Err(ImageError::new(format!(
        "cannot find images/{MANIFEST_FILE}; run `msbx image build` from a checkout of multica-sandbox"
    )))
}

/// The next build counter for this Pi version. Local tags, the last build and
/// the pin all count, so deleting a tag does not reuse a tag a pin knows.
fn next_tag(pi: &str) -> Result<String, ImageError> {
    let prefix = format!("{TAG_PREFIX}:{pi}-");
    let listed = driver::run(
        "docker",
        &["image", "ls", "--format", "{{.Repository}}:{{.Tag}}"],
    );
    if !listed.ok {
        return Err(ImageError::new(format!(
            "cannot list local images: {}",
            one_line(&listed.text())
        )));
    }
    let mut highest = 0u64;
    for line in listed.stdout.lines() {
        if let Some(counter) = line.trim().strip_prefix(&prefix) {
            if let Ok(value) = counter.parse::<u64>() {
                highest = highest.max(value);
            }
        }
    }
    for path in [build_path(), path()] {
        if let Ok(record) = read_record(&path) {
            if let Some(counter) = record.image.strip_prefix(&prefix) {
                if let Ok(value) = counter.parse::<u64>() {
                    highest = highest.max(value);
                }
            }
        }
    }
    Ok(format!("{prefix}{}", highest + 1))
}

/// The manifest digest a local tag resolves to. A tag without a manifest
/// digest cannot be named by `tag@sha256:…`; refuse it rather than write an
/// unusable pin.
fn tag_digest(tag: &str) -> Result<String, ImageError> {
    let out = driver::run(
        "docker",
        &["image", "inspect", "--format", "{{json .RepoDigests}}", tag],
    );
    if !out.ok {
        return Err(ImageError::new(format!(
            "cannot inspect `{tag}`: {}",
            one_line(&out.text())
        )));
    }
    let digests: Vec<String> = serde_json::from_str(out.stdout.trim()).map_err(|error| {
        ImageError::new(format!("`{tag}` has unreadable repo digests: {error}"))
    })?;
    let digest = digests
        .iter()
        .filter_map(|entry| entry.rsplit_once('@').map(|(_, digest)| digest.trim()))
        .find(|digest| valid_digest(digest))
        .map(|digest| digest.to_ascii_lowercase());
    digest.ok_or_else(|| {
        ImageError::new(format!(
            "`{tag}` has no manifest digest; build it with `msbx image build` (buildx --load)"
        ))
    })
}

fn image_labels(tag: &str) -> Result<BTreeMap<String, String>, ImageError> {
    let out = driver::run(
        "docker",
        &[
            "image",
            "inspect",
            "--format",
            "{{json .Config.Labels}}",
            tag,
        ],
    );
    if !out.ok {
        return Err(ImageError::new(format!(
            "cannot inspect `{tag}`: {}",
            one_line(&out.text())
        )));
    }
    let text = out.stdout.trim();
    if text.is_empty() || text == "null" {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(text)
        .map_err(|error| ImageError::new(format!("`{tag}` has unreadable labels: {error}")))
}

/// Rebuild the record from the image's labels. The labels are the image's own
/// claim about its versions, written by the Dockerfile from the manifest.
fn record_from_labels(
    image: &str,
    digest: &str,
    labels: &BTreeMap<String, String>,
) -> Result<Record, ImageError> {
    let label = |name: &str| -> Result<String, ImageError> {
        labels
            .get(name)
            .cloned()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ImageError::new(format!(
                    "`{image}` has no `{name}` label; run `msbx image build` again"
                ))
            })
    };
    let pi = label("com.msbx.pi.version")?;
    let mut versions = BTreeMap::new();
    versions.insert("base".to_string(), label("com.msbx.base")?);
    versions.insert("node".to_string(), label("com.msbx.node.version")?);
    versions.insert("pnpm".to_string(), label("com.msbx.pnpm.version")?);
    versions.insert("rust".to_string(), label("com.msbx.rust.version")?);
    versions.insert("go".to_string(), label("com.msbx.go.version")?);
    versions.insert("python".to_string(), label("com.msbx.python.version")?);
    versions.insert("multica".to_string(), label("com.msbx.multica.version")?);

    let mut packages = BTreeMap::new();
    let raw = label("com.msbx.pi.packages")?;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&raw).map_err(|error| {
        ImageError::new(format!(
            "`{image}` has an unreadable `com.msbx.pi.packages` label: {error}"
        ))
    })?;
    for package in parsed {
        let name = package
            .get("name")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty());
        let version = package
            .get("version")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty());
        match (name, version) {
            (Some(name), Some(version)) => {
                packages.insert(name.to_string(), version.to_string());
            }
            _ => {
                return Err(ImageError::new(format!(
                    "`{image}` has an incomplete `com.msbx.pi.packages` label"
                )))
            }
        }
    }

    Ok(Record {
        image: image.to_string(),
        digest: digest.to_string(),
        pi,
        built_at: labels.get("com.msbx.built_at").cloned().unwrap_or_default(),
        versions,
        packages,
    })
}

fn read_pin_file(path: &Path) -> Result<File, ImageError> {
    let text = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ImageError::new(format!(
                "no harness image pin at {}; run `msbx image build && msbx image pin`",
                path.display()
            ))
        } else {
            ImageError::new(format!("cannot read {}: {error}", path.display()))
        }
    })?;
    serde_json::from_str(&text).map_err(|error| {
        ImageError::new(format!(
            "{} is not a harness image pin: {error}",
            path.display()
        ))
    })
}

fn read_record(path: &Path) -> Result<Record, ImageError> {
    let text = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ImageError::new(format!(
                "no built image recorded at {}; run `msbx image build`",
                path.display()
            ))
        } else {
            ImageError::new(format!("cannot read {}: {error}", path.display()))
        }
    })?;
    serde_json::from_str(&text).map_err(|error| {
        ImageError::new(format!("{} is not a build record: {error}", path.display()))
    })
}

fn write_record(path: &Path, record: &Record) -> Result<(), ImageError> {
    let body = serde_json::to_string_pretty(record)
        .map_err(|error| ImageError::new(format!("cannot render the pin: {error}")))?;
    state::write_private(path, &format!("{body}\n"))
        .map_err(|error| ImageError::new(format!("cannot write {}: {error}", path.display())))
}

/// `$XDG_STATE_HOME/msbx/image.json`.
pub fn path() -> PathBuf {
    state::state_dir().join(IMAGE_FILE)
}

/// The build's only source of versions. The Dockerfile gets every value from
/// here, and the image labels echo them back for `pin`.
#[derive(Debug, Deserialize)]
struct Manifest {
    schema: u32,
    base: String,
    node: Tool,
    pnpm: String,
    rust: Tool,
    go: Tool,
    python: String,
    packages: BTreeMap<String, String>,
    pi: Pi,
    multica: Multica,
}

#[derive(Debug, Deserialize)]
struct Tool {
    image: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct Pi {
    package: String,
    version: String,
    urls: BTreeMap<String, Download>,
    packages: Vec<Package>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Package {
    name: String,
    version: String,
    url: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct Download {
    url: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct Multica {
    version: String,
    urls: BTreeMap<String, Download>,
}

impl Manifest {
    fn validate(&self) -> Result<(), ImageError> {
        if self.schema != 1 {
            return Err(ImageError::new(format!(
                "unsupported image manifest schema {}; expected 1",
                self.schema
            )));
        }
        if self.pi.package.trim().is_empty() {
            return Err(ImageError::new("pi.package is empty"));
        }
        for (where_, image) in [
            ("base", &self.base),
            ("node.image", &self.node.image),
            ("rust.image", &self.rust.image),
            ("go.image", &self.go.image),
        ] {
            let Some((_, digest)) = image.rsplit_once('@') else {
                return Err(ImageError::new(format!(
                    "{where_} must be pinned by digest (`name@sha256:…`), not `{image}`"
                )));
            };
            if !valid_digest(digest) {
                return Err(ImageError::new(format!(
                    "{where_} has no sha256 digest: `{image}`"
                )));
            }
        }
        for (where_, version) in [
            ("node.version", &self.node.version),
            ("pnpm", &self.pnpm),
            ("rust.version", &self.rust.version),
            ("go.version", &self.go.version),
            ("python", &self.python),
            ("pi.version", &self.pi.version),
            ("multica.version", &self.multica.version),
        ] {
            exact(where_, version)?;
        }
        for (arch, download) in &self.pi.urls {
            download.validate(&format!("pi.urls.{arch}"))?;
        }
        for (arch, download) in &self.multica.urls {
            download.validate(&format!("multica.urls.{arch}"))?;
        }
        for arch in ["amd64", "arm64"] {
            if !self.pi.urls.contains_key(arch) {
                return Err(ImageError::new(format!(
                    "the manifest has no pi.urls.{arch}"
                )));
            }
            if !self.multica.urls.contains_key(arch) {
                return Err(ImageError::new(format!(
                    "the manifest has no multica.urls.{arch}"
                )));
            }
        }
        if !self
            .pi
            .urls
            .values()
            .all(|download| download.url.contains(&self.pi.version))
        {
            return Err(ImageError::new(
                "a pi url does not name the pinned Pi version",
            ));
        }
        if !self
            .multica
            .urls
            .values()
            .all(|download| download.url.contains(&self.multica.version))
        {
            return Err(ImageError::new(
                "a multica url does not name the pinned CLI version",
            ));
        }
        if self.pi.packages.is_empty() {
            return Err(ImageError::new(
                "pi.packages is empty; the image must pin the packages it loads",
            ));
        }
        for package in &self.pi.packages {
            exact(&format!("pi.packages.{}", package.name), &package.version)?;
            if package.name.trim().is_empty() || package.name.contains(char::is_whitespace) {
                return Err(ImageError::new("a pi.packages name is unusable"));
            }
            if !package.url.starts_with("https://")
                || !package.url.contains(&package.version)
                || !valid_sha256(&package.sha256)
            {
                return Err(ImageError::new(format!(
                    "pi.packages.{} must have an https url naming the version and a sha256 checksum",
                    package.name
                )));
            }
        }
        for name in APT_PACKAGES {
            if !self.packages.contains_key(*name) {
                return Err(ImageError::new(format!(
                    "the manifest does not record the `{name}` package version"
                )));
            }
        }
        for (name, version) in &self.packages {
            if apt_arg(name).is_none() {
                return Err(ImageError::new(format!(
                    "the manifest records an unknown package `{name}`; add it to the Dockerfile and this build"
                )));
            }
            exact(&format!("packages.{name}"), version)?;
        }
        Ok(())
    }

    fn build_args(&self, built_at: &str) -> Result<Vec<(String, String)>, ImageError> {
        let mut args = vec![
            ("BASE_IMAGE".into(), self.base.clone()),
            ("NODE_IMAGE".into(), self.node.image.clone()),
            ("RUST_IMAGE".into(), self.rust.image.clone()),
            ("GO_IMAGE".into(), self.go.image.clone()),
            ("NODE_VERSION".into(), self.node.version.clone()),
            ("PNPM_VERSION".into(), self.pnpm.clone()),
            ("RUST_VERSION".into(), self.rust.version.clone()),
            ("GO_VERSION".into(), self.go.version.clone()),
            ("PYTHON_VERSION".into(), self.python.clone()),
            ("PI_VERSION".into(), self.pi.version.clone()),
            ("MULTICA_VERSION".into(), self.multica.version.clone()),
            ("BUILT_AT".into(), built_at.to_string()),
            (
                "PI_PACKAGES_JSON".into(),
                serde_json::to_string(&self.pi.packages).map_err(|error| {
                    ImageError::new(format!("cannot render pi.packages: {error}"))
                })?,
            ),
        ];
        for (name, version) in &self.packages {
            let arg = apt_arg(name).expect("validated");
            args.push((arg.to_string(), version.clone()));
        }
        for (arch, download) in &self.pi.urls {
            let suffix = arch.to_ascii_uppercase();
            args.push((format!("PI_URL_{suffix}"), download.url.clone()));
            args.push((format!("PI_SHA256_{suffix}"), download.sha256.clone()));
        }
        for (arch, download) in &self.multica.urls {
            let suffix = arch.to_ascii_uppercase();
            args.push((format!("MULTICA_URL_{suffix}"), download.url.clone()));
            args.push((format!("MULTICA_SHA256_{suffix}"), download.sha256.clone()));
        }
        Ok(args)
    }
}

impl Download {
    fn validate(&self, where_: &str) -> Result<(), ImageError> {
        if !self.url.starts_with("https://") {
            return Err(ImageError::new(format!("{where_} must be an https url")));
        }
        if !valid_sha256(&self.sha256) {
            return Err(ImageError::new(format!(
                "{where_} must carry a sha256 checksum"
            )));
        }
        Ok(())
    }
}

/// The Debian packages the Dockerfile installs. The manifest records each one
/// at its exact version.
const APT_PACKAGES: &[&str] = &[
    "ca-certificates",
    "git",
    "gh",
    "jq",
    "ripgrep",
    "procps",
    "socat",
    "build-essential",
    "python3",
    "libstdc++6",
    "curl",
];

fn apt_arg(name: &str) -> Option<&'static str> {
    Some(match name {
        "ca-certificates" => "APT_CA_CERTIFICATES",
        "git" => "APT_GIT",
        "gh" => "APT_GH",
        "jq" => "APT_JQ",
        "ripgrep" => "APT_RIPGREP",
        "procps" => "APT_PROCPPS",
        "socat" => "APT_SOCAT",
        "build-essential" => "APT_BUILD_ESSENTIAL",
        "python3" => "APT_PYTHON3",
        "libstdc++6" => "APT_LIBSTDCXX6",
        "curl" => "APT_CURL",
        _ => return None,
    })
}

/// A version must be exact: no whitespace, no range, no `latest`. Debian
/// versions may carry an epoch, a revision and a `+`/`~` suffix.
fn exact(where_: &str, version: &str) -> Result<(), ImageError> {
    let version = version.trim();
    let bad = version.is_empty()
        || version.chars().any(char::is_whitespace)
        || version == "latest"
        || version == "stable"
        || !version.chars().next().is_some_and(|c| c.is_ascii_digit());
    if bad {
        return Err(ImageError::new(format!(
            "{where_} must be an exact version, not `{version}`"
        )));
    }
    Ok(())
}

/// `msbx-pi:0.52.0-1` → `0.52.0`. Only the last `-<number>` build counter is
/// stripped, so a tag that cannot name a version yields `None`.
fn pi_from_tag(image: &str) -> Option<String> {
    let tag = image.rsplit_once(':')?.1;
    let (version, build) = tag.rsplit_once('-')?;
    if build.is_empty()
        || !build.chars().all(|c| c.is_ascii_digit())
        || !version.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(version.to_string())
}

fn valid_digest(digest: &str) -> bool {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_sha256(sha: &str) -> bool {
    sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit())
}

/// An epoch second as RFC 3339 UTC. Used for the build's `built_at`, so the
/// image and the pin record the same string without a date crate.
fn rfc3339(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let rest = epoch % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month as u32, day as u32)
}

fn one_line(text: &str) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    if joined.is_empty() {
        "no output".to_string()
    } else if joined.chars().count() > 240 {
        let mut truncated: String = joined.chars().take(240).collect();
        truncated.push('…');
        truncated
    } else {
        joined
    }
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    image: String,
    #[serde(default)]
    digest: String,
    #[serde(default)]
    pi: Option<String>,
    #[serde(default)]
    pi_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("image-{}-{tag}", std::process::id()))
    }

    fn pin(path: &Path, body: &str) {
        state::create_private_dir(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn reads_a_pin_and_names_the_image_by_digest() {
        let dir = scratch("read");
        let path = dir.join(IMAGE_FILE);
        pin(
            &path,
            r#"{"image":"msbx-pi:0.52.0-1","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","pi":"0.52.0"}"#,
        );
        let pin = Pin::load_from(&path).unwrap();
        assert_eq!(pin.pi, "0.52.0");
        assert_eq!(
            pin.reference(),
            "msbx-pi:0.52.0-1@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_pin_names_the_command_that_writes_it() {
        let dir = scratch("missing");
        let error = Pin::load_from(&dir.join(IMAGE_FILE))
            .unwrap_err()
            .to_string();
        assert!(error.contains("msbx image pin"), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pin_without_an_explicit_version_reads_it_from_the_tag() {
        let dir = scratch("tag-version");
        let path = dir.join(IMAGE_FILE);
        pin(
            &path,
            r#"{"image":"msbx-pi:0.52.0-3","digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}"#,
        );
        assert_eq!(Pin::load_from(&path).unwrap().pi, "0.52.0");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_a_tag_without_a_sha256_digest_and_a_pin_without_a_version() {
        let dir = scratch("invalid");
        let path = dir.join(IMAGE_FILE);
        pin(&path, r#"{"image":"msbx-pi:1","digest":"latest","pi":"1"}"#);
        assert!(Pin::load_from(&path)
            .unwrap_err()
            .to_string()
            .contains("sha256"));
        pin(
            &path,
            r#"{"image":"msbx-pi:1","digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#,
        );
        assert!(Pin::load_from(&path)
            .unwrap_err()
            .to_string()
            .contains("Pi version"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_manifest_is_the_one_version_source() {
        let text = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(SOURCE_DIR)
                .join(MANIFEST_FILE),
        )
        .unwrap();
        let manifest: Manifest = serde_json::from_str(&text).unwrap();
        manifest.validate().unwrap();
        let args = manifest.build_args("2026-01-01T00:00:00Z").unwrap();
        let names: Vec<&str> = args.iter().map(|(name, _)| name.as_str()).collect();
        for required in [
            "BASE_IMAGE",
            "NODE_IMAGE",
            "RUST_IMAGE",
            "GO_IMAGE",
            "PI_VERSION",
            "PI_PACKAGES_JSON",
            "PI_URL_AMD64",
            "PI_URL_ARM64",
            "MULTICA_URL_AMD64",
            "MULTICA_URL_ARM64",
            "APT_LIBSTDCXX6",
            "APT_PROCPPS",
        ] {
            assert!(names.contains(&required), "missing {required}");
        }
        let packages = serde_json::to_string(&manifest.pi.packages).unwrap();
        assert!(packages.contains("pi-subagents"));
    }

    #[test]
    fn a_floating_version_is_refused() {
        assert!(exact("pi", "latest").is_err());
        assert!(exact("node", "^26").is_err());
        assert!(exact("node", "26.12.0").is_ok());
        assert!(exact("ca-certificates", "20250419~deb12u2").is_ok());
    }

    #[test]
    fn the_build_clock_is_rfc3339_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
