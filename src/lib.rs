//! `msbx` — one sandbox namespace per scope.
//!
//! This crate is the engine of the per-scope sandbox: the Compose lifecycle, the
//! scope namespace and its generated override, the manifest reader, the three
//! value layers and the scope envelope renderer, the loopback port-block
//! allocator, the generated tunnel sidecars, scope resolution, the
//! `msbx.test` resolver, the harness image pin and the run leases. The `msbx`
//! binary is the command surface and `msbx-run` is the container shim.

#![forbid(unsafe_code)]

pub mod dns;
pub mod doctor;
pub mod driver;
pub mod env;
pub mod image;
pub mod init;
pub mod lease;
pub mod lifecycle;
pub mod manifest;
pub mod platform;
pub mod ports;
pub mod resolve;
pub mod scope;
pub mod state;
pub mod store;
pub mod wrapper;
