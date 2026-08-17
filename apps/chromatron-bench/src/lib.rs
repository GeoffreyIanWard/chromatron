//! Criterion benchmarks for the milestone gates in `docs/bench/baselines.md`.
//!
//! Benchmarks here are written *before* the code they measure. M0 exists to try
//! to break the architecture (`docs/milestones/M0-dual-scale-proof.md`), and a
//! gate written afterwards tends to measure whatever the implementation happened
//! to do rather than what the milestone asked for.
//!
//! A second effect matters as much: the benchmark is the first real caller of
//! each API, so signatures get designed against a use site instead of in the
//! abstract.
//!
//! # Reading a failure
//!
//! Every gate names its target from `baselines.md` in its own assertion message.
//! A gate that fails is not a benchmark to be tuned — it is the architecture
//! reporting a problem, and M0 says to stop and revise rather than proceed.

/// Reference hardware thread count from `bench/baselines.md`.
///
/// Fixed rather than read from `num_cpus`: `03-conventions.md` requires the
/// thread count to come from config, because a machine-varying count makes a
/// reproducibility investigation harder than it needs to be.
pub const BENCH_THREADS: usize = 8;

/// Targets from `docs/bench/baselines.md#m0`, in one place so a gate and its
/// documented number cannot drift apart silently.
pub mod targets {
    use std::time::Duration;

    /// `ecs_iterate_1m_2comp` — < 3 ms, 1 thread.
    pub const ECS_ITERATE_1M: Duration = Duration::from_millis(3);
    /// `ecs_tick_1m_3systems` — < 33 ms, 8 threads. One tick at 30 Hz.
    pub const ECS_TICK_1M: Duration = Duration::from_millis(33);
    /// `field_stencil_16m_cells` — < 12 ms, 8 threads.
    pub const FIELD_STENCIL_16M: Duration = Duration::from_millis(12);
    /// `field_halo_exchange_16_chunks` — < 1 ms.
    pub const FIELD_HALO_16_CHUNKS: Duration = Duration::from_millis(1);
    /// `ecs_spawn_batch_100k_speedup` — >= 1.75x versus a `spawn` loop.
    ///
    /// Was 20x, re-baselined against `bevy_ecs` 0.19 where a single spawn costs
    /// about 24 ns and a batched one about 12 ns. See the baseline-changes note
    /// in `docs/bench/baselines.md`.
    pub const SPAWN_BATCH_SPEEDUP: f64 = 1.75;
    /// `alloc_per_tick_steady_state` — exactly zero.
    pub const ALLOCATIONS_PER_TICK: u64 = 0;
}

pub mod counting_alloc;
pub mod gate;
