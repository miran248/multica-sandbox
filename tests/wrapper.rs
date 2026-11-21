//! `msbx-run`, exercised end to end through the shim binary with a recording
//! `docker` stand-in, a fake `multica` issue tree and a fixture image pin. The
//! real `msbx` binary renders the envelope and starts the box, so the test
//! covers the launch the daemon performs without a Docker daemon.

#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const MANIFEST: &str = r#"x-msbx:
  project: signals
  publish:
    pg: 5432
services:
  pg:
    image: alpine:3.20
  client:
    image: alpine:3.20
"#;

const ENV_MANIFEST: &str = r#"x-msbx:
  project: signals
  publish:
    pg: 5432
  env:
    pg: [DATABASE_URL]
services:
  pg:
    image: alpine:3.20
  client:
    image: alpine:3.20
"#;

const DATABASE_URL: &str = "postgres://{{host:pg}}:{{port:pg}}/{{scope}}";
const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PI_VERSION: &str = "0.52.0";

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    checkout: PathBuf,
    bin: PathBuf,
    state_home: PathBuf,
    config_home: PathBuf,
    task_id: String,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("wrapper-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let checkout = home.join("work").join("signals");
        let bin = root.join("bin");
        fs::create_dir_all(checkout.join(".multica")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let fixture = Fixture {
            state_home: home.join(".local").join("state"),
            config_home: home.join(".config"),
            root,
            home,
            checkout,
            bin,
            task_id: format!("01a1fe4b-{:08x}", std::process::id()),
        };
        fixture.write_fake_docker();
        fixture.write_fake_multica();
        fixture.write_pin(PI_VERSION);
        fixture
    }

    fn with_manifest(self, text: &str) -> Fixture {
        fs::write(self.checkout.join(".multica").join("compose.yaml"), text).unwrap();
        self
    }

    fn without_manifest(self) -> Fixture {
        let _ = fs::remove_file(self.checkout.join(".multica").join("compose.yaml"));
        self
    }

    fn write_store_env(&self, body: &str) {
        let dir = self
            .config_home
            .join("msbx")
            .join("projects")
            .join("signals");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("env");
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn write_harness_store(&self, body: &str) {
        let dir = self.config_home.join("msbx").join("harness");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("env");
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn write_pin(&self, pi: &str) {
        let dir = self.state_home.join("msbx");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("image.json"),
            format!("{{\"image\":\"msbx-pi:{pi}-1\",\"digest\":\"{DIGEST}\",\"pi\":\"{pi}\"}}"),
        )
        .unwrap();
    }

    fn without_pin(&self) {
        let _ = fs::remove_file(self.state_home.join("msbx").join("image.json"));
    }

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write_fake_docker(&self) {
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
case "$1" in
  ps)
    if [ -f '{orphan_id}' ]; then cat '{orphan_id}'; fi
    exit 0
    ;;
  inspect)
    if [ "$3" = '{{{{.Name}}}}' ]; then
      if [ -f '{orphan_name}' ]; then cat '{orphan_name}'; else echo "$4"; fi
    else
      if [ -f '{orphan_lease}' ]; then cat '{orphan_lease}'; fi
    fi
    exit 0
    ;;
esac
if [ "$1" = run ]; then
  case "$*" in
    *" --entrypoint /bin/sh "*)
      src=''
      for arg in "$@"; do
        case "$arg" in
          *:/seed:rw) src="${{arg%:/seed:rw}}" ;;
        esac
      done
      if [ -n "$src" ]; then
        printf 'baseline\n' > "$src/settings.json"
        mkdir -p "$src/skills" "$src/roles/implementer"
        printf 'overlay\n' > "$src/roles/implementer/AGENTS.md"
        printf 'overlay\n' > "$src/AGENTS.md"
      fi
      exit "${{SEED_EXIT:-0}}"
      ;;
  esac
  case "$*" in
    *--list-models*) printf 'model-a\nmodel-b\n' ;;
    *--probe*) printf 'probe ok\n' ;;
  esac
  case "$*" in
    *" --name "*)
      if [ -f '{run_sleep}' ]; then exec sleep 30; fi
      ;;
  esac
  if [ -n "${{FAKE_READ_STDIN:-}}" ]; then IFS= read -r line; printf 'STDIN %s\n' "$line"; fi
  exit "${{FAKE_EXIT:-0}}"
fi
if [ "$1" = rm ]; then exit 0; fi
exit 0
"#,
            log = self.root.join("docker.log").display(),
            orphan_id = self.root.join("orphan-id").display(),
            orphan_name = self.root.join("orphan-name").display(),
            orphan_lease = self.root.join("orphan-lease").display(),
            run_sleep = self.root.join("run_sleep").display(),
        );
        Self::write(&self.bin.join("docker"), &script);
    }

    fn write_fake_multica(&self) {
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
if [ "$1" = issue ] && [ "$2" = get ]; then
  if [ "$3" = issue-uuid-261 ]; then
    printf '%s\n' '{{"identifier":"MIR-261","status_category":"in_progress","archived_at":null}}'
  else
    printf '%s\n' '{{"identifier":"MIR-42","status_category":"in_progress","archived_at":null}}'
  fi
  exit 0
fi
echo "multica: cannot resolve $*" >&2
exit 1
"#,
            log = self.root.join("multica.log").display(),
        );
        Self::write(&self.bin.join("multica"), &script);
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

    fn sessions(&self) -> PathBuf {
        let dir = self.root.join("sessions");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config_root(&self) -> PathBuf {
        let dir = self.root.join("multica-config");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = self.command(args, envs);
        command.stdin(Stdio::null()).output().unwrap()
    }

    fn run_with_stdin(&self, args: &[&str], envs: &[(&str, &str)], stdin: &str) -> Output {
        let mut command = self.command(args, envs);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn command(&self, args: &[&str], envs: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_msbx-run"));
        command
            .args(args)
            .current_dir(&self.checkout)
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
            .env("MSBX_BIN", env!("CARGO_BIN_EXE_msbx"))
            .env("AMBIENT_SECRET", "must-not-reach-the-container")
            .env_remove("MSBX_SCOPE")
            .env_remove("MSBX_EPIC")
            .env_remove("MSBX_PROJECT")
            .env_remove("MSBX_RUN_SOFT")
            .env_remove("FAKE_EXIT")
            .env_remove("SEED_EXIT")
            .env_remove("FAKE_READ_STDIN");
        // The runtime that runs this test injects its own MULTICA_* names;
        // remove them so the assertions describe only what this fixture sets.
        for (name, _) in std::env::vars() {
            if name.starts_with("MULTICA_") {
                command.env_remove(&name);
            }
        }
        command
            .env("MULTICA_CLI_PATH", self.bin.join("multica"))
            .env("MULTICA_TASK_ID", &self.task_id)
            .env("MULTICA_AGENT_ID", "agent-1")
            .env("MULTICA_AGENT_NAME", "implementer")
            .env("MULTICA_TOKEN", "task-token")
            .env("MULTICA_DAEMON_PORT", "19681")
            .env("MULTICA_TASK_CONFIG_ROOT", self.config_root());
        for (name, value) in envs {
            command.env(name, value);
        }
        command
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join("docker.log")).unwrap_or_default()
    }

    fn multica_log(&self) -> String {
        fs::read_to_string(self.root.join("multica.log")).unwrap_or_default()
    }

    fn hold_run(&self) {
        fs::write(self.root.join("run_sleep"), "").unwrap();
    }

    fn clear_log(&self) {
        let _ = fs::remove_file(self.root.join("docker.log"));
        let _ = fs::remove_file(self.root.join("multica.log"));
    }

    fn msbx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_msbx"))
            .args(args)
            .current_dir(&self.checkout)
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
            .output()
            .unwrap()
    }

    fn state(&self) -> Value {
        let path = self.state_home.join("msbx").join("state.json");
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn lease_dir(&self) -> PathBuf {
        self.state_home
            .join("msbx")
            .join("leases")
            .join("signals")
            .join("mir-42")
    }

    fn write_lease(&self, task: &str, pid: u32, started_at: u64) {
        let dir = self.lease_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{task}.json")),
            format!(
                "{{\"task\":\"{task}\",\"pid\":{pid},\"started_at\":{started_at},\"container\":\"signals-mir-42-harness-{task}\"}}"
            ),
        )
        .unwrap();
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
        "msbx-run failed: {}\n{}",
        stdout(output),
        stderr(output)
    );
}

/// The `docker run` lines, with the seed invocation removed.
fn container_runs(log: &str) -> Vec<Vec<String>> {
    log.lines()
        .filter(|line| line.starts_with("run "))
        .filter(|line| !line.contains(" --entrypoint /bin/sh "))
        .map(|line| line.split_whitespace().map(str::to_string).collect())
        .collect()
}

fn seed_runs(log: &str) -> Vec<String> {
    log.lines()
        .filter(|line| line.starts_with("run ") && line.contains(" --entrypoint /bin/sh "))
        .map(str::to_string)
        .collect()
}

fn args_after_image<'a>(args: &'a [String], digest: &str) -> &'a [String] {
    let image = args
        .iter()
        .position(|arg| arg.contains(digest))
        .expect("the image reference is in argv");
    &args[image + 1..]
}

fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn host_identity() -> String {
    let uid = Command::new("id").arg("-u").output().unwrap();
    let gid = Command::new("id").arg("-g").output().unwrap();
    format!(
        "{}:{}",
        String::from_utf8_lossy(&uid.stdout).trim(),
        String::from_utf8_lossy(&gid.stdout).trim()
    )
}

#[test]
fn a_task_runs_pi_in_one_container_with_exactly_four_mounts() {
    let fixture = Fixture::new("mounts").with_manifest(ENV_MANIFEST);
    fixture.write_store_env(&format!("DATABASE_URL={DATABASE_URL}\n"));
    let sessions = fixture.sessions();
    let session = sessions.join("task.jsonl");
    let config = fixture.config_root();
    let output = fixture.run(
        &[
            "--model",
            "flash",
            "--session",
            session.to_str().unwrap(),
            "--thinking",
            "high",
        ],
        &[("MSBX_SCOPE", "mir-42")],
    );
    assert_ok(&output);

    let log = fixture.log();
    let runs = container_runs(&log);
    assert_eq!(runs.len(), 1, "one task container:\n{log}");
    let args = &runs[0];
    let task8 = &fixture.task_id[..8];
    assert_eq!(
        value_after(args, "--name"),
        Some(format!("signals-mir-42-harness-{task8}").as_str())
    );
    assert_eq!(
        value_after(args, "--workdir"),
        Some(fixture.checkout.to_str().unwrap())
    );
    assert_eq!(value_after(args, "--user"), Some(host_identity().as_str()));
    assert_eq!(value_after(args, "--network"), Some("signals-mir-42"));
    assert!(args.iter().any(|arg| arg == "--init"));
    assert!(args.iter().any(|arg| arg == "--rm"));

    let mounts: Vec<&str> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == "--volume")
        .map(|(index, _)| args[index + 1].as_str())
        .collect();
    assert_eq!(mounts.len(), 4, "exactly four mounts: {mounts:?}");
    let checkout = fixture.checkout.to_str().unwrap();
    assert!(mounts.contains(&format!("{checkout}:{checkout}:rw").as_str()));
    let config = config.display();
    assert!(mounts.contains(&format!("{config}:{config}:rw").as_str()));
    assert!(mounts.contains(&format!("{}:{}:rw", sessions.display(), sessions.display()).as_str()));
    let agent = fixture
        .state_home
        .join("msbx")
        .join("agents")
        .join("agent-1")
        .join("pi");
    assert!(mounts.contains(&format!("{}:/home/agent/.pi:rw", agent.display()).as_str()));
    assert!(
        !mounts
            .iter()
            .any(|mount| mount.contains(".ssh") || mount.contains("docker.sock")),
        "{mounts:?}"
    );

    let harness = state_dir_path(&fixture, &["signals", "mir-42", "env", "harness.env"]);
    assert_eq!(
        value_after(args, "--env-file"),
        Some(harness.to_str().unwrap())
    );
    let env_file = fs::read_to_string(&harness).unwrap();
    assert!(
        env_file.contains("MSBX_PG_HOST=pg.mir-42.signals.msbx.test"),
        "{env_file}"
    );
    assert!(
        env_file.contains("MSBX_PG_PORT=5432"),
        "container port: {env_file}"
    );
    assert!(env_file.contains("DATABASE_URL=postgres://pg.mir-42.signals.msbx.test:5432/mir-42"));

    assert!(
        args.iter()
            .any(|arg| arg == &format!("msbx-pi:{PI_VERSION}-1@{DIGEST}")),
        "the image is named by digest: {args:?}"
    );
    assert_eq!(
        args_after_image(args, DIGEST),
        &[
            "--model",
            "flash",
            "--session",
            session.to_str().unwrap(),
            "--thinking",
            "high"
        ]
    );

    // The ambient host environment is not forwarded. The token is passed by
    // name only, so its value never enters argv.
    assert!(!log.contains("AMBIENT_SECRET"), "{log}");
    assert!(
        !log.contains("task-token"),
        "a token value reached argv: {log}"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--env" && pair[1] == "MULTICA_TOKEN"),
        "{args:?}"
    );
    assert!(
        args.windows(2).any(|pair| {
            pair[0] == "--env" && pair[1] == "MULTICA_CLI_PATH=/usr/local/bin/multica"
        }),
        "{args:?}"
    );
    assert!(
        !args
            .windows(2)
            .any(|pair| pair[0] == "--env" && pair[1] == "HOME=/root"),
        "{args:?}"
    );
    assert!(args
        .windows(2)
        .any(|pair| { pair[0] == "--label" && pair[1] == "com.msbx.harness=true" }));
    assert!(args
        .windows(2)
        .any(|pair| { pair[0] == "--label" && pair[1].starts_with("com.msbx.lease=") }));
}

fn state_dir_path(fixture: &Fixture, parts: &[&str]) -> PathBuf {
    let mut path = fixture.state_home.join("msbx");
    for part in parts {
        path.push(part);
    }
    path
}

#[test]
fn a_manifestless_checkout_still_runs_in_the_container_on_the_default_bridge() {
    let fixture = Fixture::new("manifestless").without_manifest();
    let output = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert_ok(&output);
    let log = fixture.log();
    let runs = container_runs(&log);
    assert_eq!(runs.len(), 1, "{log}");
    let args = &runs[0];
    assert_eq!(
        value_after(args, "--network"),
        None,
        "default bridge: {args:?}"
    );
    assert_eq!(
        value_after(args, "--env-file"),
        None,
        "no envelope: {args:?}"
    );
    assert!(
        !fixture.state_home.join("msbx").join("state.json").exists(),
        "no box was recorded"
    );
    assert!(!log.contains("compose"), "no box was started:\n{log}");
}

#[test]
fn the_container_exit_status_is_the_launch_status() {
    let fixture = Fixture::new("status").with_manifest(MANIFEST);
    let output = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42"), ("FAKE_EXIT", "7")]);
    assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
}

#[test]
fn stdin_stdout_and_the_container_exit_status_pass_through() {
    let fixture = Fixture::new("stdin").with_manifest(MANIFEST);
    let output = fixture.run_with_stdin(
        &["--mode", "json"],
        &[
            ("MSBX_SCOPE", "mir-42"),
            ("FAKE_READ_STDIN", "1"),
            ("FAKE_EXIT", "3"),
        ],
        "the prompt\n",
    );
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("STDIN the prompt"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn the_agent_dir_is_seeded_once_from_the_baseline_and_the_role_overlay() {
    let fixture = Fixture::new("seed").with_manifest(MANIFEST);
    let agent = fixture
        .state_home
        .join("msbx")
        .join("agents")
        .join("agent-1")
        .join("pi");

    let first = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert_ok(&first);
    assert_eq!(seed_runs(&fixture.log()).len(), 1, "{}", fixture.log());
    assert!(agent.join("settings.json").is_file());
    assert!(agent
        .join("roles")
        .join("implementer")
        .join("AGENTS.md")
        .is_file());
    let marker = fs::read_to_string(agent.join(".msbx-seed")).unwrap();
    assert!(marker.contains(DIGEST), "{marker}");
    assert!(marker.contains("implementer"), "{marker}");

    fixture.clear_log();
    let second = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert_ok(&second);
    assert!(
        seed_runs(&fixture.log()).is_empty(),
        "a second run does not re-seed: {}",
        fixture.log()
    );
}

#[test]
fn a_failed_seed_leaves_no_agent_dir_and_no_lease() {
    let fixture = Fixture::new("seed-fail").with_manifest(MANIFEST);
    let output = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42"), ("SEED_EXIT", "1")]);
    assert!(!output.status.success());
    let agent = fixture
        .state_home
        .join("msbx")
        .join("agents")
        .join("agent-1")
        .join("pi");
    assert!(!agent.exists(), "a failed seed is not committed");
    assert!(!fixture.lease_dir().exists(), "no lease was taken");
    assert!(
        stderr(&output).contains("cannot seed the agent dir"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_harness_envelope_is_a_file_and_never_argv() {
    let fixture = Fixture::new("envelope").with_manifest(ENV_MANIFEST);
    fixture.write_store_env(&format!("DATABASE_URL={DATABASE_URL}\n"));
    let output = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert_ok(&output);
    let log = fixture.log();
    assert!(
        !log.contains("DATABASE_URL=postgres://"),
        "a value reached argv: {log}"
    );
    assert!(log.contains("--env-file"), "{log}");
    let harness = state_dir_path(&fixture, &["signals", "mir-42", "env", "harness.env"]);
    let body = fs::read_to_string(&harness).unwrap();
    assert!(body.contains("DATABASE_URL="), "{body}");
}

#[test]
fn the_last_lease_stops_the_box_and_a_sibling_lease_keeps_it_up() {
    let fixture = Fixture::new("last-lease").with_manifest(MANIFEST);
    let first = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert_ok(&first);
    assert_eq!(
        fixture.state()["blocks"]["signals-mir-42"]["lease_managed"].as_bool(),
        Some(true),
        "up --lease marks the box lease-managed"
    );
    let log = fixture.log();
    assert!(
        log.contains("-p signals-mir-42 stop"),
        "the last lease stops the box:\n{log}"
    );
    assert!(
        !fixture.lease_dir().exists()
            || fs::read_dir(fixture.lease_dir()).unwrap().next().is_none()
    );

    // A sibling run holds a live lease. This run must not stop the box.
    fixture.write_lease("sibling", std::process::id(), now());
    fixture.clear_log();
    let second = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert_ok(&second);
    assert!(
        !fixture.log().contains("-p signals-mir-42 stop"),
        "a live sibling keeps the box up:\n{}",
        fixture.log()
    );
    assert!(fixture.lease_dir().join("sibling.json").is_file());
}

#[test]
fn a_stale_lease_is_dropped_by_up_and_by_gc() {
    let fixture = Fixture::new("stale").with_manifest(MANIFEST);
    fixture.write_lease("dead", 4_200_000, now());
    let output = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert_ok(&output);
    assert!(
        !fixture.lease_dir().join("dead.json").exists(),
        "up drops a stale lease"
    );

    fixture.write_lease("dead-again", 4_200_000, now());
    fixture.clear_log();
    let gc = fixture.msbx(&["gc", "--dry-run"]);
    assert!(gc.status.success(), "{}", stderr(&gc));
    assert!(
        stdout(&gc).contains("would drop 1 stale lease(s)"),
        "{}",
        stdout(&gc)
    );
    assert!(
        fixture.lease_dir().join("dead-again.json").is_file(),
        "a dry run drops nothing"
    );
}

#[test]
fn gc_drops_a_stale_lease_for_a_scope_with_no_box() {
    let fixture = Fixture::new("ghost-lease");
    let dir = fixture
        .state_home
        .join("msbx")
        .join("leases")
        .join("ghost")
        .join("mir-99");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("dead.json"),
        format!(
            "{{\"task\":\"dead\",\"pid\":4200000,\"started_at\":{},\"container\":\"ghost-mir-99-harness-dead\"}}\n",
            now()
        ),
    )
    .unwrap();

    let dry = fixture.msbx(&["gc", "--dry-run"]);
    assert!(dry.status.success(), "{}", stderr(&dry));
    assert!(
        stdout(&dry).contains("would drop 1 stale lease(s): ghost/mir-99/dead"),
        "{}",
        stdout(&dry)
    );
    assert!(dir.join("dead.json").is_file(), "a dry run drops nothing");

    let swept = fixture.msbx(&["gc"]);
    assert!(swept.status.success(), "{}", stderr(&swept));
    assert!(
        stdout(&swept).contains("dropped 1 stale lease(s): ghost/mir-99/dead"),
        "{}",
        stdout(&swept)
    );
    assert!(
        !dir.join("dead.json").exists(),
        "the manifest-less lease is gone"
    );
}

#[test]
fn a_killed_shims_orphan_container_is_swept() {
    let fixture = Fixture::new("orphan").with_manifest(MANIFEST);
    let first = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert_ok(&first);

    let lease = fixture.lease_dir().join("dead-again.json");
    fixture.write_lease("dead-again", 4_200_000, now());
    fs::write(fixture.root.join("orphan-id"), "container-id\n").unwrap();
    fs::write(
        fixture.root.join("orphan-name"),
        "signals-mir-42-harness-dead\n",
    )
    .unwrap();
    fs::write(
        fixture.root.join("orphan-lease"),
        format!("{}\n", lease.display()),
    )
    .unwrap();
    fixture.clear_log();

    let gc = fixture.msbx(&["gc"]);
    assert!(gc.status.success(), "{}", stderr(&gc));
    assert!(
        fixture.log().contains("rm -f container-id"),
        "the orphan container is removed:\n{}",
        fixture.log()
    );
    assert!(
        stdout(&gc).contains("removed orphan harness container signals-mir-42-harness-dead"),
        "{}\n{}",
        stdout(&gc),
        stderr(&gc)
    );
}

#[test]
fn version_answers_from_the_pin_without_docker() {
    let fixture = Fixture::new("version").with_manifest(MANIFEST);
    let output = fixture.run(&["--version"], &[]);
    assert_ok(&output);
    assert_eq!(stdout(&output).trim(), PI_VERSION);
    assert_eq!(fixture.log(), "", "no Docker call");
}

#[test]
fn a_missing_pin_is_a_loud_failure_with_the_fix() {
    let fixture = Fixture::new("missing-pin").with_manifest(MANIFEST);
    fixture.without_pin();
    let output = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("msbx image pin"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fixture.log(), "");
}

#[test]
fn probes_run_in_a_throwaway_container_and_never_resolve_a_scope() {
    let fixture = Fixture::new("probe").without_manifest();
    fixture.write_harness_store("PI_API_KEY=probe-key\n");
    let output = fixture.run(&["--probe"], &[]);
    assert_ok(&output);
    assert!(stdout(&output).contains("probe ok"), "{}", stdout(&output));
    let log = fixture.log();
    let runs = container_runs(&log);
    assert_eq!(runs.len(), 1, "{log}");
    let args = &runs[0];
    assert!(!log.contains("--volume"), "a probe mounts nothing:\n{log}");
    assert_eq!(value_after(args, "--network"), None);
    assert_eq!(value_after(args, "--name"), None);
    assert_eq!(args_after_image(args, DIGEST), &["--probe"]);
    assert_eq!(
        value_after(args, "--env-file"),
        Some(
            fixture
                .config_home
                .join("msbx")
                .join("harness")
                .join("env")
                .to_str()
                .unwrap()
        ),
        "a probe reads the harness store"
    );
    assert!(
        !log.contains("probe-key"),
        "a store value never enters argv:\n{log}"
    );
    assert_eq!(
        fixture.multica_log(),
        "",
        "a probe does not resolve a scope"
    );

    fixture.clear_log();
    let rpc = fixture.run(
        &[
            "--mode",
            "rpc",
            "--no-session",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
        ],
        &[],
    );
    assert_ok(&rpc);
    assert_eq!(container_runs(&fixture.log()).len(), 1, "{}", fixture.log());
    assert!(!fixture.log().contains("--volume"));
}

#[test]
fn list_models_is_cached_by_digest() {
    let fixture = Fixture::new("models").without_manifest();
    let first = fixture.run(&["--list-models"], &[]);
    assert_ok(&first);
    assert!(stdout(&first).contains("model-a"), "{}", stdout(&first));
    assert_eq!(container_runs(&fixture.log()).len(), 1);

    fixture.clear_log();
    let second = fixture.run(&["--list-models"], &[]);
    assert_ok(&second);
    assert_eq!(stdout(&second), "model-a\nmodel-b\n");
    assert_eq!(fixture.log(), "", "the second call is cached");
    let cache = state_dir_path(
        &fixture,
        &[
            "cache",
            &format!("list-models-{}.txt", &DIGEST["sha256:".len()..]),
        ],
    );
    assert!(cache.is_file(), "{}", cache.display());
}

#[test]
fn a_resolution_failure_is_loud_and_starts_nothing() {
    let fixture = Fixture::new("no-scope").with_manifest(MANIFEST);
    let output = fixture.run(&[], &[]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("cannot resolve the scope"), "{message}");
    assert!(message.contains("MSBX_SCOPE"), "{message}");
    assert_eq!(fixture.log(), "");
}

#[test]
fn the_scope_comes_from_the_task_branch_when_no_variable_is_set() {
    let fixture = Fixture::new("branch").with_manifest(MANIFEST);
    fixture.branch("agent/implementer/mir-261-eebfc37c0c33");
    let output = fixture.run(&["task"], &[]);
    assert_ok(&output);
    let log = fixture.log();
    assert!(log.contains("--name signals-mir-261-harness-"), "{log}");
    assert!(
        !fixture.multica_log().contains("parent"),
        "no parent lookup"
    );
}

#[test]
fn a_missing_session_directory_is_a_loud_failure_before_the_container() {
    let fixture = Fixture::new("session").with_manifest(MANIFEST);
    let missing = fixture.root.join("gone").join("a.jsonl");
    let output = fixture.run(
        &["--session", missing.to_str().unwrap()],
        &[("MSBX_SCOPE", "mir-42")],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("session directory does not exist"),
        "{}",
        stderr(&output)
    );
    assert_eq!(container_runs(&fixture.log()).len(), 0);
}

#[test]
fn a_docker_failure_is_loud_and_has_no_host_harness_fallback() {
    let fixture = Fixture::new("docker-fail").with_manifest(MANIFEST);
    // A docker that fails every invocation fails the seed first; pre-create the
    // agent dir so the task container is the failing step.
    fs::create_dir_all(
        fixture
            .state_home
            .join("msbx")
            .join("agents")
            .join("agent-1")
            .join("pi"),
    )
    .unwrap();
    let script = "#!/bin/sh\nexit 1\n";
    Fixture::write(&fixture.bin.join("docker"), script);
    let output = fixture.run(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("`msbx up` failed"),
        "{}",
        stderr(&output)
    );
    assert!(!fixture.root.join("harness.log").exists());
}

#[test]
fn a_signalled_shim_releases_its_lease_and_removes_its_container() {
    let fixture = Fixture::new("signal").with_manifest(MANIFEST);
    fixture.hold_run();
    let mut command = fixture.command(&["task"], &[("MSBX_SCOPE", "mir-42")]);
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let mut child = command.spawn().unwrap();

    let lease = fixture
        .lease_dir()
        .join(format!("{}.json", fixture.task_id));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if !container_runs(&fixture.log()).is_empty() && lease.is_file() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        !container_runs(&fixture.log()).is_empty() && lease.is_file(),
        "the task container and lease started:\n{}",
        fixture.log()
    );

    let sent = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success(), "SIGTERM was delivered");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("the signalled shim did not exit:\n{}", fixture.log());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    assert_eq!(status.code(), Some(130), "the signalled shim exits 130");
    assert!(!lease.exists(), "the lease was released on the signal");
    let task8 = &fixture.task_id[..8];
    assert!(
        fixture
            .log()
            .contains(&format!("rm -f signals-mir-42-harness-{task8}")),
        "the handler removed the container:\n{}",
        fixture.log()
    );
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
