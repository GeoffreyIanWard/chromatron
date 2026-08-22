//! Times each stage of block generation, from below the firewall.
//!
//! `ADR-0004` bans clocks inside sim code, so the pipeline cannot time
//! itself; this example replays `generate_block`'s exact stage sequence with
//! a stopwatch around each stage. It exists to answer one recurring question
//! — *where does the time go now?* — every time the sub-20-second exit
//! criterion is worked on. Numbers are machine-specific; compare runs on the
//! same machine only.
//!
//! ```bash
//! cargo run --release -p chromatron-game --example genprofile
//! ```

// A stopwatch is this example's entire purpose. The workspace-wide ban on
// wall-clock reads exists to keep time out of *sim logic*; this is a
// measurement harness below the firewall, the same exemption `cx-app`'s
// window driver documents.
#![allow(clippy::disallowed_methods)]

use std::time::Instant;

use cx_core::math::BlockCoord;
use cx_worldgen::{BlockGrid, ElevationGenerator, WorldSettings, block::BlockCoordinates};

fn main() {
    let seed = 20_260_821u64;
    let settings = WorldSettings::default();
    let coordinates = BlockCoordinates::new(BlockCoord::new(0, 0));
    let started = Instant::now();

    let generator = ElevationGenerator::with_world(seed, settings.terrain, settings.world);

    let stage = Instant::now();
    let elevation = BlockGrid::base_elevation(&generator, coordinates);
    println!("base elevation      {:>8.2?}", stage.elapsed());

    // Inside erosion, the two repeating costs: the initial full network
    // build (priority-flood fill included) and the pit-free rebuild that
    // runs between rounds. Timed on copies so the pipeline run below stays
    // exactly what ships.
    let stage = Instant::now();
    let full = cx_worldgen::FlowNetwork::build(elevation.clone());
    println!("  one full network build    {:>8.2?}", stage.elapsed());
    drop(full);
    let stage = Instant::now();
    let pitfree = cx_worldgen::FlowNetwork::build_pit_free(elevation.clone());
    println!("  one pit-free rebuild      {:>8.2?}", stage.elapsed());

    // One full pass of receiver iteration — the share math every stage leans
    // on. Its cost times passes-per-round says whether shares are the hot
    // spot or a bystander.
    let stage = Instant::now();
    let mut sink = 0.0f32;
    for z in 0..cx_worldgen::block::EDGE {
        for x in 0..cx_worldgen::block::EDGE {
            if let Some(cell) = cx_worldgen::ErosionCell::new(x, z) {
                pitfree.for_each_receiver(cell, |_, share| sink += share);
            }
        }
    }
    println!(
        "  one receiver-share pass   {:>8.2?} (sink {sink:.1})",
        stage.elapsed()
    );
    drop(pitfree);

    let stage = Instant::now();
    let (eroded, network, erosion) =
        cx_worldgen::erode(elevation, seed, coordinates, settings.erosion);
    println!(
        "erode (fill+route+{} rounds) {:>8.2?}",
        erosion.rounds,
        stage.elapsed()
    );

    let stage = Instant::now();
    let (relaxed, thermal) = cx_worldgen::relax(eroded, settings.thermal);
    println!(
        "thermal ({} rounds)  {:>8.2?}",
        thermal.rounds,
        stage.elapsed()
    );

    let stage = Instant::now();
    let carved = cx_worldgen::carve(relaxed, &network, settings.carve);
    println!("carve               {:>8.2?}", stage.elapsed());

    println!("total               {:>8.2?}", started.elapsed());

    // A digest of the final terrain, so a speed change can prove it moved no
    // bits. FNV over the raw float bits, whole grid, fixed order.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for value in carved.drained.as_slice() {
        for byte in value.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    println!("terrain hash        {hash:#018x}");
    // The two per-chunk exit criteria (S07/M2): extraction from a resident
    // block under 5 ms, and the full-resolution bake under 200 ms. Measured
    // on the block just generated, over a handful of chunks so one lucky
    // cache line does not write the milestone numbers.
    let network = cx_worldgen::FlowNetwork::build(carved.drained.clone());
    let block = cx_worldgen::GeneratedBlock {
        coordinates,
        terrain: carved.drained,
        ground: carved.ground,
        network,
        generator,
        report: cx_worldgen::BlockReport {
            erosion,
            thermal,
            carve: carved.report,
        },
    };
    let mut worst_bake = std::time::Duration::ZERO;
    let mut worst_water = std::time::Duration::ZERO;
    let mut worst_fields = std::time::Duration::ZERO;
    for chunk in [(0, 0), (7, 7), (15, 15), (3, 12)] {
        let chunk = cx_core::math::ChunkCoord::new(chunk.0, chunk.1);
        let stage = Instant::now();
        let baked = cx_worldgen::bake_chunk(
            &block.terrain,
            &block.network,
            &block.generator,
            block.coordinates,
            chunk,
            cx_worldgen::BakeSettings::SMOOTH,
        );
        worst_bake = worst_bake.max(stage.elapsed());
        let stage = Instant::now();
        let _ = cx_worldgen::bake_water(&block, chunk, cx_worldgen::WaterSettings::DEFAULT);
        worst_water = worst_water.max(stage.elapsed());
        if let Some(baked) = baked {
            let stage = Instant::now();
            let _ = cx_worldgen::derive_fields(&baked);
            worst_fields = worst_fields.max(stage.elapsed());
        }
    }
    println!("worst chunk bake    {worst_bake:>8.2?} (criterion: < 200 ms)");
    println!("worst chunk water   {worst_water:>8.2?}");
    println!("worst derive fields {worst_fields:>8.2?}");
    println!(
        "(erosion mean lowering {:.2} m, carve deepest {:.1} m)",
        erosion.mean_lowering, carved.report.deepest
    );
}
