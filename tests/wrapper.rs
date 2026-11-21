//! `msbx-run`, exercised end to end through the wrapper binary with a recording
//! `docker` stand-in, a fake `multica` issue tree and a fake harness on `PATH`.
//! The real `msbx` binary renders the envelope and `msbx env export` feeds the
//! harness, so the test covers the launch the runtime performs.

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
    client: [DATABASE_URL]
services:
  pg:
    image: alpine:3.20
  client:
    image: alpine:3.20
"#;

const DATABASE_URL: &str = "postgres://{{host:pg}}:{{port:pg}}/{{epic}}";

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    checkout: PathBuf,
    bin: PathBuf,
    state_home: PathBuf,
    config_home: PathBuf,
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
        };
        fixture.write_fake_docker();
        fixture.write_fake_multica();
        fixture.write_fake_harness();
        fixture.branch("main");
        fixture
    }

    fn with_manifest(self, text: &str) -> Fixture {
        fs::write(self.checkout.join(".multica").join("compose.yaml"), text).unwrap();
        self
    }

    fn with_task_context(self) -> Fixture {
        let dir = self.home.join("work").join(".multica");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("daemon_task_context.json"),
            r#"{"managed_by":"multica-daemon-task","agent_id":"agent-1","issue_id":"issue-uuid-261"}"#,
        )
        .unwrap();
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

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write_fake_docker(&self) {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ -f '{}' ]; then echo 'docker: boom' >&2; exit 1; fi\nexit 0\n",
            self.root.join("docker.log").display(),
            self.root.join("docker-fail").display(),
        );
        Self::write(&self.bin.join("docker"), &script);
    }

    fn fail_docker(&self) {
        fs::write(self.root.join("docker-fail"), "").unwrap();
    }

    fn write_fake_multica(&self) {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'issue get mir-261 --output json') printf '%s\\n' '{{\"identifier\":\"MIR-261\",\"parent_issue_id\":\"parent-178\"}}' ;;\n  'issue get issue-uuid-261 --output json') printf '%s\\n' '{{\"identifier\":\"MIR-261\",\"parent_issue_id\":\"parent-178\"}}' ;;\n  'issue get parent-178 --output json') printf '%s\\n' '{{\"identifier\":\"MIR-178\",\"parent_issue_id\":null}}' ;;\n  'issue get mir-178 --output json') printf '%s\\n' '{{\"identifier\":\"MIR-178\",\"parent_issue_id\":null}}' ;;\n  *) echo \"multica: cannot resolve $*\" >&2; exit 1 ;;\nesac\nexit 0\n",
            self.root.join("multica.log").display(),
        );
        Self::write(&self.bin.join("multica"), &script);
    }

    fn break_multica(&self) {
        Self::write(
            &self.bin.join("multica"),
            "#!/bin/sh\necho 'multica: offline' >&2\nexit 1\n",
        );
    }

    fn write_fake_harness(&self) {
        let script = format!(
            concat!(
                "#!/bin/sh\n",
                "printf '%s\\n' \"$*\" >> '{}/harness.log'\n",
                "printf 'ARGS %s\\n' \"$*\"\n",
                "printf 'ENV DATABASE_URL=%s\\n' \"${{DATABASE_URL-<unset>}}\"\n",
                "printf 'ENV MSBX_PG_HOST=%s\\n' \"${{MSBX_PG_HOST-<unset>}}\"\n",
                "printf 'ENV MSBX_PG_PORT=%s\\n' \"${{MSBX_PG_PORT-<unset>}}\"\n",
                "if [ -n \"${{HARNESS_READ_STDIN-}}\" ]; then IFS= read -r line; printf 'STDIN %s\\n' \"$line\"; fi\n",
                "exit \"${{HARNESS_EXIT-0}}\"\n"
            ),
            self.root.display(),
        );
        Self::write(&self.bin.join("dsh"), &script);
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
            .env("MULTICA_CLI_PATH", self.bin.join("multica"))
            .env("MSBX_HARNESS", self.bin.join("dsh"))
            .env("MULTICA_TASK_ID", "task-abc")
            .env_remove("MSBX_EPIC")
            .env_remove("MSBX_PROJECT")
            .env_remove("MSBX_RUN_SOFT")
            .env_remove("HARNESS_EXIT")
            .env_remove("HARNESS_READ_STDIN");
        for (name, value) in envs {
            command.env(name, value);
        }
        command
    }

    fn log(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }

    fn docker_log(&self) -> String {
        self.log("docker.log")
    }

    fn multica_log(&self) -> String {
        self.log("multica.log")
    }

    fn harness_log(&self) -> String {
        self.log("harness.log")
    }

    fn state(&self) -> Value {
        let path = self.state_home.join("msbx").join("state.json");
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn cache_path(&self) -> PathBuf {
        self.state_home
            .join("msbx")
            .join(".wrapper")
            .join("task-abc.epic")
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

#[test]
fn passes_a_checkout_without_a_manifest_through_untouched() {
    let fixture = Fixture::new("pass-through").without_manifest();
    let output = fixture.run(&["model", "run"], &[]);
    assert_ok(&output);
    assert_eq!(fixture.docker_log(), "");
    assert_eq!(fixture.multica_log(), "");
    assert!(
        stdout(&output).contains("ARGS model run"),
        "{}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("ENV DATABASE_URL=<unset>"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn starts_the_box_and_exports_the_envelope_before_the_harness() {
    let fixture = Fixture::new("export").with_manifest(ENV_MANIFEST);
    fixture.write_store_env(&format!("DATABASE_URL={DATABASE_URL}\n"));
    let output = fixture.run(&["task"], &[("MSBX_EPIC", "mir-42")]);
    assert_ok(&output);

    let port = fixture.state()["blocks"]["signals-mir-42"]["start"]
        .as_u64()
        .unwrap();
    let docker = fixture.docker_log();
    assert!(docker.contains("up -d --wait"), "{docker}");
    assert!(docker.contains("-p signals-mir-42"), "{docker}");
    let out = stdout(&output);
    assert!(
        out.contains(&format!(
            "ENV DATABASE_URL=postgres://pg.mir-42.signals.msbx.test:{port}/mir-42"
        )),
        "{out}"
    );
    assert!(
        out.contains("ENV MSBX_PG_HOST=pg.mir-42.signals.msbx.test"),
        "{out}"
    );
    assert!(out.contains(&format!("ENV MSBX_PG_PORT={port}")), "{out}");
    assert!(
        !out.contains("up signals-mir-42"),
        "the up summary stayed out of the harness stdout: {out}"
    );
}

#[test]
fn resolves_the_epic_from_the_task_branch_through_the_parent_and_caches_it() {
    let fixture = Fixture::new("branch").with_manifest(MANIFEST);
    fixture.branch("agent/implementer/mir-261-eebfc37c0c33");
    let output = fixture.run(&[], &[]);
    assert_ok(&output);

    let docker = fixture.docker_log();
    assert!(docker.contains("-p signals-mir-178"), "{docker}");
    assert_eq!(
        fixture.multica_log(),
        "issue get mir-261 --output json\nissue get parent-178 --output json\n"
    );
    assert!(stdout(&output).contains("ENV MSBX_PG_HOST=pg.mir-178.signals.msbx.test"));
    assert_eq!(
        fs::read_to_string(fixture.cache_path()).unwrap().trim(),
        "mir-178"
    );

    // The second launch of the same task must not ask Multica again.
    fixture.break_multica();
    let output = fixture.run(&[], &[]);
    assert_ok(&output);
    assert!(stdout(&output).contains("MSBX_PG_HOST=pg.mir-178"));
}

#[test]
fn a_task_on_the_epic_itself_resolves_to_that_epic() {
    let fixture = Fixture::new("epic-task").with_manifest(MANIFEST);
    fixture.branch("agent/implementer/mir-178-spec");
    let output = fixture.run(&[], &[]);
    assert_ok(&output);
    assert!(fixture.docker_log().contains("-p signals-mir-178"));
}

#[test]
fn the_daemon_task_context_resolves_the_issue_behind_a_task_slot_branch() {
    let fixture = Fixture::new("task-context")
        .with_manifest(MANIFEST)
        .with_task_context();
    fixture.branch("agent/implementer/eebfc37c0c33");
    let output = fixture.run(&[], &[]);
    assert_ok(&output);
    assert!(fixture.docker_log().contains("-p signals-mir-178"));
    assert_eq!(
        fixture.multica_log(),
        "issue get issue-uuid-261 --output json\nissue get parent-178 --output json\n"
    );
}

#[test]
fn a_resolution_failure_is_loud_and_starts_nothing() {
    let fixture = Fixture::new("no-epic").with_manifest(MANIFEST);
    fixture.branch("agent/implementer/eebfc37c0c33");
    let output = fixture.run(&[], &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("set MSBX_EPIC"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fixture.harness_log(), "");
    assert_eq!(fixture.docker_log(), "");
}

#[test]
fn a_bad_msbx_epic_is_a_loud_error_not_a_default() {
    let fixture = Fixture::new("bad-epic").with_manifest(MANIFEST);
    let output = fixture.run(&[], &[("MSBX_EPIC", "Not An Epic")]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("MSBX_EPIC is not a valid epic"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fixture.harness_log(), "");
    assert_eq!(fixture.docker_log(), "");
}

#[test]
fn probe_and_list_models_never_start_a_box() {
    let fixture = Fixture::new("probe").with_manifest(ENV_MANIFEST);
    fixture.fail_docker();
    for flag in ["--probe", "--list-models"] {
        let output = fixture.run(&[flag], &[]);
        assert_ok(&output);
        assert!(
            stdout(&output).contains(&format!("ARGS {flag}")),
            "{}",
            stdout(&output)
        );
        assert!(stdout(&output).contains("ENV DATABASE_URL=<unset>"));
    }
    assert_eq!(fixture.docker_log(), "");
    assert_eq!(fixture.multica_log(), "");
}

#[test]
fn a_failed_up_fails_the_launch() {
    let fixture = Fixture::new("failed-up").with_manifest(MANIFEST);
    fixture.fail_docker();
    let output = fixture.run(&["task"], &[("MSBX_EPIC", "mir-42")]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("`msbx up` failed for epic `mir-42`"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fixture.harness_log(), "");
}

#[test]
fn soft_mode_prints_the_failure_and_passes_the_prompt_through() {
    let fixture = Fixture::new("soft").with_manifest(MANIFEST);
    fixture.fail_docker();
    let output = fixture.run(
        &["task"],
        &[
            ("MSBX_EPIC", "mir-42"),
            ("MSBX_RUN_SOFT", "1"),
            ("HARNESS_EXIT", "3"),
        ],
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr(&output).contains("`msbx up` failed"),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("MSBX_RUN_SOFT=1"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fixture.harness_log(), "task\n");
    assert!(stdout(&output).contains("ENV DATABASE_URL=<unset>"));
}

#[test]
fn forwards_the_original_arguments_stdin_and_exit_status() {
    let fixture = Fixture::new("forward").with_manifest(MANIFEST);
    let output = fixture.run_with_stdin(
        &["--model", "flash"],
        &[
            ("MSBX_EPIC", "mir-42"),
            ("HARNESS_EXIT", "7"),
            ("HARNESS_READ_STDIN", "1"),
        ],
        "the prompt\n",
    );
    assert_eq!(output.status.code(), Some(7));
    assert!(
        stdout(&output).contains("ARGS --model flash"),
        "{}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("STDIN the prompt"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn refuses_to_launch_itself_as_the_harness() {
    let fixture = Fixture::new("self").without_manifest();
    let output = fixture.run(&[], &[("MSBX_HARNESS", env!("CARGO_BIN_EXE_msbx-run"))]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("refusing to launch itself"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_missing_sandbox_manifest_value_is_a_loud_failure() {
    let fixture = Fixture::new("missing-value").with_manifest(ENV_MANIFEST);
    let output = fixture.run(&[], &[("MSBX_EPIC", "mir-42")]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("DATABASE_URL"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fixture.harness_log(), "");
}
