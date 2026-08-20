//! The fps clause of `render_100k_instances_fps` (`bench/baselines.md#m1`).
//!
//! M1's exit table has two clauses for 100,000 instanced meshes: **under 20 draw
//! calls**, and **at 60 fps**. The first is hardware-independent and gated in
//! `cx-render`'s `draw_calls.rs`, where it measures 1. The second sat in the
//! table as *"not measured — needs hardware"*. This measures it.
//!
//! # Recorded, not gated — the same reason as every other frame number
//!
//! `crates/cx-app/tests/frame_cost.rs` sets out the argument at length and it is
//! not repeated here: `queue.submit` applies backpressure, so a wall-clock frame
//! measurement includes the GPU's cost, and the same code measures 3.5 ms on an
//! M4 Pro and 105 ms on WARP. A threshold that admits that spread measures the
//! runner, not the renderer.
//!
//! The difference here is that backpressure is not a *distortion* of this
//! measurement — it is the measurement. "How many of these frames per second"
//! is a whole-system question, and the GPU's share of it belongs in the answer.
//! What that costs is comparability across devices, which is exactly what
//! recording the device alongside the number buys back.
//!
//! # The worst case on purpose
//!
//! Every instance is in front of the camera, and the offscreen path draws
//! directly rather than through the cull pass. So this is 100,000 instances
//! actually rasterized — 1.2 million triangles of unit cube — not 100,000
//! submitted and mostly discarded. A number that clears 60 fps here is a
//! stronger claim than one that clears it after culling did most of the work.
//!
//! # What it asserts
//!
//! A frame rate is only interesting if the frames contained the scene. This
//! project has repeatedly produced checks that reported success while drawing
//! nothing — a debug pass that issued a draw call for zero lines, an overlay
//! that produced primitives and no pixels. So the assertions here are about the
//! work having happened: 100,000 instances extracted, and one draw call. Both
//! hold on any hardware.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    // As in `frame_cost.rs`: `clippy.toml` bans wall-clock reads so time cannot
    // leak into sim logic and break determinism (ADR-0004). Reporting how long a
    // frame took is the sanctioned exception — it runs in a test, and nothing it
    // reads reaches sim state.
    clippy::disallowed_methods
)]

use std::time::{Duration, Instant};

use cx_app::FrameLoop;
use cx_core::Fixed;
use cx_core::math::{ChunkCoord, Vec3, WorldPos};
use cx_ecs::{PreviousTransform, SimSchedule, SimWorld, Transform, WorldConfig};
use cx_render::testing::device_or_skip;
use cx_render::{Camera, InstancedRenderer, MeshData, RenderDevice};
use cx_time::TickRate;
use cx_view::ExtractedInstance;

/// The count the gate names.
const INSTANCES: usize = 100_000;

/// 1920x1080. The gate does not name a resolution, so this picks the common
/// desktop one rather than the 640x360 the frame-cost test uses — at 100,000
/// instances the answer should be about the geometry, but running it at a
/// postage stamp would quietly turn it into a measurement of nothing.
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;

/// The gate's threshold, as a frame budget.
const SIXTY_FPS: Duration = Duration::from_nanos(16_666_667);

const WARMUP_FRAMES: usize = 20;
const MEASURED_FRAMES: usize = 120;

/// The grid's side, in instances. 316 squared is just over 100,000.
const SIDE: usize = 316;

/// Metres between instance centres. The mesh is a one-metre cube, so 1.2 leaves
/// a visible gap without leaving most of the screen empty — the first attempt
/// used 2.0 and the coverage check below caught it: the frame was 13% geometry,
/// which measures instance throughput but almost no fill rate.
const SPACING: f32 = 1.2;

/// Where instance `index` sits, shared by the timed run and the verification
/// render so the two cannot drift apart.
fn grid_position(index: usize) -> Vec3 {
    Vec3::new(
        (index % SIDE) as f32 * SPACING,
        0.0,
        (index / SIDE) as f32 * SPACING,
    )
}

/// The centre of the grid, for the camera to look at.
fn grid_centre() -> Vec3 {
    let half = SIDE as f32 * SPACING / 2.0;
    Vec3::new(half, 0.0, half)
}

/// A 100,000-instance grid.
///
/// Static: nothing moves. A per-frame integration over 100,000 entities would
/// put the sim's cost into a number that is supposed to be about the renderer,
/// and `extract_100k_instances` already measures that side on its own.
fn build_world() -> (SimWorld, SimSchedule) {
    let mut world = SimWorld::new(WorldConfig::default());

    world.spawn_batch((0..INSTANCES).map(|index| {
        let position = WorldPos::new(ChunkCoord::new(0, 0), grid_position(index));
        let transform = Transform::from_position(position);
        (transform, PreviousTransform(transform))
    }));

    (world, SimSchedule::new())
}

/// The same grid as [`build_world`], as the renderer would receive it.
fn extracted_grid() -> Vec<ExtractedInstance> {
    (0..INSTANCES)
        .map(|index| ExtractedInstance {
            position: grid_position(index),
            rotation: cx_core::math::Quat::IDENTITY,
            scale: Vec3::ONE,
            palette: 0,
        })
        .collect()
}

/// What fraction of the frame the grid actually covers, from the same camera.
///
/// The frame rate above is measured without a readback, so nothing in it can
/// tell a fast frame from an empty one — and a camera pointed slightly wrong
/// would clip the whole grid and report a magnificent number. This draws one
/// frame small enough to read back and counts what is not the clear colour.
///
/// It is a separate render because `FrameLoop` deliberately has no readback on
/// the measured path: adding one would put a GPU synchronisation point in the
/// middle of the thing being timed.
fn coverage(camera: &Camera) -> f64 {
    const SIZE: u32 = 320;
    const CLEAR: cx_render::Rgba = [0.0, 0.0, 0.0, 1.0];

    let device = RenderDevice::headless().expect("a device was already acquired");
    let mut renderer = InstancedRenderer::new(&device, &MeshData::unit_cube())
        .expect("the unit cube is a valid mesh");

    let (readback, _) = renderer
        .render(&device, SIZE, SIZE, camera, &extracted_grid(), CLEAR)
        .expect("the verification frame should render");

    // Written out when asked. A coverage percentage says the grid is on screen;
    // it cannot say the frame looks like 100,000 cubes seen from above, and
    // looking at rendered output has caught things in this project that
    // reasoning about it did not. PPM because it needs no encoder.
    if let Ok(directory) = std::env::var("CX_DUMP_FRAME") {
        let mut ppm = format!("P6\n{SIZE} {SIZE}\n255\n").into_bytes();
        for y in 0..SIZE {
            for x in 0..SIZE {
                ppm.extend_from_slice(&readback.pixel(x, y).unwrap_or([0, 0, 0, 255])[..3]);
            }
        }
        let path = std::path::Path::new(&directory).join("instance_throughput.ppm");
        if let Err(error) = std::fs::write(&path, ppm) {
            eprintln!("could not write {}: {error}", path.display());
        }
    }

    let covered = (0..SIZE)
        .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
        .filter(|(x, y)| {
            let pixel = readback.pixel(*x, *y).expect("in bounds");
            pixel[0] > 8 || pixel[1] > 8 || pixel[2] > 8
        })
        .count();

    f64::from(covered as u32) / f64::from(SIZE * SIZE)
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    // Nearest-rank, as in `frame_cost.rs`: a reported figure is a real
    // observation rather than an interpolation between two.
    let index = ((sorted.len() as f64 * fraction).ceil() as usize).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

#[test]
fn one_hundred_thousand_instances_draw_in_one_call_and_the_rate_is_recorded() {
    if device_or_skip().is_none() {
        return;
    }

    let mut frame_loop = FrameLoop::offscreen(TickRate::default(), WIDTH, HEIGHT, INSTANCES)
        .expect("a device was just acquired, so the loop should build");
    let (mut world, mut schedule) = build_world();

    // Close enough that the grid fills the frame, far enough that all of it is
    // in it. The coverage check at the end is what holds this honest — a camera
    // that framed less would measure a smaller scene and report it as this one.
    let centre = grid_centre();
    let camera = Camera::looking_at(Vec3::new(centre.x, 150.0, centre.z + 330.0), centre);

    // 144 Hz, matching the render rate the milestone's other frame numbers use.
    let delta = Fixed::from_micros(6_944);

    for _ in 0..WARMUP_FRAMES {
        frame_loop
            .frame(&mut world, &mut schedule, &camera, delta)
            .expect("warmup frame should render");
    }

    let mut samples = Vec::with_capacity(MEASURED_FRAMES);
    let mut draw_calls = 0;
    let mut extracted = 0;

    for _ in 0..MEASURED_FRAMES {
        let started = Instant::now();
        let report = frame_loop
            .frame(&mut world, &mut schedule, &camera, delta)
            .expect("measured frame should render");
        samples.push(started.elapsed());

        extracted = report.extracted;
        draw_calls = report.draw.map_or(0, |draw| draw.draw_calls);
    }

    samples.sort_unstable();
    let median = percentile(&samples, 0.5);
    let p99 = percentile(&samples, 0.99);
    let fps = |frame: Duration| 1.0 / frame.as_secs_f64();

    // Recorded with the device that produced it, per the milestone's decision
    // for every hardware-dependent number.
    println!(
        "render_100k_instances_fps: {:.0} fps median ({median:?}), {:.0} fps at p99 ({p99:?}), \
         {INSTANCES} instances at {WIDTH}x{HEIGHT} in {draw_calls} draw call(s), \
         target 60 fps, on {}",
        fps(median),
        fps(p99),
        frame_loop.device().info().summary()
    );

    if median > SIXTY_FPS {
        // Not a failure: on a software rasterizer it is the expected result, and
        // failing here would be the timing gate this project already decided
        // against. Said out loud so a run that misses the target is not silently
        // indistinguishable from one that clears it.
        println!(
            "render_100k_instances_fps: BELOW the 60 fps target on this device \
             ({:.0} fps). Expected on a software rasterizer; on the reference \
             hardware it is a regression worth chasing.",
            fps(median)
        );
    }

    // The hardware-independent half. A frame rate says nothing unless the frames
    // held the scene: without these three, an early return that drew nothing —
    // or a camera pointed past the grid — would report a magnificent number.
    let covered = coverage(&camera);
    println!(
        "render_100k_instances_fps: the grid covers {:.0}% of the frame",
        covered * 100.0
    );
    assert!(
        covered > 0.2,
        "the grid covered only {:.1}% of the frame, so the rate above was \
         measured against a mostly empty screen rather than 100,000 instances",
        covered * 100.0
    );

    assert_eq!(
        extracted, INSTANCES,
        "the frame rate above was measured over frames that did not contain the \
         scene — {extracted} instances were extracted, not {INSTANCES}"
    );

    assert!(
        draw_calls > 0 && draw_calls < 20,
        "gate render_100k_instances_fps (draw-call clause, bench/baselines.md#m1): \
         {INSTANCES} instances took {draw_calls} draw calls, which must be between \
         1 and 19"
    );
}
