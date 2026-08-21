//! Renders one offscreen still of terrain and writes it as a PPM.
//!
//! The windowed demo is the real deliverable, but a window cannot be captured
//! in a review or attached to a PR. This loads a block through the same disk
//! cache the demo warms, meshes a neighbourhood of chunks exactly as the demo
//! would, and renders one frame headless — so what the image shows is what
//! the window shows, minus the flying.
//!
//! ```bash
//! cargo run --release -p chromatron-game --example still -- /tmp/terrain.ppm
//! ```
//!
//! PPM because it needs no image dependency; `sips -s format png` converts it.

use cx_core::math::{BlockCoord, CELL_SIZE, CELLS_PER_CHUNK_EDGE, ChunkCoord, Vec3};
use cx_render::{Camera, FrameContents, FrameRenderer, MeshData, RenderDevice, TerrainMeshData};
use cx_worldgen::{BlockCache, WorldSettings, bake_chunk};

/// Same seed as the windowed demo, so the cache is shared.
const SEED: u64 = 20_260_821;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "terrain-still.ppm".to_owned());

    let cache = BlockCache::new(std::env::temp_dir().join("chromatron-block-cache"));
    let settings = WorldSettings::default();

    tracing::info!("loading (or generating) block (0, 0)");
    let block = cache.get_or_generate(SEED, BlockCoord::new(0, 0), settings);

    let device = RenderDevice::headless()?;
    let mut renderer = FrameRenderer::new(&device, &MeshData::unit_cube(), 16)?;

    // A 3x3 chunk neighbourhood at full resolution, meshed at the demo's 2 m.
    for cz in 0..3 {
        for cx in 0..3 {
            let chunk = ChunkCoord::new(cx, cz);
            let Some(elevation) = bake_chunk(
                &block.terrain,
                &block.network,
                &block.generator,
                block.coordinates,
                chunk,
                cx_worldgen::BakeSettings::SMOOTH,
            ) else {
                continue;
            };
            let Some(mesh) = TerrainMeshData::from_heights(
                elevation.as_slice(),
                CELLS_PER_CHUNK_EDGE as usize,
                CELL_SIZE,
                4,
            ) else {
                continue;
            };
            renderer.terrain_mut().upload(&device, chunk, &mesh);
        }
    }
    renderer.terrain_mut().set_origin(ChunkCoord::new(0, 0));

    // Aim relative to the terrain actually generated rather than guessing
    // its elevation: block heights vary by hundreds of metres with the
    // continental map.
    let sample = bake_chunk(
        &block.terrain,
        &block.network,
        &block.generator,
        block.coordinates,
        ChunkCoord::new(1, 1),
        cx_worldgen::BakeSettings::SMOOTH,
    )
    .expect("chunk (1, 1) is inside block (0, 0)");
    let (min, max) = sample
        .as_slice()
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), h| {
            (lo.min(*h), hi.max(*h))
        });
    tracing::info!(min, max, "centre chunk elevation range");
    tracing::info!(
        chunks = renderer.terrain().chunk_count(),
        "meshes uploaded, rendering"
    );

    // Where the windowed demo starts: high over the middle chunk, looking
    // down across the neighbourhood.
    let camera = Camera::looking_at(
        Vec3::new(768.0, max + 350.0, 1500.0),
        Vec3::new(768.0, (min + max) * 0.5, 700.0),
    );

    let (readback, _, terrain, _, _) = renderer.render_offscreen(
        &device,
        [1280, 720],
        &camera,
        FrameContents::default(),
        None,
        [0.05, 0.06, 0.09, 1.0],
    )?;
    tracing::info!(
        draws = terrain.draw_calls,
        triangles = terrain.triangles,
        "rendered"
    );

    let mut ppm = format!("P6\n{} {}\n255\n", readback.width, readback.height).into_bytes();
    for y in 0..readback.height {
        for x in 0..readback.width {
            let [r, g, b, _] = readback.pixel(x, y).unwrap_or([0, 0, 0, 255]);
            ppm.extend_from_slice(&[r, g, b]);
        }
    }
    std::fs::write(&out, ppm)?;
    tracing::info!(%out, "written");
    Ok(())
}
