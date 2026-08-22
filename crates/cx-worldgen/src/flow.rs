//! Step 2 of S07's pipeline: depression fill and flow routing.
//!
//! Three things that are really one thing, because each needs the last:
//!
//! 1. **Depression fill** — raise every closed basin to its outlet, so water has
//!    somewhere to go from anywhere.
//! 2. **Flow direction** — D8, the steepest downhill neighbour of eight.
//! 3. **Flow accumulation** — how many cells drain through each cell, which is
//!    the discharge proxy every later stage uses. Hydraulic erosion incises in
//!    proportion to it (step 3) and channel carving sizes channels by it (step 5).
//!
//! # Why fill first
//!
//! A raw noise surface is full of closed basins — local minima with no downhill
//! neighbour. Flow routed on it terminates in thousands of puddles, accumulation
//! never builds, and the result is not a drainage network but a scatter of
//! disconnected fragments. Erosion driven by that carves nothing.
//!
//! Filling is not a claim that real terrain has no lakes. It is the standard
//! preprocessing step for flow routing: it produces a surface on which flow is
//! *defined everywhere*. Where a basin was filled deeply, that is a lake, and
//! S07 step 7 derives water body extents from exactly this difference.
//!
//! # Priority-flood, and why not a sequential fill
//!
//! Priority-flood (Barnes, Lehman & Mulla 2014). Start from the grid's boundary,
//! pop the lowest cell not yet processed, and raise each unprocessed neighbour to
//! at least that cell's height before pushing it. Every cell is popped once, and
//! when it is popped its final height is known.
//!
//! The iterative alternative — sweep the grid repeatedly lowering cells until
//! nothing changes — is the thing `ADR-0006` forbids in spirit: its result is
//! independent of order only if it runs to complete convergence, and "enough
//! sweeps" is a tuning parameter that silently changes terrain. Priority-flood
//! has no such parameter.
//!
//! # Determinism
//!
//! Every ordering here is total, and deliberately so (`ADR-0004`).
//!
//! - The heap is keyed on `(monotonic bits of height, cell index)`. Two cells at
//!   exactly the same height — common, since noise quantises — would otherwise
//!   pop in whatever order the heap's internal comparisons produced, and the fill
//!   would differ between runs on the same seed.
//! - D8 ties break by a fixed neighbour order. Real terrain has flat ground, and
//!   on flat ground several neighbours are exactly equally downhill.
//!
//! Neither of these is a preference. `ADR-0006` promises a block generated in any
//! order is bit-identical, and an unspecified tie-break breaks that promise in a
//! way that shows up as a river moving between runs.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use cx_core::math::EROSION_CELL_SIZE;

use crate::block::{BlockGrid, CELLS, EDGE, ErosionCell};

/// The eight D8 neighbours, in the fixed order ties are broken by.
///
/// Order matters for reproducibility and nothing else — but it matters
/// absolutely. Starting north and going clockwise is the conventional choice and
/// the one a reader is least likely to be surprised by.
const NEIGHBOURS: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// D8 direction, as stored. The index into [`NEIGHBOURS`], or [`NO_FLOW`].
pub type FlowDir = u8;

/// A cell with nowhere downhill to go.
///
/// After a fill this means the cell drains off the grid's edge, not that it is a
/// pit — the fill removes pits. A pit surviving the fill is a bug, and
/// [`FlowNetwork::interior_sinks`] is what notices.
pub const NO_FLOW: FlowDir = u8::MAX;

/// A monotonic `u32` ordering of an `f32`.
///
/// `f32::to_bits` is monotonic for positive floats and *reversed* for negatives,
/// so ordering raw bits would sort terrain below sea level backwards — the fill
/// would climb out of the ocean rather than into it. This is the standard
/// total-order transform; it exists because the heap needs an integer key it can
/// tie-break against an index.
const fn order_key(height: f32) -> u32 {
    let bits = height.to_bits();
    if bits & 0x8000_0000 != 0 {
        // Negative: flip everything, so more-negative sorts lower.
        !bits
    } else {
        // Positive: set the sign bit, so every positive sorts above every
        // negative.
        bits | 0x8000_0000
    }
}

/// The flow network over one block.
///
/// Owns the three outputs of step 2. They are one type rather than three because
/// they are only meaningful together: a direction without the filled surface it
/// was computed from routes water uphill, and an accumulation without directions
/// is a number with no path attached.
#[derive(Debug)]
pub struct FlowNetwork {
    filled: BlockGrid,
    direction: Vec<FlowDir>,
    accumulation: Vec<f32>,
    /// Cells in drainage order: every cell appears **after** everything that
    /// drains into it. A by-product of [`accumulate`]'s traversal, kept because
    /// erosion needs the same ordering reversed and recomputing it would be a
    /// second full pass over 26 million cells for information already derived.
    ///
    /// Grouped by solve layer — `order[layer_starts[n]..layer_starts[n + 1]]`
    /// is layer `n`, and every receiver of a cell in layer `n` lies in an
    /// earlier layer. That is what lets erosion solve a layer's cells in
    /// parallel while remaining bit-identical to a serial walk.
    order: Vec<u32>,
    /// Bucket boundaries into [`FlowNetwork::order`], one past the last.
    layer_starts: Vec<usize>,
}

impl FlowNetwork {
    /// Fills depressions, routes flow, and accumulates drainage.
    ///
    /// Takes elevation by value and keeps the filled surface: the unfilled one
    /// has no further use, and holding both would double the largest allocation
    /// in the pipeline for no purpose.
    pub fn build(elevation: BlockGrid) -> Self {
        let filled = fill_depressions(elevation);
        let across_flats = resolve_flats(&filled);
        let direction = flow_directions(&filled, &across_flats);
        let (accumulation, order, layer_starts) = accumulate(&filled, &direction);

        Self {
            filled,
            direction,
            accumulation,
            order,
            layer_starts,
        }
    }

    /// Routes flow over a surface that is already known to be pit-free,
    /// skipping the depression fill.
    ///
    /// The fill is the most expensive part of a rebuild (measured: 1.77 s of a
    /// 3.9 s build), and mid-erosion it does nothing. The implicit erosion
    /// update is a weighted average of a cell and its receivers, so a cell can
    /// approach the cell it drains into but never cross below it — erosion
    /// cannot create a pit, and priority-flood on a pit-free surface is the
    /// identity. Skipping it changes no output; it skips proving a fact that
    /// holds by construction. `tests` re-prove the equivalence on a real block,
    /// because "holds by construction" has been wrong in this project before.
    ///
    /// The caller owns the claim that the surface is pit-free. Erosion rounds
    /// qualify; anything else should use [`FlowNetwork::build`].
    pub fn build_pit_free(surface: BlockGrid) -> Self {
        let across_flats = resolve_flats(&surface);
        let direction = flow_directions(&surface, &across_flats);
        let (accumulation, order, layer_starts) = accumulate(&surface, &direction);

        Self {
            filled: surface,
            direction,
            accumulation,
            order,
            layer_starts,
        }
    }

    /// The depression-filled surface. Steps 3–5 erode this, not the raw noise.
    pub const fn filled(&self) -> &BlockGrid {
        &self.filled
    }

    /// D8 direction at a cell — an index into the neighbour table, or
    /// [`NO_FLOW`].
    pub fn direction(&self, cell: ErosionCell) -> FlowDir {
        self.direction.get(flat(cell)).copied().unwrap_or(NO_FLOW)
    }

    /// The cell this one drains into, or `None` at an outlet.
    pub fn downstream(&self, cell: ErosionCell) -> Option<ErosionCell> {
        step(cell, self.direction(cell))
    }

    /// How many cells drain through this one, including itself.
    ///
    /// The discharge proxy. A cell with accumulation 1 is a hilltop; the large
    /// values are the channels.
    pub fn accumulation(&self, cell: ErosionCell) -> f32 {
        self.accumulation.get(flat(cell)).copied().unwrap_or(0.0)
    }

    /// Cells with no downhill neighbour that are **not** on the grid boundary.
    ///
    /// Must be zero after a fill. A surviving interior sink is water that
    /// disappears: accumulation stops there, the channel downstream of it never
    /// forms, and the visible symptom is a river that ends in the middle of a
    /// hillside. Counted rather than assumed.
    pub fn interior_sinks(&self) -> usize {
        (0..EDGE)
            .flat_map(|z| (0..EDGE).map(move |x| (x, z)))
            .filter(|(x, z)| *x > 0 && *z > 0 && *x < EDGE - 1 && *z < EDGE - 1)
            .filter_map(|(x, z)| ErosionCell::new(x, z))
            .filter(|cell| self.direction(*cell) == NO_FLOW)
            .count()
    }

    /// Calls `visit` for each downslope neighbour with its share of the flow.
    ///
    /// The same split [`accumulate`] uses, exposed because erosion needs it:
    /// incising towards a single receiver is what prints the D8 grid into the
    /// terrain. Shares sum to one.
    pub fn for_each_receiver(&self, cell: ErosionCell, visit: impl FnMut(ErosionCell, f32)) {
        each_receiver(&self.filled, &self.direction, cell, visit);
    }

    /// Metres from a cell to a specific neighbour.
    ///
    /// Diagonals are 1.414 cells away. Stream power divides by this, so treating
    /// every neighbour as one cell apart makes diagonal reaches incise 41% too
    /// fast — which is itself a grid artifact.
    pub fn distance_to(cell: ErosionCell, other: ErosionCell) -> f32 {
        let dx = other.x() as i32 - cell.x() as i32;
        let dz = other.z() as i32 - cell.z() as i32;
        if dx.abs() + dz.abs() == 2 {
            EROSION_CELL_SIZE * std::f32::consts::SQRT_2
        } else {
            EROSION_CELL_SIZE
        }
    }

    /// Drops the drainage-order list, reclaiming ~100 MB.
    ///
    /// The order exists for the erosion solve and nothing after it. A block
    /// held resident for chunk extraction never walks it again, and keeping it
    /// would cost a quarter of the block's residency footprint for data with
    /// no remaining reader. [`Self::drainage_order`] returns empty afterwards.
    pub fn shed_erosion_order(&mut self) {
        self.order = Vec::new();
        self.layer_starts = Vec::new();
    }

    /// Cells in drainage order — everything upstream of a cell comes before it.
    ///
    /// Erosion walks this **backwards**, so a cell is always solved after the
    /// one it drains into. That is what makes the implicit stream-power update
    /// a single pass rather than an iteration to convergence.
    pub fn drainage_order(&self) -> &[u32] {
        &self.order
    }

    /// Bucket boundaries of the solve layers within
    /// [`FlowNetwork::drainage_order`]. Empty after
    /// [`FlowNetwork::shed_erosion_order`].
    pub(crate) fn drainage_layer_starts(&self) -> &[usize] {
        &self.layer_starts
    }

    /// The cell a flat index refers to, for walking [`Self::drainage_order`].
    pub fn cell_at(index: u32) -> Option<ErosionCell> {
        unflat(index)
    }

    /// Metres from a cell to the one it drains into, or `None` at an outlet.
    ///
    /// Diagonal neighbours are 1.414 cells away. Stream power divides by this,
    /// so treating every neighbour as one cell apart would make diagonal
    /// reaches incise 41% too fast — and since D8 channels alternate between
    /// orthogonal and diagonal steps, the error shows up as a channel whose
    /// depth oscillates along its length.
    pub fn distance_downstream(&self, cell: ErosionCell) -> Option<f32> {
        let direction = self.direction(cell);
        let (dx, dz) = NEIGHBOURS.get(direction as usize).copied()?;
        offset(cell, dx, dz)?;

        Some(if dx.abs() + dz.abs() == 2 {
            EROSION_CELL_SIZE * std::f32::consts::SQRT_2
        } else {
            EROSION_CELL_SIZE
        })
    }

    /// The largest accumulation anywhere on the grid.
    ///
    /// On a well-formed network this is close to the cell count — nearly
    /// everything drains through one of a few outlets. A small maximum means
    /// flow is fragmenting rather than collecting, which is what a broken fill
    /// looks like.
    pub fn max_accumulation(&self) -> f32 {
        self.accumulation.iter().copied().fold(0.0, f32::max)
    }
}

/// Priority-flood depression filling.
///
/// An earlier version also returned the order cells were popped in, on the
/// grounds that non-decreasing height is a topological sort of the drainage
/// network. **It is not, once flats are resolved**: water crosses a flat to a
/// neighbour at exactly the same height, and among equal heights the pop order
/// is decided by cell index, which has nothing to do with which way the flat
/// drains. Accumulation built on it silently stopped collecting inside every
/// lake — the largest channel fell from 31% of the block to 1%. [`accumulate`]
/// derives its own order now.
fn fill_depressions(mut elevation: BlockGrid) -> BlockGrid {
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    let mut done = vec![false; CELLS];

    // Seed with the whole boundary. Everything drains off the edge of the block
    // eventually, because the grid has no outside — the halo is discarded later
    // but during generation it is where water leaves.
    for x in 0..EDGE {
        for z in [0, EDGE - 1] {
            push(&mut heap, &mut done, &elevation, x, z);
        }
    }
    for z in 1..EDGE - 1 {
        for x in [0, EDGE - 1] {
            push(&mut heap, &mut done, &elevation, x, z);
        }
    }

    while let Some(Reverse((key, index))) = heap.pop() {
        let Some(cell) = unflat(index) else { continue };
        let height = elevation.get(cell);

        for (dx, dz) in NEIGHBOURS {
            let Some(next) = offset(cell, dx, dz) else {
                continue;
            };
            let flat_next = flat(next);
            if done.get(flat_next).copied().unwrap_or(true) {
                continue;
            }

            // The fill itself: a neighbour lower than where we came from is
            // inside a depression, and is raised to exactly the level water
            // would have to reach to escape. A neighbour already higher keeps
            // its own height, which is what stops the fill from flattening
            // terrain that was never in a depression.
            //
            // Exactly, not just above. That leaves genuine flats, which
            // `resolve_flats` then gives a gradient that means something.
            let raised = elevation.get(next).max(height);
            elevation.set(next, raised);

            if let Some(slot) = done.get_mut(flat_next) {
                *slot = true;
            }
            heap.push(Reverse((order_key(raised), flat_next as u32)));
        }

        // `key` is only read to keep the tuple shape obvious at the pop site;
        // the height that matters was already written into the grid.
        let _ = key;
    }

    elevation
}

fn push(
    heap: &mut BinaryHeap<Reverse<(u32, u32)>>,
    done: &mut [bool],
    elevation: &BlockGrid,
    x: u32,
    z: u32,
) {
    let Some(cell) = ErosionCell::new(x, z) else {
        return;
    };
    let index = flat(cell);
    if done.get(index).copied().unwrap_or(true) {
        return;
    }
    if let Some(slot) = done.get_mut(index) {
        *slot = true;
    }
    heap.push(Reverse((order_key(elevation.get(cell)), index as u32)));
}

/// Gives filled flats a drainage gradient (Garbrecht & Martz 1997; Barnes 2014).
///
/// A flat is a run of cells at exactly the same height with nowhere lower to go.
/// A breadth-first sweep decides which way water crosses one, seeded at the flat
/// cells that *do* have a lower neighbour — the places water leaves — and spread
/// inward, so a cell far from any outlet gets a large distance.
/// The ordering is `(distance to outlet, hash of the coordinate)`. The first
/// term makes the flat drain towards its outlets, and is monotonic by BFS
/// construction so no cell can be a local minimum. The second decides between
/// cells the first ties — see the note on `SECONDARY` for why it is a hash and
/// not another distance.
///
/// # Why this rather than the one-line alternative
///
/// The "+epsilon" fill is three characters of change and produces drainage that
/// follows the *fill's* search order. Rendered, that is unmistakable: straight
/// 45-degree fans and horizontal combs across every basin. It was implemented,
/// looked at, and replaced by this. Assertions did not distinguish the two —
/// both fill every pit, both leave zero interior sinks, both route strictly
/// downhill. Only the picture did.
#[allow(
    clippy::float_cmp,
    reason = "A flat is defined by cells at *bitwise identical* height, so exact \
              comparison is the definition rather than a shortcut. A tolerance \
              would merge nearly-equal cells into flats that are really gentle \
              slopes, and then override their real drainage with a BFS gradient — \
              turning hillsides into lake beds. The lint's usual concern, that \
              two computations of the same quantity differ in the last bit, does \
              not apply: these are the same stored values read twice, not \
              recomputed."
)]
fn resolve_flats(filled: &BlockGrid) -> Vec<u32> {
    let heights: Vec<f32> = filled.as_slice().to_vec();
    let at = |index: usize| heights.get(index).copied().unwrap_or(f32::NAN);

    // A cell is **in a flat** when it has no strictly lower neighbour but does
    // have an equal one. Both halves matter. Without the first, every cell of a
    // uniformly sloping plane counts as flat, because its neighbours across the
    // slope are all at its own height — and the resolution then raises ordinary
    // hillside by a gradient it invented. That is not hypothetical: it is what
    // the first version of this function did, and `nothing_outside_a_pit_is_raised`
    // is what caught it.
    let mut has_lower = vec![false; CELLS];
    let mut in_flat = vec![false; CELLS];

    for z in 0..EDGE {
        for x in 0..EDGE {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };
            let index = flat(cell);
            let height = at(index);

            let mut lower = false;
            let mut equal = false;
            for (dx, dz) in NEIGHBOURS {
                let Some(next) = offset(cell, dx, dz) else {
                    continue;
                };
                let other = at(flat(next));
                lower |= other < height;
                equal |= other == height;
            }

            if let Some(slot) = has_lower.get_mut(index) {
                *slot = lower;
            }
            if let Some(slot) = in_flat.get_mut(index) {
                *slot = !lower && equal;
            }
        }
    }

    // Two breadth-first sweeps, identical in shape, differing only in what
    // seeds them.
    let sweep = || -> Vec<u32> {
        let mut distance = vec![u32::MAX; CELLS];
        let mut frontier: std::collections::VecDeque<u32> = std::collections::VecDeque::new();

        for z in 0..EDGE {
            for x in 0..EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                let index = flat(cell);
                if !in_flat.get(index).copied().unwrap_or(false) {
                    continue;
                }

                let height = at(index);

                // The grid's rim is an outlet in its own right: water leaves the
                // block there, and off-grid does not register as "lower". A flat
                // plateau reaching the boundary has no other outlet at all, and
                // without this every cell of it stays a sink.
                let mut seeds =
                    cell.x() == 0 || cell.z() == 0 || cell.x() == EDGE - 1 || cell.z() == EDGE - 1;

                for (dx, dz) in NEIGHBOURS {
                    let Some(next) = offset(cell, dx, dz) else {
                        continue;
                    };
                    let next_index = flat(next);
                    let other = at(next_index);

                    // An outlet: an equal-height neighbour that does have
                    // somewhere lower to go. That is where water leaves the
                    // flat, so the flat must drain towards it.
                    seeds |= other == height && has_lower.get(next_index).copied().unwrap_or(false);
                }

                if seeds && let Some(slot) = distance.get_mut(index) {
                    *slot = 0;
                    frontier.push_back(index as u32);
                }
            }
        }

        while let Some(index) = frontier.pop_front() {
            let index = index as usize;
            let Some(cell) = unflat(index as u32) else {
                continue;
            };
            let height = at(index);
            let here = distance.get(index).copied().unwrap_or(u32::MAX);

            for (dx, dz) in NEIGHBOURS {
                let Some(next) = offset(cell, dx, dz) else {
                    continue;
                };
                let next_index = flat(next);

                // Spread only within the same flat: same height, also flat, not
                // yet reached.
                if at(next_index) != height {
                    continue;
                }
                if !in_flat.get(next_index).copied().unwrap_or(false) {
                    continue;
                }
                if distance.get(next_index).copied().unwrap_or(0) != u32::MAX {
                    continue;
                }

                if let Some(slot) = distance.get_mut(next_index) {
                    *slot = here.saturating_add(1);
                }
                frontier.push_back(next_index as u32);
            }
        }

        distance
    };

    let to_outlet = sweep();

    /// How much room the secondary term gets below the primary one.
    ///
    /// The two gradients must not be added together as peers, and that is not a
    /// stylistic point. Distance-to-outlet falls monotonically towards an outlet
    /// — BFS guarantees every cell a strictly-decreasing path to a seed — while
    /// distance-from-higher does not, and their *sum* has local minima in the
    /// interior of a flat. A cell at such a minimum has no equal-height
    /// neighbour with a smaller value, so it drains nowhere. Summing them left
    /// 618,140 interior sinks on one block, and the count is what found it.
    ///
    /// So outlet distance is the high word and the other is the low word: the
    /// primary ordering keeps its monotonicity, and the secondary only chooses
    /// between cells the primary ties.
    ///
    /// # The low word is a positional hash, not the from-higher distance
    ///
    /// Both are geometric, and that turned out to matter. Cells at the same
    /// outlet distance form a contour, and on the regular grid those contours are
    /// straight diagonal lines — so a tie-break that is itself a smooth function
    /// of position sends every cell of a contour the same way, and drainage
    /// crosses a lake in parallel combs at 45 degrees. Erosion then carves the
    /// combs, and that is where the herringbone in an eroded block came from.
    ///
    /// Inside a filled flat the drainage direction is **genuinely arbitrary** —
    /// a lake surface has no slope to follow — so choosing it by a hash of the
    /// cell's own coordinate is not an approximation of something better. It is
    /// the honest representation of a free choice, and unlike a geometric
    /// tie-break it has no pattern for erosion to find.
    ///
    /// Deterministic, per `ADR-0006`: the same coordinate hashes the same way in
    /// every run, so a block still regenerates identically.
    const SECONDARY: u32 = 256;

    // The tie-break field. `u32::MAX` means "not in a flat", which is most of a
    // block — only cells with no lower neighbour at all need one.
    let mut across = vec![u32::MAX; CELLS];

    for z in 0..EDGE {
        for x in 0..EDGE {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };
            let index = flat(cell);

            if !in_flat.get(index).copied().unwrap_or(false) {
                continue;
            }
            let Some(outward) = to_outlet.get(index).copied().filter(|d| *d != u32::MAX) else {
                // A flat with no outlet reachable at all. The fill plus the rim
                // seeding leaves none of these on a well-formed grid; skipping
                // is the safe response rather than inventing a slope.
                continue;
            };

            // A hash of the cell's own coordinate, in `0..SECONDARY`. Replaces
            // the distance-from-higher term, which was smooth in position and so
            // gave every cell of a contour the same answer.
            let jitter = flat_jitter(cell);

            // **Far from the outlet is high; close to incoming hillside is
            // high.** Both terms were inverted in the first attempt, which put a
            // flat's lowest point at its centre — so the centre had no downhill
            // neighbour and stayed a sink.
            //
            // Compared only between cells of the same flat, which is what makes
            // an arbitrary low word safe: it decides between equals and never
            // between a cell and its outlet.
            // The `+ 1` is not slack. An outlet — an equal-height neighbour that
            // does have somewhere lower to go — reads as zero above, so a flat
            // cell sitting *at* its outlet would tie with it at zero and the
            // strictly-less comparison would reject its own escape route. One
            // step puts every flat cell above the ground it drains into.
            //
            // Safe here in a way it was not in the version that added this to
            // elevation: a tie-break field has no units and cannot invert a real
            // slope.
            let potential = outward
                .saturating_add(1)
                .saturating_mul(SECONDARY)
                .saturating_add(jitter);

            if let Some(slot) = across.get_mut(index) {
                *slot = potential;
            }
        }
    }

    across
}

/// D8: the steepest downhill neighbour, or [`NO_FLOW`].
///
/// Steepest by **gradient**, not by drop. The diagonal neighbours are 1.414
/// cells away, so comparing raw height differences biases flow diagonally — the
/// symptom is a drainage network whose channels all run at 45 degrees, which
/// looks like a rendering artefact rather than a routing bug.
#[allow(
    clippy::float_cmp,
    reason = "A flat is defined by cells at *bitwise identical* height, so exact \
              comparison is the definition rather than a shortcut. A tolerance \
              would merge nearly-equal cells into flats that are really gentle \
              slopes, and then override their real drainage with a BFS gradient — \
              turning hillsides into lake beds. The lint's usual concern, that \
              two computations of the same quantity differ in the last bit, does \
              not apply: these are the same stored values read twice, not \
              recomputed."
)]
fn flow_directions(filled: &BlockGrid, across_flats: &[u32]) -> Vec<FlowDir> {
    // Row-parallel: each cell's direction reads only the (shared, immutable)
    // surface and flat gradients, and writes only its own slot. Same bits at
    // any thread count — see `crate::parallel`.
    let mut direction = vec![NO_FLOW; CELLS];
    crate::parallel::fill_grid(&mut direction, |z, row| {
        for (x, slot) in row.iter_mut().enumerate() {
            let Some(cell) = ErosionCell::new(x as u32, z) else {
                continue;
            };
            let height = filled.get(cell);

            let mut best_slope = 0.0f32;
            let mut best = NO_FLOW;

            for (index, (dx, dz)) in NEIGHBOURS.iter().enumerate() {
                let Some(next) = offset(cell, *dx, *dz) else {
                    continue;
                };

                let drop = height - filled.get(next);
                if drop <= 0.0 {
                    continue;
                }

                let distance = if dx.abs() + dz.abs() == 2 {
                    EROSION_CELL_SIZE * std::f32::consts::SQRT_2
                } else {
                    EROSION_CELL_SIZE
                };
                let slope = drop / distance;

                // Strictly greater: the first neighbour in `NEIGHBOURS` order
                // wins a tie, which is what makes flat ground reproducible.
                if slope > best_slope {
                    best_slope = slope;
                    best = index as FlowDir;
                }
            }

            // No strictly-lower neighbour: this cell is inside a flat, and the
            // resolved gradient decides which way water crosses it.
            //
            // Deliberately a *tie-break* rather than a nudge to elevation. The
            // version before this one added the gradient to the filled surface,
            // and on real terrain that broke everything: adjacent noise cells
            // can differ by less than the smallest usable step, so lifting a
            // flat cell pushed it above genuinely lower ground nearby. Real
            // slopes inverted, new sinks appeared where none had been, and the
            // largest channel fell from 31% of the block to under 2%. Used only
            // to order equal-height neighbours, it cannot do any of that.
            if best == NO_FLOW {
                let here = across_flats.get(flat(cell)).copied().unwrap_or(u32::MAX);

                if here != u32::MAX {
                    let mut lowest = here;
                    for (index, (dx, dz)) in NEIGHBOURS.iter().enumerate() {
                        let Some(next) = offset(cell, *dx, *dz) else {
                            continue;
                        };
                        // Same height only. A higher neighbour is not somewhere
                        // water goes, whatever its gradient value says.
                        if filled.get(next) != height {
                            continue;
                        }

                        // An equal-height neighbour with no flat value is one
                        // that *does* have somewhere lower to go — the outlet
                        // itself. It is the most attractive destination there
                        // is, not the least, so it reads as zero rather than as
                        // the unset sentinel. Getting this backwards leaves a
                        // filled pit sitting next to its own escape route.
                        let there = match across_flats.get(flat(next)).copied() {
                            Some(u32::MAX) | None => 0,
                            Some(value) => value,
                        };

                        // Strictly less, so the first neighbour in the fixed
                        // order wins a tie — the same rule as the slope
                        // comparison above, for the same reason.
                        if there < lowest {
                            lowest = there;
                            best = index as FlowDir;
                        }
                    }
                }
            }

            *slot = best;
        }
    });

    direction
}

/// Flow accumulation, in drainage order.
///
/// Every cell contributes itself, then hands its running total to whatever it
/// drains into. Doing that correctly needs each cell processed only once
/// everything upstream of it already has been — so this is Kahn's algorithm over
/// the direction field: count how many cells drain into each cell, start from
/// the ones nothing drains into, and release a cell the moment its last
/// contributor has been handled.
///
/// # Why not sort by height
///
/// Because it does not work. The obvious order — process cells from high to low
/// — is a topological order only while every cell drains strictly downhill, and
/// flat resolution deliberately breaks that: water crosses a lake to a neighbour
/// at exactly its own height. The first version of this function used the fill's
/// pop order for exactly that reason and it was wrong in exactly that place,
/// with accumulation refusing to collect inside any lake. Deriving the order
/// from the direction field itself cannot make that mistake, whatever later
/// stages do to the field.
/// Flow accumulation, split across every downslope neighbour.
///
/// # Why not simply follow the D8 direction
///
/// Because D8 has only eight directions, and on a smooth surface every channel
/// snaps to the nearest one. Accumulation then arrives in single-cell lines
/// running at multiples of 45 degrees. That is survivable on its own — but
/// erosion multiplies by `A^m` and feeds the result back into the surface, so
/// the bias compounds: a line that captures slightly more cuts slightly deeper,
/// which captures more still. Twelve rounds of that turns a landscape into a
/// circuit board, with channels in hard diagonal and orthogonal runs and a
/// herringbone texture over every hillside.
///
/// That is not a prediction. It is what the first version produced, and it was
/// found by rendering the eroded block and looking at it — every assertion about
/// erosion passed against it, because "channels cut more than hillslopes" is
/// still perfectly true when the channels are straight lines.
///
/// So flow is split across all downslope neighbours, weighted by the slope
/// **squared** (Freeman 1991; Quinn 1991). Squaring keeps channels sharp —
/// weighting by raw slope smears drainage across hillsides, and ever-higher
/// powers converge back on D8 — while removing the direction bias described
/// above. The exponent is fixed rather than a setting because the multiply
/// below is exact and portable where a general `powf` is neither; making it
/// tunable again means paying that portability back. The direction of steepest
/// descent still gets most of it, but the total is no longer confined to eight
/// rays, and the grid stops printing itself into the terrain.
///
/// D8 is still what [`FlowNetwork::downstream`] reports and what erosion incises
/// towards — a single receiver is what keeps the implicit stream-power solve
/// closed-form. Only the *area* term is split.
///
/// # Order
///
/// Kahn's algorithm over the multi-receiver graph: count how many cells send
/// anything to each cell, start from the ones nothing drains into, and release a
/// cell when its last contributor is done. Weights are recomputed as each cell
/// is processed rather than stored — eight per cell over 26 million cells would
/// be 840 MB, against a 0.42 GB budget for the whole working set (`ADR-0015`).
fn accumulate(filled: &BlockGrid, direction: &[FlowDir]) -> (Vec<f32>, Vec<u32>, Vec<usize>) {
    let mut accumulation = vec![1.0f32; CELLS];

    // Each cell's solve layer: one more than the deepest of its senders.
    // Finalised exactly when the cell's in-degree reaches zero, which is the
    // moment Kahn's queue pops it — so layering rides the pass for the cost
    // of one max per edge. The erosion solve runs layers in parallel; see
    // `drainage_layer_starts` for the contract.
    let mut level = vec![0u32; CELLS];
    let mut buckets: Vec<Vec<u32>> = Vec::new();

    // How many cells send flow to each. At most eight, so a byte.
    let mut incoming = vec![0u8; CELLS];
    for index in 0..CELLS {
        let Some(cell) = unflat(index as u32) else {
            continue;
        };
        // Weight-free: this pass only counts edges, and the receiver set is
        // identical with or without the share arithmetic.
        each_receiver_index(filled, direction, cell, |next| {
            if let Some(slot) = incoming.get_mut(flat(next)) {
                *slot = slot.saturating_add(1);
            }
        });
    }

    // The sources: ridge lines and everything nothing flows into.
    let mut ready: std::collections::VecDeque<u32> = (0..CELLS)
        .filter(|index| incoming.get(*index).copied().unwrap_or(1) == 0)
        .map(|index| index as u32)
        .collect();

    while let Some(index) = ready.pop_front() {
        let cell_level = level.get(index as usize).copied().unwrap_or(0) as usize;
        if buckets.len() <= cell_level {
            buckets.resize_with(cell_level + 1, Vec::new);
        }
        if let Some(bucket) = buckets.get_mut(cell_level) {
            bucket.push(index);
        }

        let index = index as usize;
        let Some(cell) = unflat(index as u32) else {
            continue;
        };
        let carried = accumulation.get(index).copied().unwrap_or(0.0);

        let mut released: [Option<usize>; 8] = [None; 8];
        let mut count = 0usize;

        each_receiver(filled, direction, cell, |next, share| {
            let next_index = flat(next);
            if let Some(slot) = accumulation.get_mut(next_index) {
                *slot += carried * share;
            }
            if let Some(slot) = released.get_mut(count) {
                *slot = Some(next_index);
            }
            count += 1;
        });

        for slot in released.iter().flatten() {
            if let Some(receiver_level) = level.get_mut(*slot) {
                *receiver_level = (*receiver_level).max(cell_level as u32 + 1);
            }
            if let Some(remaining) = incoming.get_mut(*slot) {
                *remaining = remaining.saturating_sub(1);
                if *remaining == 0 {
                    ready.push_back(*slot as u32);
                }
            }
        }
    }

    // Flatten ascending by layer. Still a topological order — an edge's
    // receiver is always in a strictly deeper layer — and the accumulation
    // sums above were written in the queue's own order, so changing what
    // *this* list holds moves no bits anywhere.
    let mut order: Vec<u32> = Vec::with_capacity(CELLS);
    let mut starts: Vec<usize> = Vec::with_capacity(buckets.len() + 1);
    for bucket in &buckets {
        starts.push(order.len());
        order.extend_from_slice(bucket);
    }
    starts.push(order.len());

    (accumulation, order, starts)
}

/// Calls `visit` once per downslope neighbour with its share of the flow.
///
/// Shares sum to one, so nothing is created or lost crossing a cell — the
/// property that makes accumulation mean "how much drains through here".
///
/// A cell with no strictly-lower neighbour falls back to its D8 direction, which
/// is where flat resolution has already decided the water goes. Without that
/// fallback, every lake would be a dead end for accumulation.
#[allow(
    clippy::float_cmp,
    reason = "Comparing a stored height to itself-derived values, as in \
              `resolve_flats`: a neighbour at exactly the same height is on a \
              flat and must be routed by the D8 fallback, not given a share of \
              zero slope."
)]
fn each_receiver(
    filled: &BlockGrid,
    direction: &[FlowDir],
    cell: ErosionCell,
    mut visit: impl FnMut(ErosionCell, f32),
) {
    let height = filled.get(cell);

    let mut weights = [0.0f32; 8];
    let mut total = 0.0f32;

    for (index, (dx, dz)) in NEIGHBOURS.iter().enumerate() {
        let Some(next) = offset(cell, *dx, *dz) else {
            continue;
        };
        let drop = height - filled.get(next);
        if drop <= 0.0 {
            continue;
        }

        let distance = if dx.abs() + dz.abs() == 2 {
            EROSION_CELL_SIZE * std::f32::consts::SQRT_2
        } else {
            EROSION_CELL_SIZE
        };

        // Gradient, not drop, for the reason `flow_directions` gives: comparing
        // raw drops would over-weight the diagonals by 41% and reintroduce the
        // bias this function exists to remove.
        //
        // Squared by multiplication, not `powf`: an exact multiply is
        // deterministic on every platform where libm's `powf` is not, and
        // this line runs for every downhill neighbour of every cell of every
        // network rebuild.
        let gradient = drop / distance;
        let weight = gradient * gradient;
        if let Some(slot) = weights.get_mut(index) {
            *slot = weight;
        }
        total += weight;
    }

    if total > 0.0 {
        for (index, weight) in weights.iter().enumerate() {
            if *weight <= 0.0 {
                continue;
            }
            let Some((dx, dz)) = NEIGHBOURS.get(index).copied() else {
                continue;
            };
            let Some(next) = offset(cell, dx, dz) else {
                continue;
            };
            visit(next, weight / total);
        }
        return;
    }

    // Flat, or an outlet. Whatever D8 decided, at full strength.
    if let Some(next) = step(cell, direction.get(flat(cell)).copied().unwrap_or(NO_FLOW)) {
        visit(next, 1.0);
    }
}

/// [`each_receiver`], without the weights: visits the same receiver set and
/// nothing else, skipping the share arithmetic entirely.
///
/// Exists for the passes that only need *who* receives — the incoming-degree
/// count above all — where computing `(drop/distance)^2` per neighbour was
/// measurable cost spent on numbers nobody read. The sets are identical
/// because a weight is positive exactly when its drop is: heights are `f32`
/// metres, so a nonzero drop is at least one ulp (~3e-5 at a few hundred
/// metres), far too large for the squared gradient to underflow to zero.
pub(crate) fn each_receiver_index(
    filled: &BlockGrid,
    direction: &[FlowDir],
    cell: ErosionCell,
    mut visit: impl FnMut(ErosionCell),
) {
    let height = filled.get(cell);
    let mut any = false;

    for (dx, dz) in NEIGHBOURS.iter() {
        let Some(next) = offset(cell, *dx, *dz) else {
            continue;
        };
        if height - filled.get(next) > 0.0 {
            visit(next);
            any = true;
        }
    }

    if !any && let Some(next) = step(cell, direction.get(flat(cell)).copied().unwrap_or(NO_FLOW)) {
        visit(next);
    }
}

/// The neighbour a direction points at.
fn step(cell: ErosionCell, direction: FlowDir) -> Option<ErosionCell> {
    let (dx, dz) = NEIGHBOURS.get(direction as usize).copied()?;
    offset(cell, dx, dz)
}

/// The cell `(dx, dz)` away, or `None` off the grid.
fn offset(cell: ErosionCell, dx: i32, dz: i32) -> Option<ErosionCell> {
    let x = cell.x().checked_add_signed(dx)?;
    let z = cell.z().checked_add_signed(dz)?;
    ErosionCell::new(x, z)
}

/// A stable pseudo-random value in `0..SECONDARY`, from a cell's coordinate.
///
/// Used to order cells within a flat that are the same distance from an outlet.
/// Hashed rather than computed, because anything computed from position smoothly
/// gives neighbouring cells similar answers — and similar answers across a
/// contour is exactly the parallel-comb pattern this replaced.
///
/// A pure function of the coordinate, so `ADR-0006` holds: the same block
/// regenerates identically, jitter included.
fn flat_jitter(cell: ErosionCell) -> u32 {
    let packed = (u64::from(cell.x()) << 32) | u64::from(cell.z());
    let hash = cx_core::hash::mix64(packed ^ 0x5F27_A1C3_9B4D_0E71);
    (hash >> 40) as u32 % 256
}

/// Row-major index. Matches [`BlockGrid`]'s own layout.
const fn flat(cell: ErosionCell) -> usize {
    (cell.z() as usize) * (EDGE as usize) + (cell.x() as usize)
}

fn unflat(index: u32) -> Option<ErosionCell> {
    ErosionCell::new(index % EDGE, index / EDGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid with a single deep pit in otherwise sloping ground.
    fn sloping_with_pit() -> BlockGrid {
        let mut grid = BlockGrid::filled(0.0);

        for z in 0..EDGE {
            for x in 0..EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                // A plane tilted along +X, so every cell has somewhere to go.
                grid.set(cell, 100.0 - x as f32 * 0.01);
            }
        }

        // One pit, well inside the grid.
        if let Some(pit) = ErosionCell::new(100, 100) {
            grid.set(pit, -50.0);
        }

        grid
    }

    #[test]
    fn the_total_order_on_floats_is_actually_ordered() {
        // The fill climbs out of a basin in height order. If negatives sorted
        // backwards — which raw `to_bits` does — it would climb out of the
        // ocean instead of into it, and every coastline would fill solid.
        let values = [-1000.0f32, -1.0, -0.0, 0.0, 0.5, 1.0, 1000.0];
        for pair in values.windows(2) {
            let (low, high) = (pair[0], pair[1]);
            assert!(
                order_key(low) <= order_key(high),
                "{low} and {high} order backwards"
            );
        }
    }

    #[test]
    fn a_pit_is_filled_to_its_surroundings() {
        let network = FlowNetwork::build(sloping_with_pit());
        let pit = ErosionCell::new(100, 100).expect("in range");

        let filled = network.filled().get(pit);
        assert!(
            filled > -50.0,
            "the pit was left at {filled} m, so it was not filled at all"
        );

        // Filled to roughly the surrounding plane, not to some arbitrary level.
        let neighbour = ErosionCell::new(101, 100).expect("in range");
        let around = network.filled().get(neighbour);
        assert!(
            (filled - around).abs() < 1.0,
            "the pit filled to {filled} m against surroundings at {around} m"
        );
    }

    #[test]
    fn nothing_outside_a_pit_is_raised() {
        // A fill that raised the whole surface would also remove every pit, and
        // every test above would pass. What it would destroy is the terrain.
        let before = sloping_with_pit();
        let network = FlowNetwork::build(sloping_with_pit());

        for (x, z) in [(500u32, 500u32), (1000, 300), (2000, 2000), (77, 4001)] {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };
            assert_eq!(
                network.filled().get(cell),
                before.get(cell),
                "cell ({x}, {z}) was raised, but it was never in a depression"
            );
        }
    }

    #[test]
    fn no_interior_sink_survives_the_fill() {
        // The property the fill exists for. A surviving sink is water that
        // vanishes mid-hillside, and every stage downstream inherits the hole.
        let network = FlowNetwork::build(sloping_with_pit());
        assert_eq!(
            network.interior_sinks(),
            0,
            "the fill left interior cells with nowhere to drain"
        );
    }

    #[test]
    fn flow_runs_downhill_everywhere() {
        let network = FlowNetwork::build(sloping_with_pit());

        for (x, z) in [(10u32, 10u32), (500, 500), (2500, 1234), (5000, 5000)] {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };
            let Some(next) = network.downstream(cell) else {
                continue;
            };

            assert!(
                network.filled().get(next) < network.filled().get(cell),
                "({x}, {z}) drains uphill"
            );
        }
    }

    #[test]
    fn following_flow_always_reaches_an_edge() {
        // A cycle in the direction field is the failure mode that hangs the
        // pipeline rather than producing wrong terrain — two cells pointing at
        // each other, and accumulation never terminates. Bounded so this test
        // fails rather than hangs if one exists.
        let network = FlowNetwork::build(sloping_with_pit());
        let mut cell = ErosionCell::new(2_000, 2_000).expect("in range");

        let mut steps = 0;
        while let Some(next) = network.downstream(cell) {
            cell = next;
            steps += 1;
            assert!(
                steps < CELLS,
                "following flow did not terminate, so the direction field has a cycle"
            );
        }

        let (x, z) = (cell.x(), cell.z());
        assert!(
            x == 0 || z == 0 || x == EDGE - 1 || z == EDGE - 1,
            "flow terminated at ({x}, {z}), which is not the grid edge"
        );
    }

    #[test]
    fn accumulation_collects_rather_than_fragmenting() {
        let network = FlowNetwork::build(sloping_with_pit());

        // A source drains only itself. On a plane tilted along +X the only
        // sources are in column 0 — every other column receives from the one
        // uphill of it, which is what "accumulation" means and is worth being
        // wrong about once rather than assuming.
        let source = ErosionCell::new(0, 2_500).expect("in range");
        assert_eq!(
            network.accumulation(source),
            1.0,
            "a cell with nothing uphill of it should carry only itself"
        );

        // And a cell one column downhill carries that source as well as itself.
        let below = ErosionCell::new(1, 2_500).expect("in range");
        assert!(
            network.accumulation(below) > 1.0,
            "the cell below a source should carry it"
        );

        // On a *perfect plane* the right answer is one full row, not a large
        // fraction of the grid: every row drains east independently and nothing
        // converges, because there are no valleys to converge into. Asserting a
        // large maximum here would have been asserting the fixture had terrain
        // it does not have — the convergence claim belongs on real noise, and
        // `tests/flow_network.rs` is where it is made.
        //
        // What this does say is that each row drains *all the way across*. A
        // maximum well under one row would mean flow stalling partway.
        let max = network.max_accumulation();
        assert!(
            max >= EDGE as f32,
            "the largest channel carries {max} cells, less than the {EDGE} of a \
             single row, so flow is stalling before it reaches the edge"
        );
    }

    /// The epsilon does not accumulate into visible terrain.
    ///
    /// The +epsilon fill trades flat basins for a slight tilt, and the trade is
    /// only acceptable if the tilt stays small. A basin filled to metres above
    /// its outlet would be a plateau where there should be a lake.
    #[test]
    fn a_filled_basin_stays_close_to_its_outlet() {
        // A wide, shallow, closed basin — the shape that accumulates the most
        // epsilon, because the fill has to cross all of it.
        let mut grid = BlockGrid::filled(100.0);
        for z in 200..800 {
            for x in 200..800 {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                grid.set(cell, 40.0);
            }
        }

        let network = FlowNetwork::build(grid);

        let mut highest = f32::NEG_INFINITY;
        for z in 200..800 {
            for x in 200..800 {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                highest = highest.max(network.filled().get(cell));
            }
        }

        // The outlet is the 100 m rim. The basin should fill to about that, not
        // above it by anything a player could stand on.
        assert!(
            highest < 100.0 + 0.5,
            "a 600-cell basin filled to {highest} m against a 100 m rim, so the \
             epsilon is accumulating into terrain"
        );
        assert_eq!(
            network.interior_sinks(),
            0,
            "the basin filled but still does not drain"
        );
    }

    #[test]
    fn diagonal_neighbours_are_measured_as_further_away() {
        // Comparing raw drops rather than gradients biases flow diagonally,
        // producing a network whose channels all run at 45 degrees. This is a
        // surface where the diagonal has the larger *drop* and the orthogonal
        // neighbour has the steeper *gradient*.
        let mut grid = BlockGrid::filled(100.0);
        let centre = ErosionCell::new(50, 50).expect("in range");
        grid.set(centre, 10.0);

        // East is 2 m away and 1.0 m down: gradient 0.50.
        // South-east is 2.83 m away and 1.3 m down: gradient 0.46 — bigger drop,
        // gentler slope.
        grid.set(ErosionCell::new(51, 50).expect("in range"), 9.0);
        grid.set(ErosionCell::new(51, 51).expect("in range"), 8.7);

        let directions = flow_directions(&grid, &resolve_flats(&grid));
        let chosen = directions.get(flat(centre)).copied().unwrap_or(NO_FLOW);

        assert_eq!(
            NEIGHBOURS.get(chosen as usize).copied(),
            Some((1, 0)),
            "flow took the larger drop rather than the steeper gradient"
        );
    }

    #[test]
    fn a_flat_tie_breaks_the_same_way_every_time() {
        // Real terrain has flat ground, and on it several neighbours are exactly
        // equally downhill. Unspecified here means a river that moves between
        // runs on the same seed, which `ADR-0006` forbids.
        let mut grid = BlockGrid::filled(50.0);
        let centre = ErosionCell::new(30, 30).expect("in range");
        grid.set(centre, 51.0);

        let first = flow_directions(&grid, &resolve_flats(&grid));
        let second = flow_directions(&grid, &resolve_flats(&grid));

        assert_eq!(
            first.get(flat(centre)),
            second.get(flat(centre)),
            "the same surface routed two different ways"
        );
        assert_eq!(
            first.get(flat(centre)).copied(),
            Some(0),
            "a tie should go to the first neighbour in the fixed order"
        );
    }
}
