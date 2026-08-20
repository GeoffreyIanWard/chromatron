//! The block grid: the surface steps 2–5 of S07's pipeline operate on.
//!
//! A block is the unit of generation (`ADR-0006`) — 8,192 m square, generated
//! with a 1,024 m halo that is eroded along with it and then discarded. This
//! module is the grid itself: where its cells are in the world, how to index
//! them, and how to fill one from base elevation.
//!
//! # Two grids, and why the distinction is in the type system
//!
//! Erosion runs at [`EROSION_CELL_SIZE`] — 2 m — while `ELEVATION` is stored at
//! [`cx_core::math::CELL_SIZE`], 0.5 m (`ADR-0015`). Two grids over the same
//! ground, four field cells to an erosion cell on each axis.
//!
//! Confusing them is the obvious bug and a quiet one: an index computed on the
//! wrong grid still lands inside the buffer, still produces terrain, and is
//! wrong by a factor of four in a way that looks like a tuning problem. So a
//! position on this grid is an [`ErosionCell`] rather than a pair of `u32`s, and
//! it cannot be built out of range.
//!
//! # The halo is part of the grid, not beside it
//!
//! Halo cells are indexed exactly like core cells, with the origin at the
//! *halo's* corner rather than the block's. An erosion stage sweeping the whole
//! buffer needs no special case at the boundary, which is the point of having a
//! halo at all — the alternative is every stage carrying its own edge handling,
//! and edge handling is where iterative solvers go wrong.
//!
//! [`BlockGrid::core`] is what the bake keeps. Everything outside it exists to
//! make the inside correct and is then thrown away.

use cx_core::math::{
    BLOCK_SIZE, CHUNK_SIZE, ChunkCoord, EROSION_CELL_SIZE, EROSION_CELLS_PER_BLOCK_EDGE,
    EROSION_CELLS_PER_BLOCK_EDGE_HALOED, GENERATION_HALO_CHUNKS,
};

use crate::elevation::ElevationGenerator;

/// A cell on the erosion grid of one block, halo included.
///
/// Cannot be constructed out of range: every stage indexes with these, so a
/// bounds mistake is a `None` at the point it is made rather than a wrong
/// height a hundred iterations later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ErosionCell {
    x: u32,
    z: u32,
}

impl ErosionCell {
    /// A cell, or `None` if either axis is outside the haloed grid.
    pub const fn new(x: u32, z: u32) -> Option<Self> {
        if x < EROSION_CELLS_PER_BLOCK_EDGE_HALOED && z < EROSION_CELLS_PER_BLOCK_EDGE_HALOED {
            Some(Self { x, z })
        } else {
            None
        }
    }

    /// Index along +X, including the halo.
    pub const fn x(self) -> u32 {
        self.x
    }

    /// Index along +Z, including the halo.
    pub const fn z(self) -> u32 {
        self.z
    }

    /// Whether this cell survives the halo discard.
    ///
    /// A stage that reports a statistic — how much sediment moved, how many
    /// cells drain to the edge — must count over the core only. Halo cells are
    /// computed with less context than core cells by construction, so folding
    /// them into a measurement makes the measurement partly about the halo.
    pub const fn is_core(self) -> bool {
        let low = HALO_CELLS;
        let high = HALO_CELLS + EROSION_CELLS_PER_BLOCK_EDGE;
        self.x >= low && self.x < high && self.z >= low && self.z < high
    }
}

/// Erosion cells of halo on each side.
pub const HALO_CELLS: u32 = (GENERATION_HALO_CHUNKS as f32 * CHUNK_SIZE / EROSION_CELL_SIZE) as u32;

/// Cells along one edge of the buffer, halo included.
pub const EDGE: u32 = EROSION_CELLS_PER_BLOCK_EDGE_HALOED;

/// Cells in the whole buffer.
pub const CELLS: usize = (EDGE as usize) * (EDGE as usize);

/// One `f32` field over a block's erosion grid.
///
/// Named rather than a bare `Vec<f32>` because the pipeline has several of them
/// — elevation, water, sediment, flow accumulation — and they are all the same
/// shape and none of them are interchangeable.
#[derive(Debug, Clone)]
pub struct BlockGrid {
    cells: Vec<f32>,
}

impl BlockGrid {
    /// A grid filled with `value`.
    ///
    /// Allocates [`CELLS`] floats — 100 MB at the sizes `ADR-0015` settles on.
    /// That is a deliberate single allocation per field per in-flight block, and
    /// the frontier's concurrency is bounded by how many of them fit in the
    /// budget rather than the other way round.
    pub fn filled(value: f32) -> Self {
        Self {
            cells: vec![value; CELLS],
        }
    }

    /// Base elevation over a whole block, halo included — step 1 of the
    /// pipeline, evaluated onto the grid steps 2–5 will erode.
    ///
    /// Sampled at erosion-cell centres. Sampling corners would bias the whole
    /// block half a cell towards its origin, and because every block would be
    /// biased identically the result looks like correct terrain in the wrong
    /// place rather than like an off-by-one.
    pub fn base_elevation(generator: &ElevationGenerator, block: BlockCoordinates) -> Self {
        let mut cells = Vec::with_capacity(CELLS);

        for z in 0..EDGE {
            for x in 0..EDGE {
                let (world_x, world_z) = block.cell_centre(x, z);
                cells.push(generator.height_at(world_x, world_z));
            }
        }

        Self { cells }
    }

    /// The value at a cell.
    pub fn get(&self, cell: ErosionCell) -> f32 {
        // `ErosionCell` cannot be out of range, so this index is in bounds by
        // construction — but the sim lint set bans unchecked indexing outright
        // and a silent 0.0 is a better failure than a panic in a release build.
        self.cells.get(index_of(cell)).copied().unwrap_or_default()
    }

    /// Replaces the value at a cell.
    pub fn set(&mut self, cell: ErosionCell, value: f32) {
        if let Some(slot) = self.cells.get_mut(index_of(cell)) {
            *slot = value;
        }
    }

    /// Every value, row-major with +X fastest.
    ///
    /// The order matters: erosion stages parallelise by row band (`ADR-0008`),
    /// and a band is a contiguous slice only in this order.
    pub fn as_slice(&self) -> &[f32] {
        &self.cells
    }

    /// Every value, mutably.
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.cells
    }

    /// The lowest and highest values over the **core**, ignoring the halo.
    ///
    /// Core-only for the reason [`ErosionCell::is_core`] gives: a halo cell is
    /// computed with less surrounding context, so including it would make a
    /// range partly a statement about the halo.
    pub fn core_range(&self) -> (f32, f32) {
        let mut low = f32::INFINITY;
        let mut high = f32::NEG_INFINITY;

        for z in HALO_CELLS..HALO_CELLS + EROSION_CELLS_PER_BLOCK_EDGE {
            for x in HALO_CELLS..HALO_CELLS + EROSION_CELLS_PER_BLOCK_EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                let value = self.get(cell);
                low = low.min(value);
                high = high.max(value);
            }
        }

        if low > high { (0.0, 0.0) } else { (low, high) }
    }
}

/// Row-major index, +X fastest.
const fn index_of(cell: ErosionCell) -> usize {
    (cell.z as usize) * (EDGE as usize) + (cell.x as usize)
}

/// Where a block sits in the world, for turning grid indices into metres.
///
/// A separate type from `cx_core`'s `BlockCoord` because this carries the halo
/// offset with it. Every stage that samples a positional function needs the same
/// conversion, and doing it at each call site is how a block ends up generated
/// one halo-width away from where it belongs — terrain that is correct, and in
/// the wrong place, which is far harder to see than terrain that is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockCoordinates {
    block: cx_core::math::BlockCoord,
}

impl BlockCoordinates {
    /// Coordinates for a block.
    pub const fn new(block: cx_core::math::BlockCoord) -> Self {
        Self { block }
    }

    /// The block this describes.
    pub const fn block(self) -> cx_core::math::BlockCoord {
        self.block
    }

    /// World metres at the centre of erosion cell `(x, z)`, halo included.
    ///
    /// Cell `(0, 0)` is the *halo's* corner, one halo-width outside the block.
    pub fn cell_centre(self, x: u32, z: u32) -> (f32, f32) {
        let origin_x = self.block.x as f32 * BLOCK_SIZE - HALO_CELLS as f32 * EROSION_CELL_SIZE;
        let origin_z = self.block.z as f32 * BLOCK_SIZE - HALO_CELLS as f32 * EROSION_CELL_SIZE;

        (
            origin_x + (x as f32 + 0.5) * EROSION_CELL_SIZE,
            origin_z + (z as f32 + 0.5) * EROSION_CELL_SIZE,
        )
    }

    /// The erosion cell containing a chunk's minimum corner.
    ///
    /// How the bake finds a chunk's slice of the block. `None` when the chunk is
    /// not in this block, which is a caller error rather than a coordinate to
    /// clamp — clamping would silently extract the wrong terrain.
    pub fn chunk_origin_cell(self, chunk: ChunkCoord) -> Option<ErosionCell> {
        if chunk.block() != self.block {
            return None;
        }

        let (offset_x, offset_z) = chunk.offset_in_block();
        let per_chunk = (CHUNK_SIZE / EROSION_CELL_SIZE) as u32;

        ErosionCell::new(
            HALO_CELLS + offset_x * per_chunk,
            HALO_CELLS + offset_z * per_chunk,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::{
        BLOCK_CHUNKS, BlockCoord, CELL_SIZE, CELLS_PER_EROSION_CELL, EROSION_CELLS_PER_BLOCK_EDGE,
    };

    /// `ADR-0015`'s arithmetic, as an assertion.
    ///
    /// The whole decision rests on these numbers, and a later change to
    /// `EROSION_CELL_SIZE` or the halo width would move them silently — the code
    /// would still build and still generate terrain, just with a working set the
    /// budget cannot hold. This is the check that would notice.
    #[test]
    fn the_working_set_fits_the_budget_adr_0015_names() {
        assert_eq!(EROSION_CELLS_PER_BLOCK_EDGE, 4_096, "8,192 m at 2 m");
        assert_eq!(EDGE, 5_120, "plus 1,024 m of halo on each side");
        assert_eq!(CELLS, 26_214_400);

        // Elevation, water, sediment and flow accumulation as `f32`, flow
        // direction as `u8` — the five the pipeline needs resident at once.
        let working_set = CELLS * (4 + 4 + 4 + 4 + 1);
        let budget = 8 * 1024 * 1024 * 1024 / 10; // 0.8 GB

        assert!(
            working_set < budget,
            "the erosion working set is {:.2} GB against a {:.1} GB budget; \
             ADR-0015's decision no longer holds",
            working_set as f64 / 1024.0f64.powi(3),
            budget as f64 / 1024.0f64.powi(3)
        );
    }

    #[test]
    fn the_two_grids_agree_about_the_same_ground() {
        // Four field cells to an erosion cell on each axis. If this drifted, the
        // bake's resample would stretch terrain rather than fail.
        assert_eq!(CELLS_PER_EROSION_CELL, 4);
        assert_eq!(EROSION_CELL_SIZE, CELL_SIZE * CELLS_PER_EROSION_CELL as f32);
        assert_eq!(HALO_CELLS, 512, "1,024 m of halo at 2 m");
    }

    #[test]
    fn a_cell_cannot_be_built_outside_the_grid() {
        assert!(ErosionCell::new(0, 0).is_some());
        assert!(ErosionCell::new(EDGE - 1, EDGE - 1).is_some());
        assert!(ErosionCell::new(EDGE, 0).is_none());
        assert!(ErosionCell::new(0, EDGE).is_none());
    }

    #[test]
    fn the_core_is_the_middle_and_the_halo_surrounds_it() {
        let core = |x, z| ErosionCell::new(x, z).expect("in range").is_core();

        assert!(!core(0, 0), "the grid's corner is halo");
        assert!(!core(HALO_CELLS - 1, HALO_CELLS), "just outside is halo");
        assert!(core(HALO_CELLS, HALO_CELLS), "the core's first cell");

        let last = HALO_CELLS + EROSION_CELLS_PER_BLOCK_EDGE - 1;
        assert!(core(last, last), "the core's last cell");
        assert!(!core(last + 1, last), "just past it is halo again");
    }

    #[test]
    fn the_core_is_exactly_a_block() {
        let counted = (0..EDGE)
            .flat_map(|z| (0..EDGE).map(move |x| (x, z)))
            .filter(|(x, z)| ErosionCell::new(*x, *z).is_some_and(ErosionCell::is_core))
            .count();

        assert_eq!(
            counted,
            (EROSION_CELLS_PER_BLOCK_EDGE as usize).pow(2),
            "the core must be exactly the block, no more and no less"
        );
    }

    #[test]
    fn the_grids_origin_is_the_halos_corner_not_the_blocks() {
        let coordinates = BlockCoordinates::new(BlockCoord::new(0, 0));

        // Cell 0 sits a halo-width *outside* the block, in negative world
        // coordinates. If this were 0.0-ish, every block would be generated one
        // halo-width from where it belongs — terrain that is correct and in the
        // wrong place, which is much harder to see than terrain that is wrong.
        let (x, z) = coordinates.cell_centre(0, 0);
        let expected = -(HALO_CELLS as f32) * EROSION_CELL_SIZE + 0.5 * EROSION_CELL_SIZE;
        assert_eq!(x, expected);
        assert_eq!(z, expected);

        // And the core's first cell is at the block's own origin.
        let (x, _) = coordinates.cell_centre(HALO_CELLS, HALO_CELLS);
        assert_eq!(x, 0.5 * EROSION_CELL_SIZE);
    }

    #[test]
    fn adjacent_blocks_overlap_exactly_in_their_halos() {
        // The halo is only useful if it holds the *same ground* the neighbour
        // holds as core. If these disagreed, erosion near a seam would be run
        // against different terrain on each side and the seam would be visible
        // no matter how wide the halo was.
        let left = BlockCoordinates::new(BlockCoord::new(0, 0));
        let right = BlockCoordinates::new(BlockCoord::new(1, 0));

        // The right block's first halo column covers ground that is inside the
        // left block, one halo-width in from the seam between them.
        let (right_x, _) = right.cell_centre(0, HALO_CELLS);
        let (left_x, _) = left.cell_centre(EROSION_CELLS_PER_BLOCK_EDGE, HALO_CELLS);

        assert_eq!(
            right_x, left_x,
            "the same ground must have the same world coordinate in both blocks"
        );

        // Pinned absolutely, not only relatively. The two indices differ by
        // exactly the block offset, so *any* uniform shift of both origins keeps
        // them equal — the comparison above passes even with the halo offset
        // dropped from `cell_centre` entirely, which was checked by doing it.
        // Only an absolute value catches that.
        assert_eq!(
            right_x,
            BLOCK_SIZE - HALO_CELLS as f32 * EROSION_CELL_SIZE + 0.5 * EROSION_CELL_SIZE,
            "the right block's halo must start one halo-width before the seam"
        );

        // And that ground must be *core* in the left block. A halo that
        // overlapped only the neighbour's halo would still line up numerically
        // while covering terrain nobody computed with full context.
        let overlapped =
            ErosionCell::new(EROSION_CELLS_PER_BLOCK_EDGE, HALO_CELLS).expect("in range");
        assert!(
            overlapped.is_core(),
            "the halo must cover ground the neighbour computed as core"
        );
    }

    #[test]
    fn a_chunk_maps_to_its_own_slice_of_the_block() {
        let coordinates = BlockCoordinates::new(BlockCoord::new(0, 0));

        let first = coordinates
            .chunk_origin_cell(ChunkCoord::new(0, 0))
            .expect("chunk 0 is in block 0");
        assert_eq!((first.x(), first.z()), (HALO_CELLS, HALO_CELLS));

        // The last chunk of the block starts one chunk short of the core's end.
        let per_chunk = (CHUNK_SIZE / EROSION_CELL_SIZE) as u32;
        let last = coordinates
            .chunk_origin_cell(ChunkCoord::new(
                BLOCK_CHUNKS as i32 - 1,
                BLOCK_CHUNKS as i32 - 1,
            ))
            .expect("the block's last chunk");
        assert_eq!(
            last.x(),
            HALO_CELLS + EROSION_CELLS_PER_BLOCK_EDGE - per_chunk
        );
    }

    #[test]
    fn a_chunk_from_another_block_is_rejected_rather_than_clamped() {
        // Clamping would extract real terrain from the wrong place, which draws
        // a plausible landscape and puts it somewhere it does not belong.
        let coordinates = BlockCoordinates::new(BlockCoord::new(0, 0));
        assert!(
            coordinates
                .chunk_origin_cell(ChunkCoord::new(BLOCK_CHUNKS as i32, 0))
                .is_none()
        );
        assert!(
            coordinates
                .chunk_origin_cell(ChunkCoord::new(-1, 0))
                .is_none()
        );
    }

    #[test]
    fn a_grid_round_trips_a_written_value() {
        let mut grid = BlockGrid::filled(0.0);
        let cell = ErosionCell::new(7, 11).expect("in range");

        grid.set(cell, 42.5);
        assert_eq!(grid.get(cell), 42.5);

        // Row-major with +X fastest: the neighbour along X is adjacent in
        // memory, which is what makes a row band a contiguous slice.
        let along_x = ErosionCell::new(8, 11).expect("in range");
        assert_eq!(index_of(along_x), index_of(cell) + 1);

        let along_z = ErosionCell::new(7, 12).expect("in range");
        assert_eq!(index_of(along_z), index_of(cell) + EDGE as usize);
    }
}
