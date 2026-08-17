//! Implements S13 — snapshots, deltas, migrations, replay logs.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`,
//! `kira`, `egui`, or any crate below the firewall. Enforced by
//! `tools/ci-checks`.
//!
//! Spec: `docs/specs/S13-`*. Not yet implemented.
