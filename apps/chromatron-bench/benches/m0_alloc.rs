//! M0 allocation gate — `alloc_per_tick_steady_state`, target exactly zero.
//!
//! `03-conventions.md`: no allocation inside per-tick systems; scratch buffers
//! are preallocated in resources and reused.
//!
//! This is the gate most likely to be quietly broken by an innocuous change, and
//! the one whose failure is hardest to attribute after the fact — an allocation
//! per tick is invisible at 30 Hz and ruinous at 10,000x time acceleration
//! (S03), which is exactly the configuration nobody is profiling when the change
//! lands.
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

fn integrate_positions(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut position, velocity) in query.iter_mut() {
        position.0 += velocity.0;
    }
}

fn bench_alloc_per_tick_steady_state(_c: &mut Criterion) {
    let mut world = SimWorld::new(WorldConfig {
        threads: BENCH_THREADS,
        ..WorldConfig::default()
    });
    world.spawn_batch(
        (0..ENTITY_COUNT).map(|i| (Position(Vec3::splat(i as f32)), Velocity(Vec3::Y))),
    );

    let mut schedule = SimSchedule::new();
    schedule.add_system(Phase::AgentAct, integrate_positions);

    for _ in 0..WARMUP_TICKS {
        schedule.run(&mut world);
    }

    // Measure a run of ticks rather than one: a buffer that doubles its capacity
    // every N ticks would pass a single-tick check by luck.
    const MEASURED_TICKS: usize = 128;
    let (_, report) = counting_alloc::measure(|| {
        for _ in 0..MEASURED_TICKS {
            schedule.run(&mut world);
        }
    });

    assert_eq!(
        report.allocations,
        targets::ALLOCATIONS_PER_TICK,
        "gate alloc_per_tick_steady_state: {} allocations ({} bytes) across {MEASURED_TICKS} \
         steady-state ticks, target 0 (docs/bench/baselines.md#m0).\n\n\
         Something in the tick is allocating. The usual causes are a `Vec` built per tick \
         instead of a reused scratch buffer in a resource, a boxed trait object on a hot \
         path, or a command buffer that is not being drained and reused across ticks. \
         03-conventions.md has the rule; a heap profile over these ticks has the culprit.",
        report.allocations,
        report.bytes
    );
}

criterion_group!(m0_alloc, bench_alloc_per_tick_steady_state);
criterion_main!(m0_alloc);
