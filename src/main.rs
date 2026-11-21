//! `msbx` — one sandbox namespace per scope.

#![forbid(unsafe_code)]

use multica_sandbox::{dns, doctor, init, lifecycle};
use std::process::ExitCode;

const USAGE: &str = "\
msbx — one sandbox namespace per scope

Usage:
  msbx up [--scope S] [--project P] [--file F]   Start or reconcile the scope box
  msbx down [--scope S] [--project P]            Stop the box, keep its volumes
  msbx rm [--scope S] [--project P] [--yes]      Destroy the box and its volumes
  msbx status [--scope S] [--project P] [--json] Show boxes, health and ports
  msbx gc [--dry-run]                            Final-state and idle sweep
  msbx env ls|export [--scope S] [--project P]   Names, or the host-context exports
  msbx doctor                                    Diagnose the Docker host; changes nothing
  msbx init [--check] [--keep]                   Set up the host, then run the driver checks
  msbx dns [--listen ADDR]                       Serve the msbx.test resolver
  msbx --version
  msbx --help

`up` reads `.multica/compose.yaml` (or `--file`), refuses a checkout bind and a
service `build:` inside the checkout, resolves the manifest's names from the
host, project and harness stores, gives the scope its port block, writes the
generated envelope and override outside the checkout and starts the Compose
project. The scope is `--scope`, `MSBX_SCOPE`, the daemon task context or the
worktree branch; `--epic` and `MSBX_EPIC` are retired and name the replacement.

`env ls` prints the harness envelope's names without values; `env export`
prints `env/host.env`, the host context with the assigned loopback ports. Both
read what the last `up` rendered. A final ticket box (done, cancelled, archived
or gone) and an idle chat or ticket box are removed by `gc`, which `--dry-run`
only lists.

`init` installs the msbx.test resolver and the msbx dns unit (or prints the one
privileged command it needs), then runs C1-C7 against a synthetic two-scope
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
