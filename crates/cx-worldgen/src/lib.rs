//! Implements S07 — worldgen: positional terrain generation.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # What exists so far
//!
//! **Step 1 of S07's pipeline: base elevation.** A pure function of
//! `(world_seed, position)`, so generating chunk (3, 1) before (0, 0) produces
//! identical terrain to the reverse (`ADR-0006`).
//!
//! Steps 2 to 9 — depression fill, flow routing, hydraulic and thermal erosion,
//! channel carving, biomes, scatter — are **block**-granular and iterative, and
//! are M2 work. This is the surface they start from, which is why terrain from
//! this crate is smooth: it has not been eroded yet, and that should be visible
//! rather than disguised.

// The sim lint set bans `unwrap`, `expect`, `panic!`, unchecked indexing, and
// float equality, because a sim crate must not panic in release. Tests are the
// documented exception (`03-conventions.md`) — and exact float comparison is the
// *point* in several of them: positional generation promises bit-identical
// results, not approximately equal ones.
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

pub mod block;
pub mod elevation;
pub mod flow;
pub mod hydraulic;
pub mod module;

pub use block::{BlockCoordinates, BlockGrid, ErosionCell};
pub use elevation::{ElevationGenerator, TerrainShape};
pub use flow::{FlowNetwork, NO_FLOW};
pub use hydraulic::{ErosionReport, ErosionSettings, erode};
pub use module::{ELEVATION, UNGENERATED, Worldgen, WorldgenModule, generate_elevation};
