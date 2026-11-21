//! The Compose lifecycle, exercised end to end through the `msbx` binary with a
//! recording `docker` stand-in and a fake `multica` CLI on `PATH`. The stand-in
//! logs every Compose invocation and prints a `ps` fixture, so the tests assert
//! what `msbx` asked Docker for without needing a daemon.

#![cfg(unix)]

use multica_sandbox::env::parse_exports;
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

const ENV_MANIFEST: &str = r#"x-msbx:
  project: signals
  publish:
    pg: 5432
  env:
    pg: [DATABASE_URL]
    client: [STRIPE_KEY]
  secrets:
    client: [SMTP_PASSWORD]
services:
  pg:
    image: postgres:17-alpine
  client:
    image: alpine:3.20
    command: ["sleep", "300"]
"#;

const DATABASE_URL: &str = "postgres://{{host:pg}}:{{port:pg}}/{{scope}}/{{project}}";
const STRIPE_KEY: &str = "sk_live_marker";
const SMTP_PASSWORD: &str = "hunter2-marker";
const HOST_TOKEN: &str = "host-token-marker";
const HARNESS_TOKEN: &str = "harness-token-marker";

const TUNNEL_MANIFEST: &str = r#"x-msbx:
  project: signals
  env:
    pg: [MSBX_TUNNEL_SSH_USER]
  secrets:
    pg: [MSBX_TUNNEL_SSH_KEY]
  tunnels:
    pg:
      host: db.internal.example.com
      port: 5432
      via: ssh
services:
  client:
    image: alpine:3.20
    command: ["sleep", "300"]
"#;

const CLOUDFLARED_MANIFEST: &str = r#"x-msbx:
  project: signals
  tunnels:
    pg:
      host: db.internal.example.com
      port: 5432
      via: cloudflared
services:
  client:
    image: alpine:3.20
    command: ["sleep", "300"]
"#;

const TUNNEL_SSH_KEY: &str = "a2V5LW1hcmtlcg==";

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
    image: String,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    healthcheck: Option<Healthcheck>,
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default)]
    env_file: Vec<String>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default)]
    volumes: Vec<String>,
    labels: BTreeMap<String, String>,
    networks: BTreeMap<String, Attached>,
}

#[derive(Debug, Deserialize)]
struct Healthcheck {
    test: Vec<String>,
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
    config_home: PathBuf,
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
            config_home: home.join(".config"),
            log: root.join("docker.log"),
            ps: root.join("ps.json"),
            root,
            home,
            checkout,
            bin,
        };
        fixture.write_manifest(MANIFEST);
        fixture.write_fake_docker();
        fixture.write_fake_multica();
        fixture
    }

    fn write_manifest(&self, text: &str) {
        fs::write(self.checkout.join(".multica").join("compose.yaml"), text).unwrap();
    }

    fn write_fake_docker(&self) {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf 'ENV %s %s\\n' \"$MSBX_PG_HOST\" \"$MSBX_PG_PORT\" >> '{}'\ncase \"$*\" in\n  *\" ps \"*)\n    if [ -f '{}' ]; then cat '{}'; fi ;;\nesac\nexit 0\n",
            self.log.display(),
            self.log.display(),
            self.ps.display(),
            self.ps.display()
        );
        write_exec(&self.bin.join("docker"), &script);
    }

    fn write_fake_multica(&self) {
        let dir = self.root.join("multica");
        fs::create_dir_all(&dir).unwrap();
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = issue ] && [ \"$2\" = get ]; then\n  if [ -f '{}/fail' ]; then echo 'multica: not found' >&2; exit 1; fi\n  if [ -f \"{}/$3.json\" ]; then cat \"{}/$3.json\"; exit 0; fi\n  printf '%s\\n' '{{\"identifier\":\"MIR-42\",\"status_category\":\"in_progress\",\"archived_at\":null}}'\n  exit 0\nfi\necho \"multica: cannot resolve $*\" >&2\nexit 1\n",
            self.root.join("multica.log").display(),
            dir.display(),
            dir.display(),
            dir.display(),
        );
        write_exec(&self.bin.join("multica"), &script);
    }

    fn set_issue(&self, scope: &str, json: &str) {
        let dir = self.root.join("multica");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{scope}.json")), json).unwrap();
    }

    fn fail_multica(&self) {
        let dir = self.root.join("multica");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("fail"), "").unwrap();
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
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("MULTICA_CLI_PATH", self.bin.join("multica"))
            .env_remove("MSBX_SCOPE")
            .env_remove("MSBX_EPIC")
            .env_remove("MSBX_PROJECT");
        for (name, value) in envs {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    fn branch(&self, name: &str) {
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(&self.checkout)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        if !self.checkout.join(".git").exists() {
            run(&["init", "-q"]);
        }
        run(&["checkout", "-q", "-B", name]);
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn clear_log(&self) {
        let _ = fs::remove_file(&self.log);
    }

    fn multica_log(&self) -> String {
        fs::read_to_string(self.root.join("multica.log")).unwrap_or_default()
    }

    fn clear_multica_log(&self) {
        let _ = fs::remove_file(self.root.join("multica.log"));
    }

    fn override_path(&self, scope: &str) -> PathBuf {
        self.envelope_dir(scope).join("override.yaml")
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

    fn override_doc(&self, scope: &str) -> Override {
        serde_yaml_ng::from_str(&fs::read_to_string(self.override_path(scope)).unwrap()).unwrap()
    }

    fn envelope_dir(&self, scope: &str) -> PathBuf {
        self.state_home.join("msbx").join("signals").join(scope)
    }

    fn write_store_env(&self, body: &str) {
        let dir = self
            .config_home
            .join("msbx")
            .join("projects")
            .join("signals");
        fs::create_dir_all(&dir).unwrap();
        write_private(&dir.join("env"), body);
    }

    fn write_store_secret(&self, name: &str, body: &str) {
        let dir = self
            .config_home
            .join("msbx")
            .join("projects")
            .join("signals")
            .join("secrets");
        fs::create_dir_all(&dir).unwrap();
        write_private(&dir.join(name), body);
    }

    fn write_host_env(&self, body: &str) {
        let path = self.config_home.join("msbx").join("host.env");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, body);
    }

    fn write_host_allow(&self, body: &str) {
        let path = self.config_home.join("msbx").join("host-allow");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, body);
    }

    fn write_harness_env(&self, body: &str) {
        let path = self.config_home.join("msbx").join("harness").join("env");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, body);
    }

    fn harness(&self, scope: &str) -> BTreeMap<String, String> {
        let text =
            fs::read_to_string(self.envelope_dir(scope).join("env").join("harness.env")).unwrap();
        text.lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    fn host(&self, scope: &str) -> BTreeMap<String, String> {
        let text =
            fs::read_to_string(self.envelope_dir(scope).join("env").join("host.env")).unwrap();
        parse_exports(&text)
            .unwrap()
            .into_iter()
            .collect::<BTreeMap<_, _>>()
    }

    fn checkout(&self) -> PathBuf {
        fs::canonicalize(&self.checkout).unwrap()
    }
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn write_private(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn tree_contains(dir: &Path, needle: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if tree_contains(&path, needle) {
                return true;
            }
        } else if fs::read_to_string(&path)
            .map(|text| text.contains(needle))
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
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
    let output = fixture.up(&["up", "--scope", "MIR-42"]);
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
    assert!(
        log.contains("ENV pg.mir-42.signals.msbx.test 5432"),
        "the Compose process gets the service-context pair: {log}"
    );

    let override_path = fixture.override_path("mir-42");
    assert!(override_path.is_file(), "the override is written");
    assert!(
        !override_path.starts_with(fixture.checkout()),
        "the override lives outside the checkout"
    );
    let doc = fixture.override_doc("mir-42");
    let pg = &doc.services["pg"];
    assert_eq!(pg.container_name, "signals-mir-42-pg");
    assert_eq!(pg.ports, vec![format!("127.0.0.1:{start}:5432")]);
    assert_eq!(
        pg.networks["default"].aliases,
        vec!["pg.mir-42.signals.msbx.test"]
    );
    assert_eq!(pg.labels["com.msbx.scope"], "mir-42");
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
    assert_eq!(entry["scope"].as_str(), Some("mir-42"));
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
    assert!(text.contains("project signals, scope mir-42"), "{text}");
}

#[test]
fn two_sibling_tickets_run_the_same_stack_on_distinct_boxes() {
    let fixture = Fixture::new("siblings");
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
    assert_ok(&fixture.up(&["up", "--scope", "mir-43"]));

    let first = fixture.block_start("signals-mir-42");
    let second = fixture.block_start("signals-mir-43");
    assert_ne!(first, second, "sibling tickets get distinct blocks");
    assert_eq!(second, first + 16, "each block is 16 ports");

    let doc_42 = fixture.override_doc("mir-42");
    let doc_43 = fixture.override_doc("mir-43");
    assert_eq!(doc_42.services["pg"].container_name, "signals-mir-42-pg");
    assert_eq!(doc_43.services["pg"].container_name, "signals-mir-43-pg");
    assert_eq!(
        doc_42.services["pg"].networks["default"].aliases,
        vec!["pg.mir-42.signals.msbx.test"]
    );
    assert_eq!(
        doc_43.services["pg"].networks["default"].aliases,
        vec!["pg.mir-43.signals.msbx.test"]
    );
    assert_eq!(
        fixture.state()["blocks"].as_object().unwrap().len(),
        2,
        "two boxes are recorded"
    );
}

#[test]
fn a_chat_scope_gets_its_own_box() {
    let fixture = Fixture::new("chat");
    assert_ok(&fixture.up(&["up", "--scope", "chat-1a2b3c4d"]));
    let doc = fixture.override_doc("chat-1a2b3c4d");
    assert_eq!(
        doc.services["pg"].container_name,
        "signals-chat-1a2b3c4d-pg"
    );
    assert_eq!(
        doc.services["pg"].networks["default"].aliases,
        vec!["pg.chat-1a2b3c4d.signals.msbx.test"]
    );
    assert_eq!(
        fixture.entry("signals-chat-1a2b3c4d")["scope"].as_str(),
        Some("chat-1a2b3c4d")
    );
}

#[test]
fn a_second_up_changes_no_port_and_regenerates_the_same_override() {
    let fixture = Fixture::new("idempotent");
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
    let first_override = fs::read_to_string(fixture.override_path("mir-42")).unwrap();
    let first_port = fixture.block_start("signals-mir-42");

    fixture.clear_log();
    let second = fixture.up(&["up", "--scope", "mir-42"]);
    assert_ok(&second);

    assert_eq!(fixture.block_start("signals-mir-42"), first_port);
    assert_eq!(
        fs::read_to_string(fixture.override_path("mir-42")).unwrap(),
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
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
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
    let output = fixture.up(&["up", "--scope", "mir-42"]);
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
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
    let start = fixture.block_start("signals-mir-42");
    fs::write(
        &fixture.ps,
        format!(
            "{{\"Service\":\"pg\",\"Name\":\"signals-mir-42-pg\",\"State\":\"running\",\"Health\":\"healthy\",\"ExitCode\":0,\"Publishers\":[{{\"URL\":\"127.0.0.1\",\"TargetPort\":5432,\"PublishedPort\":{start},\"Protocol\":\"tcp\"}}]}}\n"
        ),
    )
    .unwrap();

    let output = fixture.up(&["status", "--scope", "mir-42", "--json"]);
    assert_ok(&output);
    let report: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(
        report["boxes"][0]["compose_project"].as_str(),
        Some("signals-mir-42")
    );
    assert_eq!(report["boxes"][0]["scope"].as_str(), Some("mir-42"));
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

    let human = fixture.up(&["status", "--scope", "mir-42"]);
    assert_ok(&human);
    let text = stdout(&human);
    assert!(text.contains("healthy"), "{text}");
    assert!(
        text.contains(&format!("127.0.0.1:{start} → 5432")),
        "{text}"
    );
    assert!(text.contains("signals/mir-42"), "{text}");
}

#[test]
fn down_keeps_state_and_rm_requires_yes_and_frees_the_block() {
    let fixture = Fixture::new("down-rm");
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
    let start = fixture.block_start("signals-mir-42");

    fixture.clear_log();
    assert_ok(&fixture.up(&["down", "--scope", "mir-42"]));
    assert!(
        fixture.log().contains("-p signals-mir-42 stop"),
        "{}",
        fixture.log()
    );
    assert_eq!(fixture.block_start("signals-mir-42"), start);
    assert!(
        fixture.override_path("mir-42").is_file(),
        "down keeps the envelope"
    );

    fixture.clear_log();
    let refused = fixture.up(&["rm", "--scope", "mir-42"]);
    assert_eq!(refused.status.code(), Some(64), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("--yes"), "{}", stderr(&refused));
    assert!(!fixture.log().contains("down -v"));

    let removed = fixture.up(&["rm", "--scope", "mir-42", "--yes"]);
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
        !fixture.envelope_dir("mir-42").exists(),
        "rm removes the envelope"
    );
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
fn rm_without_a_scope_refuses_to_destroy_every_box() {
    let fixture = Fixture::new("rm-all");
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
    assert_ok(&fixture.up(&["up", "--scope", "mir-43"]));
    assert_eq!(fixture.state()["blocks"].as_object().unwrap().len(), 2);

    fixture.clear_log();
    let refused = fixture.up(&["rm", "--yes"]);
    assert_eq!(refused.status.code(), Some(64), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("--scope"), "{}", stderr(&refused));
    assert!(!fixture.log().contains("down -v"), "{}", fixture.log());
    assert_eq!(fixture.state()["blocks"].as_object().unwrap().len(), 2);
}

#[test]
fn gc_lists_the_idle_backstop_dry_and_then_sweeps_it() {
    let fixture = Fixture::new("gc");
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
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
    assert!(
        !fixture.envelope_dir("mir-42").exists(),
        "gc removes the envelope"
    );
}

#[test]
fn gc_removes_a_final_ticket_scope_whatever_its_idle_time() {
    let fixture = Fixture::new("gc-final");
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
    fixture.set_issue(
        "mir-42",
        r#"{"identifier":"MIR-42","status_category":"done","archived_at":null}"#,
    );
    // A lease left by an earlier run belongs to the box and is swept with it.
    let leases = fixture
        .state_home
        .join("msbx")
        .join("leases")
        .join("signals")
        .join("mir-42");
    fs::create_dir_all(&leases).unwrap();
    fs::write(leases.join("task.json"), "{}").unwrap();

    fixture.clear_log();
    let dry = fixture.up(&["gc", "--dry-run"]);
    assert_ok(&dry);
    let text = stdout(&dry);
    assert!(text.contains("would remove signals-mir-42"), "{text}");
    assert!(text.contains("the issue is done"), "{text}");
    assert!(fixture.log().is_empty(), "a dry run runs no docker command");
    assert!(
        fixture.envelope_dir("mir-42").exists(),
        "a dry run changes nothing"
    );

    let swept = fixture.up(&["gc"]);
    assert_ok(&swept);
    assert!(fixture.state()["blocks"].as_object().unwrap().is_empty());
    assert!(!fixture.envelope_dir("mir-42").exists());
    assert!(!leases.exists(), "the leases are removed with the box");
    assert!(stdout(&swept).contains("the issue is done"));
}

#[test]
fn gc_removes_a_cancelled_or_archived_scope_and_a_gone_issue() {
    let fixture = Fixture::new("gc-final-states");
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
    assert_ok(&fixture.up(&["up", "--scope", "mir-43"]));
    fixture.set_issue(
        "mir-42",
        r#"{"identifier":"MIR-42","status_category":"cancelled","archived_at":null}"#,
    );
    fixture.set_issue(
        "mir-43",
        r#"{"identifier":"MIR-43","status_category":"in_progress","archived_at":"2026-01-01T00:00:00Z"}"#,
    );
    assert_ok(&fixture.up(&["gc"]));
    assert!(fixture.state()["blocks"].as_object().unwrap().is_empty());

    let gone = Fixture::new("gc-gone");
    assert_ok(&gone.up(&["up", "--scope", "mir-44"]));
    gone.fail_multica();
    let swept = gone.up(&["gc"]);
    assert_ok(&swept);
    assert!(gone.state()["blocks"].as_object().unwrap().is_empty());
    assert!(stdout(&swept).contains("the issue is gone"));
}

#[test]
fn gc_removes_a_chat_box_after_two_idle_days_and_a_ticket_after_fourteen() {
    let fixture = Fixture::new("gc-idle");
    assert_ok(&fixture.up(&["up", "--scope", "chat-1a2b3c4d"]));
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));

    let two_days = 2 * 24 * 60 * 60;
    let mut state = fixture.state();
    state["blocks"]["signals-chat-1a2b3c4d"]["last_used"] =
        serde_json::json!(multica_now() - two_days - 10);
    state["blocks"]["signals-mir-42"]["last_used"] =
        serde_json::json!(multica_now() - two_days - 10);
    fs::write(
        fixture.state_path(),
        serde_json::to_string_pretty(&state).unwrap() + "\n",
    )
    .unwrap();

    assert_ok(&fixture.up(&["gc"]));
    let blocks = fixture.state()["blocks"].as_object().unwrap().clone();
    assert!(
        !blocks.contains_key("signals-chat-1a2b3c4d"),
        "a chat box idle for two days is removed"
    );
    assert!(
        blocks.contains_key("signals-mir-42"),
        "a ticket box is kept before 14 days"
    );

    let mut state = fixture.state();
    state["blocks"]["signals-mir-42"]["last_used"] = serde_json::json!(1);
    fs::write(
        fixture.state_path(),
        serde_json::to_string_pretty(&state).unwrap() + "\n",
    )
    .unwrap();
    assert_ok(&fixture.up(&["gc"]));
    assert!(fixture.state()["blocks"].as_object().unwrap().is_empty());
}

fn multica_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn up_makes_the_final_check_and_removes_a_final_boxes_state() {
    let fixture = Fixture::new("up-final");
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
    fixture.set_issue(
        "mir-42",
        r#"{"identifier":"MIR-42","status_category":"cancelled","archived_at":null}"#,
    );

    fixture.clear_log();
    let refused = fixture.up(&["up", "--scope", "mir-42"]);
    assert!(!refused.status.success());
    let message = stderr(&refused);
    assert!(message.contains("the scope is final"), "{message}");
    assert!(message.contains("cancelled"), "{message}");
    assert!(
        fixture.log().contains("down -v"),
        "the box is removed: {}",
        fixture.log()
    );
    assert!(
        fixture.state()["blocks"].as_object().unwrap().is_empty(),
        "up releases the final block"
    );
    assert!(!fixture.envelope_dir("mir-42").exists());
}

#[test]
fn up_refuses_a_bind_of_the_checkout_root_but_allows_a_subpath() {
    let fixture = Fixture::new("state-bind");
    fixture.write_manifest(
        r#"x-msbx:
  project: signals
services:
  client:
    image: alpine:3.20
    volumes:
      - ./:/work
"#,
    );
    let output = fixture.up(&["up", "--scope", "mir-42"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("binds the checkout root"), "{message}");
    assert!(message.contains("state only"), "{message}");
    assert!(fixture.log().is_empty(), "no container command was run");
    assert!(!fixture.state_path().exists(), "no block was allocated");

    fixture.write_manifest(
        r#"x-msbx:
  project: signals
services:
  client:
    image: alpine:3.20
    volumes:
      - ./init.sql:/docker-entrypoint-initdb.d/init.sql:ro
"#,
    );
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
}

#[test]
fn up_refuses_a_build_inside_the_checkout() {
    let fixture = Fixture::new("state-build");
    fixture.write_manifest(
        r#"x-msbx:
  project: signals
services:
  web:
    build: .
"#,
    );
    let output = fixture.up(&["up", "--scope", "mir-42"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("inside the checkout"), "{message}");
    assert!(message.contains("harness container"), "{message}");
    assert!(fixture.log().is_empty(), "no container command was run");
    assert!(!fixture.state_path().exists(), "no block was allocated");

    // A build context outside the checkout is still a published image.
    fixture.write_manifest(
        r#"x-msbx:
  project: signals
services:
  web:
    build: ../shared
"#,
    );
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
}

#[test]
fn a_host_name_resolves_only_when_the_allowlist_lists_it() {
    let fixture = Fixture::new("host-store");
    fixture.write_manifest(
        "x-msbx:\n  project: signals\n  env:\n    client: [HOST_TOKEN]\nservices:\n  client:\n    image: alpine:3.20\n",
    );
    fixture.write_host_env(&format!("HOST_TOKEN={HOST_TOKEN}\n"));

    let refused = fixture.up(&["up", "--scope", "mir-42"]);
    assert!(!refused.status.success());
    let message = stderr(&refused);
    assert!(message.contains("`HOST_TOKEN`"), "{message}");
    assert!(message.contains("host-allow"), "{message}");
    assert!(fixture.log().is_empty(), "no container command was run");
    assert!(!fixture.state_path().exists(), "no block was allocated");

    fixture.write_host_allow("HOST_TOKEN\n");
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));
    let env = fs::read_to_string(
        fixture
            .envelope_dir("mir-42")
            .join("env")
            .join("client.env"),
    )
    .unwrap();
    assert!(env.contains(&format!("HOST_TOKEN={HOST_TOKEN}")), "{env}");
    assert_eq!(fixture.harness("mir-42")["HOST_TOKEN"], HOST_TOKEN);
    assert_eq!(fixture.host("mir-42")["HOST_TOKEN"], HOST_TOKEN);
}

#[test]
fn the_harness_store_never_enters_the_service_envelope_but_reaches_the_harness() {
    let fixture = Fixture::new("harness-store");
    fixture.write_harness_env(&format!("GH_TOKEN={HARNESS_TOKEN}\n"));
    assert_ok(&fixture.up(&["up", "--scope", "mir-42"]));

    let envelope = fixture.envelope_dir("mir-42");
    for file in ["pg.env", "client.env"] {
        let path = envelope.join("env").join(file);
        let text = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !text.contains(HARNESS_TOKEN),
            "{file} is a service envelope and carries no harness value"
        );
    }
    let override_text = fs::read_to_string(fixture.override_path("mir-42")).unwrap();
    assert!(
        !override_text.contains(HARNESS_TOKEN),
        "the service override carries no harness value"
    );
    let state_text = fs::read_to_string(fixture.state_path()).unwrap();
    assert!(!state_text.contains(HARNESS_TOKEN));

    assert_eq!(fixture.harness("mir-42")["GH_TOKEN"], HARNESS_TOKEN);
    assert_eq!(fixture.host("mir-42")["GH_TOKEN"], HARNESS_TOKEN);
}

#[test]
fn up_renders_container_ports_into_role_and_harness_and_loopback_ports_into_host() {
    let fixture = Fixture::new("env-render");
    fixture.write_manifest(ENV_MANIFEST);
    fixture.write_store_env(&format!(
        "DATABASE_URL={DATABASE_URL}\nSTRIPE_KEY={STRIPE_KEY}\n"
    ));
    fixture.write_store_secret("SMTP_PASSWORD", &format!("{SMTP_PASSWORD}\n"));
    fixture.write_harness_env(&format!("GH_TOKEN={HARNESS_TOKEN}\n"));

    let output = fixture.up(&["up", "--scope", "MIR-42"]);
    assert_ok(&output);
    let start = fixture.block_start("signals-mir-42");
    let envelope = fixture.envelope_dir("mir-42");

    let container_url = "postgres://pg.mir-42.signals.msbx.test:5432/mir-42/signals";
    let host_url = format!("postgres://pg.mir-42.signals.msbx.test:{start}/mir-42/signals");
    let pg_env = fs::read_to_string(envelope.join("env").join("pg.env")).unwrap();
    assert!(
        pg_env.contains(&format!("DATABASE_URL={container_url}")),
        "{pg_env}"
    );
    let client_env = fs::read_to_string(envelope.join("env").join("client.env")).unwrap();
    assert!(
        client_env.contains(&format!("STRIPE_KEY={STRIPE_KEY}")),
        "{client_env}"
    );
    let secret = envelope.join("secrets").join("SMTP_PASSWORD");
    assert_eq!(
        fs::read_to_string(&secret).unwrap(),
        format!("{SMTP_PASSWORD}\n")
    );

    let harness = fixture.harness("mir-42");
    assert_eq!(harness["DATABASE_URL"], container_url);
    assert_eq!(harness["MSBX_PG_PORT"], "5432");
    assert_eq!(harness["SMTP_PASSWORD"], SMTP_PASSWORD);
    assert!(!harness.contains_key("SMTP_PASSWORD_FILE"));
    assert_eq!(harness["GH_TOKEN"], HARNESS_TOKEN);

    let host = fixture.host("mir-42");
    assert_eq!(host["DATABASE_URL"], host_url);
    assert_eq!(host["MSBX_PG_PORT"], start.to_string());
    assert_eq!(
        host["SMTP_PASSWORD_FILE"],
        secret.to_string_lossy().to_string()
    );
    assert!(!host.contains_key("SMTP_PASSWORD"));

    #[cfg(unix)]
    for path in [
        envelope.join("env").join("pg.env"),
        envelope.join("env").join("client.env"),
        envelope.join("env").join("harness.env"),
        envelope.join("env").join("host.env"),
        secret.clone(),
    ] {
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{} is private", path.display());
    }

    let doc = fixture.override_doc("mir-42");
    let pg = &doc.services["pg"];
    assert_eq!(
        pg.env_file,
        vec![envelope
            .join("env")
            .join("pg.env")
            .to_string_lossy()
            .to_string()]
    );
    let client = &doc.services["client"];
    assert_eq!(
        client.environment["SMTP_PASSWORD_FILE"],
        "/run/msbx/secrets/SMTP_PASSWORD"
    );
    assert_eq!(
        client.volumes,
        vec![format!(
            "{}:/run/msbx/secrets/SMTP_PASSWORD:ro",
            secret.display()
        )]
    );

    // No value leaks into the checkout, the override, the state, docker argv or
    // what msbx prints. The envelopes carry them by design.
    let override_text = fs::read_to_string(fixture.override_path("mir-42")).unwrap();
    let state_text = fs::read_to_string(fixture.state_path()).unwrap();
    let log = fixture.log();
    let printed = format!("{}{}", stdout(&output), stderr(&output));
    for value in [STRIPE_KEY, SMTP_PASSWORD, HARNESS_TOKEN] {
        assert!(
            !override_text.contains(value),
            "the override carries no value"
        );
        assert!(!state_text.contains(value), "the state carries no value");
        assert!(!log.contains(value), "no value reaches docker argv or env");
        assert!(!printed.contains(value), "msbx prints names, not values");
        assert!(
            !tree_contains(&fixture.checkout(), value),
            "no value reaches the checkout"
        );
    }
    let printed = stdout(&output);
    assert!(
        printed.contains("DATABASE_URL") && printed.contains("SMTP_PASSWORD"),
        "up names what it rendered: {printed}"
    );
}

#[test]
fn env_ls_reads_the_harness_names_and_export_prints_the_host_context() {
    let fixture = Fixture::new("env-verbs");
    fixture.write_manifest(ENV_MANIFEST);
    fixture.write_store_env(&format!(
        "DATABASE_URL={DATABASE_URL}\nSTRIPE_KEY={STRIPE_KEY}\n"
    ));
    fixture.write_store_secret("SMTP_PASSWORD", &format!("{SMTP_PASSWORD}\n"));
    assert_ok(&fixture.up(&["up", "--scope", "MIR-42"]));
    let start = fixture.block_start("signals-mir-42");

    let ls = fixture.up(&["env", "ls", "--scope", "MIR-42"]);
    assert_ok(&ls);
    let names_text = stdout(&ls);
    let names: Vec<&str> = names_text.lines().map(str::trim).collect();
    assert!(names.contains(&"DATABASE_URL"), "{names:?}");
    assert!(names.contains(&"MSBX_PG_PORT"), "{names:?}");
    assert!(names.contains(&"SMTP_PASSWORD"), "{names:?}");
    assert!(
        !stdout(&ls).contains(STRIPE_KEY) && !stdout(&ls).contains(SMTP_PASSWORD),
        "ls prints names without values"
    );

    let export = fixture.up(&["env", "export", "--scope", "MIR-42"]);
    assert_ok(&export);
    let text = stdout(&export);
    assert!(
        text.contains(&format!("export MSBX_PG_PORT='{start}'")),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "export DATABASE_URL='postgres://pg.mir-42.signals.msbx.test:{start}/mir-42/signals'"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("export STRIPE_KEY='{STRIPE_KEY}'")),
        "{text}"
    );

    // The scope comes from MSBX_SCOPE like the other verbs.
    let from_env = fixture.run_at(
        &fixture.checkout,
        &["env", "ls"],
        &[("MSBX_SCOPE", "MIR-42")],
    );
    assert_ok(&from_env);
    assert!(stdout(&from_env).contains("DATABASE_URL"));

    let missing = fixture.up(&["env", "ls"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("cannot resolve the scope"),
        "{}",
        stderr(&missing)
    );

    let before = Fixture::new("env-before");
    let output = before.up(&["env", "ls", "--scope", "mir-42"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("run `msbx up` first"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_scope_comes_from_the_flag_then_the_environment_then_the_task_context_then_the_branch() {
    let fixture = Fixture::new("scope-order");
    fixture.branch("agent/implementer/mir-77-eebfc37c0c33");

    // The branch is the last fallback.
    assert_ok(&fixture.up(&["up"]));
    assert!(fixture.state()["blocks"]
        .as_object()
        .unwrap()
        .contains_key("signals-mir-77"));

    // The daemon task context (an issue id) beats the branch, and it is not
    // followed by a parent lookup.
    let marker = fixture.home.join("work").join(".multica");
    fs::create_dir_all(&marker).unwrap();
    fs::write(
        marker.join("daemon_task_context.json"),
        r#"{"managed_by":"multica-daemon-task","issue_id":"issue-uuid-261"}"#,
    )
    .unwrap();
    fixture.clear_multica_log();
    assert_ok(&fixture.up(&["up"]));
    assert!(
        fixture.state()["blocks"]
            .as_object()
            .unwrap()
            .contains_key("signals-mir-42"),
        "the task context wins over the branch"
    );
    let calls = fixture.multica_log();
    assert!(
        calls.starts_with("issue get issue-uuid-261 --output json\n"),
        "{calls}"
    );
    assert!(
        !calls.contains("parent"),
        "there is no parent lookup: {calls}"
    );

    // MSBX_SCOPE beats the marker.
    assert_ok(&fixture.run_at(&fixture.checkout, &["up"], &[("MSBX_SCOPE", "mir-55")]));
    assert!(fixture.state()["blocks"]
        .as_object()
        .unwrap()
        .contains_key("signals-mir-55"));

    // --scope beats MSBX_SCOPE.
    assert_ok(&fixture.run_at(
        &fixture.checkout,
        &["up", "--scope", "mir-56"],
        &[("MSBX_SCOPE", "mir-55")],
    ));
    assert!(fixture.state()["blocks"]
        .as_object()
        .unwrap()
        .contains_key("signals-mir-56"));
}

#[test]
fn a_chat_task_context_resolves_a_chat_scope() {
    let fixture = Fixture::new("chat-context");
    fixture.branch("agent/implementer/mir-77-x");
    let marker = fixture.home.join("work").join(".multica");
    fs::create_dir_all(&marker).unwrap();
    fs::write(
        marker.join("daemon_task_context.json"),
        r#"{"managed_by":"multica-daemon-task","chat_session_id":"1a2b3c4d5e6f"}"#,
    )
    .unwrap();
    assert_ok(&fixture.up(&["up"]));
    assert!(fixture.state()["blocks"]
        .as_object()
        .unwrap()
        .contains_key("signals-chat-1a2b3c4d"));
}

#[test]
fn a_resolution_failure_is_loud_and_starts_nothing() {
    let fixture = Fixture::new("no-scope");
    let output = fixture.up(&["up"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("cannot resolve the scope"), "{message}");
    assert!(message.contains("MSBX_SCOPE"), "{message}");
    assert!(fixture.log().is_empty());
    assert!(!fixture.state_path().exists());
}

#[test]
fn the_retired_epic_spellings_name_their_replacements() {
    let fixture = Fixture::new("legacy");
    let flag = fixture.up(&["up", "--epic", "mir-42"]);
    assert_eq!(flag.status.code(), Some(64), "{}", stderr(&flag));
    assert!(
        stderr(&flag).contains("`--epic` is retired"),
        "{}",
        stderr(&flag)
    );
    assert!(stderr(&flag).contains("`--scope`"), "{}", stderr(&flag));

    let env = fixture.run_at(&fixture.checkout, &["up"], &[("MSBX_EPIC", "mir-42")]);
    assert!(!env.status.success());
    assert!(
        stderr(&env).contains("MSBX_EPIC is retired"),
        "{}",
        stderr(&env)
    );
    assert!(stderr(&env).contains("MSBX_SCOPE"), "{}", stderr(&env));

    fixture.write_manifest(ENV_MANIFEST);
    fixture.write_store_env(&format!(
        "DATABASE_URL={{{{epic}}}}\nSTRIPE_KEY={STRIPE_KEY}\n"
    ));
    fixture.write_store_secret("SMTP_PASSWORD", &format!("{SMTP_PASSWORD}\n"));
    let token = fixture.up(&["up", "--scope", "mir-42"]);
    assert!(!token.status.success());
    assert!(
        stderr(&token).contains("`{{epic}}` is retired"),
        "{}",
        stderr(&token)
    );
    assert!(stderr(&token).contains("`{{scope}}`"), "{}", stderr(&token));
    assert!(fixture.log().is_empty(), "no container command was run");
    assert!(!fixture.state_path().exists(), "no block was allocated");
}

#[test]
fn reserved_names_are_refused_at_up_naming_name_layer_and_role() {
    let fixture = Fixture::new("reserved");
    fixture.write_manifest(
        "x-msbx:\n  project: signals\n  env:\n    client: [PATH]\nservices:\n  client:\n    image: alpine:3.20\n",
    );
    let output = fixture.up(&["up", "--scope", "mir-42"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("`PATH`"), "{message}");
    assert!(message.contains("layer `project`"), "{message}");
    assert!(message.contains("role `client`"), "{message}");
    assert!(fixture.log().is_empty());
    assert!(!fixture.state_path().exists());

    let harness = Fixture::new("reserved-harness");
    harness.write_harness_env("MULTICA_TOKEN=secret\n");
    let output = harness.up(&["up", "--scope", "mir-42"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("`MULTICA_TOKEN`"), "{message}");
    assert!(message.contains("layer `harness`"), "{message}");
    assert!(message.contains("role `harness`"), "{message}");
    assert!(harness.log().is_empty());
    assert!(!harness.state_path().exists());
}

#[test]
fn an_invalid_manifest_is_refused_before_docker_runs() {
    let fixture = Fixture::new("invalid");
    fixture.write_manifest(
        "x-msbx:\n  project: signals\n  publish:\n    db: 5432\nservices:\n  pg: {}\n",
    );
    let output = fixture.up(&["up", "--scope", "mir-42"]);
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
    let output = fixture.run_at(&outside, &["up", "--scope", "mir-42"], &[]);
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
    let output = fixture.run_at(&projects, &["up", "--scope", "mir-42"], &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("off limits"),
        "{}",
        stderr(&output)
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
            "--scope",
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

#[test]
fn a_missing_store_value_stops_up_before_a_block_or_a_container() {
    let fixture = Fixture::new("env-missing");
    fixture.write_manifest(ENV_MANIFEST);

    let output = fixture.up(&["up", "--scope", "MIR-42"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("STRIPE_KEY"), "{message}");
    assert!(message.contains("x-msbx.env.client"), "{message}");
    assert!(fixture.log().is_empty(), "no container command ran");
    assert!(!fixture.state_path().exists(), "no block was allocated");

    fixture.write_store_env(&format!(
        "DATABASE_URL={DATABASE_URL}\nSTRIPE_KEY={STRIPE_KEY}\n"
    ));
    fixture.write_store_secret("SMTP_PASSWORD", &format!("{SMTP_PASSWORD}\n"));
    assert_ok(&fixture.up(&["up", "--scope", "MIR-42"]));
}

#[test]
fn unknown_or_unpublished_tokens_stop_up() {
    let fixture = Fixture::new("env-tokens");
    fixture.write_manifest(ENV_MANIFEST);
    fixture.write_store_secret("SMTP_PASSWORD", &format!("{SMTP_PASSWORD}\n"));

    for (value, expected) in [
        ("{{port:ghost}}", "names no service or tunnel"),
        ("{{host:ghost}}", "names no service or tunnel"),
        ("{{port:client}}", "no host port"),
        ("{{nope}}", "token is not known"),
    ] {
        fixture.clear_log();
        fixture.write_store_env(&format!(
            "DATABASE_URL=http://{value}\nSTRIPE_KEY={STRIPE_KEY}\n"
        ));
        let output = fixture.up(&["up", "--scope", "MIR-42"]);
        assert!(!output.status.success(), "{value} was accepted");
        let message = stderr(&output);
        assert!(message.contains(expected), "{value}: {message}");
        assert!(fixture.log().is_empty(), "{value} started docker");
    }
}

#[test]
fn a_tunnelled_role_gets_a_generated_sidecar_and_no_host_port() {
    let fixture = Fixture::new("tunnel-ssh");
    fixture.write_manifest(TUNNEL_MANIFEST);
    fixture.write_store_env("MSBX_TUNNEL_SSH_USER=tunnel\n");
    fixture.write_store_secret("MSBX_TUNNEL_SSH_KEY", &format!("{TUNNEL_SSH_KEY}\n"));

    let output = fixture.up(&["up", "--scope", "MIR-42"]);
    assert_ok(&output);

    let doc = fixture.override_doc("mir-42");
    let pg = &doc.services["pg"];
    assert_eq!(pg.container_name, "signals-mir-42-pg");
    assert_eq!(pg.image, "alpine:3.20");
    assert_eq!(pg.command.len(), 3, "sh -c <script>: {:?}", pg.command);
    let script = &pg.command[2];
    assert!(
        script.contains("-L \"0.0.0.0:5432:db.internal.example.com:5432\""),
        "the sidecar listens on the role's port: {script}"
    );
    assert!(
        script.contains("$${MSBX_TUNNEL_SSH_USER"),
        "Compose must not interpolate the script's variables: {script}"
    );
    assert!(
        script.contains("ExitOnForwardFailure=yes") && script.contains("ConnectTimeout=10"),
        "an unreachable endpoint fails instead of hanging: {script}"
    );
    assert!(
        pg.ports.is_empty(),
        "an unpublished tunnel has no host port: {:?}",
        pg.ports
    );
    assert_eq!(
        pg.networks["default"].aliases,
        vec!["pg.mir-42.signals.msbx.test"]
    );
    assert_eq!(pg.labels["com.msbx.tunnel"], "ssh");
    let healthcheck = pg.healthcheck.as_ref().unwrap();
    assert_eq!(healthcheck.test, vec!["CMD-SHELL", "nc -z 127.0.0.1 5432"]);
    assert!(
        pg.env_file[0].ends_with("/env/pg.env"),
        "the tunnel role's env file is injected: {:?}",
        pg.env_file
    );
    assert_eq!(
        pg.environment["MSBX_TUNNEL_SSH_KEY_FILE"],
        "/run/msbx/secrets/MSBX_TUNNEL_SSH_KEY"
    );
    assert!(pg.volumes[0]
        .ends_with("/secrets/MSBX_TUNNEL_SSH_KEY:/run/msbx/secrets/MSBX_TUNNEL_SSH_KEY:ro"));
    assert!(
        !doc.services["client"]
            .labels
            .contains_key("com.msbx.tunnel"),
        "a local service is not labelled as a tunnel"
    );

    let entry = fixture.entry("signals-mir-42");
    assert_eq!(entry["tunnels"][0]["role"].as_str(), Some("pg"));
    assert_eq!(entry["tunnels"][0]["via"].as_str(), Some("ssh"));
    assert_eq!(
        entry["tunnels"][0]["upstream"].as_str(),
        Some("db.internal.example.com:5432")
    );

    let printed = stdout(&output);
    assert!(printed.contains("tunnel pg"), "{printed}");
    assert!(printed.contains("no host port"), "{printed}");
    assert!(printed.contains("pg.mir-42.signals.msbx.test"), "{printed}");

    fs::write(
        &fixture.ps,
        "{\"Service\":\"pg\",\"Name\":\"signals-mir-42-pg\",\"State\":\"running\",\"Health\":\"healthy\",\"ExitCode\":0,\"Publishers\":[]}\n",
    )
    .unwrap();
    let status = fixture.up(&["status", "--scope", "mir-42", "--json"]);
    assert_ok(&status);
    let report: Value = serde_json::from_str(&stdout(&status)).unwrap();
    let service = &report["boxes"][0]["services"][0];
    assert_eq!(service["role"].as_str(), Some("pg"));
    assert_eq!(service["tunnel"]["via"].as_str(), Some("ssh"));
    assert_eq!(
        service["tunnel"]["upstream"].as_str(),
        Some("db.internal.example.com:5432")
    );

    let human = fixture.up(&["status", "--scope", "mir-42"]);
    assert_ok(&human);
    assert!(
        stdout(&human).contains("tunnel:ssh"),
        "a tunnelled role is listed distinctly: {}",
        stdout(&human)
    );
}

#[test]
fn a_cloudflared_tunnel_is_selectable() {
    let fixture = Fixture::new("tunnel-cloudflared");
    fixture.write_manifest(CLOUDFLARED_MANIFEST);
    let output = fixture.up(&["up", "--scope", "MIR-42"]);
    assert_ok(&output);

    let doc = fixture.override_doc("mir-42");
    let pg = &doc.services["pg"];
    assert_eq!(pg.image, "cloudflare/cloudflared:2026.11.1");
    assert_eq!(
        pg.command,
        vec![
            "access".to_string(),
            "tcp".to_string(),
            "--hostname".to_string(),
            "db.internal.example.com".to_string(),
            "--url".to_string(),
            "0.0.0.0:5432".to_string(),
        ]
    );
    assert!(pg.ports.is_empty(), "{:?}", pg.ports);
    assert_eq!(
        pg.networks["default"].aliases,
        vec!["pg.mir-42.signals.msbx.test"]
    );
    assert_eq!(pg.labels["com.msbx.tunnel"], "cloudflared");

    let human = fixture.up(&["status", "--scope", "mir-42"]);
    assert_ok(&human);
    assert!(
        stdout(&human).contains("tunnel:cloudflared"),
        "{}",
        stdout(&human)
    );
}

#[test]
fn a_published_tunnel_gets_a_loopback_port_from_the_block() {
    let fixture = Fixture::new("tunnel-published");
    fixture.write_manifest(
        "x-msbx:\n  project: signals\n  publish:\n    pg: 5432\n  tunnels:\n    pg:\n      host: db.internal.example.com\n      port: 5432\n      via: ssh\nservices:\n  client: {}\n",
    );
    let output = fixture.up(&["up", "--scope", "MIR-42"]);
    assert_ok(&output);
    let start = fixture.block_start("signals-mir-42");
    let doc = fixture.override_doc("mir-42");
    assert_eq!(
        doc.services["pg"].ports,
        vec![format!("127.0.0.1:{start}:5432")]
    );
    let printed = stdout(&output);
    assert!(
        printed.contains(&format!("127.0.0.1:{start} → 5432")),
        "{printed}"
    );
}

#[test]
fn a_tunnel_published_at_another_port_is_refused_before_docker() {
    let fixture = Fixture::new("tunnel-port-mismatch");
    fixture.write_manifest(
        "x-msbx:\n  project: signals\n  publish:\n    pg: 6432\n  tunnels:\n    pg:\n      host: db.internal.example.com\n      port: 5432\n      via: ssh\nservices:\n  client: {}\n",
    );
    let output = fixture.up(&["up", "--scope", "MIR-42"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("x-msbx.publish.pg"), "{message}");
    assert!(message.contains("x-msbx.tunnels.pg.port"), "{message}");
    assert!(fixture.log().is_empty(), "no container command was run");
    assert!(!fixture.state_path().exists(), "no block was allocated");
}
