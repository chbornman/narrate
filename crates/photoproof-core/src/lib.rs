//! Photoproof core: the append-only journal engine.
//!
//! Normative contracts live in `spec/` at the repository root; this crate
//! implements them. No Tauri, no UI dependencies — drivable from tests and
//! a future CLI.

pub mod id;
