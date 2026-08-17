//! Implements S14 (minimal) — state hashing and the determinism harness.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # Why this exists at M0 rather than M7
//!
//! S14 is an M7 spec, but its state hashing lands now on purpose: a determinism
//! bug introduced while the engine is four crates large is findable in an
//! afternoon, and the same bug found at M7 means bisecting through a year of
//! commits with no way to tell which tick first went wrong.
//!
//! What is here is the hashing and the comparison harness. The inspector, the
//! query console, metrics, and the field overlay are all still M7.
//!
//! # The two properties
//!
//! **Order independence.** Entity iteration order is unspecified and varies with
//! thread count; per-entity hashes therefore combine commutatively. See
//! [`hash`] for why that combine is `wrapping_add` and not XOR.
//!
//! **Configuration awareness.** A hash carries its module-set fingerprint
//! (`ADR-0012`), so comparing across configurations fails loudly instead of
//! reporting a divergence that is really a different world.

#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod determinism;
pub mod hash;

pub use determinism::{Divergence, HashSequence, Scenario, compare_thread_counts, run_scenario};
pub use hash::{StateHash, StateHashable, StateHasher};
