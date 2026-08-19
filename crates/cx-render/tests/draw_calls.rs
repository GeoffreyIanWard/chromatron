//! The correctness half of `render_100k_instances_fps` (`bench/baselines.md#m1`).
//!
//! That gate has two clauses: **at least 60 fps**, and **fewer than 20 draw
//! calls**. They need very different things to be measured honestly.
//!
//! Frame rate needs real hardware — a number from a software rasterizer is not
//! comparable to a GPU, and asserting one on a shared CI runner would be
//! theatre. The draw-call count needs no hardware at all: it is a property of
//! how the renderer batches, identical on a laptop, a runner, and a workstation.
//!
//! So the count is asserted here, where it runs everywhere, and frame rate is
//! recorded against named hardware. See the open question in
//! `docs/milestones/M1-loop-and-pixels.md`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use cx_core::math::Vec3;
use cx_render::instanced::instance_at;
use cx_render::testing::device_or_skip;
use cx_render::{Camera, InstancedRenderer, MeshData};

/// The count the M1 gate names.
const INSTANCE_COUNT: usize = 100_000;

/// The gate's draw-call ceiling.
const MAX_DRAW_CALLS: u32 = 20;

#[test]
fn one_hundred_thousand_instances_stay_under_the_draw_call_ceiling() {
    let Some(device) = device_or_skip() else {
        return;
    };

    let renderer = InstancedRenderer::new(&device, &MeshData::unit_cube())
        .expect("the cube pipeline should build");

    // Spread over a grid so this is not a degenerate case where every instance
    // lands on the same pixel — the batching should not depend on where things
    // are, and a scene that overlaps perfectly would hide a culling mistake.
    let instances: Vec<_> = (0..INSTANCE_COUNT)
        .map(|i| {
            let x = (i % 320) as f32 - 160.0;
            let z = (i / 320) as f32 - 156.0;
            instance_at(Vec3::new(x * 2.0, 0.0, z * 2.0))
        })
        .collect();

    let camera = Camera::looking_at(Vec3::new(0.0, 200.0, 200.0), Vec3::ZERO);

    // A small target deliberately: this measures batching, not fill rate, and a
    // large one would spend the whole test rasterizing on a software adapter.
    let (readback, stats) = renderer
        .render(&device, 128, 128, &camera, &instances, [0.0, 0.0, 0.0, 1.0])
        .expect("rendering 100k instances should work");

    assert_eq!(stats.instances, INSTANCE_COUNT as u32);
    assert!(
        stats.draw_calls < MAX_DRAW_CALLS,
        "gate render_100k_instances_fps (draw-call clause, bench/baselines.md#m1): \
         {INSTANCE_COUNT} instances cost {} draw calls, ceiling is {MAX_DRAW_CALLS}.\n\n\
         Instancing means the count should not scale with instances at all. If this rises, \
         something is issuing a draw per instance or per chunk rather than per mesh.",
        stats.draw_calls
    );

    // And it must have actually drawn something: a renderer that culls
    // everything trivially satisfies a draw-call ceiling.
    let lit = readback
        .pixels
        .chunks_exact(4)
        .filter(|pixel| pixel.first().is_some_and(|red| *red > 20))
        .count();
    assert!(
        lit > 100,
        "the scene should be visible, only {lit} pixels were lit"
    );
}
