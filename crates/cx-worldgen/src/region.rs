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
use crate::flow::BlockBoundary;

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

impl RegionSettings {
    /// No regional model at all: every boundary stays open, exactly the
    /// pre-regional behaviour. For profiles whose semantics do not involve
    /// seams — `no-erosion` above all, whose terrain has no erosion for the
    /// coarse model to track — and for tests that generate throwaway blocks
    /// where the model would be minutes of CI time spent proving nothing.
    pub const NONE: Self = Self {
        cell_size: 32.0,
        radius_blocks: -1,
        margin: 0.0,
        erosion_rounds: 0,
        erosion_timestep: 0.0,
        erodibility: 0.0,
    };

    /// Whether this configuration builds a model at all.
    pub const fn is_none(&self) -> bool {
        self.radius_blocks < 0
    }
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
    /// Each coarse cell's D8 receiver on the final surface, `u32::MAX` none.
    receiver: Vec<u32>,
    /// Accumulated catchment per coarse cell, square metres.
    area: Vec<f32>,
}

/// A block edge, for edge-canonical windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// `z = 0`.
    North,
    /// `z = EDGE - 1`.
    South,
    /// `x = 0`.
    West,
    /// `x = EDGE - 1`.
    East,
}

impl RegionalWater {
    /// Builds the model for the window **canonical to one edge** of a block.
    ///
    /// Centred on the edge's midpoint — a point the two blocks sharing that
    /// edge agree on exactly — so both construct byte-identical models for
    /// it. This is what restores cross-block agreement now that the coarse
    /// drainage is window-dependent: flat routing and accumulation see the
    /// whole window, so *which* window must be a property of the seam, not
    /// of whichever block asked.
    pub(crate) fn for_edge(
        generator: &ElevationGenerator,
        block: BlockCoordinates,
        side: Side,
        settings: RegionSettings,
    ) -> Self {
        let origin = block.block();
        let (mid_x, mid_z) = match side {
            Side::North => (
                (origin.x as f32 + 0.5) * BLOCK_SIZE,
                origin.z as f32 * BLOCK_SIZE,
            ),
            Side::South => (
                (origin.x as f32 + 0.5) * BLOCK_SIZE,
                (origin.z as f32 + 1.0) * BLOCK_SIZE,
            ),
            Side::West => (
                origin.x as f32 * BLOCK_SIZE,
                (origin.z as f32 + 0.5) * BLOCK_SIZE,
            ),
            Side::East => (
                (origin.x as f32 + 1.0) * BLOCK_SIZE,
                (origin.z as f32 + 0.5) * BLOCK_SIZE,
            ),
        };
        Self::around(generator, mid_x, mid_z, settings)
    }

    /// Builds the model for `block` and its neighbourhood, centred on the
    /// block. Kept for uses that want one window per block (tests, tooling);
    /// boundary conditions use [`RegionalWater::for_edge`] instead.
    ///
    /// The lattice is aligned to the **world**, not to the block: cell `i`
    /// covers world x in `[i·cell, (i+1)·cell)` for a global integer `i`.
    pub fn for_block(
        generator: &ElevationGenerator,
        block: BlockCoordinates,
        settings: RegionSettings,
    ) -> Self {
        let origin = block.block();
        Self::around(
            generator,
            (origin.x as f32 + 0.5) * BLOCK_SIZE,
            (origin.z as f32 + 0.5) * BLOCK_SIZE,
            settings,
        )
    }

    /// The window itself: a square of coarse lattice centred near a world
    /// point, snapped outward to lattice lines.
    fn around(
        generator: &ElevationGenerator,
        centre_x: f32,
        centre_z: f32,
        settings: RegionSettings,
    ) -> Self {
        if settings.is_none() {
            return Self {
                start: (0, 0),
                width: 0,
                height: 0,
                cell_size: 1.0,
                margin: 0.0,
                filled: Vec::new(),
                receiver: Vec::new(),
                area: Vec::new(),
            };
        }
        let cell = settings.cell_size.max(1.0);
        let reach = settings.radius_blocks.max(0) as f32 * BLOCK_SIZE;

        // Wide enough that, centred on an edge midpoint, the window covers
        // both adjacent blocks' haloed grids plus the neighbourhood reach.
        let halo_span = crate::block::HALO_CELLS as f32 * EROSION_CELL_SIZE;
        let half = BLOCK_SIZE + reach + halo_span + 2.0 * EROSION_CELL_SIZE;
        let low_x = ((centre_x - half) / cell).floor() as i64;
        let low_z = ((centre_z - half) / cell).floor() as i64;
        let cells_across = ((2.0 * half) / cell).ceil() as usize + 2;

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

        // Route the final surface once and keep the drainage: the influx a
        // block's boundary receives is read straight off it.
        let (receiver, _, area) = route_coarse(&filled, cells_across, cells_across, cell);

        Self {
            start: (low_x, low_z),
            width: cells_across,
            height: cells_across,
            cell_size: cell,
            margin: settings.margin,
            filled,
            receiver,
            area,
        }
    }

    /// The regional water floor at a world position, metres — the level the
    /// region says water stands (or flows) at, less the margin.
    ///
    /// Nearest lattice cell, deliberately uninterpolated: two blocks asking
    /// about the same position must get the same answer, and nearest-cell on
    /// a shared lattice cannot disagree.
    pub fn floor_at(&self, world_x: f32, world_z: f32) -> f32 {
        if self.filled.is_empty() {
            return f32::NEG_INFINITY;
        }
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

    /// A block's boundary conditions: pour floors at every boundary cell,
    /// and the influx of drainage the region routes into the grid.
    ///
    /// Each side's conditions come from the window **canonical to that
    /// edge** ([`RegionalWater::for_edge`]), so the two blocks sharing a
    /// seam derive byte-identical floors and influx for it — the property
    /// every seam guarantee here rests on. Influx is read off the edge
    /// window's coarse drainage: every coarse cell outside the block's fine
    /// grid whose receiver lies inside deposits its catchment onto the fine
    /// boundary cell nearest the entry, restricted to the side the flow
    /// actually crossed.
    pub fn boundary_for_block(
        generator: &ElevationGenerator,
        block: BlockCoordinates,
        settings: RegionSettings,
    ) -> BlockBoundary {
        let mut boundary = BlockBoundary::open();
        if settings.is_none() {
            return boundary;
        }
        // Four independent windows, built concurrently; applied in a fixed
        // order afterwards, so the result is the sequential one exactly.
        let sides = [Side::North, Side::South, Side::West, Side::East];
        let models: Vec<(Side, Self)> = std::thread::scope(|scope| {
            let handles: Vec<_> = sides
                .into_iter()
                .map(|side| {
                    scope.spawn(move || (side, Self::for_edge(generator, block, side, settings)))
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .collect()
        });
        for (side, model) in &models {
            model.apply_side(&mut boundary, *side, block);
        }
        boundary
    }

    /// Writes one side's floors and influx into `boundary` from this model.
    fn apply_side(&self, boundary: &mut BlockBoundary, side: Side, block: BlockCoordinates) {
        let edge = crate::block::EDGE;

        // Floors along the side.
        for i in 0..edge {
            let (world_x, world_z) = match side {
                Side::North => block.cell_centre(i, 0),
                Side::South => block.cell_centre(i, edge - 1),
                Side::West => block.cell_centre(0, i),
                Side::East => block.cell_centre(edge - 1, i),
            };
            let floors = match side {
                Side::North => &mut boundary.north,
                Side::South => &mut boundary.south,
                Side::West => &mut boundary.west,
                Side::East => &mut boundary.east,
            };
            if let Some(slot) = floors.get_mut(i as usize) {
                *slot = self.floor_at(world_x, world_z);
            }
        }

        if self.filled.is_empty() {
            return;
        }

        // The fine grid's world rectangle, from corner cell centres plus the
        // half-cell to the true edge.
        let half = EROSION_CELL_SIZE / 2.0;
        let (min_cx, min_cz) = block.cell_centre(0, 0);
        let (max_cx, max_cz) = block.cell_centre(edge - 1, edge - 1);
        let (min_x, min_z) = (min_cx - half, min_cz - half);
        let (max_x, max_z) = (max_cx + half, max_cz + half);
        let inside = |x: f32, z: f32| x >= min_x && x < max_x && z >= min_z && z < max_z;
        let fine_index = |along: f32, from: f32| -> usize {
            (((along - from) / EROSION_CELL_SIZE).floor() as i64).clamp(0, edge as i64 - 1) as usize
        };

        for cz in 0..self.height {
            for cx in 0..self.width {
                let at = cz * self.width + cx;
                let world_x =
                    (self.start.0 + cx as i64) as f32 * self.cell_size + 0.5 * self.cell_size;
                let world_z =
                    (self.start.1 + cz as i64) as f32 * self.cell_size + 0.5 * self.cell_size;
                if inside(world_x, world_z) {
                    continue;
                }
                let Some(to) = self.receiver.get(at).copied() else {
                    continue;
                };
                if to == u32::MAX {
                    continue;
                }
                let rx = (to as usize) % self.width;
                let rz = (to as usize) / self.width;
                let recv_x =
                    (self.start.0 + rx as i64) as f32 * self.cell_size + 0.5 * self.cell_size;
                let recv_z =
                    (self.start.1 + rz as i64) as f32 * self.cell_size + 0.5 * self.cell_size;
                if !inside(recv_x, recv_z) {
                    continue;
                }

                // Which edge the flow crossed — kept only when it is the
                // side this model is canonical for.
                let crossed = if world_x < min_x {
                    Side::West
                } else if world_x >= max_x {
                    Side::East
                } else if world_z < min_z {
                    Side::North
                } else {
                    Side::South
                };
                if crossed != side {
                    continue;
                }

                let carried = self.area.get(at).copied().unwrap_or(0.0);
                let slot = match side {
                    Side::West => boundary.influx_west.get_mut(fine_index(recv_z, min_z)),
                    Side::East => boundary.influx_east.get_mut(fine_index(recv_z, min_z)),
                    Side::North => boundary.influx_north.get_mut(fine_index(recv_x, min_x)),
                    Side::South => boundary.influx_south.get_mut(fine_index(recv_x, min_x)),
                };
                if let Some(slot) = slot {
                    *slot += carried;
                }
            }
        }
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

/// [`route_coarse`] without flat continuation: drainage dies at every flat.
///
/// Kept as its own thing because the coarse *erosion* is calibrated against
/// it — see the comment at its use. Height-sorted order is receiver-first
/// here precisely because flats have no receivers to mis-order.
fn route_coarse_truncated(
    heights: &[f32],
    width: usize,
    height: usize,
    cell_size: f32,
) -> (Vec<u32>, Vec<u32>, Vec<f32>) {
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

    let mut order: Vec<u32> = (0..heights.len() as u32).collect();
    order.sort_by(|a, b| {
        let ha = heights.get(*a as usize).copied().unwrap_or(0.0);
        let hb = heights.get(*b as usize).copied().unwrap_or(0.0);
        ha.total_cmp(&hb).then(a.cmp(b))
    });

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

    (receiver, order, area)
}

/// D8 routing and accumulation over a coarse grid, flats included.
///
/// Returns each cell's receiver (`u32::MAX` for none), a source-first
/// topological order, and the accumulated catchment area in square metres.
fn route_coarse(
    heights: &[f32],
    width: usize,
    height: usize,
    cell_size: f32,
) -> (Vec<u32>, Vec<u32>, Vec<f32>) {
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

    // Route the flats. A filled surface leaves every lake and basin floor
    // with no downhill neighbour, and a receiver of "none" there truncates
    // every river at its first flat — measured as coarse rivers two orders
    // of magnitude smaller than the fine ones they were meant to inform.
    // The cure is the same as the fine pipeline's `resolve_flats`, in
    // miniature: breadth-first from each flat's outlets (flat cells that do
    // have a downhill neighbour), walking across equal-height neighbours and
    // pointing each newly reached cell back the way the wave came. Fixed
    // neighbour order and a FIFO queue keep it deterministic.
    let mut wave: std::collections::VecDeque<u32> = (0..heights.len() as u32)
        .filter(|at| receiver.get(*at as usize).copied().unwrap_or(u32::MAX) != u32::MAX)
        .collect();
    while let Some(at) = wave.pop_front() {
        let (x, z) = ((at as usize) % width, (at as usize) / width);
        let here = heights.get(at as usize).copied().unwrap_or(0.0);
        for (dx, dz) in neighbours {
            let nx = x as i64 + dx;
            let nz = z as i64 + dz;
            if nx < 0 || nz < 0 || nx >= width as i64 || nz >= height as i64 {
                continue;
            }
            let next = index(nx as usize, nz as usize);
            if receiver.get(next).copied().unwrap_or(0) != u32::MAX {
                continue;
            }
            #[allow(
                clippy::float_cmp,
                reason = "a flat is *exactly* equal heights — the fill raises basin cells \
                          to precisely their pour level, and the wave must walk that \
                          exact plateau, not a tolerance band around it"
            )]
            if heights.get(next).copied().unwrap_or(0.0) != here {
                continue;
            }
            if let Some(slot) = receiver.get_mut(next) {
                *slot = at;
            }
            wave.push_back(next as u32);
        }
    }

    // Accumulate by in-degree (Kahn), not by height order: within a flat the
    // receiver chain runs at one height, and a height sort would happily
    // drain a cell before its senders had paid in.
    let mut incoming = vec![0u32; heights.len()];
    for to in &receiver {
        if *to != u32::MAX
            && let Some(slot) = incoming.get_mut(*to as usize)
        {
            *slot += 1;
        }
    }
    let cell_area = cell_size * cell_size;
    let mut area: Vec<f32> = vec![cell_area; heights.len()];
    let mut order: Vec<u32> = Vec::with_capacity(heights.len());
    let mut ready: std::collections::VecDeque<u32> = (0..heights.len() as u32)
        .filter(|at| incoming.get(*at as usize).copied().unwrap_or(1) == 0)
        .collect();
    while let Some(at) = ready.pop_front() {
        order.push(at);
        let Some(to) = receiver.get(at as usize).copied() else {
            continue;
        };
        if to == u32::MAX {
            continue;
        }
        let carried = area.get(at as usize).copied().unwrap_or(0.0);
        if let Some(slot) = area.get_mut(to as usize) {
            *slot += carried;
        }
        if let Some(remaining) = incoming.get_mut(to as usize) {
            *remaining -= 1;
            if *remaining == 0 {
                ready.push_back(to);
            }
        }
    }

    (receiver, order, area)
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
    for _ in 0..settings.erosion_rounds {
        // Fill, so routing terminates at the boundary rather than in pits.
        fill_coarse(heights, width, height);

        // Deliberately the *flat-truncated* drainage, not the connected one
        // route_coarse builds: this accumulation drives the erosion whose
        // saddles the boundary floors are read from, and it was calibrated —
        // measured at a 0.00 m median seam step — with rivers restarting at
        // every flat. Swapping in connected drainage multiplied the A^m term
        // by orders of magnitude, over-cut the saddles, and brought 90 m
        // seam steps straight back. Influx wants the connected drainage;
        // floors want this one; they get one each.
        let (receiver, order, area) = route_coarse_truncated(heights, width, height, cell_size);

        // The implicit solve, receivers first (ascending heights — the
        // truncated router's order).
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
    fn the_two_blocks_sharing_an_edge_build_the_identical_model_for_it() {
        // The property every seam guarantee rests on, in its strongest form:
        // the window is canonical to the *edge*, so the two blocks sharing
        // it construct byte-identical models — filled surface and drainage
        // both — and everything derived (floors, influx) agrees for free.
        // Block-centred windows cannot promise this any more: coarse flat
        // routing and accumulation are window-dependent, which is exactly
        // why boundary conditions stopped using them.
        let generator = generator();
        let west_block = BlockCoordinates::new(BlockCoord::new(0, 0));
        let east_block = BlockCoordinates::new(BlockCoord::new(1, 0));

        let from_west =
            RegionalWater::for_edge(&generator, west_block, Side::East, RegionSettings::DEFAULT);
        let from_east =
            RegionalWater::for_edge(&generator, east_block, Side::West, RegionSettings::DEFAULT);

        assert_eq!(from_west.start, from_east.start, "different windows");
        assert_eq!(
            from_west.filled, from_east.filled,
            "the shared edge's filled surface differs between the two blocks"
        );
        assert_eq!(
            from_west.area, from_east.area,
            "the shared edge's drainage differs between the two blocks"
        );
    }

    #[test]
    fn something_flows_into_a_block_and_both_sides_say_how_much() {
        // Non-vacuousness for the identity above: the shared edge carries
        // real drainage, and the assembled boundary sees it.
        let generator = generator();
        let east_block = BlockCoordinates::new(BlockCoord::new(1, 0));
        let boundary =
            RegionalWater::boundary_for_block(&generator, east_block, RegionSettings::DEFAULT);
        let entering: f32 = boundary.influx_west.iter().sum();
        assert!(
            entering > 0.0,
            "nothing flows across this seam at all, so edge-model identity proves little"
        );
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
        let seal = RegionalWater::boundary_for_block(
            &generator,
            BlockCoordinates::new(BlockCoord::new(0, 0)),
            RegionSettings::DEFAULT,
        );

        for side in [&seal.north, &seal.south, &seal.west, &seal.east] {
            assert_eq!(side.len(), crate::block::EDGE as usize);
            assert!(
                side.iter().all(|v| v.is_finite()),
                "an unsealed boundary cell"
            );
        }
    }
}
