//! `multica-sandbox` — one sandbox namespace per epic.
//!
//! Only the spike verbs are implemented. `up`, `down`, `status`, `exec`, `env`
//! and `secrets` arrive with the spec, after the driver is measured.

#![forbid(unsafe_code)]

mod doctor;
mod driver;
mod spike;

use std::process::ExitCode;

const USAGE: &str = "\
multica-sandbox — one sandbox namespace per epic

Usage:
  multica-sandbox doctor          Check the host: macOS, container CLI, zone files
  multica-sandbox spike [--keep]  Run the driver assertions (needs macOS 26)
  multica-sandbox --version
  multica-sandbox --help

Exit codes for spike: 0 all assertions passed, 1 a failed assertion,
2 one or more assertions were skipped.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V") | Some("--version") | Some("version") => {
            println!("multica-sandbox {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("doctor") => doctor::run(),
        Some("spike") => spike::run(&args[1..]),
        Some(other) => {
            eprintln!("multica-sandbox: unknown command `{other}`");
            eprint!("\n{USAGE}");
            ExitCode::from(64)
        }
    }
}
