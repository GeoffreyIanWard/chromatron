//! Where per-tick allocations come from.
//!
//! `03-conventions.md` bans allocation inside per-tick systems. `ADR-0014`
//! records why that is measured against the single-threaded executor: bevy_ecs's
//! multi-threaded executor allocates for scope setup and task futures, and none
//! of that is ours.
//!
//! This test is the evidence behind that ADR, kept so the claim stays true. If
//! the single-threaded numbers ever stop being zero, engine code has started
//! allocating and the benchmark gate will fail for a real reason.

// A counting global allocator cannot be written in safe Rust, and this is test
// code that never ships inside the simulation.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use cx_ecs::{Component, Phase, Query, SimSchedule, SimWorld, WorldConfig};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

struct Counting;

// SAFETY: every call forwards to the system allocator with the caller's layout
// unchanged; the counter is atomic and touches no allocation state.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

#[derive(Component, Clone, Copy)]
struct Position(f32);

fn integrate(mut query: Query<&mut Position>) {
    for mut position in query.iter_mut() {
        position.0 += 1.0;
    }
}

/// Allocations per tick for a schedule with `systems` systems.
fn per_tick(single_threaded: bool, systems: usize) -> f64 {
    const WARMUP: usize = 64;
    const MEASURED: usize = 100;

    let mut world = SimWorld::new(WorldConfig::default());
    world.spawn_batch((0..1_000).map(|_| Position(0.0)));

    let mut schedule = SimSchedule::new();
    if single_threaded {
        schedule.set_single_threaded();
    }
    for _ in 0..systems {
        schedule.add_system(Phase::AgentAct, integrate);
    }

    for _ in 0..WARMUP {
        schedule.run(&mut world);
    }

    let before = ALLOCATIONS.load(Ordering::Acquire);
    for _ in 0..MEASURED {
        schedule.run(&mut world);
    }
    let after = ALLOCATIONS.load(Ordering::Acquire);

    (after - before) as f64 / MEASURED as f64
}

/// Both measurements live in one test deliberately.
///
/// The allocation counter is process-wide, and cargo runs tests in parallel
/// threads, so two concurrent measurements contaminate each other — the
/// single-threaded case first read 0.07 allocations per tick purely because the
/// multi-threaded case was running beside it. Sequential measurement in one test
/// is the fix; a mutex would work too but would leave the trap in place for the
/// next test added to this file.
#[test]
fn allocation_attribution() {
    // The property 03-conventions.md actually states. Single-threaded, so the
    // executor contributes nothing and anything counted here is ours.
    for systems in [0, 1, 3] {
        let measured = per_tick(true, systems);
        assert!(
            measured.abs() < f64::EPSILON,
            "{systems} systems allocated {measured} times per tick; engine code must allocate \
             zero in the steady state (03-conventions.md, ADR-0014)"
        );
    }

    // The shape of bevy_ecs's cost rather than exact numbers: a fixed setup cost
    // plus roughly one allocation per system.
    let base = per_tick(false, 0);
    let three = per_tick(false, 3);

    assert!(
        base > 0.0,
        "the multi-threaded executor was expected to allocate; it did not, which would mean \
         ADR-0014 no longer describes reality"
    );
    assert!(
        base <= 16.0,
        "executor base cost is {base} allocations per tick, above the 16 budgeted in ADR-0014"
    );
    assert!(
        three - base <= 4.0,
        "per-system cost grew: {base} with no systems, {three} with three"
    );
}
