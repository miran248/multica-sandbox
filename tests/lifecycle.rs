//! The Compose lifecycle, exercised end to end through the `msbx` binary with a
//! recording `docker` stand-in on `PATH`. The stand-in logs every Compose
//! invocation and prints a `ps` fixture, so the tests assert what `msbx` asked
//! Docker for without needing a daemon.

#![cfg(unix)]

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MANIFEST: &str = r#"x-msbx:
  project: signals
  publish:
    pg: 5432
services:
  pg:
    image: postgres:17-alpine
  client:
    image: alpine:3.20
    command: ["sleep", "300"]
"#;

/// The generated override, as Compose sees it.
#[derive(Debug, Deserialize)]
struct Override {
    services: BTreeMap<String, Service>,
    networks: BTreeMap<String, Network>,
}

#[derive(Debug, Deserialize)]
struct Service {
    container_name: String,
    #[serde(default)]
    ports: Vec<String>,
    labels: BTreeMap<String, String>,
    networks: BTreeMap<String, Attached>,
}

#[derive(Debug, Deserialize)]
struct Attached {
    aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Network {
    name: String,
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    checkout: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    ps: PathBuf,
    state_home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("lifecycle-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let checkout = home.join("work").join("signals");
        let bin = root.join("bin");
        fs::create_dir_all(checkout.join(".multica")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let fixture = Fixture {
            state_home: home.join(".local").join("state"),
            log: root.join("docker.log"),
            ps: root.join("ps.json"),
            root,
            home,
            checkout,
            bin,
        };
        fixture.write_manifest(MANIFEST);
        fixture.write_fake_docker();
        fixture
    }

    fn write_manifest(&self, text: &str) {
        fs::write(self.checkout.join(".multica").join("compose.yaml"), text).unwrap();
    }

    fn write_fake_docker(&self) {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  *\" ps \"*)\n    if [ -f '{}' ]; then cat '{}'; fi ;;\nesac\nexit 0\n",
            self.log.display(),
            self.ps.display(),
            self.ps.display()
        );
        let path = self.bin.join("docker");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn up(&self, args: &[&str]) -> Output {
        self.run_at(&self.checkout, args, &[])
    }

    fn run_at(&self, dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_msbx"));
        command
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("MSBX_EPIC")
            .env_remove("MSBX_PROJECT");
        for (name, value) in envs {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn clear_log(&self) {
        let _ = fs::remove_file(&self.log);
    }

    fn override_path(&self) -> PathBuf {
        self.state_home
            .join("msbx")
            .join("signals")
            .join("mir-42")
            .join("override.yaml")
    }

    fn state_path(&self) -> PathBuf {
        self.state_home.join("msbx").join("state.json")
    }

    fn state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.state_path()).unwrap()).unwrap()
    }

    fn entry(&self, key: &str) -> Value {
        self.state()["blocks"][key].clone()
    }

    fn block_start(&self, key: &str) -> u16 {
        self.entry(key)["start"].as_u64().unwrap() as u16
    }

    fn override_doc(&self) -> Override {
        serde_yaml_ng::from_str(&fs::read_to_string(self.override_path()).unwrap()).unwrap()
    }

    fn checkout(&self) -> PathBuf {
        fs::canonicalize(&self.checkout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
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
        "msbx failed: {}\n{}",
        stdout(output),
        stderr(output)
    );
}

#[test]
fn up_starts_the_box_and_writes_the_override_outside_the_checkout() {
    let fixture = Fixture::new("up");
    let output = fixture.up(&["up", "--epic", "MIR-42"]);
    assert_ok(&output);

    let start = fixture.block_start("signals-mir-42");
    let log = fixture.log();
    assert!(
        log.contains(&format!(
            "-p signals-mir-42 --project-directory {}",
            fixture.checkout().display()
        )),
        "the checkout is the project directory: {log}"
    );
    assert!(
        log.contains(&format!(
            "-f {}/.multica/compose.yaml",
            fixture.checkout().display()
        )),
        "the manifest is the base file: {log}"
    );
    assert!(
        log.contains(" up -d --wait --wait-timeout 240"),
        "up waits for the project: {log}"
    );

    let override_path = fixture.override_path();
    assert!(override_path.is_file(), "the override is written");
    assert!(
        !override_path.starts_with(fixture.checkout()),
        "the override lives outside the checkout"
    );
    let doc = fixture.override_doc();
    let pg = &doc.services["pg"];
    assert_eq!(pg.container_name, "signals-mir-42-pg");
    assert_eq!(pg.ports, vec![format!("127.0.0.1:{start}:5432")]);
    assert_eq!(
        pg.networks["default"].aliases,
        vec!["pg.mir-42.signals.msbx.test"]
    );
    assert_eq!(pg.labels["com.msbx.epic"], "mir-42");
    let client = &doc.services["client"];
    assert_eq!(client.container_name, "signals-mir-42-client");
    assert!(
        client.ports.is_empty(),
        "an unpublished role has no host port"
    );
    assert_eq!(
        client.networks["default"].aliases,
        vec!["client.mir-42.signals.msbx.test"]
    );
    assert_eq!(doc.networks["default"].name, "signals-mir-42");

    let entry = fixture.entry("signals-mir-42");
    assert_eq!(entry["project"].as_str(), Some("signals"));
    assert_eq!(entry["epic"].as_str(), Some("mir-42"));
    assert_eq!(entry["published"][0]["role"].as_str(), Some("pg"));
    assert_eq!(
        entry["published"][0]["port"].as_u64(),
        Some(u64::from(start))
    );
    let text = stdout(&output);
    assert!(
        text.contains(&format!("127.0.0.1:{start} → 5432")),
        "{text}"
    );
    assert!(text.contains("pg.mir-42.signals.msbx.test"), "{text}");
}

#[test]
fn a_second_up_changes_no_port_and_regenerates_the_same_override() {
    let fixture = Fixture::new("idempotent");
    assert_ok(&fixture.up(&["up", "--epic", "mir-42"]));
    let first_override = fs::read_to_string(fixture.override_path()).unwrap();
    let first_port = fixture.block_start("signals-mir-42");

    fixture.clear_log();
    let second = fixture.up(&["up", "--epic", "mir-42"]);
    assert_ok(&second);

    assert_eq!(fixture.block_start("signals-mir-42"), first_port);
    assert_eq!(
        fs::read_to_string(fixture.override_path()).unwrap(),
        first_override
    );
    let log = fixture.log();
    let ups = log.lines().filter(|line| line.contains(" up -d ")).count();
    assert_eq!(ups, 1, "the second up still reconciles once");
    assert!(!stdout(&second).contains("publish changed"));
}

#[test]
fn changing_publish_reallocates_ports_and_prints_the_old_and_new_ones() {
    let fixture = Fixture::new("republish");
    assert_ok(&fixture.up(&["up", "--epic", "mir-42"]));
    let old = fixture.block_start("signals-mir-42");

    fixture.write_manifest(
        r#"x-msbx:
  project: signals
  publish:
    web: 3000
    pg: 5432
services:
  pg:
    image: postgres:17-alpine
  web:
    image: nginx:1.27
"#,
    );
    let output = fixture.up(&["up", "--epic", "mir-42"]);
    assert_ok(&output);

    assert_eq!(
        fixture.block_start("signals-mir-42"),
        old,
        "the block itself stays stable"
    );
    let entry = fixture.entry("signals-mir-42");
    assert_eq!(entry["published"][0]["role"].as_str(), Some("web"));
    assert_eq!(entry["published"][0]["port"].as_u64(), Some(u64::from(old)));
    assert_eq!(entry["published"][1]["role"].as_str(), Some("pg"));
    assert_eq!(
        entry["published"][1]["port"].as_u64(),
        Some(u64::from(old + 1))
    );

    let printed = stdout(&output);
    assert!(printed.contains("publish changed"), "{printed}");
    assert!(
        printed.contains(&format!("pg {old} → {}", old + 1)),
        "{printed}"
    );
    assert!(printed.contains(&format!("web new on {old}")), "{printed}");
}

#[test]
fn status_reports_services_health_and_ports_as_json() {
    let fixture = Fixture::new("status");
    assert_ok(&fixture.up(&["up", "--epic", "mir-42"]));
    let start = fixture.block_start("signals-mir-42");
    fs::write(
        &fixture.ps,
        format!(
            "{{\"Service\":\"pg\",\"Name\":\"signals-mir-42-pg\",\"State\":\"running\",\"Health\":\"healthy\",\"ExitCode\":0,\"Publishers\":[{{\"URL\":\"127.0.0.1\",\"TargetPort\":5432,\"PublishedPort\":{start},\"Protocol\":\"tcp\"}}]}}\n"
        ),
    )
    .unwrap();

    let output = fixture.up(&["status", "--epic", "mir-42", "--json"]);
    assert_ok(&output);
    let report: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(
        report["boxes"][0]["compose_project"].as_str(),
        Some("signals-mir-42")
    );
    assert_eq!(
        report["boxes"][0]["block"]["start"].as_u64(),
        Some(u64::from(start))
    );
    let service = &report["boxes"][0]["services"][0];
    assert_eq!(service["role"].as_str(), Some("pg"));
    assert_eq!(service["state"].as_str(), Some("running"));
    assert_eq!(service["health"].as_str(), Some("healthy"));
    assert_eq!(
        service["ports"][0]["published"].as_u64(),
        Some(u64::from(start))
    );
    assert_eq!(
        service["ports"][0]["name"].as_str(),
        Some("pg.mir-42.signals.msbx.test")
    );

    let human = fixture.up(&["status", "--epic", "mir-42"]);
    assert_ok(&human);
    let text = stdout(&human);
    assert!(text.contains("healthy"), "{text}");
    assert!(
        text.contains(&format!("127.0.0.1:{start} → 5432")),
        "{text}"
    );
}

#[test]
fn down_keeps_state_and_rm_requires_yes_and_frees_the_block() {
    let fixture = Fixture::new("down-rm");
    assert_ok(&fixture.up(&["up", "--epic", "mir-42"]));
    let start = fixture.block_start("signals-mir-42");

    fixture.clear_log();
    assert_ok(&fixture.up(&["down", "--epic", "mir-42"]));
    assert!(
        fixture.log().contains("-p signals-mir-42 stop"),
        "{}",
        fixture.log()
    );
    assert_eq!(fixture.block_start("signals-mir-42"), start);
    assert!(fixture.override_path().is_file(), "down keeps the envelope");

    fixture.clear_log();
    let refused = fixture.up(&["rm", "--epic", "mir-42"]);
    assert_eq!(refused.status.code(), Some(64), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("--yes"), "{}", stderr(&refused));
    assert!(!fixture.log().contains("down -v"));

    let removed = fixture.up(&["rm", "--epic", "mir-42", "--yes"]);
    assert_ok(&removed);
    assert!(
        fixture
            .log()
            .contains("-p signals-mir-42 down -v --remove-orphans"),
        "{}",
        fixture.log()
    );
    assert!(fixture.state()["blocks"].as_object().unwrap().is_empty());
    assert!(
        stdout(&removed).contains(&format!(
            "removed signals-mir-42 ({start}..={} freed)",
            start + 15
        )),
        "{}",
        stdout(&removed)
    );
}

#[test]
fn gc_lists_idle_boxes_dry_and_then_sweeps_them() {
    let fixture = Fixture::new("gc");
    assert_ok(&fixture.up(&["up", "--epic", "mir-42"]));
    let start = fixture.block_start("signals-mir-42");

    // Age the entry past the 14-day window.
    let mut state = fixture.state();
    state["blocks"]["signals-mir-42"]["last_used"] = serde_json::json!(1);
    fs::write(
        fixture.state_path(),
        serde_json::to_string_pretty(&state).unwrap() + "\n",
    )
    .unwrap();

    fixture.clear_log();
    let dry = fixture.up(&["gc", "--dry-run"]);
    assert_ok(&dry);
    assert!(
        stdout(&dry).contains(&format!(
            "would remove signals-mir-42 ({start}..={})",
            start + 15
        )),
        "{}",
        stdout(&dry)
    );
    assert!(fixture.log().is_empty(), "a dry run runs no docker command");
    assert_eq!(
        fixture.entry("signals-mir-42")["last_used"].as_u64(),
        Some(1)
    );

    let swept = fixture.up(&["gc"]);
    assert_ok(&swept);
    assert!(
        fixture
            .log()
            .contains("-p signals-mir-42 down -v --remove-orphans"),
        "{}",
        fixture.log()
    );
    assert!(fixture.state()["blocks"].as_object().unwrap().is_empty());
    assert!(stdout(&swept).contains("removed signals-mir-42"));
}

#[test]
fn an_invalid_manifest_is_refused_before_docker_runs() {
    let fixture = Fixture::new("invalid");
    fixture.write_manifest(
        "x-msbx:\n  project: signals\n  publish:\n    db: 5432\nservices:\n  pg: {}\n",
    );
    let output = fixture.up(&["up", "--epic", "mir-42"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("x-msbx.publish.db"),
        "the offending key is named: {}",
        stderr(&output)
    );
    assert!(fixture.log().is_empty(), "no container command was run");
    assert!(!fixture.state_path().exists(), "no block was allocated");
}

#[test]
fn up_refuses_checkouts_outside_home_and_under_projects() {
    let fixture = Fixture::new("home-guard");
    let outside = fixture.root.join("outside");
    fs::create_dir_all(outside.join(".multica")).unwrap();
    fs::write(outside.join(".multica").join("compose.yaml"), MANIFEST).unwrap();
    let output = fixture.run_at(&outside, &["up", "--epic", "mir-42"], &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("under $HOME"),
        "{}",
        stderr(&output)
    );
    assert!(fixture.log().is_empty());

    let projects = fixture.home.join("projects").join("signals");
    fs::create_dir_all(projects.join(".multica")).unwrap();
    fs::write(projects.join(".multica").join("compose.yaml"), MANIFEST).unwrap();
    let output = fixture.run_at(&projects, &["up", "--epic", "mir-42"], &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("off limits"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_epic_comes_from_the_flag_or_the_environment() {
    let fixture = Fixture::new("epic");
    let missing = fixture.up(&["up"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("no epic"), "{}", stderr(&missing));

    let from_env = fixture.run_at(&fixture.checkout, &["up"], &[("MSBX_EPIC", "MIR-42")]);
    assert_ok(&from_env);
    assert_eq!(
        fixture.entry("signals-mir-42")["epic"].as_str(),
        Some("mir-42")
    );
}

#[test]
fn file_selects_the_manifest_from_any_directory() {
    let fixture = Fixture::new("file");
    let elsewhere = fixture.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let manifest = fixture.checkout.join(".multica").join("compose.yaml");
    let output = fixture.run_at(
        &elsewhere,
        &[
            "up",
            "--epic",
            "mir-42",
            "--file",
            manifest.to_str().unwrap(),
        ],
        &[],
    );
    assert_ok(&output);
    assert!(fixture.log().contains(&format!(
        "--project-directory {}",
        fixture.checkout().display()
    )));
}
