//! `msbx init` — set up the host, then prove the driver contract.
//!
//! `init` is idempotent: it starts the `container` system (installing the
//! kernel on first run) and registers the `msbx.test` resolver through `sudo`,
//! then runs seven checks against the real `container` CLI. Every check prints
//! what it observed; a skipped check makes the run incomplete (exit 2), never
//! a pass. `--check` performs no setup and only measures.

use crate::driver;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

const PROJECT: &str = "demo";
const ZONE: &str = "msbx.test";
const PG_IMAGE: &str = "docker.io/library/postgres:16-alpine";
const ALPINE_IMAGE: &str = "docker.io/library/alpine:3.20";
const PG_PORT: u16 = 5432;

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

fn alias_for(epic: &str) -> String {
    format!("pg.{epic}.{PROJECT}.{ZONE}")
}

fn container_for(epic: &str) -> String {
    format!("{PROJECT}-{epic}-pg")
}

/// What `init` found or made on the host; `C1` reports it. `None` means the
/// step was never probed because an earlier one failed.
struct Host {
    macos: String,
    macos_ok: bool,
    container: Option<String>,
    system: Option<bool>,
    resolver: Option<bool>,
}

impl Host {
    /// Everything the checks after C1 need. The resolver is part of it: without
    /// names there is no point creating containers.
    fn ready(&self) -> bool {
        self.macos_ok
            && self.container.is_some()
            && self.system == Some(true)
            && self.resolver == Some(true)
    }

    fn check(&self) -> Check {
        let mut parts = vec![format!("macOS {}", self.macos)];
        parts.push(match &self.container {
            Some(version) => version.clone(),
            None => "container missing".to_string(),
        });
        match self.system {
            Some(true) => parts.push("system running".to_string()),
            Some(false) => parts.push("system not running".to_string()),
            None => {}
        }
        match self.resolver {
            Some(true) => parts.push(format!("{ZONE} resolver installed")),
            Some(false) => parts.push(format!("{ZONE} resolver missing")),
            None => {}
        }
        let detail = parts.join(" · ");

        if self.ready() {
            return Check::pass("C1", "host", detail);
        }

        let mut why = Vec::new();
        if !self.macos_ok {
            why.push("macOS 26 or newer is required".to_string());
        }
        if self.container.is_none() {
            why.push("`container` is not installed".to_string());
        }
        if self.system == Some(false) {
            why.push("the container system is not running".to_string());
        }
        if self.resolver == Some(false) {
            why.push(format!("the {ZONE} resolver is not installed"));
        }
        Check::fail("C1", "host", format!("{detail} — {}", why.join("; ")))
    }
}

fn step(label: &str, subject: &str, detail: &str) {
    let tag = format!("[{label}]");
    println!("  {tag:<6} {subject:<9} {detail}");
}

fn setup_host(check_only: bool) -> Host {
    let macos = driver::macos_version();
    let major = driver::macos_major();
    let macos_ok = major.map(|value| value >= 26).unwrap_or(false);
    if macos_ok {
        step("ok", "macOS", &macos);
    } else {
        step(
            "fail",
            "macOS",
            &format!("{macos} — `init` needs macOS 26 or newer"),
        );
    }

    if !driver::has("container") {
        step(
            "fail",
            "container",
            "not on PATH; install Apple's container CLI",
        );
        return Host {
            macos,
            macos_ok,
            container: None,
            system: None,
            resolver: None,
        };
    }

    let version = driver::run("container", &["--version"]);
    let version = version.stdout.trim().to_string();
    step("ok", "container", &version);

    if !macos_ok {
        return Host {
            macos,
            macos_ok,
            container: Some(version),
            system: None,
            resolver: None,
        };
    }

    let system = if driver::system_running() {
        step("ok", "system", "already running");
        Some(true)
    } else if check_only {
        step("skip", "system", "not running (`--check`: not started)");
        Some(false)
    } else {
        step(
            "run",
            "system",
            "container system start --kernel-install (first run installs the kernel)",
        );
        let started = driver::run_attached("container", &["system", "start", "--kernel-install"])
            && wait_for_system(Duration::from_secs(180));
        if started {
            step("ok", "system", "started");
            Some(true)
        } else {
            step(
                "fail",
                "system",
                "did not come up; run `container system start` and read its output",
            );
            Some(false)
        }
    };

    let resolver = if system != Some(true) {
        step(
            "skip",
            "resolver",
            "skipped: the container system is not running",
        );
        None
    } else if driver::dns_domain(ZONE) {
        step("ok", "resolver", &format!("{ZONE} already installed"));
        Some(true)
    } else if check_only {
        step(
            "skip",
            "resolver",
            &format!("{ZONE} missing (`--check`: not created); run: sudo container system dns create {ZONE}"),
        );
        Some(false)
    } else if driver::is_interactive() {
        step(
            "run",
            "resolver",
            &format!("sudo container system dns create {ZONE}"),
        );
        let created = driver::run_attached("sudo", &["container", "system", "dns", "create", ZONE])
            && driver::dns_domain(ZONE);
        if created {
            step("ok", "resolver", &format!("{ZONE} installed"));
            Some(true)
        } else {
            step(
                "fail",
                "resolver",
                &format!("could not install {ZONE}; run: sudo container system dns create {ZONE}"),
            );
            Some(false)
        }
    } else {
        step(
            "skip",
            "resolver",
            &format!("no terminal for sudo; run: sudo container system dns create {ZONE}"),
        );
        Some(false)
    };

    Host {
        macos,
        macos_ok,
        container: Some(version),
        system,
        resolver,
    }
}

fn wait_for_system(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if driver::system_running() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(500));
    }
}

fn skip_rest(checks: &mut Vec<Check>) {
    for (id, title) in [
        ("C2", "ports"),
        ("C3", "host reachability"),
        ("C4", "names"),
        ("C5", "cross-container"),
        ("C6", "alias precedence"),
        ("C7", "mounts and env"),
    ] {
        checks.push(Check::skip(id, title, "skipped: C1 did not pass"));
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let keep = args.iter().any(|arg| arg == "--keep");
    let check_only = args.iter().any(|arg| arg == "--check");
    let temp = std::env::temp_dir().join(format!("msbx-init-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp);

    let mut checks: Vec<Check> = Vec::new();
    let mut names: Vec<String> = Vec::new();

    println!(
        "msbx init{}
",
        if check_only { " (check only)" } else { "" }
    );

    let host = setup_host(check_only);
    checks.push(host.check());

    if !host.ready() {
        skip_rest(&mut checks);
        return finish(&checks, &temp, &names, keep);
    }

    let env_file = match write_file(&temp, "postgres.env", "POSTGRES_PASSWORD=msbx-init\n") {
        Ok(path) => path,
        Err(error) => {
            checks.push(Check::fail(
                "C2",
                "ports",
                format!("cannot write the env file: {error}"),
            ));
            return finish(&checks, &temp, &names, keep);
        }
    };

    for epic in ["epic-a", "epic-b"] {
        let name = container_for(epic);
        delete(&name);
        names.push(name);
    }

    let mut started: Vec<(&'static str, Ipv4Addr)> = Vec::new();
    let mut start_errors: Vec<String> = Vec::new();

    for epic in ["epic-a", "epic-b"] {
        let name = container_for(epic);
        let network = format!("default,alias={}", alias_for(epic));
        let env = env_file.to_string_lossy().to_string();
        let out = driver::run(
            "container",
            &[
                "run",
                "--detach",
                "--name",
                name.as_str(),
                "--network",
                network.as_str(),
                "--env-file",
                env.as_str(),
                PG_IMAGE,
            ],
        );
        if !out.ok {
            start_errors.push(format!("{name}: {}", one_line(&out.text())));
            continue;
        }
        match inspect_ipv4(&name) {
            Some(ip) => started.push((epic, ip)),
            None => start_errors.push(format!(
                "{name}: no IPv4 address in `container inspect` (stdout was {})",
                one_line(&driver::run("container", &["inspect", &name]).stdout)
            )),
        }
    }

    checks.push(if start_errors.is_empty() && started.len() == 2 {
        let a = started[0].1;
        let b = started[1].1;
        if a == b {
            Check::fail(
                "C2",
                "ports",
                format!("both containers share {a}; no per-epic address"),
            )
        } else {
            Check::pass(
                "C2",
                "ports",
                format!("both Postgres on {PG_PORT}, no -p: epic-a {a}, epic-b {b}"),
            )
        }
    } else {
        Check::fail("C2", "ports", start_errors.join(" ; "))
    });

    checks.push(check_reachability(&started));

    let zone_installed = driver::dns_domain(ZONE);
    checks.push(if zone_installed {
        let mut parts = Vec::new();
        let mut ok = true;
        for (epic, expected) in &started {
            let name = alias_for(epic);
            match resolve_alias(&name) {
                Some(found) if found == *expected => parts.push(format!("{name} → {found}")),
                Some(found) => {
                    ok = false;
                    parts.push(format!("{name} → {found} (expected {expected})"));
                }
                None => {
                    ok = false;
                    parts.push(format!("{name} → unresolved"));
                }
            }
        }
        let detail = if parts.is_empty() {
            "no containers started".to_string()
        } else {
            parts.join(", ")
        };
        if ok && !parts.is_empty() {
            Check::pass("C4", "names", detail)
        } else {
            Check::fail("C4", "names", detail)
        }
    } else {
        Check::skip(
            "C4",
            "names",
            format!("the {ZONE} resolver is not installed; run: sudo container system dns create {ZONE}"),
        )
    });

    checks.push(check_cross_container(&started, zone_installed));
    checks.push(check_alias_precedence(&started, zone_installed, &mut names));
    checks.push(check_mounts_and_env(&temp, &mut names));

    finish(&checks, &temp, &names, keep)
}

fn check_reachability(started: &[(&'static str, Ipv4Addr)]) -> Check {
    if started.len() != 2 {
        return Check::skip(
            "C3",
            "host reachability",
            "skipped: C2 did not start two containers",
        );
    }
    let mut reachable = 0;
    let mut parts = Vec::new();
    for (epic, ip) in started {
        let address = SocketAddr::new(IpAddr::V4(*ip), PG_PORT);
        if driver::wait_for_tcp(address, Duration::from_secs(120)) {
            reachable += 1;
            parts.push(format!("{epic} {ip}:{PG_PORT} open"));
        } else {
            parts.push(format!("{epic} {ip}:{PG_PORT} closed"));
        }
    }
    if reachable == 2 {
        Check::pass("C3", "host reachability", parts.join(", "))
    } else {
        Check::fail("C3", "host reachability", parts.join(", "))
    }
}

fn check_cross_container(started: &[(&'static str, Ipv4Addr)], zone_installed: bool) -> Check {
    if !zone_installed {
        return Check::skip(
            "C5",
            "cross-container",
            "skipped: the msbx.test resolver is not installed",
        );
    }
    let peer = match started.iter().find(|(epic, _)| *epic == "epic-b") {
        Some((_, ip)) => *ip,
        None => return Check::skip("C5", "cross-container", "skipped: epic-b did not start"),
    };
    if !started.iter().any(|(epic, _)| *epic == "epic-a") {
        return Check::skip("C5", "cross-container", "skipped: epic-a did not start");
    }

    let alias = alias_for("epic-b");
    let origin = container_for("epic-a");
    let out = driver::run(
        "container",
        &["exec", origin.as_str(), "nslookup", alias.as_str()],
    );
    let found = out.ok && out.stdout.contains(&peer.to_string());
    if found {
        Check::pass(
            "C5",
            "cross-container",
            format!("epic-a resolves {alias} → {peer}"),
        )
    } else {
        Check::fail(
            "C5",
            "cross-container",
            format!(
                "epic-a could not resolve {alias} (expected {peer}): {}",
                one_line(&out.text())
            ),
        )
    }
}

fn check_alias_precedence(
    started: &[(&'static str, Ipv4Addr)],
    zone_installed: bool,
    names: &mut Vec<String>,
) -> Check {
    if !zone_installed {
        return Check::skip(
            "C6",
            "alias precedence",
            "skipped: the msbx.test resolver is not installed",
        );
    }
    let a_ip = match started.iter().find(|(epic, _)| *epic == "epic-a") {
        Some((_, ip)) => *ip,
        None => return Check::skip("C6", "alias precedence", "skipped: epic-a did not start"),
    };

    let gateway = format!("{PROJECT}-epic-a-gw");
    delete(&gateway);
    names.push(gateway.clone());

    let network = format!("default,alias=.epic-a.{PROJECT}.{ZONE}");
    let out = driver::run(
        "container",
        &[
            "run",
            "--detach",
            "--name",
            gateway.as_str(),
            "--network",
            network.as_str(),
            ALPINE_IMAGE,
            "sleep",
            "300",
        ],
    );
    if !out.ok {
        return Check::fail(
            "C6",
            "alias precedence",
            format!(
                "could not start the subtree-alias container: {}",
                one_line(&out.text())
            ),
        );
    }

    let gateway_ip = inspect_ipv4(&gateway);
    let alias = alias_for("epic-a");
    let resolved = resolve_alias(&alias);

    match (resolved, gateway_ip) {
        (Some(found), Some(gateway_ip)) if found == a_ip => Check::pass(
            "C6",
            "alias precedence",
            format!("exact alias wins: {alias} {found}, gateway {gateway_ip}"),
        ),
        (Some(found), Some(gateway_ip)) if found == gateway_ip => Check::pass(
            "C6",
            "alias precedence",
            format!("subtree alias shadows the role: {alias} → {found} (gateway), Postgres {a_ip}"),
        ),
        (Some(found), gateway_ip) => Check::pass(
            "C6",
            "alias precedence",
            format!("{alias} → {found}; gateway {gateway_ip:?}"),
        ),
        (None, gateway_ip) => Check::fail(
            "C6",
            "alias precedence",
            format!(
                "{alias} stopped resolving with the subtree alias present; gateway {gateway_ip:?}"
            ),
        ),
    }
}

fn check_mounts_and_env(temp: &Path, names: &mut Vec<String>) -> Check {
    let mount = temp.join("mount");
    let probe = mount.join("probe.txt");
    if let Err(error) = std::fs::create_dir_all(&mount) {
        return Check::fail(
            "C7",
            "mounts and env",
            format!("cannot create the mount: {error}"),
        );
    }
    if let Err(error) = std::fs::write(&probe, "msbx-probe") {
        return Check::fail(
            "C7",
            "mounts and env",
            format!("cannot write the probe: {error}"),
        );
    }
    let env_file = match write_file(temp, "probe.env", "INIT_MARK=msbx-env\n") {
        Ok(path) => path,
        Err(error) => {
            return Check::fail(
                "C7",
                "mounts and env",
                format!("cannot write the env file: {error}"),
            )
        }
    };

    let name = format!("{PROJECT}-epic-a-mount");
    delete(&name);
    names.push(name.clone());

    let volume = format!("{}:/work:ro", mount.display());
    let env = env_file.to_string_lossy().to_string();
    let out = driver::run(
        "container",
        &[
            "run",
            "--name",
            name.as_str(),
            "-v",
            volume.as_str(),
            "--env-file",
            env.as_str(),
            ALPINE_IMAGE,
            "sh",
            "-c",
            "cat /work/probe.txt && printf '|' && printenv INIT_MARK",
        ],
    );
    let combined = format!("{}{}", out.stdout, out.stderr);
    let ok = out.ok && out.stdout.contains("msbx-probe|msbx-env");
    if ok {
        Check::pass(
            "C7",
            "mounts and env",
            "-v read-only mount and --env-file both visible",
        )
    } else {
        Check::fail(
            "C7",
            "mounts and env",
            format!(
                "-v/--env-file did not behave as assumed: {}",
                one_line(&combined)
            ),
        )
    }
}

fn finish(checks: &[Check], temp: &Path, names: &[String], keep: bool) -> ExitCode {
    if keep {
        println!("keeping containers: {}", names.join(", "));
    } else {
        for name in names {
            delete(name);
        }
    }
    let _ = std::fs::remove_dir_all(temp);

    println!();
    for check in checks {
        let label = match check.status {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        };
        println!(
            "  {:<3} {:<5} {:<19} {}",
            check.id, label, check.title, check.detail
        );
    }
    println!();

    let failed = checks
        .iter()
        .filter(|check| check.status == Status::Fail)
        .count();
    let skipped = checks
        .iter()
        .filter(|check| check.status == Status::Skip)
        .count();
    if failed > 0 {
        println!("{failed} failed, {skipped} skipped");
        ExitCode::from(1)
    } else if skipped > 0 {
        println!("0 failed, {skipped} skipped — the init run is incomplete");
        ExitCode::from(2)
    } else {
        println!("all assertions passed");
        ExitCode::SUCCESS
    }
}

fn inspect_ipv4(name: &str) -> Option<Ipv4Addr> {
    let out = driver::run("container", &["inspect", name]);
    if !out.ok {
        return None;
    }
    for key in ["ipv4Address", "ipv4", "address"] {
        if let Some(value) = driver::json_string(&out.stdout, key) {
            if let Some(ip) = driver::ipv4_of(&value) {
                return Some(ip);
            }
        }
    }
    None
}

fn resolve_alias(name: &str) -> Option<Ipv4Addr> {
    let out = driver::run("dscacheutil", &["-q", "host", "-a", "name", name]);
    out.stdout.lines().find_map(|line| {
        let value = line.trim().strip_prefix("ip_address:")?;
        driver::ipv4_of(value.trim())
    })
}

fn delete(name: &str) {
    let _ = driver::run("container", &["delete", "--force", name]);
}

fn write_file(dir: &Path, name: &str, body: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(name);
    let mut file = std::fs::File::create(&path)?;
    file.write_all(body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
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
    } else if joined.len() > 240 {
        format!("{}…", &joined[..240])
    } else {
        joined
    }
}
