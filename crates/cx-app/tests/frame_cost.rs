//! The CPU-side clause of `frame_time_p99_30hz_sim_144hz_render`
//! (`bench/baselines.md#m1`).
//!
//! The gate asks for a 99th-percentile frame under 8 ms with a 30 Hz simulation
//! rendered at 144 Hz. Like the other GPU-dependent gates, it splits into a part
//! that can be measured honestly anywhere and a part that cannot:
//!
//! - **CPU cost per frame** — running due ticks, extracting, encoding, and
//!   submitting. This project's own work, and what this test measures.
//! - **GPU cost per frame** — rasterization. Meaningless on a software adapter,
//!   so it is recorded against named hardware instead.
//!
//! No readback here, deliberately. Reading pixels back would block until the GPU
//! finished, which would turn a CPU measurement into a measurement of lavapipe's
//! fill rate — and would be wrong for a real loop besides.
//!
//! # Why a percentile rather than a mean
//!
//! Frame time is judged by its worst frames, not its average. A loop that
//! averages 2 ms and spikes to 40 ms every thirtieth frame is visibly broken and
//! has an excellent mean. Criterion reports means, so this collects its own
//! samples and sorts them.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    // clippy.toml bans wall-clock reads so that time cannot leak into sim logic
    // and break determinism (ADR-0004). Measuring how long a frame took is the
    // sanctioned exception: it is the *only* way to observe frame cost, it runs
    // in a test rather than in the tick, and nothing it reads reaches sim state.
    // Allowing it here rather than loosening the rule keeps the exception
    // visible at the point it is taken.
    clippy::disallowed_methods
)]

use std::time::{Duration, Instant};

use cx_app::FrameLoop;
use cx_core::Fixed;
use cx_core::math::{ChunkCoord, Vec3, WorldPos};
use cx_ecs::{Phase, PreviousTransform, Query, SimSchedule, SimWorld, Transform, WorldConfig};
use cx_render::Camera;
use cx_render::testing::device_or_skip;
use cx_time::TickRate;

/// The gate's budget.
const BUDGET: Duration = Duration::from_millis(8);

/// 144 Hz, the render rate the gate names.
const FRAME_DELTA_US: u64 = 6_944;

const ENTITY_COUNT: usize = 10_000;
const WARMUP_FRAMES: usize = 30;
const MEASURED_FRAMES: usize = 300;

fn integrate(mut query: Query<(&mut Transform, &mut PreviousTransform)>) {
    for (mut transform, mut previous) in query.iter_mut() {
        previous.0 = *transform;
        transform.position = transform.position.offset(Vec3::new(0.05, 0.0, 0.02));
    }
}

fn build_world() -> (SimWorld, SimSchedule) {
    let mut world = SimWorld::new(WorldConfig::default());
    world.spawn_batch((0..ENTITY_COUNT).map(|i| {
        let position = WorldPos::new(
            ChunkCoord::new(0, 0),
            Vec3::new((i % 100) as f32, 0.0, (i / 100) as f32),
        );
        let transform = Transform::from_position(position);
        (transform, PreviousTransform(transform))
    }));

    let mut schedule = SimSchedule::new();
    schedule.add_system(Phase::AgentAct, integrate);

    (world, schedule)
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    // Nearest-rank: the smallest value at or above the requested fraction. With
    // 300 samples the p99 is the 297th, which is a real observation rather than
    // an interpolation between two.
    let index = ((sorted.len() as f64 * fraction).ceil() as usize).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

#[test]
fn frame_cpu_cost_stays_within_the_budget_at_144hz() {
    if device_or_skip().is_none() {
        return;
    }

    let mut frame_loop = FrameLoop::offscreen(TickRate::default(), 640, 360, ENTITY_COUNT)
        .expect("a device was just acquired, so the loop should build");
    let (mut world, mut schedule) = build_world();
    let camera = Camera::looking_at(Vec3::new(50.0, 60.0, 120.0), Vec3::new(50.0, 0.0, 50.0));

    let delta = Fixed::from_micros(FRAME_DELTA_US);

    // Warm up: the first frames allocate archetype storage, build pipelines, and
    // populate driver caches. The gate is about the steady state.
    for _ in 0..WARMUP_FRAMES {
        frame_loop
            .frame(&mut world, &mut schedule, &camera, delta)
            .expect("warmup frame should render");
    }

    let mut samples = Vec::with_capacity(MEASURED_FRAMES);
    let mut ticked_frames = 0;

    for _ in 0..MEASURED_FRAMES {
        let started = Instant::now();
        let report = frame_loop
            .frame(&mut world, &mut schedule, &camera, delta)
            .expect("measured frame should render");
        samples.push(started.elapsed());

        if report.ticks > 0 {
            ticked_frames += 1;
        }
    }

    samples.sort_unstable();
    let p99 = percentile(&samples, 0.99);
    let median = percentile(&samples, 0.5);

    // The scenario has to actually be the one the gate describes: a 30 Hz sim at
    // 144 Hz means roughly one frame in five runs a tick. If every frame ticked,
    // the clock is wrong and the measurement is of something else.
    let tick_ratio = ticked_frames as f64 / MEASURED_FRAMES as f64;
    assert!(
        (0.10..0.35).contains(&tick_ratio),
        "expected roughly one frame in five to tick at 30 Hz sim / 144 Hz render, got \
         {ticked_frames}/{MEASURED_FRAMES}"
    );

    println!(
        "frame_time_p99 (CPU side): p99 {p99:?}, median {median:?}, budget {BUDGET:?}, \
         {ENTITY_COUNT} entities, {ticked_frames}/{MEASURED_FRAMES} frames ticked, device {}",
        frame_loop.device().info().summary()
    );

    assert!(
        p99 <= BUDGET,
        "gate frame_time_p99_30hz_sim_144hz_render (CPU clause, bench/baselines.md#m1): \
         p99 {p99:?} exceeds the {BUDGET:?} budget (median {median:?}).\n\n\
         This measures only work this project does — running due ticks, extracting, encoding, \
         and submitting. GPU time is excluded deliberately, since a software rasterizer's \
         fill rate says nothing about hardware. A regression here is in the sim tick, the \
         extract, or the per-frame buffer uploads."
    );
}
