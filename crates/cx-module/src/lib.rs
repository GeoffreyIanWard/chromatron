//! Implements S20 — Module trait, capability registry, resolution, profiles.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # The rule this crate exists to enforce
//!
//! **Modules depend on capabilities, never on other modules** (`ADR-0012`).
//! Navigation does not depend on hydrology; it optionally consumes
//! [`cap::SURFACE_WATER`] and declares what it does when nothing provides that.
//!
//! Capability indirection is what converts "disabled" from a hazard into a
//! supported configuration. With direct module references, disabling hydrology
//! means every consumer either breaks or carries an untested branch. With
//! capabilities, each consumer has declared in advance what it does without
//! water, and CI runs that configuration.
//!
//! # Degradation resolves once, at startup
//!
//! An absent capability means the consuming system is **not scheduled**, or is
//! scheduled in a null-provider variant chosen once. There is no
//! `if let Some(water)` in a hot loop, and a disabled module costs exactly zero
//! per-tick time and zero bytes. [`Resolved::field_bytes_per_cell`] and
//! [`Resolved::contains_system`] are how the M0 gate measures that claim rather
//! than trusting it.
//!
//! # What is not composable
//!
//! The thirteen tick phases (`cx_ecs::Phase`). Modules insert systems into
//! phases; they cannot add, remove, or reorder them. That is the ordering
//! contract that makes parallel execution safe — if phases were composable,
//! determinism would depend on module load order.

#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod capability;
pub mod error;
pub mod graph;
pub mod module;
pub mod profile;
pub mod registry;
pub mod resolved;

pub use capability::{Capability, Degradation, cap};
pub use error::ModuleError;
pub use graph::{SCHEMA_VERSION, export, writers_of};
pub use module::{
    Access, FieldAccess, FieldDecl, Module, ModuleId, Registrar, SystemDecl, Version,
};
pub use profile::Profile;
pub use registry::Registry;
pub use resolved::{ModuleRecord, Resolved, SystemRecord};
