//! Implements S20 — facade: assembles the sim crates into a runnable simulation.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`,
//! `kira`, `egui`, or any crate below the firewall. Enforced by
//! `tools/ci-checks`.
//!
//! Spec: `docs/specs/S20-`*. Not yet implemented.
