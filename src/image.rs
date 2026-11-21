//! The harness image pin.
//!
//! The one harness image is built locally and pinned by digest:
//! `msbx image pin` writes `{image, digest, pi}` to
//! `$XDG_STATE_HOME/msbx/image.json`. The shim names the image by digest, never
//! by tag, so a moved tag cannot silently change what a run executes.
//!
//! Stage 4 owns building and writing the pin. This module only reads it, so
//! `msbx-run --version` and the probe containers have one source of truth and
//! the shim can be verified against a fixture pin.

use crate::state;
use serde::Deserialize;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// The pin file, under the state directory.
pub const IMAGE_FILE: &str = "image.json";

/// Why the pin is unusable. The message names the file and the fix, never a
/// value.
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
        let file: File = serde_json::from_str(&text).map_err(|error| {
            ImageError::new(format!(
                "{} is not a harness image pin: {error}",
                path.display()
            ))
        })?;
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

/// `$XDG_STATE_HOME/msbx/image.json`.
pub fn path() -> PathBuf {
    state::state_dir().join(IMAGE_FILE)
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
}
