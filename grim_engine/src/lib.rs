//! Library surface for the minimal Grim intro runner.
//!
//! This crate exposes the thin API used by the binary:
//! - `parse_args` / `EngineArgs` for CLI parsing,
//! - `run_intro` to execute the minimal boot path,
//! - `lua_host` internals, including `pre_system`/`boot_entry` bindings and telemetry.
//!
//! Module map:
//! - `lua_host::context::bindings::pre_system`: pre-`_system.lua` setup (legacy globals, search paths, stubs).
//! - `lua_host::context::bindings::boot_entry`: post-`_system.lua` helpers around the `BOOT` entrypoint.
//! - `lua_host::telemetry`: retail-aligned semantic logging used throughout the host.

pub use crate::cli::{parse_args, EngineArgs};
pub use crate::runtime::run_intro;

pub mod cli;
pub mod lua_host;
pub mod runtime;
