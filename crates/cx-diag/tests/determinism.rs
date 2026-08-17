//! The M0 determinism gates: `determinism_threads_1_4_16` and
//! `determinism_subprocess` (`bench/baselines.md#m0`, S14, `ADR-0004`).
//!
//! These are tests rather than benchmarks because they assert equality, not
//! duration — but they are gates, and CI runs them on every push.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use cx_core::glam::Vec3;
use cx_core::{RngStream, StreamId, Tick};
use cx_diag::{
    Scenario, StateHash, StateHashable, StateHasher, compare_thread_counts, run_scenario,
};
use cx_ecs::{Component, Phase, Query, Res, Resource, SimSchedule, SimWorld};

const TICKS: u64 = 10_000;
const ENTITIES: usize = 2_000;
const WORLD_SEED: u64 = 0xc47_5eed;

#[derive(Component, Clone, Copy)]
struct Position(Vec3);

#[derive(Component, Clone, Copy)]
struct Velocity(Vec3);

#[derive(Component, Clone, Copy)]
struct Energy(f32);

#[derive(Resource, Clone, Copy)]
struct SimTick(u64);

impl StateHashable for Position {
    fn state_hash(&self) -> u64 {
        self.0.state_hash()
    }
}

impl StateHashable for Energy {
    fn state_hash(&self) -> u64 {
        self.0.state_hash()
    }
}

fn integrate(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut position, velocity) in query.iter_mut() {
        position.0 += velocity.0;
    }
}

/// Draws from a per-system stream keyed on the tick, as `03-conventions.md`
/// requires. This is the system most likely to expose a determinism bug: if the
/// stream were global, or keyed on anything the scheduler can reorder, the
/// thread-count comparison would fail here.
fn metabolise(mut query: Query<&mut Energy>, tick: Res<SimTick>) {
    for (index, mut energy) in query.iter_mut().enumerate() {
        let mut rng = RngStream::new(WORLD_SEED, StreamId::Ecology, Tick(tick.0));
        // Keyed on the entity's index within its archetype *and* the tick, so
        // the draw does not depend on how many entities were processed first.
        let jitter = rng.next_range_f32(-0.001, 0.001) * (index % 7) as f32;
        energy.0 = (energy.0 + jitter).clamp(0.0, 100.0);
    }
}

fn advance_tick(mut tick: bevy_ecs::prelude::ResMut<SimTick>) {
    tick.0 += 1;
}

fn build_world(world: &mut SimWorld) {
    world.insert_resource(SimTick(0));
    world.spawn_batch((0..ENTITIES).map(|i| {
        let f = i as f32;
        (
            Position(Vec3::new(f, 0.0, f * 0.5)),
            Velocity(Vec3::new(0.01, 0.0, -0.01)),
            Energy(50.0 + (i % 13) as f32),
        )
    }));
}

fn build_schedule(schedule: &mut SimSchedule) {
    schedule.add_system(Phase::AgentAct, integrate);
    schedule.add_system(Phase::AgentDecide, metabolise);
    schedule.add_system(Phase::Diagnostics, advance_tick);
}

fn scenario(ticks: u64) -> Scenario<impl Fn(&mut SimWorld), impl Fn(&mut SimSchedule)> {
    Scenario {
        build: build_world,
        schedule: build_schedule,
        ticks,
    }
}

fn hasher() -> StateHasher {
    // The fingerprint stands in for the resolved module-set hash; the point is
    // that it participates, not what it is.
    let mut hasher = StateHasher::new(0x5013_2011);
    hasher.register_component::<Position>("Position");
    hasher.register_component::<Energy>("Energy");
    hasher
}

/// `determinism_threads_1_4_16` — exact match over 10,000 ticks.
#[test]
fn determinism_threads_1_4_16() {
    let scenario = scenario(TICKS);

    let divergence = compare_thread_counts(&scenario, &hasher(), &[1, 4, 16]);

    assert!(
        divergence.is_none(),
        "gate determinism_threads_1_4_16 (bench/baselines.md#m0): {}\n\n\
         Results must not depend on thread count (ADR-0004). The usual causes are a system \
         that both reads neighbour state and writes shared state in one phase, an unordered \
         parallel float reduction, or an RNG stream keyed on something the scheduler can \
         reorder.",
        divergence.map(|d| d.to_string()).unwrap_or_default()
    );
}

/// The same run twice in one process must agree — the cheap check that runs
/// first, because if this fails the thread-count comparison tells you nothing.
#[test]
fn the_same_scenario_twice_in_process_agrees() {
    let scenario = scenario(1_000);
    let hasher = hasher();

    let first = run_scenario(&scenario, &hasher, 4);
    let second = run_scenario(&scenario, &hasher, 4);

    assert_eq!(first.first_divergence(&second), None);
    assert_eq!(first.last(), second.last());
}

/// `determinism_subprocess` — a fresh process must reproduce the same hashes.
///
/// This catches what the in-process check cannot: state leaking between runs
/// through a `static`, a lazily-initialised global, or a cached allocation that
/// happens to be reused. Both in-process runs would agree while a fresh process
/// diverged.
///
/// Implemented by re-executing this test binary with an environment variable
/// set, which avoids plumbing a CLI subcommand for something only the test
/// needs.
#[test]
fn determinism_subprocess() {
    const CHILD_VAR: &str = "CX_DETERMINISM_CHILD";

    // Child role: print the final hash and exit.
    if std::env::var(CHILD_VAR).is_ok() {
        let sequence = run_scenario(&scenario(TICKS), &hasher(), 4);
        println!(
            "FINAL {}",
            sequence
                .last()
                .map(|hash| hash.to_string())
                .unwrap_or_default()
        );
        return;
    }

    let parent = run_scenario(&scenario(TICKS), &hasher(), 4)
        .last()
        .expect("the scenario should produce hashes");

    let executable = std::env::current_exe().expect("test binary path");
    let output = std::process::Command::new(executable)
        .args(["determinism_subprocess", "--exact", "--nocapture"])
        .env(CHILD_VAR, "1")
        .output()
        .expect("re-executing the test binary should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let child: StateHash = stdout
        .lines()
        .find_map(|line| line.strip_prefix("FINAL "))
        .map(|hex| StateHash(u128::from_str_radix(hex.trim(), 16).expect("hash should parse")))
        .unwrap_or_else(|| {
            panic!(
                "the child process printed no hash.\nstdout:\n{stdout}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
        });

    assert_eq!(
        parent, child,
        "gate determinism_subprocess (bench/baselines.md#m0): a fresh process produced a \
         different final hash after {TICKS} ticks.\n\n\
         In-process runs agreeing while a subprocess diverges means state is surviving \
         between runs — a static, a lazily-initialised global, or a reused cached \
         allocation. ADR-0004 requires reproducibility for the same build on any machine, \
         which a process-local cache silently breaks."
    );
}

/// The detector must be able to fail, or it reports nothing.
#[test]
fn the_harness_detects_an_injected_divergence() {
    let hasher = hasher();

    let honest = run_scenario(&scenario(100), &hasher, 1);

    // A scenario differing only in one entity's starting energy.
    let tampered = Scenario {
        build: |world: &mut SimWorld| {
            build_world(world);
            let mut query = world.query::<&mut Energy>();
            if let Some(mut energy) = query.iter_mut(world.inner_mut()).next() {
                energy.0 += 0.5;
            }
        },
        schedule: build_schedule,
        ticks: 100,
    };
    let tampered = run_scenario(&tampered, &hasher, 1);

    let divergence = honest.first_divergence(&tampered);
    assert_eq!(
        divergence,
        Some(Tick(0)),
        "a difference present from the first tick should be reported at tick 0"
    );
}
