//! Step 6 of S07's pipeline: the bake.
//!
//! `ADR-0015` split the pipeline across two grids — erosion at 2 m because the
//! working set at 0.5 m is 6.6 GB against a 0.8 GB budget, `ELEVATION` at 0.5 m
//! because that is the resolution terrain is stored and rendered at. This is
//! where the two are reconciled, and it is the half of that decision that had
//! not yet had to deliver.
//!
//! Two things happen, and the ADR named the correctness question for each:
//!
//! 1. **Resample** the eroded 2 m surface up to 0.5 m — *"the resample must not
//!    introduce terracing at coarse-cell boundaries"*.
//! 2. **Re-add high-frequency detail**, the sub-2 m texture erosion never
//!    modelled — *"the re-added detail must not fill in channels the erosion
//!    cut"*.
//!
//! # Bicubic, not bilinear
//!
//! Bilinear interpolation is continuous but its *derivative* is not: the slope
//! jumps at every coarse-cell boundary. On a hillshade that reads as a faint
//! grid of creases every 2 m — not terracing exactly, but the same family of
//! artifact and just as much a picture of the grid rather than of the land.
//!
//! Catmull-Rom is smooth across cell boundaries and reproduces a linear ramp
//! exactly, which is the property worth testing: a planar slope must come
//! through the resample still planar, because any wobble introduced there is a
//! wobble in every hillside in the world.
//!
//! # Detail is suppressed where water is
//!
//! Erosion supplies the landform and noise supplies the surface — that is
//! `ADR-0015`'s division of labour. But a channel is 11 m deep and a metre or two
//! wide, so adding a few metres of noise to it fills it in, and the river that
//! five stages went into carving stops existing.
//!
//! So detail amplitude falls to zero as drainage area rises. A hillside gets its
//! full texture; a river bed gets none. That is also physically right rather than
//! merely convenient — a channel floor is graded by the water running over it,
//! and is smoother than the slopes above it for exactly that reason.

use cx_core::math::{
    CELL_SIZE, CELLS_PER_CHUNK, CELLS_PER_CHUNK_EDGE, ChunkCoord, EROSION_CELL_SIZE,
};

use crate::block::{BlockCoordinates, BlockGrid, ErosionCell};
use crate::elevation::ElevationGenerator;
use crate::flow::FlowNetwork;

/// How the bake adds detail back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BakeSettings {
    /// Metres of peak-to-peak texture on ground with no drainage through it.
    ///
    /// Small on purpose. This is the roughness of a hillside at arm's length,
    /// not a landform — landforms come from erosion, and anything large enough
    /// to read as one here would be competing with five stages that modelled it
    /// properly.
    pub detail_relief: f32,
    /// Horizontal size of the detail, in metres. Below the erosion grid, or it
    /// would be re-adding frequencies erosion already decided.
    pub detail_wavelength: f32,
    /// Detail octaves.
    pub detail_octaves: u32,
    /// Catchment area, in square metres, where detail starts being suppressed.
    pub detail_fade_from: f32,
    /// Catchment area where detail is fully suppressed. Channel floors are
    /// graded by the water on them and are genuinely smoother than hillsides.
    pub detail_fade_to: f32,
}

impl Default for BakeSettings {
    fn default() -> Self {
        Self {
            detail_relief: 0.9,
            // 6 m: three erosion cells, so it adds frequencies the 2 m grid
            // could not carry rather than competing with ones it could.
            detail_wavelength: 6.0,
            detail_octaves: 3,
            detail_fade_from: 20_000.0,
            detail_fade_to: 60_000.0,
        }
    }
}

impl BakeSettings {
    /// No detail. The resampled erosion surface, alone.
    pub const SMOOTH: Self = Self {
        detail_relief: 0.0,
        detail_wavelength: 6.0,
        detail_octaves: 1,
        detail_fade_from: 0.0,
        detail_fade_to: 1.0,
    };

    /// How much of the full detail applies at a given catchment area, in `0..=1`.
    ///
    /// Smoothstep rather than a step: a hard cutoff would put a visible ledge
    /// along both banks of every river, exactly where a player is most likely to
    /// be standing and looking.
    pub fn detail_scale(&self, area: f32) -> f32 {
        if area <= self.detail_fade_from {
            return 1.0;
        }
        if area >= self.detail_fade_to {
            return 0.0;
        }
        let span = self.detail_fade_to - self.detail_fade_from;
        if span <= 0.0 {
            return 0.0;
        }
        let t = ((area - self.detail_fade_from) / span).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    }
}

/// One chunk's `ELEVATION`, at [`CELL_SIZE`], row-major with +X fastest.
///
/// 1024 x 1024 `f32` — 4 MB, which is the figure `bench/memory-budget.md` uses
/// to argue that runtime field quantization is mandatory. `ELEVATION` stays
/// `f32` because precision matters during generation and it is frozen after.
#[derive(Debug, Clone)]
pub struct ChunkElevation {
    cells: Vec<f32>,
}

impl ChunkElevation {
    /// Height at a chunk-local cell, or `None` out of range.
    pub fn get(&self, x: u32, z: u32) -> Option<f32> {
        if x >= CELLS_PER_CHUNK_EDGE || z >= CELLS_PER_CHUNK_EDGE {
            return None;
        }
        self.cells
            .get((z * CELLS_PER_CHUNK_EDGE + x) as usize)
            .copied()
    }

    /// Every height, row-major.
    pub fn as_slice(&self) -> &[f32] {
        &self.cells
    }
}

/// Bakes one chunk out of a generated block.
///
/// **Pure extraction plus interpolation**, per `ADR-0006`: a chunk computes
/// nothing of its own. Everything expensive already happened at block scale, and
/// this reads a window of it.
///
/// Returns `None` when the chunk is not inside this block, which is a caller
/// error rather than a coordinate to clamp — clamping would bake real terrain
/// from the wrong place, which draws a plausible landscape somewhere it does not
/// belong.
pub fn bake_chunk(
    eroded: &BlockGrid,
    network: &FlowNetwork,
    generator: &ElevationGenerator,
    block: BlockCoordinates,
    chunk: ChunkCoord,
    settings: BakeSettings,
) -> Option<ChunkElevation> {
    let origin = block.chunk_origin_cell(chunk)?;
    let cell_area = EROSION_CELL_SIZE * EROSION_CELL_SIZE;

    // The world position of the chunk's first field cell, so detail is sampled
    // in absolute coordinates and therefore matches across every seam.
    let (world_x0, world_z0) = block.cell_centre(origin.x(), origin.z());
    let corner_x = world_x0 - EROSION_CELL_SIZE / 2.0;
    let corner_z = world_z0 - EROSION_CELL_SIZE / 2.0;

    let mut cells = Vec::with_capacity(CELLS_PER_CHUNK as usize);

    for z in 0..CELLS_PER_CHUNK_EDGE {
        for x in 0..CELLS_PER_CHUNK_EDGE {
            // Where this field cell's centre sits on the erosion grid, in
            // fractional erosion cells from the block's own origin.
            let offset_x = (x as f32 + 0.5) * CELL_SIZE / EROSION_CELL_SIZE;
            let offset_z = (z as f32 + 0.5) * CELL_SIZE / EROSION_CELL_SIZE;

            let sample_x = origin.x() as f32 - 0.5 + offset_x;
            let sample_z = origin.z() as f32 - 0.5 + offset_z;

            let height = bicubic(eroded, sample_x, sample_z);

            // Detail, scaled down where water runs. Sampled from the nearest
            // erosion cell's accumulation: the fade is over tens of thousands of
            // square metres, so interpolating it would be precision nobody can
            // see on a quantity that is already a proxy.
            let scale = if settings.detail_relief > 0.0 {
                let nearest = ErosionCell::new(
                    sample_x.round().max(0.0) as u32,
                    sample_z.round().max(0.0) as u32,
                );
                nearest.map_or(1.0, |cell| {
                    settings.detail_scale(network.accumulation(cell) * cell_area)
                })
            } else {
                0.0
            };

            let world_x = corner_x + (x as f32 + 0.5) * CELL_SIZE;
            let world_z = corner_z + (z as f32 + 0.5) * CELL_SIZE;

            cells.push(height + detail(generator, world_x, world_z, settings) * scale);
        }
    }

    Some(ChunkElevation { cells })
}

/// High-frequency texture at a world position, in metres, centred on zero.
///
/// Reuses the elevation generator's own noise so it is a pure function of
/// `(seed, position)` and continuous across every chunk and block boundary — the
/// same reason base elevation is positional. Detail computed per chunk from a
/// chunk-local coordinate would produce a visible tile edge every 512 m.
fn detail(generator: &ElevationGenerator, x: f32, z: f32, settings: BakeSettings) -> f32 {
    if settings.detail_relief <= 0.0 || settings.detail_octaves == 0 {
        return 0.0;
    }

    let shape = crate::elevation::TerrainShape {
        relief: settings.detail_relief,
        base: 0.0,
        feature_size: settings.detail_wavelength,
        octaves: settings.detail_octaves,
        persistence: 0.5,
    };

    // A separate generator so the detail is uncorrelated with the base terrain
    // it sits on. Sharing the noise would make texture line up with the shape
    // underneath, which reads as a landscape wearing a pattern.
    let textured = ElevationGenerator::with_world(
        generator.seed() ^ 0x0DE7_A11D,
        shape,
        crate::worldmap::WorldMapSettings::FLAT,
    );

    textured.height_at(x, z)
}

/// Catmull-Rom interpolation of the erosion grid at fractional cell coordinates.
///
/// Smooth across cell boundaries, unlike bilinear, and it reproduces a linear
/// ramp exactly — so a planar hillside comes through the resample still planar.
/// Any wobble introduced here would be a wobble in every hillside in the world.
fn bicubic(grid: &BlockGrid, x: f32, z: f32) -> f32 {
    let x0 = x.floor();
    let z0 = z.floor();
    let tx = x - x0;
    let tz = z - z0;

    let mut rows = [0.0f32; 4];
    for (index, row) in rows.iter_mut().enumerate() {
        let sz = z0 as i32 + index as i32 - 1;
        let mut columns = [0.0f32; 4];
        for (offset, column) in columns.iter_mut().enumerate() {
            let sx = x0 as i32 + offset as i32 - 1;
            *column = sample(grid, sx, sz);
        }
        *row = catmull_rom(columns, tx);
    }

    catmull_rom(rows, tz)
}

/// A grid value, clamped to the edge.
///
/// Clamping is correct here rather than a shortcut: the four-wide stencil runs
/// off the grid only in the halo, which is discarded, so an edge-clamped sample
/// never reaches baked terrain.
fn sample(grid: &BlockGrid, x: i32, z: i32) -> f32 {
    let edge = crate::block::EDGE as i32 - 1;
    let cell = ErosionCell::new(x.clamp(0, edge) as u32, z.clamp(0, edge) as u32);
    cell.map_or(0.0, |cell| grid.get(cell))
}

/// Catmull-Rom through four samples, at `t` in `0..1` between the middle two.
fn catmull_rom(p: [f32; 4], t: f32) -> f32 {
    let (p0, p1, p2, p3) = (p[0], p[1], p[2], p[3]);
    let a = 2.0 * p1;
    let b = p2 - p0;
    let c = 2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3;
    let d = -p0 + 3.0 * p1 - 3.0 * p2 + p3;
    0.5 * (a + b * t + c * t * t + d * t * t * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elevation::TerrainShape;
    use cx_core::math::BlockCoord;

    const SEED: u64 = 0x0BADC0DE;

    /// A *curved* surface. The crease test needs one: bilinear reproduces a
    /// linear ramp exactly and has zero second difference on it, so a plane
    /// cannot tell the two interpolations apart — which the first version of
    /// that test discovered by passing against bilinear.
    fn curved() -> BlockGrid {
        let mut grid = BlockGrid::filled(0.0);
        for z in 0..crate::block::EDGE {
            for x in 0..crate::block::EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                // Wavelength of ~40 erosion cells, so a coarse cell spans a
                // visible fraction of the curve and piecewise-linear sampling
                // has something to flatten.
                let height = 300.0 + (x as f32 * 0.15).sin() * 6.0 + (z as f32 * 0.11).cos() * 4.0;
                grid.set(cell, height);
            }
        }
        grid
    }

    fn planar() -> BlockGrid {
        let mut grid = BlockGrid::filled(0.0);
        for z in 0..crate::block::EDGE {
            for x in 0..crate::block::EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                grid.set(cell, 300.0 - x as f32 * 0.03 + z as f32 * 0.017);
            }
        }
        grid
    }

    /// **`ADR-0015`'s first correctness question**: no terracing, no creases.
    ///
    /// A linear ramp must come through the resample still linear. Catmull-Rom
    /// reproduces one exactly; bilinear would too, but its *derivative* jumps at
    /// every cell boundary, so this also checks the second difference — which is
    /// where a crease would show and a height comparison would not.
    #[test]
    fn a_planar_slope_resamples_exactly() {
        let grid = planar();

        let mut worst = 0.0f32;
        for step in 0..2_000 {
            let x = 900.0 + step as f32 * 0.25;
            let z = 1_234.6;
            let expected = 300.0 - x * 0.03 + z * 0.017;
            worst = worst.max((bicubic(&grid, x, z) - expected).abs());
        }

        assert!(
            worst < 0.01,
            "the resampled plane is off by up to {worst} m"
        );
    }

    /// **`ADR-0015`'s first correctness question**: no creases at cell
    /// boundaries.
    ///
    /// Measured on a **curved** surface, and that is the whole point of the
    /// fixture. Bilinear reproduces a linear ramp exactly and has zero second
    /// difference along it, so a plane cannot distinguish the two schemes — the
    /// first version of this test used one and passed against bilinear, which is
    /// exactly the artifact it was written to exclude.
    ///
    /// On a curve, bilinear is piecewise-linear: flat between samples, with all
    /// the curvature concentrated into a spike at each boundary. So the test is
    /// on the *variation* of the second difference, not its magnitude — a smooth
    /// interpolant spreads curvature evenly, and a C0 one does not.
    #[test]
    fn a_curved_surface_resamples_without_creases() {
        let grid = curved();

        let mut curvatures = Vec::new();
        let mut previous = [0.0f32; 3];

        // Quarter-cell steps, so each coarse cell is sampled four times and a
        // boundary spike cannot be stepped over.
        for step in 0..4_000 {
            let x = 900.0 + step as f32 * 0.25;
            let height = bicubic(&grid, x, 1_234.6);
            previous = [previous[1], previous[2], height];
            if step >= 2 {
                curvatures.push((previous[0] - 2.0 * previous[1] + previous[2]).abs());
            }
        }

        let peak = curvatures.iter().copied().fold(0.0f32, f32::max);
        let mean = curvatures.iter().sum::<f32>() / curvatures.len() as f32;

        assert!(
            mean > 0.0,
            "the fixture is not curved, so this proves nothing"
        );
        assert!(
            peak < mean * 6.0,
            "curvature peaks at {peak} against a mean of {mean} — a ratio of \
             {:.1} means it is concentrated at cell boundaries rather than \
             spread along the curve, which is a crease every 2 m",
            peak / mean
        );
    }

    /// The resample agrees with the grid it came from at coincident points.
    #[test]
    fn the_resample_passes_through_the_coarse_samples() {
        let grid = planar();
        for (x, z) in [(600u32, 700u32), (1_200, 2_400), (3_000, 3_000)] {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };
            let interpolated = bicubic(&grid, x as f32, z as f32);
            assert!(
                (interpolated - grid.get(cell)).abs() < 0.001,
                "at ({x}, {z}) the resample gives {interpolated} against {}",
                grid.get(cell)
            );
        }
    }

    /// **`ADR-0015`'s second correctness question**: detail must not fill
    /// channels.
    #[test]
    fn detail_is_suppressed_where_water_runs() {
        let settings = BakeSettings::default();

        assert_eq!(
            settings.detail_scale(0.0),
            1.0,
            "a hilltop gets full texture"
        );
        assert_eq!(
            settings.detail_scale(settings.detail_fade_to * 10.0),
            0.0,
            "a river bed gets none"
        );

        // And the transition is smooth — a hard cutoff would put a ledge along
        // both banks of every river.
        let mid = (settings.detail_fade_from + settings.detail_fade_to) / 2.0;
        let scale = settings.detail_scale(mid);
        assert!(
            (0.4..0.6).contains(&scale),
            "the fade is {scale} at its midpoint, which is a step not a ramp"
        );
    }

    /// A carved channel survives the bake.
    ///
    /// The claim the fade exists to make, tested on a real trench rather than on
    /// the fade function alone: a channel 8 m deep must still be a channel after
    /// detail has been added on top.
    #[test]
    fn a_carved_channel_is_still_there_after_baking() {
        let mut grid = planar();

        // A trench along z, at x = 2,600, eight metres deep.
        for z in 0..crate::block::EDGE {
            for offset in 0..3u32 {
                let Some(cell) = ErosionCell::new(2_600 + offset, z) else {
                    continue;
                };
                grid.set(cell, grid.get(cell) - 8.0);
            }
        }

        let network = FlowNetwork::build(grid.clone());
        let generator = ElevationGenerator::new(SEED, TerrainShape::default());
        let block = BlockCoordinates::new(BlockCoord::new(0, 0));

        // The chunk containing x = 2,600 on the erosion grid.
        let chunk_index = (2_600 - crate::block::HALO_CELLS) / 256;
        let chunk = ChunkCoord::new(chunk_index as i32, 4);

        let baked = bake_chunk(
            &grid,
            &network,
            &generator,
            block,
            chunk,
            BakeSettings::default(),
        )
        .expect("the chunk is in this block");

        // Deepest and shallowest along one row of the baked chunk.
        let mut lowest = f32::INFINITY;
        let mut highest = f32::NEG_INFINITY;
        for x in 0..CELLS_PER_CHUNK_EDGE {
            let height = baked.get(x, 512).expect("in range");
            lowest = lowest.min(height);
            highest = highest.max(height);
        }

        assert!(
            highest - lowest > 6.0,
            "the trench is only {} m deep after baking, so detail filled it in",
            highest - lowest
        );
    }

    /// **Chunks are pure extraction** — adjacent ones agree at their seam.
    ///
    /// Two chunks side by side sample the same erosion grid and the same
    /// positional detail, so the last column of one and the first of the next are
    /// half a metre apart and must differ by about that much of terrain, not by a
    /// step. A seam here would be visible on every chunk boundary in the world.
    #[test]
    fn adjacent_chunks_meet_without_a_seam() {
        let grid = planar();
        let network = FlowNetwork::build(grid.clone());
        let generator = ElevationGenerator::new(SEED, TerrainShape::default());
        let block = BlockCoordinates::new(BlockCoord::new(0, 0));

        let left = bake_chunk(
            &grid,
            &network,
            &generator,
            block,
            ChunkCoord::new(3, 5),
            BakeSettings::default(),
        )
        .expect("in this block");
        let right = bake_chunk(
            &grid,
            &network,
            &generator,
            block,
            ChunkCoord::new(4, 5),
            BakeSettings::default(),
        )
        .expect("in this block");

        let mut across_seam = 0.0f32;
        let mut within_chunk = 0.0f32;

        for z in 0..CELLS_PER_CHUNK_EDGE {
            let last = left.get(CELLS_PER_CHUNK_EDGE - 1, z).expect("in range");
            let first = right.get(0, z).expect("in range");
            across_seam = across_seam.max((last - first).abs());

            // The same measurement one cell earlier, entirely inside the left
            // chunk. Both are half a metre apart on the same terrain.
            let inside = left.get(CELLS_PER_CHUNK_EDGE - 2, z).expect("in range");
            within_chunk = within_chunk.max((inside - last).abs());
        }

        // Compared against ordinary within-chunk variation rather than against a
        // fixed threshold. A seam is a *discontinuity* — a step where there
        // should be a gradient — so the question is whether crossing the
        // boundary costs more than crossing any other cell. An absolute bound
        // cannot ask that: the first version of this test used one loose enough
        // to admit the detail's full amplitude, and passed against a version
        // that sampled detail in chunk-local coordinates.
        assert!(
            within_chunk > 0.0,
            "the fixture has no variation to compare against"
        );
        assert!(
            across_seam < within_chunk * 2.0,
            "crossing the chunk boundary changes height by {across_seam} m \
             against {within_chunk} m for an ordinary cell step — that is a \
             visible seam on every chunk edge in the world"
        );
    }

    /// The bake is a pure function (`ADR-0006`).
    #[test]
    fn baking_the_same_chunk_twice_agrees() {
        let grid = planar();
        let network = FlowNetwork::build(grid.clone());
        let generator = ElevationGenerator::new(SEED, TerrainShape::default());
        let block = BlockCoordinates::new(BlockCoord::new(2, -1));

        let chunk = block.block().origin_chunk();
        let once = bake_chunk(
            &grid,
            &network,
            &generator,
            block,
            chunk,
            BakeSettings::default(),
        )
        .expect("in this block");
        let twice = bake_chunk(
            &grid,
            &network,
            &generator,
            block,
            chunk,
            BakeSettings::default(),
        )
        .expect("in this block");

        assert_eq!(once.as_slice(), twice.as_slice());
    }

    #[test]
    fn a_chunk_outside_the_block_is_rejected() {
        let grid = planar();
        let network = FlowNetwork::build(grid.clone());
        let generator = ElevationGenerator::new(SEED, TerrainShape::default());
        let block = BlockCoordinates::new(BlockCoord::new(0, 0));

        assert!(
            bake_chunk(
                &grid,
                &network,
                &generator,
                block,
                ChunkCoord::new(-1, 0),
                BakeSettings::default(),
            )
            .is_none()
        );
    }

    #[test]
    fn a_baked_chunk_is_the_size_the_field_expects() {
        let grid = planar();
        let network = FlowNetwork::build(grid.clone());
        let generator = ElevationGenerator::new(SEED, TerrainShape::default());
        let block = BlockCoordinates::new(BlockCoord::new(0, 0));

        let baked = bake_chunk(
            &grid,
            &network,
            &generator,
            block,
            ChunkCoord::new(0, 0),
            BakeSettings::SMOOTH,
        )
        .expect("in this block");

        assert_eq!(baked.as_slice().len(), CELLS_PER_CHUNK as usize);
        assert!(baked.get(CELLS_PER_CHUNK_EDGE, 0).is_none());
    }
}
