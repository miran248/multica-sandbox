//! `msbx` — one sandbox namespace per epic.

#![forbid(unsafe_code)]

use multica_sandbox::{dns, doctor, init, lifecycle};
use std::process::ExitCode;

const USAGE: &str = "\
msbx — one sandbox namespace per epic

Usage:
  msbx up [--epic E] [--project P] [--file F]   Start or reconcile the epic box
  msbx down [--epic E] [--project P]            Stop the box, keep its volumes
  msbx rm [--epic E] [--project P] [--yes]      Destroy the box and its volumes
  msbx status [--epic E] [--project P] [--json] Show boxes, health and ports
  msbx gc [--dry-run]                           Remove boxes idle for 14 days
  msbx env ls|export [--epic E] [--project P]   List the harness names, or export them
  msbx doctor                                   Diagnose the Docker host; changes nothing
  msbx init [--check] [--keep]                  Set up the host, then run the driver checks
  msbx dns [--listen ADDR]                      Serve the msbx.test resolver
  msbx --version
  msbx --help

`up` reads `.multica/compose.yaml` (or `--file`), resolves the project's env and
secret names from `$XDG_CONFIG_HOME/msbx/projects/<project>/`, gives the epic its
port block, writes the generated envelope and override outside the checkout and
starts the Compose project. The epic is `--epic` or `MSBX_EPIC`; the wrapper
supplies it for a task.

`env ls` prints the harness envelope's names without values; `env export` prints
shell-safe exports for the harness. Both read what the last `up` rendered.

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
        Some("up") => lifecycle::run_up(&args[1..]),
        Some("down") => lifecycle::run_down(&args[1..]),
        Some("rm") => lifecycle::run_rm(&args[1..]),
        Some("status") => lifecycle::run_status(&args[1..]),
        Some("gc") => lifecycle::run_gc(&args[1..]),
        Some("env") => lifecycle::run_env(&args[1..]),
        Some("doctor") => doctor::run(),
        Some("init") => init::run(&args[1..]),
        Some("dns") => run_dns(&args[1..]),
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
