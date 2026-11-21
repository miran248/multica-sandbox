//! `msbx-run` — the Multica runtime profile's harness wrapper.
//!
//! It starts the scope's box when the checkout carries a manifest, exports the
//! rendered envelope and execs the real harness with the original arguments and
//! stdin. See [`multica_sandbox::wrapper`].

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    multica_sandbox::wrapper::run(&args)
}
