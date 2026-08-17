//! Implements S20 — facade: assembles the sim crates into a runnable simulation.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`,
//! `kira`, `egui`, or any crate below the firewall. Enforced by
//! `tools/ci-checks`.
//!
//! # What is here
//!
//! The curated profiles (S20). A profile names actual modules, so it cannot live
//! in `cx-module` without inverting the dependency `ADR-0012` exists to protect.
//! Assembling a runnable simulation from a resolved profile is the rest of this
//! crate's job and arrives with the subsystems it would assemble.

// The sim lint set bans `unwrap`, `expect`, `panic!`, and unchecked indexing,
// because a sim crate must not panic in release. Tests are the documented
// exception (`03-conventions.md`).
#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod profile;

pub use profile::{NAMES, by_name};
