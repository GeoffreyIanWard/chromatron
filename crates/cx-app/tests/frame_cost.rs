//! Frame-loop behaviour at 144 Hz, and a recorded frame cost.
//!
//! Related to `frame_time_p99_30hz_sim_144hz_render` (`bench/baselines.md#m1`),
//! but deliberately **not** a timing gate. The reason is worth stating, because
//! the first version of this file was one and it failed on every CI platform:
//!
//! Skipping the pixel readback does not make a frame measurement CPU-only.
//! `queue.submit` applies backpressure — when the GPU cannot keep up, submit
//! blocks, and the GPU's cost lands in the wall-clock timing. On fast hardware
//! that is invisible; on a software rasterizer it dominates. Measured p99 for
//! the same code: 3.5 ms on an M4 Pro, 30 ms on lavapipe, 105 ms on WARP. Those
//! are three measurements of three different GPUs, not of this project's code.
//!
//! So frame time is **recorded with the device that produced it**, exactly as
//! the milestone decided for every other hardware-dependent number. What this
//! test *asserts* is the part that is hardware-independent: that the loop runs
//! the tick pattern a 30 Hz simulation at 144 Hz implies.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    // clippy.toml bans wall-clock reads so time cannot leak into sim logic and
    // break determinism (ADR-0004). Reporting how long a frame took is the
    // sanctioned exception: it runs in a test, and nothing it reads reaches sim
    // state. The allow sits here rather than in the config so the exception is
    // visible where it is taken.
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
    // Nearest-rank: the smallest value at or above the requested fraction, so a
    // reported figure is a real observation rather than an interpolation.
    let index = ((sorted.len() as f64 * fraction).ceil() as usize).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

#[test]
fn the_frame_loop_ticks_at_the_rate_144hz_implies_and_records_its_cost() {
    if device_or_skip().is_none() {
        return;
    }

    let mut frame_loop = FrameLoop::offscreen(TickRate::default(), 640, 360, ENTITY_COUNT)
        .expect("a device was just acquired, so the loop should build");
    let (mut world, mut schedule) = build_world();
    let camera = Camera::looking_at(Vec3::new(50.0, 60.0, 120.0), Vec3::new(50.0, 0.0, 50.0));

    let delta = Fixed::from_micros(FRAME_DELTA_US);

    // The first frames allocate archetype storage, build pipelines, and populate
    // driver caches. Anything reported is about the steady state.
    for _ in 0..WARMUP_FRAMES {
        frame_loop
            .frame(&mut world, &mut schedule, &camera, delta)
            .expect("warmup frame should render");
    }

    let mut samples = Vec::with_capacity(MEASURED_FRAMES);
    let mut ticked_frames = 0;
    let mut extracted_when_tickless = 0;

    for _ in 0..MEASURED_FRAMES {
        let started = Instant::now();
        let report = frame_loop
            .frame(&mut world, &mut schedule, &camera, delta)
            .expect("measured frame should render");
        samples.push(started.elapsed());

        if report.ticks > 0 {
            ticked_frames += 1;
        } else {
            extracted_when_tickless = report.extracted;
        }
    }

    samples.sort_unstable();

    // Recorded, not asserted. See the module docs: this number is a property of
    // the GPU as much as of this code, so it belongs in a log next to the device
    // that produced it rather than inside a threshold.
    println!(
        "frame cost: p99 {:?}, median {:?}, {ENTITY_COUNT} entities at 640x360, \
         {ticked_frames}/{MEASURED_FRAMES} frames ticked, device {}",
        percentile(&samples, 0.99),
        percentile(&samples, 0.5),
        frame_loop.device().info().summary()
    );

    // What *is* hardware-independent: a 30 Hz simulation rendered at 144 Hz runs
    // a tick roughly one frame in five. If every frame ticked, or none did, the
    // clock is wrong — and that is a real bug, catchable on any machine.
    let tick_ratio = f64::from(ticked_frames) / MEASURED_FRAMES as f64;
    assert!(
        (0.10..0.35).contains(&tick_ratio),
        "expected roughly one frame in five to tick at 30 Hz sim / 144 Hz render, got \
         {ticked_frames}/{MEASURED_FRAMES}. The clock decides this, not the renderer."
    );

    // A frame that runs no tick must still draw the world, interpolated. That is
    // the whole reason extract exists, and it holds regardless of hardware.
    assert_eq!(
        extracted_when_tickless, ENTITY_COUNT,
        "a tickless frame must still extract every entity"
    );
}
