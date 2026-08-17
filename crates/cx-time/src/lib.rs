//! Implements S03 — tick clock, accumulator, speed control, loop driver.
//!
//! Above the firewall: this crate must not depend on `wgpu`, `winit`, `kira`,
//! `egui`, or any crate below the firewall. Enforced by `tools/ci-checks`.
//!
//! # One loop, three masters
//!
//! A windowed game at 60+ fps, a debug session stepping one tick at a time, and
//! a headless batch run at 10,000x are the same code path with different
//! drivers. Everything that decides *how many ticks happen* lives in
//! [`TickClock`], so the drivers cannot disagree — which is what makes S03's
//! "identical state hashes under both drivers" checkable rather than hopeful.
//!
//! # Integer time, always
//!
//! Tick duration is `u64` microseconds and the accumulator is integer division
//! (`03-conventions.md`). A float accumulator drifts, and drift in a
//! deterministic simulation is divergence. The speed multiplier is a float
//! because it is a user-facing setting, but it lands back on integers in the
//! same expression.
//!
//! # The rate is world identity
//!
//! Tick rate is configurable within 10-120 Hz, defaulting to 30. It is recorded
//! in saves and replay logs alongside the module set, and a replay at a
//! different rate refuses rather than diverging: the same command stream at
//! 10 Hz and 30 Hz is not the same run.

#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod clock;
pub mod control;
pub mod driver;
pub mod error;

pub use clock::{CatchUp, MAX_CATCHUP, MAX_FRAME_DELTA_US, TickClock, TickRate};
pub use control::TimeControl;
pub use driver::{HeadlessDriver, PacedDriver, RunReport, StopReason};
pub use error::TimeError;
