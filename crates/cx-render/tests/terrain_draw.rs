//! The terrain pass, end to end: heights in, pixels out.
//!
//! The mesh builder is unit-tested in `terrain.rs`; what remains checkable
//! only with a device is that an uploaded chunk actually draws — right
//! pipeline, right pass ordering, right rebase — and that removing it
//! actually stops drawing it. Skips (or fails under `CX_REQUIRE_GPU`) when no
//! adapter exists, like every other device test.

use cx_core::math::{ChunkCoord, Vec3};
use cx_render::testing::device_or_skip;
use cx_render::{Camera, FrameContents, FrameRenderer, MeshData, TerrainMeshData};

const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// A flat 64-cell chunk-like grid at 50 m, meshed at stride 2.
fn flat_terrain() -> TerrainMeshData {
    let heights = vec![50.0f32; 64 * 64];
    TerrainMeshData::from_heights(&heights, 64, 8.0, 2).expect("valid inputs")
}

#[test]
fn an_uploaded_chunk_draws_and_a_removed_chunk_does_not() {
    let Some(device) = device_or_skip() else {
        return;
    };

    let mut renderer =
        FrameRenderer::new(&device, &MeshData::unit_cube(), 16).expect("renderer builds");

    let chunk = ChunkCoord::new(0, 0);
    renderer
        .terrain_mut()
        .upload(&device, chunk, &flat_terrain());
    renderer.terrain_mut().set_origin(ChunkCoord::new(0, 0));
    assert_eq!(renderer.terrain().chunk_count(), 1);

    // Above the middle of the chunk, looking down at a shallow angle so the
    // ground fills the lower half of the frame.
    let camera = Camera::looking_at(
        Vec3::new(256.0, 300.0, 500.0),
        Vec3::new(256.0, 50.0, 256.0),
    );

    let (readback, _, terrain, _, _) = renderer
        .render_offscreen(
            &device,
            [64, 64],
            &camera,
            FrameContents::default(),
            None,
            CLEAR,
        )
        .expect("the frame should render");

    assert_eq!(terrain.chunks, 1);
    assert_eq!(terrain.draw_calls, 1);
    assert!(terrain.triangles > 0);

    let ground = readback.pixel(32, 48).expect("in bounds");
    assert_ne!(
        ground,
        [0, 0, 0, 255],
        "the lower half of the frame should show lit terrain, not the clear colour"
    );
    let sky = readback.pixel(32, 2).expect("in bounds");
    assert_eq!(
        sky,
        [0, 0, 0, 255],
        "above the horizon should still be the clear colour"
    );

    // Remove it, and the same frame draws nothing.
    renderer.terrain_mut().remove(chunk);
    let (readback, _, terrain, _, _) = renderer
        .render_offscreen(
            &device,
            [64, 64],
            &camera,
            FrameContents::default(),
            None,
            CLEAR,
        )
        .expect("the frame should render");

    assert_eq!(terrain.draw_calls, 0);
    assert_eq!(
        readback.pixel(32, 48),
        Some([0, 0, 0, 255]),
        "a removed chunk must stop drawing"
    );
}

#[test]
fn the_floating_origin_moves_the_terrain_not_the_mesh() {
    let Some(device) = device_or_skip() else {
        return;
    };

    let mut renderer =
        FrameRenderer::new(&device, &MeshData::unit_cube(), 16).expect("renderer builds");
    renderer
        .terrain_mut()
        .upload(&device, ChunkCoord::new(0, 0), &flat_terrain());

    let camera = Camera::looking_at(
        Vec3::new(256.0, 300.0, 500.0),
        Vec3::new(256.0, 50.0, 256.0),
    );

    // With the origin two chunks east, chunk (0, 0) rebases 1,024 m west of
    // the camera's view and out of frame — the pixel the ground occupied goes
    // back to the clear colour without the mesh being touched.
    renderer.terrain_mut().set_origin(ChunkCoord::new(2, 0));
    let (readback, _, terrain, _, _) = renderer
        .render_offscreen(
            &device,
            [64, 64],
            &camera,
            FrameContents::default(),
            None,
            CLEAR,
        )
        .expect("the frame should render");

    // Still drawn — no culling yet — just elsewhere.
    assert_eq!(terrain.draw_calls, 1);
    assert_eq!(
        readback.pixel(32, 48),
        Some([0, 0, 0, 255]),
        "a rebased-away chunk should no longer cover the camera's view"
    );
}
