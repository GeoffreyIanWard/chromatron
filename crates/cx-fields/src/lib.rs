//! Implements S06 — chunked SoA field storage, kernels, sampling.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # The dense half of the architecture
//!
//! Sparse entities live in `bevy_ecs`; dense per-cell data lives here
//! (`ADR-0003`). Never model a terrain cell as an entity: a 1024×1024 chunk is
//! 1,048,576 cells, and at ten chunks loaded that would be ten million entities
//! doing work an array kernel does in microseconds.
//!
//! # Three properties everything else depends on
//!
//! **A field that is never written allocates nothing.** Registration records a
//! spec; storage appears on first write. One `f32` field for one chunk is 4 MB,
//! so lazy allocation is what makes the memory budget reachable at all.
//!
//! **Kernels are double-buffered.** They read `front` and write `back`. A kernel
//! reading and writing one array makes each cell's result depend on how far the
//! loop has already progressed — the classic stencil bug, invisible until the
//! output looks subtly directional.
//!
//! **Boundaries are halo, not branches.** Each array carries a border ring
//! copied from neighbours before solving, so the inner loop has no bounds checks
//! and no cross-chunk lookups.

#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod deposit;
pub mod storage;
pub mod store;

pub use deposit::{Deposit, DepositBuffer, DepositOp};
pub use storage::{ChunkField, FieldSpec, Persistence};
pub use store::{FieldId, FieldStore, Kernel, StoreConfig};
