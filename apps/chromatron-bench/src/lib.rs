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
    /// `ecs_spawn_batch_100k_speedup` — >= 1.4x versus a `spawn` loop.
    ///
    /// A **tripwire, not a performance target.** It exists so that `spawn_batch`
    /// losing its advantage is noticed, because bulk spawn is the path agent
    /// spawning and chunk activation depend on. Losing the advantage means a
    /// ratio near 1.0; the gap between 1.7 and 1.8 is which machine ran it.
    ///
    /// The margin is deliberate and was earned the hard way. This was 1.75,
    /// picked to sit just under a 1.9x figure measured once on a developer
    /// machine — and 1.75 turned out to be *inside* the range the benchmark
    /// actually produces:
    ///
    /// | Where | Ratio |
    /// |---|---|
    /// | Apple M4 Pro, three consecutive runs | 1.749, 1.772, 1.791 |
    /// | GitHub `ubuntu-latest` | 1.735 |
    ///
    /// A threshold inside the spread is a coin flip, and a gate that fails half
    /// the time is one people learn to rerun rather than read — which is worse
    /// than no gate, because it also teaches them to rerun the ones that mean
    /// something.
    ///
    /// 1.4 sits about 20% below the lowest observed value, which is room for a
    /// slower shared runner, and still fails loudly at the ratio that would
    /// matter.
    pub const SPAWN_BATCH_SPEEDUP: f64 = 1.4;
    /// `extract_100k_instances` — < 2 ms (S12).
    ///
    /// A frame at 144 fps is 6.9 ms, so this budget is already a third of one.
    pub const EXTRACT_100K_INSTANCES: Duration = Duration::from_millis(2);
    /// `alloc_per_tick_sim_code` — exactly zero.
    ///
    /// Measured single-threaded, where the executor contributes nothing, so any
    /// allocation counted is one this project wrote (`ADR-0014`).
    pub const ALLOCATIONS_PER_TICK: u64 = 0;

    /// `alloc_per_tick_executor` — bevy_ecs's multi-threaded executor overhead.
    ///
    /// Measured at 13 per tick with no systems plus one per system; the budget
    /// leaves a little room so ordinary variation does not flake, while still
    /// failing if a bevy_ecs upgrade regresses per-run cost (`ADR-0014`).
    pub const fn executor_allocation_budget(systems: usize) -> u64 {
        16 + systems as u64
    }
}

pub mod counting_alloc;
pub mod gate;
pub mod rss;
