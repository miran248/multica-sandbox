//! `msbx` — one sandbox namespace per epic.
//!
//! This crate is the engine of the per-epic sandbox: the host driver, the
//! Compose manifest reader, the loopback port-block allocator and the
//! `msbx.test` resolver. The `msbx` binary is the command surface; later verbs
//! (`up`, `down`, `rm`, `status`, `env`) are built on the modules here.

#![forbid(unsafe_code)]

pub mod dns;
pub mod doctor;
pub mod driver;
pub mod init;
pub mod manifest;
pub mod platform;
pub mod ports;
pub mod state;
