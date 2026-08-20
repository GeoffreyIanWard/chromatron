//! The flow network on real terrain (S07/M2, step 2).
//!
//! `flow.rs`'s unit tests use synthetic surfaces — a plane, a pit, a flat basin
//! — because those isolate one behaviour each. This runs the whole thing over a
//! real 5,120² block of noise, which is the only place several of step 2's
//! claims can actually be made:
//!
//! - **Convergence.** A plane produces parallel streams that never meet; only
//!   terrain with valleys produces a drainage *network*. The unit test says so
//!   explicitly and defers the claim here.
//! - **Cost.** Priority-flood is O(n log n) over 26 million cells with a heap.
//!   Whether that is seconds or minutes is not a thing to reason about.
//! - **Determinism at scale.** Ties are common on real terrain and rare on
//!   synthetic surfaces, so this is where an unspecified tie-break would show.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    // Reporting how long a stage took, as elsewhere in this project's
    // measurements: it runs in a test and nothing it reads reaches sim state.
    clippy::disallowed_methods
)]

use std::time::Instant;

use cx_core::math::BlockCoord;
use cx_worldgen::block::{CELLS, EDGE, ErosionCell};
use cx_worldgen::{BlockCoordinates, BlockGrid, ElevationGenerator, FlowNetwork, TerrainShape};

const SEED: u64 = 0x0BADC0DE;

fn block(x: i32, z: i32) -> BlockGrid {
    let generator = ElevationGenerator::new(SEED, TerrainShape::default());
    BlockGrid::base_elevation(&generator, BlockCoordinates::new(BlockCoord::new(x, z)))
}

/// The whole of step 2 over a real block, with the cost recorded.
#[test]
fn a_real_block_routes_into_a_drainage_network() {
    let elevation = block(0, 0);

    let started = Instant::now();
    let network = FlowNetwork::build(elevation);
    let elapsed = started.elapsed();

    let max = network.max_accumulation();
    println!(
        "flow network: {EDGE}x{EDGE} = {CELLS} cells in {elapsed:?} (single-threaded, \
         step 2 of 9); largest channel carries {max} cells ({:.1}% of the block)",
        f64::from(max) / CELLS as f64 * 100.0
    );

    // **The property the fill exists for.** Every interior cell must have
    // somewhere to drain. One survivor is a river ending mid-hillside, and
    // everything downstream of it inherits the hole.
    assert_eq!(
        network.interior_sinks(),
        0,
        "the fill left interior cells with nowhere to drain"
    );

    // **Convergence** — the claim a plane cannot make. Real terrain has valleys,
    // so drainage collects: the largest channel should carry far more than the
    // 5,120 cells of a single row.
    assert!(
        max > EDGE * 10,
        "the largest channel carries {max} cells, barely more than the {EDGE} of \
         one row — flow is running in parallel lines rather than collecting into \
         a network"
    );
}

/// **`ADR-0006` at the pipeline level.** Same seed, same block, same network.
///
/// Ties are common on real terrain — noise quantises, and adjacent cells land on
/// identical heights constantly. An unspecified tie-break in either the heap or
/// D8 would move channels between runs, which is the failure this asserts
/// against and the reason both orderings are total.
#[test]
fn the_same_block_produces_the_same_network_twice() {
    let first = FlowNetwork::build(block(1, -2));
    let second = FlowNetwork::build(block(1, -2));

    let mut compared = 0;
    for z in (0..EDGE).step_by(7) {
        for x in (0..EDGE).step_by(7) {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };

            assert_eq!(
                first.filled().get(cell),
                second.filled().get(cell),
                "the filled surface differs at ({x}, {z})"
            );
            assert_eq!(
                first.direction(cell),
                second.direction(cell),
                "flow direction differs at ({x}, {z})"
            );
            assert_eq!(
                first.accumulation(cell),
                second.accumulation(cell),
                "accumulation differs at ({x}, {z})"
            );
            compared += 1;
        }
    }

    assert!(compared > 500_000, "only {compared} cells were compared");
}

/// The fill raises terrain, and only where it has to.
///
/// A fill that raised everything would remove every sink and pass the test
/// above, while destroying the terrain it was given. This bounds it from the
/// other side: most of a block is hillside, not basin, and hillside must come
/// through untouched.
#[test]
fn the_fill_changes_only_a_minority_of_the_block() {
    let before = block(3, 3);
    let network = FlowNetwork::build(block(3, 3));

    let mut raised = 0usize;
    let mut examined = 0usize;
    let mut largest = 0.0f32;

    for z in (0..EDGE).step_by(3) {
        for x in (0..EDGE).step_by(3) {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };
            let difference = network.filled().get(cell) - before.get(cell);
            examined += 1;

            assert!(
                difference >= 0.0,
                "the fill *lowered* ({x}, {z}) by {}, which it must never do",
                -difference
            );

            // Above the epsilon, so a cell nudged by the fill's tilt does not
            // count as "raised" — that would make this measure the epsilon
            // rather than the filling.
            if difference > 0.01 {
                raised += 1;
                largest = largest.max(difference);
            }
        }
    }

    let fraction = raised as f64 / examined as f64;
    println!(
        "fill: {:.1}% of cells raised, deepest {largest:.1} m",
        fraction * 100.0
    );

    assert!(
        fraction < 0.5,
        "the fill raised {:.0}% of the block, which is flattening terrain \
         rather than filling basins",
        fraction * 100.0
    );
    assert!(
        raised > 0,
        "the fill raised nothing at all on a whole block of noise, so it is not \
         filling anything"
    );
}

/// Flow never runs uphill, checked over the whole block rather than at samples.
#[test]
fn no_cell_anywhere_drains_uphill() {
    let network = FlowNetwork::build(block(-1, 4));

    let mut checked = 0usize;
    for z in 0..EDGE {
        for x in 0..EDGE {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };
            let Some(next) = network.downstream(cell) else {
                continue;
            };

            let here = network.filled().get(cell);
            let there = network.filled().get(next);

            // `<=`, not `<`. Water crosses a filled flat to a neighbour at
            // exactly its own height — that is what a lake surface is, and it is
            // what flat resolution exists to direct. What must never happen is
            // *uphill*.
            assert!(
                there <= here,
                "({x}, {z}) at {here} m drains uphill to ({}, {}) at {there} m",
                next.x(),
                next.z()
            );
            checked += 1;
        }
    }

    // Nearly every cell should have a downstream. Only the grid's rim drains
    // off the edge — if this number were small, most of the block would be
    // sinks and the assertion above would be vacuous.
    assert!(
        checked > CELLS * 9 / 10,
        "only {checked} of {CELLS} cells had anywhere to drain"
    );
}
