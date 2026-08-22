//! The regional water model: shared pour levels for trans-seam basins.
//!
//! # The defect this exists to fix
//!
//! The depression fill runs per block with an open boundary — water escapes
//! wherever the grid happens to cut the terrain. A basin that spans a block
//! seam is therefore filled to a *different* level by each of the blocks that
//! see part of it: each block's flood exits through its own grid cut, and
//! the true spill saddle may lie beyond the other's halo entirely. Measured
//! by the seam walk in `tests/block_pipeline.rs`, the result was uphill steps
//! of up to 94 m in the rendered ground exactly where channels cross seams.
//!
//! # The fix: agreement by construction, not by luck
//!
//! Sample base elevation on a **world-aligned** coarse lattice covering the
//! block and its neighbours, fill *that* once, and use the coarse filled
//! surface as a floor under every fine fill's boundary: the fine flood may
//! not leave the grid below the level the region says water stands at that
//! position. Both blocks compute this from the same seed over the same world
//! lattice points — a pure positional function (`ADR-0006`) — so they arrive
//! at the *same* floor independently. The coarse model does not have to be
//! exactly right for the seam to close; it has to be **shared**, and shared
//! is what "pure function of (seed, position)" buys.
//!
//! # The margin knob
//!
//! Erosion lowers spill saddles, and the coarse model samples the uneroded
//! base — so the raw coarse level runs high, and sealing to it would overfill
//! basins whose outlets erosion has cut down. [`RegionSettings::margin`]
//! lowers the floor to compensate. Too much margin and seams reopen; too
//! little and lakes stand above their carved outlets. The seam walk measures
//! the first and renders judge the second, which is what made it a knob.

use cx_core::math::{BLOCK_SIZE, EROSION_CELL_SIZE};

use crate::block::BlockCoordinates;
use crate::elevation::ElevationGenerator;
use crate::flow::BoundarySeal;

/// How the regional model is sized. Every number is a knob on purpose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegionSettings {
    /// Coarse lattice spacing, metres. Small enough to see real spill
    /// saddles, large enough that a 3x3-block region stays a footnote in the
    /// generation bill.
    pub cell_size: f32,
    /// Blocks of neighbourhood on each side. One block each way already
    /// exceeds any basin the halo could half-see.
    pub radius_blocks: i32,
    /// Metres subtracted from the coarse floor. With the coarse model eroded
    /// (below), this no longer has to guess how far erosion cuts saddles —
    /// it only absorbs the coarse lattice's own resolution error.
    pub margin: f32,
    /// Coarse erosion rounds. The first floor was built from *uneroded* base
    /// elevation, and its pour levels missed the fine surface's by however
    /// much erosion had cut each saddle — a per-saddle error no constant
    /// margin can absorb (measured: the seam-step median got worse, not
    /// better). Eroding the coarse model with the same stream-power law
    /// makes its saddles track the fine ones.
    pub erosion_rounds: u32,
    /// Coarse erosion timestep, matched to the fine pipeline's total.
    pub erosion_timestep: f32,
    /// Coarse erodibility, matched to the fine pipeline's.
    pub erodibility: f32,
}

impl RegionSettings {
    /// Defaults: 32 m lattice, one block of neighbourhood, 3 m of margin,
    /// erosion matched to [`crate::hydraulic::ErosionSettings::DEFAULT`]'s
    /// total (6 rounds x 4e4).
    pub const DEFAULT: Self = Self {
        cell_size: 32.0,
        radius_blocks: 1,
        margin: 3.0,
        erosion_rounds: 6,
        erosion_timestep: 4.0e4,
        erodibility: 4.0e-5,
    };
}

impl Default for RegionSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The coarse filled surface around one block.
#[derive(Debug, Clone)]
pub struct RegionalWater {
    /// World-lattice index of the first cell, both axes.
    start: (i64, i64),
    /// Cells along each axis.
    width: usize,
    height: usize,
    cell_size: f32,
    margin: f32,
    /// Coarse filled heights, row-major with +X fastest.
    filled: Vec<f32>,
}

impl RegionalWater {
    /// Builds the model for `block` and its neighbourhood.
    ///
    /// The lattice is aligned to the **world**, not to the block: cell `i`
    /// covers world x in `[i·cell, (i+1)·cell)` for a global integer `i`.
    /// Two adjacent blocks therefore sample byte-identical heights at every
    /// lattice point their regions share, which is the whole trick.
    pub fn for_block(
        generator: &ElevationGenerator,
        block: BlockCoordinates,
        settings: RegionSettings,
    ) -> Self {
        let cell = settings.cell_size.max(1.0);
        let reach = settings.radius_blocks.max(0) as f32 * BLOCK_SIZE;

        // The fine grid spans the block plus its halo; the region spans that
        // plus the neighbourhood, snapped outward to lattice lines.
        let (min_x, min_z) = block.cell_centre(0, 0);
        let halo = EROSION_CELL_SIZE; // centre-to-corner slack
        let low_x = ((min_x - halo - reach) / cell).floor() as i64;
        let low_z = ((min_z - halo - reach) / cell).floor() as i64;
        let span = BLOCK_SIZE
            + 2.0 * (reach + 2.0 * halo)
            + crate::block::HALO_CELLS as f32 * EROSION_CELL_SIZE * 2.0;
        let cells_across = (span / cell).ceil() as usize + 2;

        let mut filled = vec![0.0f32; cells_across * cells_across];
        for z in 0..cells_across {
            for x in 0..cells_across {
                let world_x = (low_x + x as i64) as f32 * cell + 0.5 * cell;
                let world_z = (low_z + z as i64) as f32 * cell + 0.5 * cell;
                if let Some(slot) = filled.get_mut(z * cells_across + x) {
                    *slot = generator.height_at(world_x, world_z);
                }
            }
        }

        // Erode the coarse surface with the same implicit stream-power law
        // the fine pipeline uses, so its spill saddles are cut roughly as far
        // as the fine surface's will be. Single-receiver D8 at this scale:
        // the coarse model informs pour *levels*; nobody renders it.
        erode_coarse(&mut filled, cells_across, cells_across, cell, settings);
        fill_coarse(&mut filled, cells_across, cells_across);

        Self {
            start: (low_x, low_z),
            width: cells_across,
            height: cells_across,
            cell_size: cell,
            margin: settings.margin,
            filled,
        }
    }

    /// The regional water floor at a world position, metres — the level the
    /// region says water stands (or flows) at, less the margin.
    ///
    /// Nearest lattice cell, deliberately uninterpolated: two blocks asking
    /// about the same position must get the same answer, and nearest-cell on
    /// a shared lattice cannot disagree.
    pub fn floor_at(&self, world_x: f32, world_z: f32) -> f32 {
        let x = ((world_x / self.cell_size).floor() as i64 - self.start.0)
            .clamp(0, self.width as i64 - 1) as usize;
        let z = ((world_z / self.cell_size).floor() as i64 - self.start.1)
            .clamp(0, self.height as i64 - 1) as usize;
        self.filled
            .get(z * self.width + x)
            .copied()
            .unwrap_or(f32::NEG_INFINITY)
            - self.margin
    }

    /// The seal for a block's fine-grid boundary: the floor sampled at every
    /// boundary cell of the (haloed) grid.
    pub fn boundary_seal(&self, block: BlockCoordinates) -> BoundarySeal {
        let edge = crate::block::EDGE;
        let mut seal = BoundarySeal::open();
        for i in 0..edge {
            let (x0, z0) = block.cell_centre(i, 0);
            let (x1, z1) = block.cell_centre(i, edge - 1);
            let (x2, z2) = block.cell_centre(0, i);
            let (x3, z3) = block.cell_centre(edge - 1, i);
            if let Some(slot) = seal.north.get_mut(i as usize) {
                *slot = self.floor_at(x0, z0);
            }
            if let Some(slot) = seal.south.get_mut(i as usize) {
                *slot = self.floor_at(x1, z1);
            }
            if let Some(slot) = seal.west.get_mut(i as usize) {
                *slot = self.floor_at(x2, z2);
            }
            if let Some(slot) = seal.east.get_mut(i as usize) {
                *slot = self.floor_at(x3, z3);
            }
        }
        seal
    }
}

/// Priority-flood fill over an arbitrary coarse grid.
///
/// The same algorithm as the fine fill in `flow.rs`, at a size where clarity
/// beats sharing: a heap, boundary-seeded, raising every interior cell to the
/// level water must reach to escape.
fn fill_coarse(heights: &mut [f32], width: usize, height: usize) {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let index = |x: usize, z: usize| z * width + x;
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    let mut done = vec![false; heights.len()];

    let key = |h: f32| {
        // Total order over f32 bits, matching `flow::order_key`'s intent.
        let bits = h.to_bits();
        if bits & 0x8000_0000 == 0 {
            bits ^ 0x8000_0000
        } else {
            !bits
        }
    };

    let push = |heap: &mut BinaryHeap<Reverse<(u32, u32)>>,
                    done: &mut [bool],
                    x: usize,
                    z: usize,
                    h: f32| {
        let at = index(x, z);
        if let Some(slot) = done.get_mut(at)
            && !*slot
        {
            *slot = true;
            heap.push(Reverse((key(h), at as u32)));
        }
    };

    for x in 0..width {
        for z in [0, height - 1] {
            let h = heights.get(index(x, z)).copied().unwrap_or(0.0);
            push(&mut heap, &mut done, x, z, h);
        }
    }
    for z in 1..height.saturating_sub(1) {
        for x in [0, width - 1] {
            let h = heights.get(index(x, z)).copied().unwrap_or(0.0);
            push(&mut heap, &mut done, x, z, h);
        }
    }

    while let Some(Reverse((_, at))) = heap.pop() {
        let at = at as usize;
        let (x, z) = (at % width, at / width);
        let here = heights.get(at).copied().unwrap_or(0.0);

        for (dx, dz) in [
            (0i64, -1i64),
            (0, 1),
            (-1, 0),
            (1, 0),
            (-1, -1),
            (1, -1),
            (-1, 1),
            (1, 1),
        ] {
            let nx = x as i64 + dx;
            let nz = z as i64 + dz;
            if nx < 0 || nz < 0 || nx >= width as i64 || nz >= height as i64 {
                continue;
            }
            let (nx, nz) = (nx as usize, nz as usize);
            let next = index(nx, nz);
            if done.get(next).copied().unwrap_or(true) {
                continue;
            }
            let raised = heights.get(next).copied().unwrap_or(0.0).max(here);
            if let Some(slot) = heights.get_mut(next) {
                *slot = raised;
            }
            if let Some(slot) = done.get_mut(next) {
                *slot = true;
            }
            heap.push(Reverse((key(raised), next as u32)));
        }
    }
}

/// Coarse implicit stream-power erosion: fill, route D8, accumulate, solve,
/// repeated. A miniature of the fine pipeline with none of its refinements —
/// multi-receiver weighting, hardness, thermal, carving — because the only
/// thing anyone reads from this surface is where water pours out of basins.
fn erode_coarse(
    heights: &mut [f32],
    width: usize,
    height: usize,
    cell_size: f32,
    settings: RegionSettings,
) {
    let index = |x: usize, z: usize| z * width + x;
    let neighbours: [(i64, i64); 8] = [
        (0, -1),
        (0, 1),
        (-1, 0),
        (1, 0),
        (-1, -1),
        (1, -1),
        (-1, 1),
        (1, 1),
    ];

    for _ in 0..settings.erosion_rounds {
        // Fill, so routing terminates at the boundary rather than in pits.
        fill_coarse(heights, width, height);

        // D8: steepest descent, gradient not drop.
        let mut receiver: Vec<u32> = vec![u32::MAX; heights.len()];
        for z in 0..height {
            for x in 0..width {
                let here = heights.get(index(x, z)).copied().unwrap_or(0.0);
                let mut best = 0.0f32;
                for (dx, dz) in neighbours {
                    let nx = x as i64 + dx;
                    let nz = z as i64 + dz;
                    if nx < 0 || nz < 0 || nx >= width as i64 || nz >= height as i64 {
                        continue;
                    }
                    let next = index(nx as usize, nz as usize);
                    let drop = here - heights.get(next).copied().unwrap_or(0.0);
                    if drop <= 0.0 {
                        continue;
                    }
                    let distance = if dx.abs() + dz.abs() == 2 {
                        cell_size * std::f32::consts::SQRT_2
                    } else {
                        cell_size
                    };
                    let gradient = drop / distance;
                    if gradient > best {
                        best = gradient;
                        if let Some(slot) = receiver.get_mut(index(x, z)) {
                            *slot = next as u32;
                        }
                    }
                }
            }
        }

        // Topological order by height, descending: on a filled surface every
        // receiver is strictly lower or a flat (flats keep MAX receiver =
        // none and are skipped), so sorting by height gives receivers-first
        // when walked ascending.
        let mut order: Vec<u32> = (0..heights.len() as u32).collect();
        order.sort_by(|a, b| {
            let ha = heights.get(*a as usize).copied().unwrap_or(0.0);
            let hb = heights.get(*b as usize).copied().unwrap_or(0.0);
            ha.total_cmp(&hb).then(a.cmp(b))
        });

        // Accumulate ascending-from-the-top: walk descending heights adding
        // each cell's area to its receiver.
        let cell_area = cell_size * cell_size;
        let mut area: Vec<f32> = vec![cell_area; heights.len()];
        for at in order.iter().rev() {
            let Some(to) = receiver.get(*at as usize).copied() else {
                continue;
            };
            if to == u32::MAX {
                continue;
            }
            let carried = area.get(*at as usize).copied().unwrap_or(0.0);
            if let Some(slot) = area.get_mut(to as usize) {
                *slot += carried;
            }
        }

        // The implicit solve, receivers first (ascending heights).
        for at in &order {
            let at = *at as usize;
            let Some(to) = receiver.get(at).copied() else {
                continue;
            };
            if to == u32::MAX {
                continue;
            }
            let here = heights.get(at).copied().unwrap_or(0.0);
            let there = heights.get(to as usize).copied().unwrap_or(0.0);
            if here <= there {
                continue;
            }
            let factor = f64::from(settings.erosion_timestep)
                * f64::from(settings.erodibility)
                * f64::from(area.get(at).copied().unwrap_or(0.0)).sqrt()
                / f64::from(cell_size);
            let updated = (f64::from(here) + factor * f64::from(there)) / (1.0 + factor);
            if let Some(slot) = heights.get_mut(at) {
                *slot = updated as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elevation::TerrainShape;
    use crate::worldmap::WorldMapSettings;
    use cx_core::math::BlockCoord;

    fn generator() -> ElevationGenerator {
        ElevationGenerator::with_world(7, TerrainShape::DEFAULT, WorldMapSettings::DEFAULT)
    }

    #[test]
    fn adjacent_blocks_agree_on_the_floor_everywhere_they_overlap() {
        // The whole point: the floor is a pure function of (seed, position),
        // so two blocks' regional models must give bit-identical answers at
        // shared positions — the seam between them above all.
        let generator = generator();
        let west = RegionalWater::for_block(
            &generator,
            BlockCoordinates::new(BlockCoord::new(0, 0)),
            RegionSettings::DEFAULT,
        );
        let east = RegionalWater::for_block(
            &generator,
            BlockCoordinates::new(BlockCoord::new(1, 0)),
            RegionSettings::DEFAULT,
        );

        let seam_x = BLOCK_SIZE;
        for step in 0..64 {
            let z = step as f32 * 128.0;
            for dx in [-900.0f32, -64.0, 0.0, 64.0, 900.0] {
                assert_eq!(
                    west.floor_at(seam_x + dx, z),
                    east.floor_at(seam_x + dx, z),
                    "the two blocks disagree about the floor at ({}, {z})",
                    seam_x + dx
                );
            }
        }
    }

    #[test]
    fn the_floor_tracks_the_terrain_within_erosions_reach() {
        // The coarse model erodes and then fills, so the floor sits below the
        // sampled base wherever erosion cut and above it wherever a basin
        // filled — but never outside the band those two processes can reach.
        // A floor hundreds of metres adrift would mean the lattice, the
        // erosion, or the fill is broken.
        let generator = generator();
        let region = RegionalWater::for_block(
            &generator,
            BlockCoordinates::new(BlockCoord::new(0, 0)),
            RegionSettings::DEFAULT,
        );

        for z in (0..8192).step_by(511) {
            for x in (0..8192).step_by(511) {
                let sampled = generator.height_at(x as f32 + 16.0, z as f32 + 16.0);
                let floor = region.floor_at(x as f32 + 16.0, z as f32 + 16.0);
                assert!(
                    (floor - sampled).abs() <= 250.0,
                    "the floor at ({x}, {z}) is {floor} m against terrain at \
                     {sampled} m — outside anything erosion and filling can do"
                );
            }
        }
    }

    #[test]
    fn the_seal_covers_every_boundary_cell() {
        let generator = generator();
        let region = RegionalWater::for_block(
            &generator,
            BlockCoordinates::new(BlockCoord::new(0, 0)),
            RegionSettings::DEFAULT,
        );
        let seal = region.boundary_seal(BlockCoordinates::new(BlockCoord::new(0, 0)));

        for side in [&seal.north, &seal.south, &seal.west, &seal.east] {
            assert_eq!(side.len(), crate::block::EDGE as usize);
            assert!(
                side.iter().all(|v| v.is_finite()),
                "an unsealed boundary cell"
            );
        }
    }
}
