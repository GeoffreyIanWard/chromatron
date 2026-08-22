//! Per-chunk water: standing lakes and flowing channels (S07 step 7).
//!
//! The pipeline already knows where water is — it just never said so out
//! loud. `terrain - ground` is standing water wherever the depression fill
//! raised the surface (that is a lake, and the difference is its depth), and
//! the flow network's accumulation says where enough catchment drains through
//! a cell to be a river. This module reads both into a per-chunk grid a
//! renderer or the sim can consume, without recomputing anything.
//!
//! # Rivers are a judgement call; lakes are not
//!
//! A lake is a fact of the two surfaces. A river is a threshold on drainage
//! area, and the right threshold is the one channel carving already used —
//! [`WaterSettings::DEFAULT`] matches [`crate::CarveSettings::DEFAULT`], so
//! water appears exactly where channels were cut. The visual depth assigned
//! to a channel is likewise a presentation choice, not hydrology: the
//! pipeline does not simulate discharge volume yet, so channel water depth
//! grows gently with catchment area between two clamps and says so here.

use cx_core::math::{CELLS_PER_CHUNK_EDGE, CELLS_PER_EROSION_CELL, ChunkCoord, EROSION_CELL_SIZE};

use crate::block::ErosionCell;
use crate::pipeline::GeneratedBlock;

/// Water-grid cells along one chunk edge (256 — the erosion grid's 2 m).
pub const WATER_CELLS_PER_CHUNK_EDGE: u32 = CELLS_PER_CHUNK_EDGE / CELLS_PER_EROSION_CELL;

/// What counts as water, and how channel water presents.
///
/// Every number is a knob on purpose, like everything else in this crate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterSettings {
    /// Standing depth, in metres, below which a cell is damp ground rather
    /// than lake. Filters the fill's floating-point dust.
    pub min_standing_depth: f32,
    /// Catchment area, in square metres, at which flow becomes a river.
    ///
    /// Keep equal to [`crate::CarveSettings::channel_threshold`] unless you
    /// want water without channels or channels without water.
    pub channel_threshold: f32,
    /// Presented water depth, in metres, of a channel right at the threshold.
    pub channel_depth: f32,
    /// Exponent on `area / threshold` by which channel depth grows.
    pub channel_depth_growth: f32,
    /// Deepest a channel presents, in metres.
    pub channel_max_depth: f32,
}

impl WaterSettings {
    /// Defaults matched to [`crate::CarveSettings::DEFAULT`].
    pub const DEFAULT: Self = Self {
        min_standing_depth: 0.05,
        channel_threshold: 50_000.0,
        channel_depth: 0.5,
        channel_depth_growth: 0.35,
        channel_max_depth: 2.5,
    };
}

impl Default for WaterSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One chunk's water, on the 2 m erosion grid.
///
/// Row-major with +X fastest, [`WATER_CELLS_PER_CHUNK_EDGE`] to a side unless
/// [`ChunkWater::downsample`]d. A dry cell has zero depth; its surface value
/// is meaningless and set to the terrain height so interpolation near shores
/// stays sane.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkWater {
    surface: Vec<f32>,
    depth: Vec<f32>,
    edge: u32,
}

impl ChunkWater {
    /// Water surface elevations, metres. Meaningful only where depth is wet.
    pub fn surface(&self) -> &[f32] {
        &self.surface
    }

    /// Water depths, metres. Zero on dry ground.
    pub fn depth(&self) -> &[f32] {
        &self.depth
    }

    /// Cells along one edge.
    pub const fn edge(&self) -> u32 {
        self.edge
    }

    /// Fraction of cells that are wet, `0..=1`.
    pub fn wet_fraction(&self) -> f32 {
        if self.depth.is_empty() {
            return 0.0;
        }
        let wet = self.depth.iter().filter(|d| **d > 0.0).count();
        wet as f32 / self.depth.len() as f32
    }

    /// Bytes this grid keeps resident, for the lifecycle's budget arithmetic.
    pub fn resident_bytes(&self) -> usize {
        size_of_val(self.surface.as_slice()) + size_of_val(self.depth.as_slice())
    }

    /// A grid at `1/factor` resolution, keeping the *wettest* cell of each
    /// group rather than the average.
    ///
    /// Max, not mean, deliberately: a river is one or two cells wide, and a
    /// mean would dilute it below any threshold and erase every waterway from
    /// far terrain — which is exactly the terrain the big water shapes are
    /// judged from.
    pub fn downsample(&self, factor: u32) -> Option<Self> {
        if factor == 0 || !self.edge.is_multiple_of(factor) {
            return None;
        }
        let edge = self.edge / factor;
        let mut surface = vec![0.0f32; (edge * edge) as usize];
        let mut depth = vec![0.0f32; (edge * edge) as usize];

        for z in 0..edge {
            for x in 0..edge {
                let mut best_depth = 0.0f32;
                let mut best_surface = f32::NEG_INFINITY;
                let mut fallback_surface = f32::NEG_INFINITY;
                for dz in 0..factor {
                    for dx in 0..factor {
                        let index = ((z * factor + dz) * self.edge + (x * factor + dx)) as usize;
                        let (s, d) = match (self.surface.get(index), self.depth.get(index)) {
                            (Some(s), Some(d)) => (*s, *d),
                            _ => continue,
                        };
                        fallback_surface = fallback_surface.max(s);
                        if d > best_depth {
                            best_depth = d;
                            best_surface = s;
                        }
                    }
                }
                let out = (z * edge + x) as usize;
                if let Some(slot) = depth.get_mut(out) {
                    *slot = best_depth;
                }
                if let Some(slot) = surface.get_mut(out) {
                    *slot = if best_depth > 0.0 {
                        best_surface
                    } else {
                        fallback_surface
                    };
                }
            }
        }

        Some(Self {
            surface,
            depth,
            edge,
        })
    }
}

/// Reads one chunk's water out of a generated block.
///
/// Pure extraction, like the bake (`ADR-0006`): nothing is computed that the
/// pipeline does not already hold. Returns `None` when the chunk is not
/// inside this block, or when the chunk is entirely dry — most chunks are,
/// and a grid of zeros would be memory spent recording an absence.
pub fn bake_water(
    block: &GeneratedBlock,
    chunk: ChunkCoord,
    settings: WaterSettings,
) -> Option<ChunkWater> {
    let origin = block.coordinates.chunk_origin_cell(chunk)?;
    let edge = WATER_CELLS_PER_CHUNK_EDGE;
    let cell_area = EROSION_CELL_SIZE * EROSION_CELL_SIZE;

    let mut surface = vec![0.0f32; (edge * edge) as usize];
    let mut depth = vec![0.0f32; (edge * edge) as usize];
    let mut any_wet = false;

    for z in 0..edge {
        for x in 0..edge {
            let Some(cell) = ErosionCell::new(origin.x() + x, origin.z() + z) else {
                continue;
            };
            let terrain = block.terrain.get(cell);
            let standing = block.water_depth(cell);

            let (s, d) = if standing >= settings.min_standing_depth {
                // A lake: the filled surface is the water surface, by
                // construction of the fill.
                (terrain, standing)
            } else {
                let area = block.network.accumulation(cell) * cell_area;
                if area >= settings.channel_threshold && settings.channel_threshold.is_finite() {
                    let presented = (settings.channel_depth
                        * (area / settings.channel_threshold).powf(settings.channel_depth_growth))
                    .min(settings.channel_max_depth);
                    // The carved bed plus the presented depth: water sits in
                    // the channel, not on its banks.
                    (terrain + presented, presented)
                } else {
                    (terrain, 0.0)
                }
            };

            let index = (z * edge + x) as usize;
            if let Some(slot) = surface.get_mut(index) {
                *slot = s;
            }
            if let Some(slot) = depth.get_mut(index) {
                *slot = d;
            }
            any_wet |= d > 0.0;
        }
    }

    // Second pass: flood each channel's trench to its waterline. The spine
    // cell is where the threshold crossed, but carving dug a trench up to
    // [`crate::CarveSettings::max_width`] wide around it — every nearby cell
    // whose ground lies below the spine's water surface is under that water.
    // Without this a river renders as a 2 m thread down the middle of its own
    // 40 m channel. The stamp is a `max`, so overlapping spines commute and
    // the result is order-independent (`ADR-0004`).
    if any_wet && settings.channel_threshold.is_finite() {
        let radius: i32 = 16; // half of max_width, in 2 m cells

        // Spines are collected from `radius` beyond the chunk too — straight
        // off the block, which extends past the chunk on every side (into a
        // neighbouring chunk, or into the halo at a block edge). A trench
        // whose spine runs just across the chunk border must still flood this
        // side, or every chunk edge would cut a dry line through its rivers.
        let spines: Vec<(i32, i32, f32)> = (-radius..edge as i32 + radius)
            .flat_map(|z| (-radius..edge as i32 + radius).map(move |x| (x, z)))
            .filter_map(|(x, z)| {
                let cell = ErosionCell::new(
                    origin.x().checked_add_signed(x)?,
                    origin.z().checked_add_signed(z)?,
                )?;
                // A lake cell spreads its fill level; a channel cell spreads
                // its presented waterline. Lake spines matter: valley floors
                // are chains of small filled basins, and ground just below a
                // basin's level is its outflow — dry there reads as a river
                // inexplicably interrupted at every pond.
                if block.water_depth(cell) >= settings.min_standing_depth {
                    return Some((x, z, block.terrain.get(cell)));
                }
                let area = block.network.accumulation(cell) * cell_area;
                if area < settings.channel_threshold {
                    return None;
                }
                let presented = (settings.channel_depth
                    * (area / settings.channel_threshold).powf(settings.channel_depth_growth))
                .min(settings.channel_max_depth);
                Some((x, z, block.terrain.get(cell) + presented))
            })
            .collect();

        for (x, z, level) in spines {
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    let (nx, nz) = (x + dx, z + dz);
                    if nx < 0 || nz < 0 || nx >= edge as i32 || nz >= edge as i32 {
                        continue;
                    }
                    let Some(cell) =
                        ErosionCell::new(origin.x() + nx as u32, origin.z() + nz as u32)
                    else {
                        continue;
                    };
                    let ground = block.terrain.get(cell);
                    if ground >= level {
                        continue;
                    }
                    // A lake cell's level is the fill's, which is a fact of
                    // the surfaces; the flood is presentation and yields.
                    if block.water_depth(cell) >= settings.min_standing_depth {
                        continue;
                    }
                    let index = (nz * edge as i32 + nx) as usize;
                    let flooded = (level - ground).min(settings.channel_max_depth);
                    if let Some(slot) = depth.get_mut(index)
                        && flooded > *slot
                    {
                        *slot = flooded;
                        if let Some(surface_slot) = surface.get_mut(index) {
                            *surface_slot = level;
                        }
                    }
                }
            }
        }
    }

    any_wet.then_some(ChunkWater {
        surface,
        depth,
        edge,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::WorldSettings;
    use cx_core::math::BlockCoord;

    /// A real, small-cost source of truth: generate one block once and share
    /// it. The pipeline tests already establish what the two surfaces mean;
    /// these tests establish that this module reads them faithfully.
    fn generated() -> &'static GeneratedBlock {
        use std::sync::OnceLock;
        static BLOCK: OnceLock<GeneratedBlock> = OnceLock::new();
        BLOCK.get_or_init(|| {
            crate::pipeline::generate_block(7, BlockCoord::new(0, 0), WorldSettings::default())
        })
    }

    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn water_matches_the_two_surfaces_and_the_network() {
        let block = generated();
        let settings = WaterSettings::DEFAULT;

        let mut wet_chunks = 0;
        let origin = block.coordinates.block().origin_chunk();
        for dz in 0..16 {
            for dx in 0..16 {
                let chunk = ChunkCoord::new(origin.x + dx, origin.z + dz);
                let Some(water) = bake_water(block, chunk, settings) else {
                    continue;
                };
                wet_chunks += 1;

                let start = block.coordinates.chunk_origin_cell(chunk).expect("inside");
                for z in 0..WATER_CELLS_PER_CHUNK_EDGE {
                    for x in 0..WATER_CELLS_PER_CHUNK_EDGE {
                        let index = (z * water.edge() + x) as usize;
                        let depth = water.depth()[index];
                        if depth == 0.0 {
                            continue;
                        }
                        let cell = ErosionCell::new(start.x() + x, start.z() + z).expect("in");
                        let standing = block.water_depth(cell);
                        if standing >= settings.min_standing_depth {
                            // Lake: depth is the fill difference, surface is
                            // the filled terrain, exactly.
                            assert_eq!(depth, standing);
                            assert_eq!(water.surface()[index], block.terrain.get(cell));
                        } else {
                            // River, or its flooded trench. A spine cell
                            // crossed the threshold itself; a bank cell lies
                            // below a nearby spine's waterline. Both are
                            // bounded by the presentation clamp.
                            assert!(depth > 0.0);
                            assert!(depth <= settings.channel_max_depth);
                        }
                    }
                }
            }
        }

        // A full-sim block third-covered in basins must have wet chunks; a
        // module that returns None everywhere would pass every loop above.
        assert!(
            wet_chunks > 0,
            "an eroded block should have water somewhere"
        );
    }

    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn a_dry_chunk_is_none_not_zeros() {
        // With both thresholds infinite nothing qualifies as water, so every
        // chunk must come back None — the contract that dryness costs no
        // memory. (Seed 7's block has water in all 256 chunks at the real
        // thresholds; the drainage network reaches everywhere, which is what
        // it is for.)
        let block = generated();
        let settings = WaterSettings {
            min_standing_depth: f32::INFINITY,
            channel_threshold: f32::INFINITY,
            ..WaterSettings::DEFAULT
        };
        let origin = block.coordinates.block().origin_chunk();
        for dz in 0..16 {
            for dx in 0..16 {
                let chunk = ChunkCoord::new(origin.x + dx, origin.z + dz);
                assert!(bake_water(block, chunk, settings).is_none());
            }
        }
    }

    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn outside_the_block_is_none() {
        let block = generated();
        assert!(bake_water(block, ChunkCoord::new(1_000, 1_000), WaterSettings::DEFAULT).is_none());
    }

    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn downsampling_keeps_rivers_rather_than_averaging_them_away() {
        let block = generated();
        let origin = block.coordinates.block().origin_chunk();

        let mut checked = false;
        for dz in 0..16 {
            for dx in 0..16 {
                let chunk = ChunkCoord::new(origin.x + dx, origin.z + dz);
                let Some(water) = bake_water(block, chunk, WaterSettings::DEFAULT) else {
                    continue;
                };
                let coarse = water.downsample(2).expect("256 divides by 2");
                assert_eq!(coarse.edge(), WATER_CELLS_PER_CHUNK_EDGE / 2);

                // Max-pooling can only keep or increase the wet fraction of
                // what survives; above all it must not erase the water.
                assert!(
                    coarse.wet_fraction() > 0.0,
                    "a wet chunk downsampled to a dry one"
                );
                checked = true;
            }
        }
        assert!(checked, "no wet chunk to downsample");
    }

    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn an_infinite_threshold_still_finds_lakes() {
        // The no-erosion profile carves nothing (threshold = infinity), but a
        // filled basin is still a lake — lakes are facts, not thresholds.
        let block = generated();
        let settings = WaterSettings {
            channel_threshold: f32::INFINITY,
            ..WaterSettings::DEFAULT
        };
        let origin = block.coordinates.block().origin_chunk();

        let mut lake_cells = 0usize;
        for dz in 0..16 {
            for dx in 0..16 {
                let chunk = ChunkCoord::new(origin.x + dx, origin.z + dz);
                if let Some(water) = bake_water(block, chunk, settings) {
                    lake_cells += water.depth().iter().filter(|d| **d > 0.0).count();
                }
            }
        }
        assert!(
            lake_cells > 0,
            "a third of this block is filled basin; lakes must survive an infinite river threshold"
        );
    }
}
