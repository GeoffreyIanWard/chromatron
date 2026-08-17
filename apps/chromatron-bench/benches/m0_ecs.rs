//! M0 ECS gates (S02) — `ecs_iterate_1m_2comp`, `ecs_tick_1m_3systems`,
//! `ecs_spawn_batch_100k_speedup`.
//!
//! These are the first real callers of `cx-ecs`, so the API they use is a
//! proposal as much as a measurement. Two properties are being asserted by the
//! shape of this code, not just by its timings:
//!
//! 1. A system cannot be registered without naming a phase (S02 requires this to
//!    be a compile error, so there is deliberately no overload that omits it).
//! 2. Structural change goes through `SimCommands` and lands in
//!    `StructuralApply` — never mid-iteration.

use std::hint::black_box;
use std::time::Instant;

use chromatron_bench::{BENCH_THREADS, gate, targets};
use criterion::{Criterion, criterion_group, criterion_main};
use cx_core::glam::Vec3;
use cx_ecs::{Component, Phase, Query, SimSchedule, SimWorld, WorldConfig};

/// Four components per entity, as the gate specifies. Deliberately plain data:
/// this measures archetype iteration, not user logic.
#[derive(Component, Clone, Copy, Debug)]
struct Position(Vec3);

#[derive(Component, Clone, Copy, Debug)]
struct Velocity(Vec3);

#[derive(Component, Clone, Copy, Debug)]
struct Hunger(f32);

#[derive(Component, Clone, Copy, Debug)]
struct Age(u32);

const ENTITY_COUNT: usize = 1_000_000;
const SPAWN_BATCH_COUNT: usize = 100_000;

fn world_with_entities(count: usize, threads: usize) -> SimWorld {
    let mut world = SimWorld::new(WorldConfig {
        threads,
        ..WorldConfig::default()
    });

    // Bulk spawn: per-entity loops are a performance bug at this scale
    // (03-conventions.md), and the speedup gate below measures exactly that.
    world.spawn_batch((0..count).map(|i| {
        let f = i as f32;
        (
            Position(Vec3::new(f, 0.0, f)),
            Velocity(Vec3::new(0.0, 1.0, 0.0)),
            Hunger(0.5),
            Age(0),
        )
    }));

    world
}

/// `ecs_iterate_1m_2comp` — < 3 ms, single-threaded.
///
/// Two of the four components are touched, so the archetype is wider than the
/// query. That is the realistic case and the one that exposes a bad memory
/// layout; querying all four would read contiguously and flatter the result.
fn bench_iterate_1m_2comp(c: &mut Criterion) {
    let mut world = world_with_entities(ENTITY_COUNT, 1);

    // Built once and reused: constructing a QueryState walks the archetype list,
    // and a benchmark that rebuilt it every iteration would measure that instead
    // of iteration.
    let mut query = world.query::<(&mut Position, &Velocity)>();

    let mut group = c.benchmark_group("ecs_iterate_1m_2comp");
    group.sample_size(50);
    group.bench_function("1_thread", |b| {
        b.iter(|| {
            for (mut position, velocity) in query.iter_mut(world.inner_mut()) {
                position.0 += velocity.0;
            }
            black_box(&world);
        });
    });
    group.finish();

    gate::assert_within(
        "ecs_iterate_1m_2comp",
        gate::measured_mean("ecs_iterate_1m_2comp/1_thread"),
        targets::ECS_ITERATE_1M,
    );
}

/// `ecs_tick_1m_3systems` — < 33 ms on 8 threads.
///
/// 33 ms is not an arbitrary round number: it is one tick at the default 30 Hz
/// (`TICK_US = 33_333`). Missing it means the sim cannot keep real time with a
/// million entities and three trivial systems, before any real work exists.
fn bench_tick_1m_3systems(c: &mut Criterion) {
    fn integrate_positions(mut query: Query<(&mut Position, &Velocity)>) {
        for (mut position, velocity) in query.iter_mut() {
            position.0 += velocity.0;
        }
    }

    fn accumulate_hunger(mut query: Query<&mut Hunger>) {
        for mut hunger in query.iter_mut() {
            hunger.0 = (hunger.0 + 0.001).min(1.0);
        }
    }

    fn advance_age(mut query: Query<&mut Age>) {
        for mut age in query.iter_mut() {
            age.0 = age.0.saturating_add(1);
        }
    }

    let mut world = world_with_entities(ENTITY_COUNT, BENCH_THREADS);

    // Phase is mandatory. S02: registering a system without one fails to
    // compile, so there is no argument-less variant to reach for.
    let mut schedule = SimSchedule::new();
    schedule.add_system(Phase::AgentAct, integrate_positions);
    schedule.add_system(Phase::AgentDecide, accumulate_hunger);
    schedule.add_system(Phase::Diagnostics, advance_age);

    let mut group = c.benchmark_group("ecs_tick_1m_3systems");
    group.sample_size(30);
    group.bench_function("8_threads", |b| {
        b.iter(|| {
            schedule.run(&mut world);
            black_box(&world);
        });
    });
    group.finish();

    gate::assert_within(
        "ecs_tick_1m_3systems",
        gate::measured_mean("ecs_tick_1m_3systems/8_threads"),
        targets::ECS_TICK_1M,
    );
}

/// `ecs_spawn_batch_100k_speedup` — `spawn_batch` at least 20x a `spawn` loop.
///
/// This is a ratio rather than a duration, so it is measured directly instead of
/// through criterion: the two sides must run under identical conditions, and
/// what matters is the quotient, not either absolute number.
///
/// The gate exists because archetype moves dominate cost in an archetypal ECS
/// (`ADR-0001`). If the ratio collapses, either the batch path is not reserving
/// archetype capacity or the loop path is accidentally fast — and both are worth
/// knowing before agents arrive at M6.
fn bench_spawn_batch_speedup(c: &mut Criterion) {
    let mut group = c.benchmark_group("ecs_spawn_batch_100k");

    group.bench_function("spawn_batch", |b| {
        b.iter_batched(
            || SimWorld::new(WorldConfig::default()),
            |mut world| {
                world.spawn_batch(
                    (0..SPAWN_BATCH_COUNT).map(|i| (Position(Vec3::splat(i as f32)), Age(0))),
                );
                world
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.bench_function("spawn_loop", |b| {
        b.iter_batched(
            || SimWorld::new(WorldConfig::default()),
            |mut world| {
                for i in 0..SPAWN_BATCH_COUNT {
                    world.spawn((Position(Vec3::splat(i as f32)), Age(0)));
                }
                world
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.finish();

    let batched = time_once(|| {
        let mut world = SimWorld::new(WorldConfig::default());
        world
            .spawn_batch((0..SPAWN_BATCH_COUNT).map(|i| (Position(Vec3::splat(i as f32)), Age(0))));
        black_box(world);
    });
    let looped = time_once(|| {
        let mut world = SimWorld::new(WorldConfig::default());
        for i in 0..SPAWN_BATCH_COUNT {
            world.spawn((Position(Vec3::splat(i as f32)), Age(0)));
        }
        black_box(world);
    });

    let speedup = looped.as_secs_f64() / batched.as_secs_f64();
    assert!(
        speedup >= targets::SPAWN_BATCH_SPEEDUP,
        "gate ecs_spawn_batch_100k_speedup: {speedup:.1}x is below the required {:.0}x in \
         docs/bench/baselines.md#m0 (batch {batched:?} vs loop {looped:?}).",
        targets::SPAWN_BATCH_SPEEDUP
    );
}

fn time_once(body: impl FnOnce()) -> std::time::Duration {
    let start = Instant::now();
    body();
    start.elapsed()
}

criterion_group!(
    m0_ecs,
    bench_iterate_1m_2comp,
    bench_tick_1m_3systems,
    bench_spawn_batch_speedup
);
criterion_main!(m0_ecs);
