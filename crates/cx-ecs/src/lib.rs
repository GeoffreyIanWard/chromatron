//! Implements S02 — bevy_ecs wrapper: registration, schedules, ordering policy.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # Why a wrapper at all
//!
//! `bevy_ecs` is the ECS (`ADR-0001`); this crate is the *policy* around it. It
//! exists to make three rules unavoidable rather than remembered:
//!
//! 1. **Every system declares a phase.** [`SimSchedule::add_system`] takes one,
//!    and there is no overload that does not.
//! 2. **Structural change is deferred.** Spawns, despawns, inserts, and removes
//!    buffer through `Commands` and are flushed at exactly one point,
//!    [`Phase::StructuralApply`]. bevy's automatic flush-point insertion is
//!    switched off so that phase means what `02-architecture.md` says.
//! 3. **Iteration order is not a result.** Plain iteration is unordered and its
//!    callers must be order-independent; [`SimWorld::iter_deterministic`] exists
//!    for the rare case that genuinely needs an order, and costs a sort so the
//!    cost is visible.

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

pub mod phase;
pub mod schedule;
pub mod world;

pub use phase::Phase;
pub use schedule::SimSchedule;
pub use world::{SimWorld, WorldConfig};

/// Deferred structural change.
///
/// The only sanctioned way for a system to spawn, despawn, insert, or remove.
/// Buffered during the tick and applied in [`Phase::StructuralApply`].
pub use bevy_ecs::system::Commands as SimCommands;

/// Re-exported `bevy_ecs` items that appear in system signatures.
///
/// Re-exported rather than depended on directly so the version is pinned in one
/// place, and so a future ECS change has one crate to touch.
pub use bevy_ecs::{
    bundle::Bundle,
    change_detection::{Mut, Ref},
    component::Component,
    entity::Entity,
    query::{Added, Changed, Has, Or, With, Without},
    resource::Resource,
    schedule::IntoScheduleConfigs,
    system::ScheduleSystem,
    system::{Local, Query, Res, ResMut},
};
