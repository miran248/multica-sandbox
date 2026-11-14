//! `msbx` — one sandbox namespace per epic.
//!
//! Only the first-run verbs are implemented. `up`, `down`, `status`, `exec`,
//! `env` and `secrets` arrive with the spec, after the driver is measured.

#![forbid(unsafe_code)]

mod doctor;
mod driver;
mod init;

use std::process::ExitCode;

const USAGE: &str = "\
msbx — one sandbox namespace per epic

Usage:
  msbx doctor          Check the host: macOS, container CLI, zone files
  msbx init [--keep]   First-run checks: toolchain, ports, names, mounts
  msbx --version
  msbx --help

Exit codes for init: 0 all checks passed, 1 a check failed,
2 one or more checks were skipped.
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
        Some(other) => {
            eprintln!("msbx: unknown command `{other}`");
            eprint!("\n{USAGE}");
            ExitCode::from(64)
        }
    }
}
