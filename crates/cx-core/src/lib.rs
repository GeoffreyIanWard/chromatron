//! Implements S01 — ids, handles, arenas, rng, math re-exports, error types.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # What lives here
//!
//! Primitives every other crate builds on — small, boring, and load-bearing. The
//! reason this crate is worth reading carefully despite its size is that a
//! mistake here is inherited by every spec downstream.
//!
//! | Module | Contents |
//! |---|---|
//! | [`math`] | Coordinates, units, and the `glam` re-export |
//! | [`time`] | [`Tick`] and [`Fixed`], the integer-microsecond delta type |
//! | [`hash`] | [`hash_position`], the basis of all worldgen (`ADR-0006`) |
//! | [`rng`] | [`RngStream`] — per-system streams, no global RNG |
//! | [`handle`] | [`Handle`] and [`Arena`], generational storage |
//! | [`intern`] | [`Id`] and the order-independent [`SymbolTable`] |
//! | [`config`] | Layered config with all-errors-at-once validation |
//! | [`error`] | [`CoreError`], [`Located`], [`ErrorReport`] |
//! | [`log`] | The per-tick tracing span |
//!
//! # Three properties worth knowing before using any of it
//!
//! **Nothing here is order-dependent.** Arena iteration is by slot, interning
//! assigns ids by sorted position, and RNG streams are keyed rather than
//! sequential. Each of those is a deliberate choice in service of `ADR-0004`,
//! and each has a test that fails if it regresses.
//!
//! **Nothing here panics.** Sim crates do not panic in release
//! (`03-conventions.md`), so fallible operations return `Option` or `Result`,
//! and even genuinely broken internal invariants degrade rather than abort.
//!
//! **Nothing here allocates on a hot path.** Handles are 8 bytes and `Copy`,
//! arenas are preallocated with [`Arena::with_capacity`], and the RNG holds a
//! single `u64` of state.

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

pub mod config;
pub mod error;
pub mod handle;
pub mod hash;
pub mod intern;
pub mod log;
pub mod math;
pub mod rng;
pub mod time;

pub use config::{Config, ConfigValue, Layer};
pub use error::{CoreError, ErrorReport, Located};
pub use handle::{Arena, Handle};
pub use hash::{hash_block, hash_position, mix64};
pub use intern::{Id, Interner, SymbolTable};
pub use math::{
    BlockCoord, CELL_SIZE, CELLS_PER_CHUNK, CELLS_PER_CHUNK_EDGE, CHUNK_SIZE, CellCoord,
    ChunkCoord, TILE_CELLS, TILES_PER_CHUNK, TileCoord, WorldPos, glam,
};
pub use rng::{RngStream, StreamId};
pub use time::{Fixed, TICK_US, Tick};
