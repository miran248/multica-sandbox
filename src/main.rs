//! `msbx` — one sandbox namespace per epic.

#![forbid(unsafe_code)]

use multica_sandbox::{dns, doctor, init, ports, state};
use std::process::ExitCode;

const USAGE: &str = "\
msbx — one sandbox namespace per epic

Usage:
  msbx doctor                   Diagnose the Docker host; changes nothing
  msbx init [--check] [--keep]  Set up the host, then run the driver checks
  msbx dns [--listen ADDR]      Serve the msbx.test resolver
  msbx gc [--dry-run]           Release port blocks idle for 14 days
  msbx --version
  msbx --help

`init` installs the msbx.test resolver and the msbx dns unit (or prints the one
privileged command it needs), then runs C1-C7 against a synthetic two-epic
project. `--check` installs nothing. `--keep` keeps the containers it created.

Exit codes for init: 0 all checks passed, 1 a check failed, 2 one or more checks
were skipped. Any non-zero exit means the run is not a pass.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V") | Some("--version") | Some("version") => {
            println!("msbx {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("doctor") => doctor::run(),
        Some("init") => init::run(&args[1..]),
        Some("dns") => run_dns(&args[1..]),
        Some("gc") => run_gc(&args[1..]),
        Some(other) => {
            eprintln!("msbx: unknown command `{other}`");
            eprint!("\n{USAGE}");
            ExitCode::from(64)
        }
    }
}

fn run_dns(args: &[String]) -> ExitCode {
    let listen = match args {
        [] => dns::LISTEN.to_string(),
        [flag, value] if flag == "--listen" => value.clone(),
        _ => {
            eprintln!("msbx dns: unexpected arguments (usage: msbx dns [--listen ADDR])");
            return ExitCode::from(64);
        }
    };
    let address = match listen.parse() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("msbx dns: `{listen}` is not an address: {error}");
            return ExitCode::from(64);
        }
    };
    match dns::serve(address) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("msbx dns: {error}");
            ExitCode::from(1)
        }
    }
}

fn run_gc(args: &[String]) -> ExitCode {
    let dry_run = match args {
        [] => false,
        [flag] if flag == "--dry-run" => true,
        _ => {
            eprintln!("msbx gc: unexpected arguments (usage: msbx gc [--dry-run])");
            return ExitCode::from(64);
        }
    };

    let allocator = ports::Allocator::default_path();
    let released = match allocator.release_idle(ports::IDLE_SECS, state::now(), dry_run) {
        Ok(released) => released,
        Err(error) => {
            eprintln!("msbx gc: {error}");
            return ExitCode::from(1);
        }
    };
    if released.is_empty() {
        println!("nothing to release");
        return ExitCode::SUCCESS;
    }
    for (key, block) in &released {
        if dry_run {
            println!("would release {key} ({block})");
        } else {
            println!("released {key} ({block})");
        }
    }
    ExitCode::SUCCESS
}
