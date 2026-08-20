//! A real block, at full size (S07/M2).
//!
//! The unit tests in `block.rs` check the grid's arithmetic on constants. This
//! one allocates and fills an actual 5,120² block — 26 million cells, 100 MB —
//! because the arithmetic being right and the allocation being affordable are
//! different claims, and `ADR-0015` rests on the second one.
//!
//! It is a separate integration test rather than a unit test because it is slow
//! and large enough that it should be skippable on its own, and because running
//! it under `--nocapture` is how the cost gets recorded.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    // Reporting how long generation took. The same sanctioned exception the
    // renderer's measurements take, for the same reason: it runs in a test and
    // nothing it reads reaches sim state (`ADR-0004`).
    clippy::disallowed_methods
)]

use std::time::Instant;

use cx_core::math::{BlockCoord, ChunkCoord, EROSION_CELLS_PER_BLOCK_EDGE};
use cx_worldgen::block::{CELLS, EDGE, HALO_CELLS};
use cx_worldgen::{BlockCoordinates, BlockGrid, ElevationGenerator, ErosionCell, TerrainShape};

const SEED: u64 = 0x0BADC0DE;

fn generator() -> ElevationGenerator {
    ElevationGenerator::new(SEED, TerrainShape::default())
}

/// A full block's base elevation, with the cost recorded.
#[test]
fn a_whole_block_generates_and_costs_what_adr_0015_expects() {
    let started = Instant::now();
    let grid =
        BlockGrid::base_elevation(&generator(), BlockCoordinates::new(BlockCoord::new(0, 0)));
    let elapsed = started.elapsed();

    assert_eq!(grid.as_slice().len(), CELLS);

    let megabytes = (CELLS * size_of::<f32>()) as f64 / (1024.0 * 1024.0);
    println!(
        "block base elevation: {EDGE}x{EDGE} = {CELLS} cells, {megabytes:.0} MB, \
         filled in {elapsed:?} (single-threaded, step 1 of 9)"
    );

    // Not a gate — it is one thread doing the cheapest stage, and the 20 s
    // target in S07 is for the whole pipeline on eight. Recorded so that the
    // stages added after this one have something to be compared against.
    let (low, high) = grid.core_range();
    let shape = TerrainShape::default();
    let world = generator().world().settings();
    println!("block core elevation range: {low:.1} m to {high:.1} m");

    // Against the continental range as well as the block's own. A block sits on
    // the world map's surface (`S07`), so its absolute heights span the world
    // map's relief plus its own — asserting only on the block's would fail the
    // moment continental structure existed, and did.
    let ceiling = world.base + world.relief + shape.base + shape.relief;
    let floor = world.base - world.relief + shape.base - shape.relief;
    assert!(
        low >= floor && high <= ceiling,
        "elevation left the range the world map and shape allow: {low} to {high}, \
         against {floor} to {ceiling}"
    );

    // The span should exceed a block's *own* relief, because the continental
    // surface tilts across 8 km as well. Below the block's own relief would mean
    // the grid is sampling one place repeatedly.
    assert!(
        high - low > shape.relief * 0.25,
        "a whole block of terrain spanned only {:.1} m, which is flat enough to \
         suggest the grid is sampling one place repeatedly",
        high - low
    );
}

/// **`ADR-0006`'s promise, at block scale.** Order of generation is irrelevant.
#[test]
fn blocks_generated_in_either_order_are_identical() {
    let generator = generator();

    let first = BlockGrid::base_elevation(&generator, BlockCoordinates::new(BlockCoord::new(2, 1)));
    let _intervening =
        BlockGrid::base_elevation(&generator, BlockCoordinates::new(BlockCoord::new(9, 9)));
    let again = BlockGrid::base_elevation(&generator, BlockCoordinates::new(BlockCoord::new(2, 1)));

    assert_eq!(
        first.as_slice(),
        again.as_slice(),
        "generating another block in between changed the result, so something \
         in the path is order-dependent"
    );
}

/// **The halo holds the neighbour's terrain, cell for cell.**
///
/// This is the property the whole halo scheme depends on and the one that
/// `block.rs`'s coordinate tests can only approach: they compare world
/// coordinates, this compares generated *heights*. If the two disagreed,
/// erosion either side of a seam would run against different terrain and no
/// halo width would hide it.
#[test]
fn a_blocks_halo_holds_the_same_terrain_its_neighbour_calls_core() {
    let generator = generator();

    let left = BlockCoordinates::new(BlockCoord::new(0, 0));
    let right = BlockCoordinates::new(BlockCoord::new(1, 0));

    let left_grid = BlockGrid::base_elevation(&generator, left);
    let right_grid = BlockGrid::base_elevation(&generator, right);

    // Walk the right block's whole first halo column against the matching
    // column inside the left block's core.
    let mut compared = 0;
    for z in HALO_CELLS..HALO_CELLS + EROSION_CELLS_PER_BLOCK_EDGE {
        let in_halo = ErosionCell::new(0, z).expect("in range");
        let in_core = ErosionCell::new(EROSION_CELLS_PER_BLOCK_EDGE, z).expect("in range");

        assert!(
            in_core.is_core(),
            "the column being compared against must be core terrain"
        );
        assert_eq!(
            right_grid.get(in_halo),
            left_grid.get(in_core),
            "at z={z}, the right block's halo and the left block's core \
             disagree about the same ground"
        );

        compared += 1;
    }

    assert_eq!(
        compared, EROSION_CELLS_PER_BLOCK_EDGE as usize,
        "the whole seam should have been walked"
    );
}

/// Every chunk of a block maps to a distinct, in-core slice.
///
/// The bake extracts chunks by slicing (`ADR-0006`: "chunks are pure
/// extraction"). If two chunks resolved to the same origin, or any resolved
/// into the halo, terrain would be duplicated or taken from cells that were
/// eroded with less context — both of which draw plausible landscapes.
#[test]
fn every_chunk_of_a_block_slices_a_distinct_piece_of_the_core() {
    let block = BlockCoord::new(-3, 5);
    let coordinates = BlockCoordinates::new(block);
    let origin = block.origin_chunk();

    let mut seen = Vec::new();
    for dz in 0..cx_core::math::BLOCK_CHUNKS as i32 {
        for dx in 0..cx_core::math::BLOCK_CHUNKS as i32 {
            let chunk = ChunkCoord::new(origin.x + dx, origin.z + dz);
            let cell = coordinates
                .chunk_origin_cell(chunk)
                .expect("a chunk of this block must resolve");

            assert!(
                cell.is_core(),
                "chunk {chunk:?} resolved into the halo at ({}, {})",
                cell.x(),
                cell.z()
            );
            seen.push((cell.x(), cell.z()));
        }
    }

    let count = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        count,
        "two chunks resolved to the same cell, so terrain would be duplicated"
    );
}
