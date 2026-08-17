//! M0 allocation gates — `alloc_per_tick_sim_code` and `alloc_per_tick_executor`.
//!
//! `03-conventions.md`: no allocation inside per-tick systems; scratch buffers
//! are preallocated in resources and reused.
//!
//! This is the gate most easily broken by an innocuous change and the hardest to
//! attribute afterwards. An allocation per tick is invisible at 30 Hz and
//! ruinous at 10,000x time acceleration (S03) — which is exactly the
//! configuration nobody is profiling when the change lands.
//!
//! # Why two gates
//!
//! `bevy_ecs`'s multi-threaded executor allocates about 13 times per tick before
//! any system runs, plus one per system, for scope setup and task futures. None
//! of that is ours, and reaching a literal zero would mean forking the ECS
//! (`ADR-0014`).
//!
//! So the strict zero is asserted against the **single-threaded** executor,
//! where anything that allocates is by definition engine code, and the executor
//! gets its own ceiling. A single combined budget would have hidden the failure
//! that matters: five new allocations from a sim system sitting unnoticed inside
//! a budget of twenty.
//!
//! It is a separate binary from the other gates because installing a counting
//! global allocator affects every measurement in the process.

use chromatron_bench::counting_alloc::{self, CountingAllocator};
use chromatron_bench::{BENCH_THREADS, targets};
use criterion::{Criterion, criterion_group, criterion_main};
use cx_core::glam::Vec3;
use cx_ecs::{Component, Phase, Query, SimSchedule, SimWorld, WorldConfig};

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

#[derive(Component, Clone, Copy)]
struct Position(Vec3);

#[derive(Component, Clone, Copy)]
struct Velocity(Vec3);

const ENTITY_COUNT: usize = 100_000;

/// Ticks to run before measuring.
///
/// Startup allocation is legitimate and expected: archetype reservation, scratch
/// buffers sized on first use, chunk activation. The rule is about the steady
/// state, so the gate warms up first and says so rather than pretending tick 0
/// should be allocation-free.
const WARMUP_TICKS: usize = 64;

/// Measured ticks.
///
/// A run rather than a single tick: a buffer that doubles its capacity every N
/// ticks would pass a one-tick check by luck.
const MEASURED_TICKS: usize = 128;

const SYSTEM_COUNT: usize = 3;

fn integrate_positions(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut position, velocity) in query.iter_mut() {
        position.0 += velocity.0;
    }
}

fn decay_velocity(mut query: Query<&mut Velocity>) {
    for mut velocity in query.iter_mut() {
        velocity.0 *= 0.999;
    }
}

fn clamp_height(mut query: Query<&mut Position>) {
    for mut position in query.iter_mut() {
        position.0.y = position.0.y.min(1_000.0);
    }
}

fn build() -> (SimWorld, SimSchedule) {
    let mut world = SimWorld::new(WorldConfig {
        threads: BENCH_THREADS,
        ..WorldConfig::default()
    });
    world.spawn_batch(
        (0..ENTITY_COUNT).map(|i| (Position(Vec3::splat(i as f32)), Velocity(Vec3::Y))),
    );

    let mut schedule = SimSchedule::new();
    schedule.add_system(Phase::AgentAct, integrate_positions);
    schedule.add_system(Phase::AgentDecide, decay_velocity);
    schedule.add_system(Phase::Diagnostics, clamp_height);

    (world, schedule)
}

fn run_measured(
    schedule: &mut SimSchedule,
    world: &mut SimWorld,
) -> counting_alloc::AllocationReport {
    for _ in 0..WARMUP_TICKS {
        schedule.run(world);
    }

    let (_, report) = counting_alloc::measure(|| {
        for _ in 0..MEASURED_TICKS {
            schedule.run(world);
        }
    });

    report
}

/// `alloc_per_tick_sim_code` — exactly zero.
///
/// Single-threaded, so the executor contributes nothing and every allocation
/// counted is one this project wrote (`ADR-0014`).
fn bench_alloc_per_tick_sim_code(_c: &mut Criterion) {
    let (mut world, mut schedule) = build();
    schedule.set_single_threaded();

    let report = run_measured(&mut schedule, &mut world);

    assert_eq!(
        report.allocations,
        targets::ALLOCATIONS_PER_TICK,
        "gate alloc_per_tick_sim_code: {} allocations ({} bytes) across {MEASURED_TICKS} \
         steady-state ticks, target 0 (docs/bench/baselines.md#m0).\n\n\
         Measured single-threaded, so the executor contributes nothing and this is engine \
         code allocating. The usual causes are a Vec built per tick instead of a reused \
         scratch buffer in a resource, a boxed trait object on a hot path, or a command \
         buffer that is not drained and reused across ticks. 03-conventions.md has the rule; \
         a heap profile over these ticks has the culprit.",
        report.allocations,
        report.bytes
    );
}

/// `alloc_per_tick_executor` — bounded, not zero.
///
/// `bevy_ecs`'s multi-threaded executor allocates per run for scope setup and
/// task futures. Not ours to remove without forking the ECS, but worth a ceiling
/// so an upgrade that regresses it is visible (`ADR-0014`).
fn bench_alloc_per_tick_executor(_c: &mut Criterion) {
    let (mut world, mut schedule) = build();

    let report = run_measured(&mut schedule, &mut world);
    let per_tick = report.allocations as f64 / MEASURED_TICKS as f64;
    let budget = targets::executor_allocation_budget(SYSTEM_COUNT) as f64;

    assert!(
        per_tick <= budget,
        "gate alloc_per_tick_executor: {per_tick:.1} allocations per tick exceeds the budget \
         of {budget:.0} for {SYSTEM_COUNT} systems (docs/bench/baselines.md#m0).\n\n\
         This budget covers bevy_ecs's multi-threaded executor only — see ADR-0014. If \
         alloc_per_tick_sim_code still passes, engine code is fine and bevy_ecs's per-run \
         cost has grown, which makes this a deliberate re-baseline rather than a bug to fix \
         here."
    );
}

criterion_group!(
    m0_alloc,
    bench_alloc_per_tick_sim_code,
    bench_alloc_per_tick_executor
);
criterion_main!(m0_alloc);
