//! `msbx doctor` — describe a Docker host without changing it.
//!
//! Reports the Engine and Compose versions, daemon reachability, the
//! `msbx.test` resolver file and whether `msbx dns` answers, the harness image
//! pin, the recorded boxes with their live leases, stale leases and orphan
//! harness containers, and any leftovers from the dnsmasq era (`hours.test` /
//! `signals.test`, a stray resolver file, a running dnsmasq). Docker problems
//! are failures; host setup that `init` repairs is a warning.

use crate::dns;
use crate::driver;
use crate::image::Pin;
use crate::lease;
use crate::platform;
use crate::ports::{Allocator, Block};
use crate::state;
use std::process::ExitCode;

const ZONE: &str = platform::ZONE;

/// The minimum Engine version C1 accepts.
pub const MIN_ENGINE: (u32, u32) = (24, 0);
/// The minimum Compose version C1 accepts.
pub const MIN_COMPOSE: (u32, u32) = (2, 24);

pub fn run() -> ExitCode {
    let mut failures = 0u32;
    let mut warnings = 0u32;

    println!("msbx doctor\n");

    match driver::engine_version() {
        Ok(version) if driver::version_at_least(&version, MIN_ENGINE.0, MIN_ENGINE.1) => {
            println!("[ok]   Docker Engine {version}");
        }
        Ok(version) => {
            println!(
                "[fail] Docker Engine {version} is older than {}.{}",
                MIN_ENGINE.0, MIN_ENGINE.1
            );
            failures += 1;
        }
        Err(error) => {
            println!(
                "[fail] Docker Engine unreachable: {error}\n         install Docker and start the daemon"
            );
            failures += 1;
        }
    }

    match driver::compose_version() {
        Ok(version) if driver::version_at_least(&version, MIN_COMPOSE.0, MIN_COMPOSE.1) => {
            println!("[ok]   Docker Compose {version}");
        }
        Ok(version) => {
            println!(
                "[fail] Docker Compose {version} is older than {}.{}",
                MIN_COMPOSE.0, MIN_COMPOSE.1
            );
            failures += 1;
        }
        Err(error) => {
            println!("[fail] Docker Compose missing: {error}");
            failures += 1;
        }
    }

    match driver::daemon_version() {
        Some(version) => println!("[ok]   Docker daemon reachable (server {version})"),
        None => {
            println!("[fail] Docker daemon is not reachable (`docker info`)");
            failures += 1;
        }
    }

    match platform::os() {
        platform::Os::MacOs => println!("[ok]   macOS {}", driver::macos_version()),
        platform::Os::Linux => {
            let out = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
            let pretty = out
                .lines()
                .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                .map(|value| value.trim_matches('"').to_string())
                .unwrap_or_else(|| "Linux".to_string());
            println!("[ok]   {pretty}");
        }
        platform::Os::Other => {
            println!("[warn] unsupported host; only macOS and Linux are driven");
            warnings += 1;
        }
    }

    if platform::resolver_installed() {
        println!(
            "[ok]   {ZONE} resolver installed ({})",
            platform::resolver_file().display()
        );
    } else {
        println!(
            "[warn] {ZONE} resolver missing ({}); run `msbx init`",
            platform::resolver_file().display()
        );
        warnings += 1;
    }

    let address = platform::DNS_ADDR.parse().expect("static resolver address");
    if dns::answers(address) {
        println!(
            "[ok]   msbx dns answers at {} for {ZONE}",
            platform::DNS_ADDR
        );
    } else {
        println!(
            "[warn] no msbx dns answer at {}; run `msbx init` or start the unit",
            platform::DNS_ADDR
        );
        warnings += 1;
    }

    match platform::dns_unit_active() {
        Some(true) => println!("[ok]   msbx dns unit is running"),
        Some(false) if platform::dns_unit_installed() => {
            println!("[warn] the msbx dns unit is installed but not running");
            warnings += 1;
        }
        Some(false) => {
            println!("[warn] no msbx dns unit installed; run `msbx init`");
            warnings += 1;
        }
        None => {}
    }

    match Allocator::default_path().store().load() {
        Ok(state) if state.blocks.is_empty() => println!("[ok]   no scope boxes recorded"),
        Ok(state) => {
            println!("[ok]   {} scope box(es) recorded", state.blocks.len());
            for (key, entry) in &state.blocks {
                let block = Block { start: entry.start };
                println!(
                    "         {key}  scope {}  block {block}",
                    entry.scope.as_deref().unwrap_or("-")
                );
            }
        }
        Err(error) => {
            println!("[warn] cannot read the box state: {error}");
            warnings += 1;
        }
    }

    match Pin::load() {
        Ok(pin) => println!(
            "[ok]   harness image pinned ({}@{}), Pi {}",
            pin.image, pin.digest, pin.pi
        ),
        Err(error) => {
            println!("[warn] {error}");
            warnings += 1;
        }
    }

    let stale = lease::stale_all(state::now()).len();
    if stale > 0 {
        println!("[warn] {stale} stale lease(s); run `msbx gc`");
        warnings += 1;
    } else {
        println!("[ok]   no stale leases");
    }

    match lease::sweep_containers(None, true) {
        Ok(orphans) if orphans.is_empty() => println!("[ok]   no orphan harness containers"),
        Ok(orphans) => {
            println!(
                "[warn] {} orphan harness container(s); run `msbx gc`: {}",
                orphans.len(),
                orphans.join(", ")
            );
            warnings += 1;
        }
        Err(error) => {
            println!("[warn] cannot list harness containers: {error}");
            warnings += 1;
        }
    }

    for leftover in platform::leftovers() {
        println!(
            "[warn] leftover resolver entry {leftover} — retire it; msbx.test is the only zone"
        );
        warnings += 1;
    }

    if platform::dnsmasq_running() {
        println!("[warn] a dnsmasq process is still running; {ZONE} does not need it");
        warnings += 1;
    }

    println!();
    if failures > 0 {
        println!("{failures} failure(s), {warnings} warning(s)");
        ExitCode::from(1)
    } else {
        println!("0 failures, {warnings} warning(s)");
        ExitCode::SUCCESS
    }
}
