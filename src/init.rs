//! `msbx init` — set up the host, then prove the Docker driver against a
//! synthetic two-scope project.
//!
//! `init` is idempotent: it installs the `msbx.test` resolver and the `msbx dns`
//! unit, pulls a project through the real manifest reader and the port-block
//! allocator, starts two scopes that both run Postgres on 5432, and prints a
//! `PASS` / `FAIL` / `SKIP` table with what it observed. Any `FAIL` exits 1, any
//! `SKIP` exits 2, and both mean the run is not a pass.
//!
//! `--check` installs nothing; it measures and runs the checks against the
//! daemon it finds (starting an ephemeral one for the run if needed).
//! `--keep` leaves the containers and the generated project in place.

use crate::dns;
use crate::doctor::{MIN_COMPOSE, MIN_ENGINE};
use crate::driver;
use crate::image::Pin;
use crate::manifest::Manifest;
use crate::platform;
use crate::ports::{Allocator, Block};
use crate::state;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const PROJECT: &str = "demo";
const SCOPES: [&str; 2] = ["mir-901", "mir-902"];
const SECRET_NAME: &str = "API_TOKEN";
const SECRET_VALUE: &str = "msbx-init-secret";
const ENV_NAME: &str = "INIT_MARK";
const ENV_VALUE: &str = "msbx-init-env";
const CHECK_PORT_RANGE: (u16, u16) = (41000, 48000);
const WAIT_SECS: u64 = 240;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Skip,
}

struct Check {
    id: &'static str,
    title: &'static str,
    status: Status,
    detail: String,
}

impl Check {
    fn pass(id: &'static str, title: &'static str, detail: impl Into<String>) -> Self {
        Check {
            id,
            title,
            status: Status::Pass,
            detail: detail.into(),
        }
    }

    fn fail(id: &'static str, title: &'static str, detail: impl Into<String>) -> Self {
        Check {
            id,
            title,
            status: Status::Fail,
            detail: detail.into(),
        }
    }

    fn skip(id: &'static str, title: &'static str, detail: impl Into<String>) -> Self {
        Check {
            id,
            title,
            status: Status::Skip,
            detail: detail.into(),
        }
    }
}

struct Host {
    engine: Result<String, String>,
    compose: Result<String, String>,
    daemon: Option<String>,
    resolver: bool,
    dns: bool,
    privileged: Option<String>,
}

impl Host {
    fn docker_ok(&self) -> bool {
        matches!(&self.engine, Ok(version) if driver::version_at_least(version, MIN_ENGINE.0, MIN_ENGINE.1))
            && matches!(&self.compose, Ok(version) if driver::version_at_least(version, MIN_COMPOSE.0, MIN_COMPOSE.1))
            && self.daemon.is_some()
    }

    fn check(&self) -> Check {
        let mut parts = Vec::new();
        parts.push(match &self.engine {
            Ok(version) => format!("Docker {version}"),
            Err(error) => format!("Docker missing ({error})"),
        });
        parts.push(match &self.compose {
            Ok(version) => format!("Compose {version}"),
            Err(error) => format!("Compose missing ({error})"),
        });
        parts.push(match &self.daemon {
            Some(version) => format!("daemon reachable ({version})"),
            None => "daemon unreachable".to_string(),
        });
        parts.push(if self.resolver {
            "resolver installed".to_string()
        } else {
            "resolver missing".to_string()
        });
        parts.push(if self.dns {
            "dns answers".to_string()
        } else {
            "dns does not answer".to_string()
        });
        let detail = parts.join(" · ");

        if self.docker_ok() && self.resolver && self.dns {
            return Check::pass("C1", "host", detail);
        }

        let mut why = Vec::new();
        if let Err(error) = &self.engine {
            why.push(format!("Docker Engine: {error}"));
        } else if let Ok(version) = &self.engine {
            if !driver::version_at_least(version, MIN_ENGINE.0, MIN_ENGINE.1) {
                why.push(format!(
                    "Docker Engine {}.{} or newer is required",
                    MIN_ENGINE.0, MIN_ENGINE.1
                ));
            }
        }
        if let Err(error) = &self.compose {
            why.push(format!("Compose: {error}"));
        } else if let Ok(version) = &self.compose {
            if !driver::version_at_least(version, MIN_COMPOSE.0, MIN_COMPOSE.1) {
                why.push(format!(
                    "Docker Compose {}.{} or newer is required",
                    MIN_COMPOSE.0, MIN_COMPOSE.1
                ));
            }
        }
        if self.daemon.is_none() {
            why.push("Docker daemon is not reachable".to_string());
        }
        if !self.resolver {
            match &self.privileged {
                Some(command) => why.push(format!("resolver not installed; run: {command}")),
                None => why.push("resolver not installed; run `msbx init`".to_string()),
            }
        }
        if !self.dns {
            why.push(
                "msbx dns does not answer; run `msbx init` or check the installed unit".to_string(),
            );
        }
        Check::fail("C1", "host", format!("{detail} — {}", why.join("; ")))
    }
}

struct Scope {
    name: &'static str,
    dir: PathBuf,
    manifest: Manifest,
    block: Block,
}

impl Scope {
    fn project(&self) -> String {
        format!("{PROJECT}-{}", self.name)
    }

    fn container(&self, role: &str) -> String {
        format!("{}-{}-{role}", PROJECT, self.name)
    }

    fn alias(&self, role: &str) -> String {
        format!("{role}.{}.{PROJECT}.{}", self.name, dns::ZONE)
    }

    fn files(&self) -> (PathBuf, PathBuf) {
        let path = self.dir.join(".multica");
        (path.join("compose.yaml"), path.join("override.yaml"))
    }

    fn compose(&self, args: &[String]) -> driver::Output {
        let (base, override_file) = self.files();
        let mut all = vec![
            "compose".to_string(),
            "--ansi".to_string(),
            "never".to_string(),
            "-p".to_string(),
            self.project(),
            "-f".to_string(),
            base.to_string_lossy().to_string(),
            "-f".to_string(),
            override_file.to_string_lossy().to_string(),
        ];
        all.extend(args.iter().cloned());
        driver::run_owned("docker", &all)
    }
}

/// The synthetic project every `init` run measures. It is a real manifest: the
/// reader validates it, the allocator gives each scope a block, and the override
/// carries the names, aliases, env files and secret mount.
const CHECK_MANIFEST: &str = r#"x-msbx:
  project: demo
  publish:
    pg: 5432
services:
  pg:
    image: postgres:17-alpine
  client:
    image: alpine:3.20
    command: ["sleep", "300"]
  probe:
    image: alpine:3.20
    command: ["sleep", "300"]
"#;

pub fn run(args: &[String]) -> ExitCode {
    let check_only = args.iter().any(|arg| arg == "--check");
    let keep = args.iter().any(|arg| arg == "--keep");

    println!(
        "msbx init{}
",
        if check_only { " (check only)" } else { "" }
    );

    let work = state::init_dir();
    if let Err(error) = reset_work(&work) {
        eprintln!("msbx init: cannot prepare {}: {error}", work.display());
        return ExitCode::from(1);
    }

    let mut privileged = Some(platform::privileged_command());
    if !check_only {
        match std::env::current_exe() {
            Ok(exe) => {
                let setup = platform::install(&exe);
                for step in &setup.steps {
                    print_step(step);
                }
                privileged = setup.privileged.or(privileged);
            }
            Err(error) => eprintln!("msbx init: cannot find my own binary: {error}"),
        }
    }

    println!();
    // C1 measures the daemon the host actually runs; a transient daemon is only
    // started afterwards so C4 can still measure the name contract on a host
    // where the unit is not installed (CI).
    let dns_answering = dns::answers(platform::DNS_ADDR.parse().expect("static resolver address"));
    let dns_guard = ensure_dns();
    let host = Host {
        engine: driver::engine_version(),
        compose: driver::compose_version(),
        daemon: driver::daemon_version(),
        resolver: platform::resolver_installed(),
        dns: dns_answering,
        privileged,
    };

    let mut checks = vec![host.check()];
    if !host.docker_ok() {
        skip_rest(&mut checks, "Docker is not usable");
        drop(dns_guard);
        return finish(&checks, &[], &work, keep);
    }

    let scopes = match prepare(&work) {
        Ok(scopes) => scopes,
        Err(error) => {
            checks.push(Check::fail("C2", "ports", error));
            skip_rest(&mut checks, "the synthetic project could not be written");
            drop(dns_guard);
            return finish(&checks, &[], &work, keep);
        }
    };

    let started = start(&scopes);
    checks.push(check_ports(&scopes, &started));
    if !started.all_ok() {
        for (id, title) in [
            ("C3", "host reachability"),
            ("C4", "names"),
            ("C5", "in-scope"),
            ("C6", "isolation"),
            ("C7", "env"),
            ("C8", "harness container"),
        ] {
            checks.push(Check::skip(
                id,
                title,
                "skipped: the check project did not start",
            ));
        }
        drop(dns_guard);
        return finish(&checks, &scopes, &work, keep);
    }

    checks.push(check_reachability(&scopes));
    checks.push(check_names(&scopes));
    checks.push(check_in_scope(&scopes));
    checks.push(check_isolation(&scopes));
    checks.push(check_env(&scopes, &work));
    checks.push(check_harness(&scopes, &work));

    drop(dns_guard);
    finish(&checks, &scopes, &work, keep)
}

fn reset_work(work: &Path) -> std::io::Result<()> {
    if work.exists() {
        std::fs::remove_dir_all(work)?;
    }
    state::create_private_dir(&work.join("env"))?;
    state::create_private_dir(&work.join("secrets"))
}

fn prepare(work: &Path) -> Result<Vec<Scope>, String> {
    state::write_private(
        &work.join("env").join("pg.env"),
        "POSTGRES_PASSWORD=msbx-init\n",
    )
    .map_err(|error| format!("cannot write the Postgres env file: {error}"))?;
    state::write_private(
        &work.join("env").join("probe.env"),
        &format!("{ENV_NAME}={ENV_VALUE}\n"),
    )
    .map_err(|error| format!("cannot write the probe env file: {error}"))?;
    state::write_private(&work.join("secrets").join(SECRET_NAME), SECRET_VALUE)
        .map_err(|error| format!("cannot write the secret file: {error}"))?;

    let allocator = Allocator::with_range(
        work.join("state.json"),
        CHECK_PORT_RANGE.0,
        CHECK_PORT_RANGE.1,
    );
    let at = state::now();
    let mut scopes = Vec::with_capacity(SCOPES.len());
    for name in SCOPES {
        let dir = work.join("project").join(name);
        let manifest_dir = dir.join(".multica");
        state::create_private_dir(&manifest_dir)
            .map_err(|error| format!("cannot create {}: {error}", manifest_dir.display()))?;
        std::fs::write(manifest_dir.join("compose.yaml"), CHECK_MANIFEST)
            .map_err(|error| format!("cannot write the check manifest: {error}"))?;

        let manifest = Manifest::from_repo(&dir)
            .map_err(|error| format!("the synthetic manifest is invalid: {error}"))?;
        let block = allocator
            .allocate(&format!("{PROJECT}-{name}"), at)
            .map_err(|error| format!("cannot allocate a port block: {error}"))?;
        let scope = Scope {
            name,
            dir,
            manifest,
            block,
        };
        write_override(&scope, work)
            .map_err(|error| format!("cannot write the override for {name}: {error}"))?;
        scopes.push(scope);
    }
    Ok(scopes)
}

fn write_override(scope: &Scope, work: &Path) -> std::io::Result<()> {
    let pg_port = scope
        .manifest
        .host_port(scope.block, "pg")
        .unwrap_or(scope.block.start);
    let pg_env = yaml_path(&work.join("env").join("pg.env"));
    let probe_env = yaml_path(&work.join("env").join("probe.env"));
    let secret = work
        .join("secrets")
        .join(SECRET_NAME)
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let override_path = scope.dir.join(".multica").join("override.yaml");
    let body = format!(
        r#"services:
  pg:
    container_name: {pg_container}
    env_file:
      - {pg_env}
    ports:
      - "127.0.0.1:{pg_port}:5432"
    networks:
      default:
        aliases:
          - {alias}
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 1s
      timeout: 3s
      retries: 30
  client:
    container_name: {client}
  probe:
    container_name: {probe}
    env_file:
      - {probe_env}
    environment:
      {secret_name}_FILE: /run/msbx/secrets/{secret_name}
    volumes:
      - "{secret}:/run/msbx/secrets/{secret_name}:ro"
networks:
  default:
    name: {network}
"#,
        pg_container = scope.container("pg"),
        client = scope.container("client"),
        probe = scope.container("probe"),
        alias = scope.alias("pg"),
        network = scope.project(),
        secret_name = SECRET_NAME,
    );
    std::fs::write(override_path, body)
}

fn yaml_path(path: &Path) -> String {
    format!(
        "\"{}\"",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

#[derive(Default)]
struct Start {
    errors: Vec<String>,
}

impl Start {
    fn all_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

fn start(scopes: &[Scope]) -> Start {
    let mut start = Start::default();
    for scope in scopes {
        let up = scope.compose(&[
            "up".to_string(),
            "-d".to_string(),
            "--wait".to_string(),
            "--wait-timeout".to_string(),
            WAIT_SECS.to_string(),
        ]);
        if !up.ok {
            start
                .errors
                .push(format!("{}: {}", scope.name, one_line(&up.text())));
        }
    }
    start
}

fn check_ports(scopes: &[Scope], started: &Start) -> Check {
    if !started.all_ok() {
        return Check::fail("C2", "ports", started.errors.join(" ; "));
    }
    if scopes.len() != 2 {
        return Check::fail("C2", "ports", "expected two synthetic scopes");
    }

    let mut parts = Vec::new();
    let mut observed = Vec::new();
    for scope in scopes {
        let container = scope.container("pg");
        let out = driver::run("docker", &["port", &container, "5432/tcp"]);
        let port = out
            .stdout
            .trim()
            .rsplit(':')
            .next()
            .and_then(|value| value.parse::<u16>().ok());
        match port {
            Some(port) => {
                observed.push(port);
                parts.push(format!("{} 127.0.0.1:{port}→5432", scope.name));
            }
            None => {
                return Check::fail(
                    "C2",
                    "ports",
                    format!(
                        "{container} publishes no 5432 port: {}",
                        one_line(&out.text())
                    ),
                )
            }
        }
    }

    let expected: Vec<u16> = scopes
        .iter()
        .filter_map(|scope| scope.manifest.host_port(scope.block, "pg"))
        .collect();
    let distinct = observed[0] != observed[1];
    if observed == expected && distinct {
        Check::pass(
            "C2",
            "ports",
            format!("both Postgres on 5432: {}", parts.join(", ")),
        )
    } else {
        Check::fail(
            "C2",
            "ports",
            format!("assigned {expected:?}, observed {observed:?} (host ports must be distinct)"),
        )
    }
}

fn check_reachability(scopes: &[Scope]) -> Check {
    let mut parts = Vec::new();
    let mut reachable = 0;
    for scope in scopes {
        let Some(port) = scope.manifest.host_port(scope.block, "pg") else {
            continue;
        };
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        if driver::wait_for_tcp(address, Duration::from_secs(WAIT_SECS)) {
            reachable += 1;
            parts.push(format!("127.0.0.1:{port} open"));
        } else {
            parts.push(format!("127.0.0.1:{port} closed"));
        }
    }
    if reachable == scopes.len() {
        Check::pass("C3", "host reachability", parts.join(", "))
    } else {
        Check::fail("C3", "host reachability", parts.join(", "))
    }
}

fn check_names(scopes: &[Scope]) -> Check {
    let address: SocketAddr = platform::DNS_ADDR.parse().expect("static resolver address");
    let mut parts = Vec::new();
    let mut ok = true;
    for scope in scopes {
        let name = scope.alias("pg");
        match dns::query(address, &name, 1) {
            Ok(answer) if answer.records == [Ipv4Addr::LOCALHOST] => {
                parts.push(format!("{name} → 127.0.0.1"));
            }
            Ok(answer) => {
                ok = false;
                parts.push(format!("{name} → {:?}", answer.records));
            }
            Err(error) => {
                ok = false;
                parts.push(format!("{name} → no answer ({error})"));
            }
        }
        if platform::resolver_installed() {
            match platform::system_resolve(&name) {
                Some(Ipv4Addr::LOCALHOST) => {}
                Some(other) => {
                    ok = false;
                    parts.push(format!("system resolver: {name} → {other}"));
                }
                None => {
                    ok = false;
                    parts.push(format!("system resolver: {name} unresolved"));
                }
            }
        }
    }
    let detail = if parts.is_empty() {
        "no names checked".to_string()
    } else {
        parts.join(", ")
    };
    if ok && !parts.is_empty() {
        Check::pass("C4", "names", detail)
    } else {
        Check::fail("C4", "names", detail)
    }
}

fn check_in_scope(scopes: &[Scope]) -> Check {
    let scope = &scopes[0];
    let service = scope.alias("pg");
    let script = format!(
        "getent hosts pg >/dev/null && getent hosts {service} >/dev/null && nc -z -w 2 pg 5432 && nc -z -w 2 {service} 5432 && echo OK"
    );
    let out = scope.compose(&[
        "exec".to_string(),
        "-T".to_string(),
        "client".to_string(),
        "sh".to_string(),
        "-c".to_string(),
        script,
    ]);
    if out.ok && out.stdout.contains("OK") {
        Check::pass(
            "C5",
            "in-scope",
            format!("{} reaches pg and {service} on 5432", scope.name),
        )
    } else {
        Check::fail(
            "C5",
            "in-scope",
            format!(
                "{} could not reach pg by service name and alias: {}",
                scope.name,
                one_line(&out.text())
            ),
        )
    }
}

fn check_isolation(scopes: &[Scope]) -> Check {
    let first = &scopes[0];
    let second = &scopes[1];
    let out = driver::run(
        "docker",
        &[
            "inspect",
            "-f",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}",
            &second.container("pg"),
        ],
    );
    let Some(address) = driver::first_ipv4(&out.stdout) else {
        return Check::fail(
            "C6",
            "isolation",
            format!(
                "cannot read {}'s address: {}",
                second.container("pg"),
                one_line(&out.text())
            ),
        );
    };

    let alias = second.alias("pg");
    let script =
        format!("nc -z -w 2 {address} 5432; echo ip:$?; nc -z -w 2 {alias} 5432; echo alias:$?");
    let probe = first.compose(&[
        "exec".to_string(),
        "-T".to_string(),
        "client".to_string(),
        "sh".to_string(),
        "-c".to_string(),
        script,
    ]);
    let stdout = probe.stdout.trim();
    let ip_failed = stdout
        .lines()
        .find_map(|line| line.strip_prefix("ip:"))
        .is_some_and(|code| code.trim() != "0");
    let alias_failed = stdout
        .lines()
        .find_map(|line| line.strip_prefix("alias:"))
        .is_some_and(|code| code.trim() != "0");

    if probe.ok && ip_failed && alias_failed {
        Check::pass(
            "C6",
            "isolation",
            format!(
                "{} cannot reach {alias} at {address}:5432 (address and name both refused)",
                first.name
            ),
        )
    } else {
        Check::fail(
            "C6",
            "isolation",
            format!(
                "{} reached {} across the network boundary: {}",
                first.name,
                second.name,
                one_line(stdout)
            ),
        )
    }
}

fn check_env(scopes: &[Scope], work: &Path) -> Check {
    let scope = &scopes[0];
    let script = format!("printf '%s|' \"${ENV_NAME}\"; cat /run/msbx/secrets/{SECRET_NAME}");
    let out = scope.compose(&[
        "exec".to_string(),
        "-T".to_string(),
        "probe".to_string(),
        "sh".to_string(),
        "-c".to_string(),
        script,
    ]);
    let expected = format!("{ENV_VALUE}|{SECRET_VALUE}");
    let visible = out.ok && out.stdout.trim() == expected;
    let absent = !tree_contains(&work.join("project"), SECRET_VALUE);

    if visible && absent {
        Check::pass(
            "C7",
            "env",
            format!(
                "{ENV_NAME} and /run/msbx/secrets/{SECRET_NAME} visible; secret value absent from the checkout"
            ),
        )
    } else {
        Check::fail(
            "C7",
            "env",
            format!(
                "env/secret visible: {visible} (expected `{expected}`, got `{}`); secret absent from the checkout: {absent}",
                one_line(&out.text())
            ),
        )
    }
}

/// Does any file under `dir` contain `needle`? Used to prove a store value did
/// not leak into a checkout.
fn tree_contains(dir: &Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if tree_contains(&path, needle) {
                return true;
            }
        } else if std::fs::read_to_string(&path)
            .map(|text| text.contains(needle))
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// C8: run a throwaway container from the pinned image against a real scope
/// network and check the harness contract — the four mounts at identical
/// paths, the host UID/GID on a bind-mounted write, the in-image CLI, a
/// published role's alias and the agent-dir seed. Stage 4 owns the image; until
/// it is pinned, C8 records a skip instead of a failure.
fn check_harness(scopes: &[Scope], work: &Path) -> Check {
    let pin = match Pin::load() {
        Ok(pin) => pin,
        Err(error) => {
            return Check::skip(
                "C8",
                "harness container",
                format!("skipped: no image pin yet ({error})"),
            )
        }
    };
    let Some(scope) = scopes.first() else {
        return Check::skip("C8", "harness container", "skipped: no check scope");
    };
    let Some(identity) = host_identity() else {
        return Check::fail("C8", "harness container", "cannot read the host UID/GID");
    };
    let Some(host_uid) = identity.split(':').next() else {
        return Check::fail("C8", "harness container", "cannot read the host UID");
    };

    let dir = work.join("c8");
    let workdir = dir.join("workdir");
    let config = dir.join("config");
    let sessions = dir.join("sessions");
    let agent = dir.join("agent").join("pi");
    for path in [&workdir, &config, &sessions, &agent] {
        if let Err(error) = state::create_private_dir(path) {
            return Check::fail(
                "C8",
                "harness container",
                format!("cannot prepare {}: {error}", path.display()),
            );
        }
    }
    if let Err(error) = std::fs::write(
        agent.join(".msbx-seed"),
        format!("digest {}\nagent c8\n", pin.digest),
    ) {
        return Check::fail(
            "C8",
            "harness container",
            format!("cannot write the seed marker: {error}"),
        );
    }

    // The image's baseline must exist before the shim can seed anything.
    let seed = format!(
        "set -e\ncp -a /opt/msbx/agent/. {SEED_DIR}/\nif [ -d '/opt/msbx/agent/roles/implementer' ]; then cp -a '/opt/msbx/agent/roles/implementer'/. {SEED_DIR}/; fi\n",
    );
    let seeded = docker(
        &[
            "run",
            "--rm",
            "--user",
            &identity,
            "--entrypoint",
            "/bin/sh",
            "--volume",
            &format!("{}:{SEED_DIR}:rw", agent.display()),
            &pin.reference(),
            "-c",
            &seed,
        ],
        work,
    );
    if !seeded.ok {
        return Check::fail(
            "C8",
            "harness container",
            format!(
                "the image baseline did not seed: {}",
                one_line(&seeded.text())
            ),
        );
    }

    let alias = scope.alias("pg");
    let script = format!(
        "set -e\ntest \"$(id -u)\" = '{host_uid}' || exit 10\ntouch '{workfile}' || exit 11\ntest -w '{config}' || exit 12\ntest -x {cli} || exit 13\ntest -f '{seedfile}' || exit 14\ntest -n \"$(find /home/agent/.pi -mindepth 1 ! -name .msbx-seed | head -n 1)\" || exit 16\ngetent hosts '{alias}' >/dev/null || exit 15\necho ok\n",
        workfile = workdir.join(".msbx-c8-write").display(),
        config = config.display(),
        cli = IN_IMAGE_CLI,
        seedfile = "/home/agent/.pi/.msbx-seed",
    );
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--user".to_string(),
        identity.clone(),
        "--workdir".to_string(),
        workdir.to_string_lossy().to_string(),
        "--network".to_string(),
        scope.project(),
    ];
    if cfg!(target_os = "linux") {
        args.push("--add-host".to_string());
        args.push("host.docker.internal:host-gateway".to_string());
    }
    for source in [&workdir, &config, &sessions] {
        args.push("--volume".to_string());
        args.push(format!("{}:{}:rw", source.display(), source.display()));
    }
    args.push("--volume".to_string());
    args.push(format!("{}:/home/agent/.pi:rw", agent.display()));
    args.push("--entrypoint".to_string());
    args.push("/bin/sh".to_string());
    args.push(pin.reference());
    args.push("-c".to_string());
    args.push(script);
    let run = driver::run_owned("docker", &args);
    let marker = workdir.join(".msbx-c8-write");
    let owner = std::fs::metadata(&marker)
        .ok()
        .and_then(|meta| file_uid(&meta));
    let _ = std::fs::remove_file(&marker);
    if !run.ok {
        return Check::fail(
            "C8",
            "harness container",
            format!(
                "the harness container check exited non-zero: {}",
                one_line(&run.text())
            ),
        );
    }
    if owner != Some(host_uid.parse().unwrap_or(0)) {
        return Check::fail(
            "C8",
            "harness container",
            format!(
                "the bind-mounted write is owned by {:?}, not uid {host_uid}",
                owner
            ),
        );
    }

    Check::pass(
        "C8",
        "harness container",
        format!(
            "four mounts, uid {host_uid} on the bind write and in the container, {cli} present, {alias} resolves, agent dir seeded",
            cli = IN_IMAGE_CLI
        ),
    )
}

/// The seed target inside the image.
const SEED_DIR: &str = "/seed";
/// The CLI the image must carry.
const IN_IMAGE_CLI: &str = "/usr/local/bin/multica";

fn docker(args: &[&str], dir: &Path) -> driver::Output {
    driver::run_in(dir, "docker", args)
}

#[cfg(unix)]
fn host_identity() -> Option<String> {
    let uid = driver::run("id", &["-u"]);
    let gid = driver::run("id", &["-g"]);
    (uid.ok && gid.ok).then(|| format!("{}:{}", uid.stdout.trim(), gid.stdout.trim()))
}

#[cfg(not(unix))]
fn host_identity() -> Option<String> {
    None
}

#[cfg(unix)]
fn file_uid(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.uid())
}

#[cfg(not(unix))]
fn file_uid(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

fn skip_rest(checks: &mut Vec<Check>, why: &str) {
    for (id, title) in [
        ("C2", "ports"),
        ("C3", "host reachability"),
        ("C4", "names"),
        ("C5", "in-scope"),
        ("C6", "isolation"),
        ("C7", "env"),
        ("C8", "harness container"),
    ] {
        checks.push(Check::skip(id, title, format!("skipped: {why}")));
    }
}

fn finish(checks: &[Check], scopes: &[Scope], work: &Path, keep: bool) -> ExitCode {
    if !keep {
        for scope in scopes {
            let down = scope.compose(&[
                "down".to_string(),
                "-v".to_string(),
                "--remove-orphans".to_string(),
            ]);
            if !down.ok {
                eprintln!(
                    "msbx init: could not remove {}: {}",
                    scope.project(),
                    one_line(&down.text())
                );
            }
        }
    }

    if keep {
        println!("keeping the check project under {}", work.display());
        for scope in scopes {
            println!("  {} ({} project)", scope.project(), scope.dir.display());
        }
    } else {
        let _ = std::fs::remove_dir_all(work);
    }

    println!();
    for check in checks {
        let label = match check.status {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        };
        println!(
            "  {:<3} {:<5} {:<18} {}",
            check.id, label, check.title, check.detail
        );
    }
    println!();

    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let skipped = checks.iter().filter(|c| c.status == Status::Skip).count();
    if failed > 0 {
        println!("{failed} failed, {skipped} skipped");
        ExitCode::from(1)
    } else if skipped > 0 {
        println!("0 failed, {skipped} skipped — the init run is incomplete");
        ExitCode::from(2)
    } else {
        println!("all checks passed");
        ExitCode::SUCCESS
    }
}

fn print_step(step: &platform::Step) {
    let label = match step.label {
        platform::Label::Ok => "ok",
        platform::Label::Run => "run",
        platform::Label::Skip => "skip",
        platform::Label::Fail => "fail",
    };
    let tag = format!("[{label}]");
    println!("  {tag:<7} {:<9} {}", step.subject, step.detail);
}

/// A daemon this run started only so C4 can be measured; killed on drop.
struct TransientDns {
    child: Option<Child>,
}

impl Drop for TransientDns {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Make sure something answers on 127.0.0.1:5335. The installed unit should;
/// if it does not (and `--check` installed nothing), start the binary itself
/// for the duration of the checks.
fn ensure_dns() -> TransientDns {
    let address: SocketAddr = platform::DNS_ADDR.parse().expect("static resolver address");
    if dns::answers(address) {
        return TransientDns { child: None };
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("msbx init: cannot find my own binary: {error}");
            return TransientDns { child: None };
        }
    };
    let child = Command::new(exe)
        .arg("dns")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if dns::answers(address) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    TransientDns { child }
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
