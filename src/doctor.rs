//! `msbx doctor` — what the host must provide before a box can run.

use crate::driver;
use std::path::Path;
use std::process::ExitCode;

const RESOLVER: &str = "/etc/resolver/msbx.test";
const RETIRED_RESOLVER: &str = "/etc/resolver/test";

pub fn run() -> ExitCode {
    let mut failures = 0u32;
    let mut warnings = 0u32;

    println!("msbx doctor\n");

    let version = driver::macos_version();
    match driver::macos_major() {
        Some(major) if major >= 26 => println!("[ok]   macOS {version}"),
        Some(major) => {
            println!("[warn] macOS {version}: the Apple container driver needs 26 or newer (found {major})");
            warnings += 1;
        }
        None => {
            println!("[fail] cannot read the macOS version (`sw_vers -productVersion`)");
            failures += 1;
        }
    }

    if driver::has("container") {
        let cli = driver::run("container", &["--version"]);
        println!("[ok]   container {}", cli.stdout.trim());
    } else {
        println!("[fail] `container` is not on PATH");
        failures += 1;
    }

    if driver::has("container") {
        let status = driver::run("container", &["system", "status"]);
        let running = status.ok || driver::run("container", &["ls"]).ok;
        if running {
            println!("[ok]   container system is running");
        } else {
            println!(
                "[fail] container system is not running (`container system start`): {}",
                one_line(&status.text())
            );
            failures += 1;
        }
    }

    if Path::new(RESOLVER).exists() {
        println!("[ok]   {RESOLVER} is installed");
    } else {
        println!(
            "[warn] {RESOLVER} is missing; run once: sudo container system dns create msbx.test"
        );
        warnings += 1;
    }

    if Path::new(RETIRED_RESOLVER).exists() {
        println!("[warn] {RETIRED_RESOLVER} still routes .test to the retired dnsmasq");
        warnings += 1;
    }

    if driver::run("pgrep", &["-x", "dnsmasq"]).ok {
        println!(
            "[warn] a dnsmasq process is running; retire it in the same step as {RETIRED_RESOLVER}"
        );
        warnings += 1;
    }

    println!();
    if failures > 0 {
        println!("{failures} check(s) failed, {warnings} warning(s)");
        ExitCode::from(1)
    } else {
        println!("0 failures, {warnings} warning(s)");
        ExitCode::SUCCESS
    }
}

fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}
