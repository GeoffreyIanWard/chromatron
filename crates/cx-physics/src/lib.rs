//! Implements S11 — physics facade.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # The facade is the deliverable; rapier is M8
//!
//! S11 says *adopt, do not write* — `rapier3d`, pinned, with its determinism
//! features enabled, behind a facade so physics types never leak into gameplay
//! and the dependency stays replaceable.
//!
//! What exists is that facade, plus the one case that needs no solver: bodies
//! that fall and rest on the terrain. It is named [`falling::FallingBody`]
//! rather than `RigidBody` on purpose — there are no contacts between entities,
//! no constraints, and no broad phase, and a type that claims more than it does
//! is how a placeholder survives into a release.
//!
//! Three things it establishes that rapier will inherit rather than replace:
//!
//! - **The participation rule.** Only entities with a body are queried, so the
//!   majority of a million-entity simulation never touches physics — S11's
//!   requirement, and far cheaper to establish now than to retrofit.
//! - **The fixed timestep.** Never variable; a trajectory that depended on the
//!   frame rate would diverge between two machines running the same seed.
//! - **The `ELEVATION` read.** The first reader of that field, declared as a
//!   read: `ADR-0011` permits exactly two writers and physics is neither.

// The sim lint set bans `unwrap`, `expect`, `panic!`, unchecked indexing, and
// float equality, because a sim crate must not panic in release. Tests are the
// documented exception (`03-conventions.md`) — and exact float comparison is the
// point in several of them, since a fixed timestep promises bit-identical
// trajectories rather than approximately equal ones.
#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::float_cmp
    )
)]

pub mod falling;
pub mod module;

pub use falling::{FallingBody, PhysicsConfig, Step, place, step};
pub use module::{PhysicsModule, step_bodies};
