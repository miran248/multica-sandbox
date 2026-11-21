//! `msbx image build|pin|show`, exercised through the binary with a recording
//! `docker` stand-in. The real Docker build is covered by the image build run,
//! not by `cargo test`.

#![cfg(unix)]

use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const MOVED: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    state_home: PathBuf,
    config_home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("image-cli-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let fixture = Fixture {
            state_home: home.join(".local").join("state"),
            config_home: home.join(".config"),
            root,
            home,
            bin,
        };
        fixture.write_labels();
        fixture.write_fake_docker();
        fixture
    }

    fn write_labels(&self) {
        let packages = json!([{"name": "pi-subagents", "version": "0.89.2"}]).to_string();
        let labels = json!({
            "com.msbx.pi.version": "1.6.0",
            "com.msbx.base": "sha256:3333333333333333333333333333333333333333333333333333333333333333",
            "com.msbx.node.version": "26.12.0",
            "com.msbx.pnpm.version": "12.21.0",
            "com.msbx.rust.version": "1.99.1",
            "com.msbx.go.version": "1.27.4",
            "com.msbx.python.version": "3.11.2",
            "com.msbx.multica.version": "0.11.0",
            "com.msbx.built_at": "2026-01-01T00:00:00Z",
            "com.msbx.pi.packages": packages,
        });
        fs::write(self.root.join("labels.json"), labels.to_string()).unwrap();
        fs::write(self.root.join("digest"), format!("[\"msbx-pi@{DIGEST}\"]")).unwrap();
    }

    fn write_fake_docker(&self) {
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
case "$1" in
  image)
    case "$2" in
      ls)
        if [ -f '{tags}' ]; then cat '{tags}'; else printf 'msbx-pi:1.6.0-2\nother:latest\n'; fi
        ;;
      inspect)
        case "$4" in
          *RepoDigests*) cat '{digest}' ;;
          *Labels*) cat '{labels}' ;;
        esac
        ;;
    esac
    ;;
  buildx)
    exit "${{BUILD_EXIT:-0}}"
    ;;
esac
exit 0
"#,
            log = self.root.join("docker.log").display(),
            tags = self.root.join("tags").display(),
            digest = self.root.join("digest").display(),
            labels = self.root.join("labels.json").display(),
        );
        let path = self.bin.join("docker");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn moved_digest(&self) {
        fs::write(self.root.join("digest"), format!("[\"msbx-pi@{MOVED}\"]")).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_msbx"));
        command
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env(
                "MSBX_IMAGE_DIR",
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("images"),
            )
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .stdin(Stdio::null());
        for (name, value) in envs {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join("docker.log")).unwrap_or_default()
    }

    fn pin_path(&self) -> PathBuf {
        self.state_home.join("msbx").join("image.json")
    }

    fn build_path(&self) -> PathBuf {
        self.state_home.join("msbx").join("image-build.json")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
}

fn build(fixture: &Fixture) {
    let output = fixture.run(&["image", "build"]);
    assert_ok(&output);
}

#[test]
fn build_records_a_new_tag_and_pin_records_its_digest() {
    let fixture = Fixture::new("build-pin");
    let output = fixture.run(&["image", "build"]);
    assert_ok(&output);
    assert!(
        stdout(&output).contains("built msbx-pi:1.6.0-3"),
        "{}",
        stdout(&output)
    );
    assert!(fixture.log().contains("buildx build --load"));
    assert!(fixture.log().contains("--build-arg PI_VERSION=1.6.0"));
    assert!(fixture
        .log()
        .contains("--build-arg APT_LIBSTDCXX6=12.2.0-14+deb12u1"));

    let record = fs::read_to_string(fixture.build_path()).unwrap();
    assert!(record.contains("\"image\": \"msbx-pi:1.6.0-3\""));
    assert!(record.contains(DIGEST));
    let mode = fs::metadata(fixture.build_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    let output = fixture.run(&["image", "pin"]);
    assert_ok(&output);
    let pin = fs::read_to_string(fixture.pin_path()).unwrap();
    assert!(pin.contains("\"image\": \"msbx-pi:1.6.0-3\""));
    assert!(pin.contains(DIGEST));
    assert!(pin.contains("\"pi\": \"1.6.0\""));
    assert!(pin.contains("pi-subagents"));

    let output = fixture.run(&["image", "show"]);
    assert_ok(&output);
    let text = stdout(&output);
    assert!(text.contains("msbx-pi:1.6.0-3"), "{text}");
    assert!(text.contains("1.6.0"), "{text}");
    assert!(text.contains("26.12.0"), "{text}");
    assert!(text.contains("pi-subagents 0.89.2"), "{text}");
}

#[test]
fn a_tag_that_moved_after_the_build_is_refused() {
    let fixture = Fixture::new("moved");
    build(&fixture);
    fixture.moved_digest();
    let output = fixture.run(&["image", "pin"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("moved"), "{}", stderr(&output));
    assert!(!fixture.pin_path().exists());
}

#[test]
fn an_existing_pin_for_the_same_tag_with_another_digest_is_refused() {
    let fixture = Fixture::new("repin");
    build(&fixture);
    assert_ok(&fixture.run(&["image", "pin"]));
    let pinned = fs::read_to_string(fixture.pin_path()).unwrap();
    assert!(pinned.contains(DIGEST));

    // The tag is now a different image, and the build record was refreshed in
    // step with it: a fresh pin would silently replace the old one.
    fixture.moved_digest();
    let mut record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixture.build_path()).unwrap()).unwrap();
    record["digest"] = json!(MOVED);
    fs::write(fixture.build_path(), record.to_string()).unwrap();
    let output = fixture.run(&["image", "pin"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("refusing to repin"),
        "{}",
        stderr(&output)
    );
    assert!(fs::read_to_string(fixture.pin_path())
        .unwrap()
        .contains(DIGEST));
}

#[test]
fn pin_without_a_build_names_the_command_that_builds_it() {
    let fixture = Fixture::new("no-build");
    let output = fixture.run(&["image", "pin"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("msbx image build"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn show_without_a_pin_names_the_command_that_writes_it() {
    let fixture = Fixture::new("no-pin");
    let output = fixture.run(&["image", "show"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("msbx image pin"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_floating_manifest_is_refused_before_docker_runs() {
    let fixture = Fixture::new("floating");
    let source = fixture.root.join("images");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("images"),
        &source,
    );
    let path = source.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    manifest["base"] = json!("debian:bookworm-slim");
    fs::write(&path, manifest.to_string()).unwrap();

    let output = fixture.run_env(
        &["image", "build"],
        &[("MSBX_IMAGE_DIR", source.to_str().unwrap())],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("pinned by digest"),
        "{}",
        stderr(&output)
    );
    assert!(!fixture.log().contains("buildx"));
}

#[test]
fn a_failed_build_records_nothing() {
    let fixture = Fixture::new("build-fail");
    let output = fixture.run_env(&["image", "build"], &[("BUILD_EXIT", "1")]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("was not built"),
        "{}",
        stderr(&output)
    );
    assert!(!fixture.build_path().exists());
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else if entry.path().extension().and_then(|s| s.to_str()) == Some("md")
            || entry.file_name() == "Dockerfile"
            || entry.file_name() == "manifest.json"
            || entry.file_name() == "entrypoint.sh"
        {
            fs::copy(entry.path(), &target).unwrap();
        } else {
            // The agent tree carries settings and prompts; copy it wholesale.
            fs::create_dir_all(target.parent().unwrap()).ok();
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[test]
fn the_command_surface_prints_usage() {
    let fixture = Fixture::new("usage");
    let output = fixture.run(&["image", "--help"]);
    assert_ok(&output);
    assert!(stdout(&output).contains("msbx image build|pin|show"));
}

#[test]
fn the_pin_is_written_private() {
    let fixture = Fixture::new("private");
    build(&fixture);
    assert_ok(&fixture.run(&["image", "pin"]));
    let mode = fs::metadata(fixture.pin_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}
