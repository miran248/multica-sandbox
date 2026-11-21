//! `msbx` — one sandbox namespace per epic.
//!
//! This crate is the engine of the per-epic sandbox: the Compose lifecycle, the
//! epic namespace and its generated override, the manifest reader, the project
//! value store and the epic envelope renderer, the loopback port-block allocator
//! and the `msbx.test` resolver. The `msbx` binary is the command surface;
//! tunnels are a later stage.

#![forbid(unsafe_code)]

pub mod dns;
pub mod doctor;
pub mod driver;
pub mod env;
pub mod epic;
pub mod init;
pub mod lifecycle;
pub mod manifest;
pub mod platform;
pub mod ports;
pub mod state;
pub mod store;
