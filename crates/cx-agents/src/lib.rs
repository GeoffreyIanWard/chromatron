//! Implements S10 — agents: sensing, deciding, acting.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # What exists so far
//!
//! **The read-then-write split**, which is the part of S10 everything else is
//! built on: an agent decides in `AgentDecide` and writes only its own
//! [`intent::Intent`]; only `AgentAct` mutates the world. Plus local separation
//! steering against the S05 index, and deterministic resolution of contested
//! claims by `Entity`.
//!
//! S10's flow fields, A* over the region graph, cost grids derived from field
//! data, utility scoring, and agent LOD are M6. Local steering is the bottom
//! tier of S10's own navigation ladder and the only one needing no
//! infrastructure that does not exist yet.
//!
//! The phase split and the tiebreak are here early on purpose: both are cheap
//! now and expensive once a content set depends on them.

// The sim lint set bans `unwrap`, `expect`, `panic!`, unchecked indexing, and
// float equality, because a sim crate must not panic in release. Tests are the
// documented exception (`03-conventions.md`).
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

pub mod behaviour;
pub mod intent;
pub mod module;

pub use behaviour::{TickSeconds, apply_intents, decide_steering, resolve_claims};
pub use intent::{Agent, Claimable, Intent, SenseRadius};
pub use module::AgentsModule;
