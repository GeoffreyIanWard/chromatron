//! Debug draw, end to end and against the M1 budget.
//!
//! | Check | Target |
//! |---|---|
//! | Debug draw 10,000 lines | < 1 ms |
//!
//! The shape maths is unit-tested in `cx-view`, and the pipeline's format
//! handling in `cx-render`. What is left — and what these cover — is whether the
//! lines actually reach the pixels, and what they cost when there are a lot of
//! them.

use cx_app::FrameLoop;
use cx_core::Fixed;
use cx_core::math::{ChunkCoord, Vec3, WorldPos};
use cx_render::Camera;
use cx_render::testing::device_or_skip;
use cx_time::TickRate;
use cx_view::DebugColour;

use cx_ecs::{SimSchedule, SimWorld, WorldConfig};

/// Lines the M1 budget names.
const GATE_LINES: usize = 10_000;

/// Frames measured per configuration.
const SAMPLES: usize = 30;

/// One tick of real time at 30 Hz.
const ONE_TICK: Fixed = Fixed::from_micros(33_334);

const ORIGIN: ChunkCoord = ChunkCoord { x: 0, z: 0 };

fn at(x: f32, y: f32, z: f32) -> WorldPos {
    WorldPos::new(ORIGIN, Vec3::new(x, y, z))
}

fn empty_world() -> (SimWorld, SimSchedule) {
    (SimWorld::new(WorldConfig::default()), SimSchedule::new())
}

fn loop_or_skip(width: u32, height: u32, capacity: usize) -> Option<FrameLoop> {
    device_or_skip()?;
    Some(
        FrameLoop::offscreen(TickRate::default(), width, height, capacity)
            .expect("a device was just acquired"),
    )
}

/// A line drawn across the middle of an empty scene has to show up in the
/// pixels.
///
/// The check the unit tests cannot make: every stage between `DebugDraw::line`
/// and a coloured pixel — rebasing, the vertex layout, the `Unorm8x4` colour
/// conversion, the pass that loads rather than clears — can be individually
/// plausible and collectively draw nothing.
#[test]
fn a_debug_line_reaches_the_pixels() {
    let Some(mut frame_loop) = loop_or_skip(64, 64, 16) else {
        return;
    };
    let (mut world, mut schedule) = empty_world();
    let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 4.0), Vec3::ZERO);

    // A horizontal red line through the origin, so it crosses the middle row.
    frame_loop
        .debug()
        .line(at(-2.0, 0.0, 0.0), at(2.0, 0.0, 0.0), DebugColour::RED);

    let (report, readback) = frame_loop
        .frame_with_readback(&mut world, &mut schedule, &camera, None, ONE_TICK)
        .expect("the frame should render");

    assert_eq!(report.debug.lines, 1);
    assert_eq!(report.debug.draw_calls, 1);
    assert_eq!(
        report.draw.map(|stats| stats.instances),
        Some(0),
        "the scene is empty; only the line should be drawn"
    );

    let corner = readback.pixel(2, 2).expect("in bounds");

    // Scanned rather than sampled at one coordinate: a one-pixel line lands on
    // whichever row the rasterizer picks, and which side of a pixel centre a
    // mathematically-centred line falls on is not something to assert.
    let mut on_line = Vec::new();
    for y in 0..64 {
        for x in 0..64 {
            let pixel = readback.pixel(x, y).expect("in bounds");
            if pixel[0] > corner[0] + 60 {
                on_line.push((x, y, pixel));
            }
        }
    }

    assert!(
        !on_line.is_empty(),
        "the line should be visible somewhere; every pixel matched the {corner:?} background"
    );
    assert!(
        on_line.len() >= 32,
        "a line spanning the frame should cover most of a row, covered {} pixels",
        on_line.len()
    );

    let rows: std::collections::BTreeSet<u32> = on_line.iter().map(|(_, y, _)| *y).collect();
    assert!(
        rows.len() <= 2,
        "a horizontal line should occupy one row, touched {rows:?}"
    );

    let (_, _, lit) = on_line[0];
    assert!(
        lit[0] > lit[2],
        "a red line should be red-dominant, got {lit:?}"
    );
}

/// Debug geometry must not survive into the next frame.
///
/// Immediate mode means whoever wants a line asks again. A buffer that kept its
/// contents would accumulate every line ever drawn, which looks like a leak in
/// the renderer rather than in the caller.
#[test]
fn lines_do_not_persist_into_the_next_frame() {
    let Some(mut frame_loop) = loop_or_skip(32, 32, 16) else {
        return;
    };
    let (mut world, mut schedule) = empty_world();
    let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 4.0), Vec3::ZERO);

    frame_loop
        .debug()
        .cross(at(0.0, 0.0, 0.0), 2.0, DebugColour::GREEN);

    let first = frame_loop
        .frame(&mut world, &mut schedule, &camera, ONE_TICK)
        .expect("the frame should render");
    assert_eq!(first.debug.lines, 3, "a cross is three lines");

    let second = frame_loop
        .frame(&mut world, &mut schedule, &camera, ONE_TICK)
        .expect("the frame should render");
    assert_eq!(
        second.debug.lines, 0,
        "nothing was queued this frame, so nothing should be drawn"
    );
    assert_eq!(
        second.debug.draw_calls, 0,
        "an empty debug pass should not be encoded at all"
    );
}

/// Every shape reaches the renderer with the segment count it claims.
///
/// `cx-view` asserts the counts in isolation; this asserts nothing is lost on
/// the way through rebasing and upload.
#[test]
fn every_shape_survives_the_trip_to_the_gpu() {
    let Some(mut frame_loop) = loop_or_skip(32, 32, 512) else {
        return;
    };
    let (mut world, mut schedule) = empty_world();
    let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 8.0), Vec3::ZERO);

    let debug = frame_loop.debug();
    debug.line(at(0.0, 0.0, 0.0), at(1.0, 0.0, 0.0), DebugColour::WHITE);
    debug.cross(at(0.0, 0.0, 0.0), 1.0, DebugColour::WHITE);
    debug.aabb(at(-1.0, -1.0, -1.0), at(1.0, 1.0, 1.0), DebugColour::WHITE);
    debug.sphere(at(0.0, 0.0, 0.0), 1.0, DebugColour::WHITE);
    debug.arrow(at(0.0, 0.0, 0.0), at(0.0, 2.0, 0.0), DebugColour::WHITE);

    // 1 + 3 + 12 + 48 + 5.
    let expected = 1 + 3 + 12 + 3 * 16 + 5;

    let report = frame_loop
        .frame(&mut world, &mut schedule, &camera, ONE_TICK)
        .expect("the frame should render");
    assert_eq!(report.debug.lines, expected);
    assert_eq!(report.debug.draw_calls, 1, "all of it in one draw call");
}

/// **M1 budget: 10,000 debug lines in under 1 ms.**
///
/// Recorded rather than gated, for the reason `M1-loop-and-pixels.md` sets out
/// at length: `queue.submit` applies backpressure, so any wall-clock frame
/// measurement carries the GPU's cost, and CI's software rasterizers are 10x
/// slower than hardware with 5x run-to-run variance. A threshold that admits
/// that spread is not measuring this code.
///
/// What *is* asserted is hardware-independent: 10,000 lines still cost exactly
/// one draw call, and all 10,000 arrive. Both are real bugs when they break —
/// a per-line draw call would be the obvious wrong implementation — and both
/// are catchable on any machine.
#[test]
fn ten_thousand_lines_cost_one_draw_call() {
    let Some(mut frame_loop) = loop_or_skip(640, 360, GATE_LINES) else {
        return;
    };
    let (mut world, mut schedule) = empty_world();
    let camera = Camera::looking_at(Vec3::new(0.0, 40.0, 120.0), Vec3::ZERO);

    let device = frame_loop.device().info().summary();

    // Warm up: the first frame builds buffers the rest reuse, and including it
    // would measure pipeline setup rather than drawing.
    queue_lines(&mut frame_loop);
    frame_loop
        .frame(&mut world, &mut schedule, &camera, ONE_TICK)
        .expect("the frame should render");

    // Measured against a baseline of the same frame with no lines, because the
    // budget is for *debug draw*, not for a frame. Reporting the whole frame
    // would credit debug draw with the sim, the extract, and the scene pass.
    let mut baseline = Vec::with_capacity(SAMPLES);
    let mut with_lines = Vec::with_capacity(SAMPLES);
    let mut lines_drawn = 0;
    let mut draw_calls = 0;

    for _ in 0..SAMPLES {
        baseline.push(time_frame(&mut frame_loop, &mut world, &mut schedule, &camera).1);

        queue_lines(&mut frame_loop);
        let (report, elapsed) = time_frame(&mut frame_loop, &mut world, &mut schedule, &camera);
        with_lines.push(elapsed);

        lines_drawn = report.debug.lines;
        draw_calls = report.debug.draw_calls;
    }

    assert_eq!(
        lines_drawn as usize, GATE_LINES,
        "every queued line must reach the GPU"
    );
    assert_eq!(
        draw_calls, 1,
        "10,000 lines must be one draw call, not one per line"
    );

    let empty = median_ms(&mut baseline);
    let drawn = median_ms(&mut with_lines);

    println!(
        "debug_draw_10k_lines: {:.2} ms marginal ({drawn:.2} ms with lines, {empty:.2} ms \
         without), budget 1 ms, on {device}",
        drawn - empty,
    );
}

/// Median of a set of timings, in milliseconds.
fn median_ms(samples: &mut [std::time::Duration]) -> f64 {
    samples.sort_unstable();
    samples[samples.len() / 2].as_secs_f64() * 1_000.0
}

/// Runs one frame and reports how long it took.
#[allow(
    clippy::disallowed_methods,
    reason = "measuring elapsed time is the entire point of this test, and nothing here \
              reaches the simulation"
)]
fn time_frame(
    frame_loop: &mut FrameLoop,
    world: &mut SimWorld,
    schedule: &mut SimSchedule,
    camera: &Camera,
) -> (cx_app::FrameReport, std::time::Duration) {
    let started = std::time::Instant::now();
    let report = frame_loop
        .frame(world, schedule, camera, ONE_TICK)
        .expect("the frame should render");
    (report, started.elapsed())
}

/// Queues 10,000 lines as a grid, which is what a debug overlay at this scale
/// realistically looks like.
fn queue_lines(frame_loop: &mut FrameLoop) {
    let debug = frame_loop.debug();
    let half = GATE_LINES / 2;

    for index in 0..half {
        let offset = index as f32 * 0.1 - half as f32 * 0.05;
        debug.line(
            at(offset, 0.0, -50.0),
            at(offset, 0.0, 50.0),
            DebugColour::BLUE,
        );
        debug.line(
            at(-50.0, 0.0, offset),
            at(50.0, 0.0, offset),
            DebugColour::GREEN,
        );
    }
}
