//! Implements S01 — ids, handles, arenas, rng, math re-exports, error types.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! This half of S01 is the determinism-critical part: coordinates, the integer
//! clock, positional hashing, and per-system RNG streams. Nothing here is
//! order-dependent, nothing here panics, and nothing here allocates on a hot
//! path.

// The sim lint set bans `unwrap`, `expect`, `panic!`, and unchecked indexing,
// because a sim crate must not panic in release. Tests are the documented
// exception (`03-conventions.md`): a test that cannot assert is not a test, and
// a panicking assertion there is the intended failure mode.
#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod hash;
pub mod math;
pub mod rng;
pub mod time;

pub use hash::{hash_block, hash_position, mix64};
pub use math::{
    BlockCoord, CELL_SIZE, CELLS_PER_CHUNK, CELLS_PER_CHUNK_EDGE, CHUNK_SIZE, CellCoord,
    ChunkCoord, TILE_CELLS, TILES_PER_CHUNK, TileCoord, WorldPos, glam,
};
pub use rng::{RngStream, StreamId};
pub use time::{Fixed, TICK_US, Tick};
