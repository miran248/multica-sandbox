//! `msbx-run` — the Multica runtime profile's container shim.
//!
//! It resolves the scope and the project, takes a lease, starts the scope's box
//! when the checkout carries a manifest, seeds the agent dir and runs Pi in one
//! container from the pinned image with the original arguments, stdin, stdout,
//! stderr and exit status. See [`multica_sandbox::wrapper`].

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    multica_sandbox::wrapper::run(&args)
}
