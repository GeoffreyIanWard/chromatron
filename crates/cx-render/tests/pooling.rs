//! Per-frame allocations, counted (M1).
//!
//! M1 recorded this as outstanding work: the frame path created its instance
//! buffer, its colour target, its depth target, its debug vertex buffer, and the
//! cull pass's bind group **every frame**. The milestone also said what a gate
//! for it should look like — *"a per-frame allocation count would be a genuinely
//! hardware-independent gate for this, in the same shape as
//! `alloc_per_tick_sim_code` (`ADR-0014`)"*. This is it.
//!
//! # Why a count rather than a time
//!
//! Every other renderer measurement in M1 is recorded rather than gated, because
//! a frame time on an M4 Pro and a frame time on lavapipe in CI are different
//! numbers by two orders of magnitude and no single threshold is meaningful for
//! both. A creation count is not like that: zero is zero everywhere.
//!
//! # Why it asserts in both directions
//!
//! A test that only says "the count did not move" passes just as happily against
//! a counter that is never incremented. So [`the_counter_moves_when_a_buffer_grows`]
//! makes the same counter move, on purpose. Without it this file would be one
//! more check that reports success without checking.

use cx_core::math::{Quat, Vec3};
use cx_render::testing::device_or_skip;
use cx_render::{Camera, FrameContents, FrameRenderer, MeshData, Rgba};
use cx_view::{DebugVertex, ExtractedInstance};

/// Offscreen size. Small: this counts allocations, not pixels.
const SIZE: [u32; 2] = [64, 64];

const CLEAR: Rgba = [0.0, 0.0, 0.0, 1.0];

fn instance(index: usize) -> ExtractedInstance {
    let offset = index as f32 * 0.01;
    ExtractedInstance {
        position: Vec3::new(offset, 0.0, 0.0),
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
        palette: (index % 4) as u32,
    }
}

/// A couple of debug lines, so the debug pool is exercised too.
fn lines() -> Vec<DebugVertex> {
    let colour = [255, 0, 0, 255];
    vec![
        DebugVertex {
            position: [-1.0, 0.0, 0.0],
            colour,
        },
        DebugVertex {
            position: [1.0, 0.0, 0.0],
            colour,
        },
    ]
}

/// Renders one frame and returns the renderer's creation count afterwards.
fn frame(
    device: &cx_render::RenderDevice,
    renderer: &mut FrameRenderer,
    instances: &[ExtractedInstance],
    debug: &[DebugVertex],
) -> u32 {
    renderer
        .render_offscreen(
            device,
            SIZE,
            &Camera::looking_at(Vec3::new(0.0, 2.0, 6.0), Vec3::ZERO),
            FrameContents { instances, debug },
            None,
            CLEAR,
        )
        .expect("the frame should render");

    renderer.creations()
}

/// **The gate.** A steady-state frame creates nothing.
#[test]
fn a_repeated_frame_creates_no_gpu_resources() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let mut renderer = FrameRenderer::new(&device, &MeshData::unit_cube(), 1_024)
        .expect("the unit cube is a valid mesh");

    let instances: Vec<ExtractedInstance> = (0..500).map(instance).collect();
    let debug = lines();

    // The first frame is the warm-up: it allocates the pools, and it is
    // supposed to. What matters is what the second one costs.
    let warm_up = frame(&device, &mut renderer, &instances, &debug);
    assert!(
        warm_up > 0,
        "the first frame allocated nothing at all, which means the counter is \
         not wired to the allocations it claims to count"
    );

    for round in 0..8 {
        let after = frame(&device, &mut renderer, &instances, &debug);
        assert_eq!(
            after,
            warm_up,
            "frame {round} created {} GPU resources; a steady-state frame must \
             create none",
            after - warm_up
        );
    }
}

/// The same instance count in a different arrangement still creates nothing.
///
/// The gate above holds the input identical, so it would also pass against a
/// pool that keyed on the exact bytes and rebuilt on any change. What the pool
/// actually promises is that *contents* are free and only *capacity* costs.
#[test]
fn changing_the_contents_is_free() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let mut renderer = FrameRenderer::new(&device, &MeshData::unit_cube(), 1_024)
        .expect("the unit cube is a valid mesh");

    let first: Vec<ExtractedInstance> = (0..300).map(instance).collect();
    let baseline = frame(&device, &mut renderer, &first, &lines());

    let second: Vec<ExtractedInstance> = (0..300).map(|index| instance(300 - index)).collect();
    let after = frame(&device, &mut renderer, &second, &lines());

    assert_eq!(
        after, baseline,
        "moving every instance reallocated: the pool is keyed on contents \
         rather than on capacity"
    );
}

/// Shrinking does not reallocate either.
///
/// A pool that sized itself to each frame would reallocate on the way down and
/// again on the way back up, and the count would only betray it over a run that
/// varied. Growth is one-way by design.
#[test]
fn a_smaller_frame_reuses_the_larger_buffer() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let mut renderer = FrameRenderer::new(&device, &MeshData::unit_cube(), 1_024)
        .expect("the unit cube is a valid mesh");

    let large: Vec<ExtractedInstance> = (0..600).map(instance).collect();
    let baseline = frame(&device, &mut renderer, &large, &lines());

    let small: Vec<ExtractedInstance> = (0..10).map(instance).collect();
    let shrunk = frame(&device, &mut renderer, &small, &lines());
    assert_eq!(shrunk, baseline, "a smaller frame reallocated");

    let regrown = frame(&device, &mut renderer, &large, &lines());
    assert_eq!(
        regrown, baseline,
        "returning to the original size reallocated, so the buffer had shrunk"
    );
}

/// **The gate can fail.** Growing past capacity does move the counter.
///
/// This is the half that makes the other three mean something.
#[test]
fn the_counter_moves_when_a_buffer_grows() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let mut renderer = FrameRenderer::new(&device, &MeshData::unit_cube(), 200_000)
        .expect("the unit cube is a valid mesh");

    let small: Vec<ExtractedInstance> = (0..8).map(instance).collect();
    let baseline = frame(&device, &mut renderer, &small, &lines());

    // Far past the doubling headroom of the first frame, so this must allocate
    // whatever the growth policy is.
    let large: Vec<ExtractedInstance> = (0..100_000).map(instance).collect();
    let after = frame(&device, &mut renderer, &large, &lines());

    assert!(
        after > baseline,
        "a frame with 12,500x the instances created nothing new, so the count \
         is not observing allocations"
    );

    // And having grown, it settles again.
    let settled = frame(&device, &mut renderer, &large, &lines());
    assert_eq!(settled, after, "it kept growing at a fixed size");
}

/// A resized target reallocates once, then settles.
///
/// The colour and depth textures cannot be reused across sizes — unlike a
/// buffer, a texture's extent is fixed at creation. So the honest claim for
/// the target pool is "once per size", and a window that is
/// being dragged is expected to allocate.
#[test]
fn a_resize_costs_once_and_then_settles() {
    let Some(device) = device_or_skip() else {
        return;
    };
    let mut renderer = FrameRenderer::new(&device, &MeshData::unit_cube(), 1_024)
        .expect("the unit cube is a valid mesh");
    let instances: Vec<ExtractedInstance> = (0..100).map(instance).collect();

    frame(&device, &mut renderer, &instances, &lines());
    let baseline = frame(&device, &mut renderer, &instances, &lines());

    let mut at_new_size = || {
        renderer
            .render_offscreen(
                &device,
                [128, 96],
                &Camera::looking_at(Vec3::new(0.0, 2.0, 6.0), Vec3::ZERO),
                FrameContents {
                    instances: &instances,
                    debug: &lines(),
                },
                None,
                CLEAR,
            )
            .expect("the frame should render");
        renderer.creations()
    };

    let after_resize = at_new_size();

    assert!(
        after_resize > baseline,
        "a new target size reused a texture of the old size, which is not \
         something wgpu allows"
    );

    let settled = at_new_size();

    assert_eq!(
        settled, after_resize,
        "the new size is allocating every frame"
    );
}
