//! Implements S05 — spatial index: uniform hash, neighbour queries.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # What exists so far
//!
//! The **uniform spatial hash** and its neighbour queries — S05's primary
//! structure. Sorted flat arrays rather than a hash map, because query results
//! reach agent decisions and `ADR-0004` forbids depending on unspecified
//! iteration order.
//!
//! The BVH for static geometry, `raycast`, `sweep`, and the coarse-to-fine path
//! that answers from S09 aggregates are M6 and absent. S05 is an M6 spec; the
//! hash is here early because `cx-agents` needs neighbour queries before then,
//! and because a wrong *ordering* rule is far cheaper to fix before agents
//! depend on it than after.

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

pub mod grid;
pub mod module;

pub use grid::{Found, GridCell, SpatialGrid};
pub use module::{SpatialIndex, SpatialModule, rebuild_index};
