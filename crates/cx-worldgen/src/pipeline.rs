//! The block generation pipeline, as one function.
//!
//! S07's stages 1–6 composed: base elevation on the world map, depression fill
//! and flow routing, hydraulic erosion, thermal relaxation, channel carving, and
//! the surfaces the bake and the derived fields read.
//!
//! # Why this exists as a single call
//!
//! `ADR-0006` promises a block is a pure function of `(world_seed,
//! block_coord)`. Six stages threaded together by hand at each call site is six
//! chances to thread them differently, and "differently" here means a world that
//! does not regenerate — which is the one property the whole design rests on.
//! One entry point makes the promise checkable. `tests/block_pipeline.rs` is the
//! check, at the 4x4 size M2's exit criterion states; a fast 2x2 version runs in
//! this module's own tests on every commit.
//!
//! # Concurrency is bounded by memory, not by cores
//!
//! S07 asks for generation on a background pool and 20 s per block on 8 threads.
//! That reads as eight blocks at once, and the arithmetic says otherwise:
//!
//! | | |
//! |---|---|
//! | Resident per block — filled surface, accumulation, drainage order, direction, ground | 0.415 GB |
//! | Worst transient — flat resolution's height copy plus two distance maps | 0.293 GB |
//! | **Peak per in-flight block** | **~0.71 GB** |
//! | Budget (`bench/memory-budget.md`) | 0.8 GB |
//!
//! **1.13 blocks fit.** So the pool generates one block at a time, and the eight
//! threads go *inside* a block — which is what `ADR-0008` said in the first
//! place: *"grid-based erosion is deterministic and parallelizes by row band."*
//!
//! That is worth being explicit about, because "8 background threads" invites
//! the other reading, and the other reading exceeds the budget by 7x on the
//! first frontier that gets busy.

use cx_core::math::BlockCoord;

use crate::block::{BlockCoordinates, BlockGrid};
use crate::carve::CarveSettings;
use crate::elevation::{ElevationGenerator, TerrainShape};
use crate::flow::FlowNetwork;
use crate::hydraulic::ErosionSettings;
use crate::thermal::ThermalSettings;
use crate::worldmap::WorldMapSettings;

/// Everything a world's terrain is shaped by.
///
/// One struct rather than five arguments: these travel together through every
/// stage, and S07's `full-sim` and `no-erosion` profiles are two values of this
/// rather than two code paths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldSettings {
    /// Continental structure (`crate::worldmap`).
    pub world: WorldMapSettings,
    /// A block's own relief.
    pub terrain: TerrainShape,
    /// Hydraulic erosion, stage 3.
    pub erosion: ErosionSettings,
    /// Thermal relaxation, stage 4.
    pub thermal: ThermalSettings,
    /// Channel carving, stage 5.
    pub carve: CarveSettings,
}

impl Default for WorldSettings {
    fn default() -> Self {
        Self {
            world: WorldMapSettings::DEFAULT,
            terrain: TerrainShape::DEFAULT,
            erosion: ErosionSettings::DEFAULT,
            thermal: ThermalSettings::DEFAULT,
            carve: CarveSettings::DEFAULT,
        }
    }
}

impl WorldSettings {
    /// S07's `no-erosion` profile: a valid world, differing from `full-sim` only
    /// in terrain shape.
    ///
    /// Not a separate code path. Every stage still runs — a world without
    /// erosion still needs drainage — they simply do nothing, which is what
    /// makes this testable as the identity rather than as an untaken branch.
    pub const NO_EROSION: Self = Self {
        world: WorldMapSettings::DEFAULT,
        terrain: TerrainShape::DEFAULT,
        erosion: ErosionSettings::NONE,
        thermal: ThermalSettings::NONE,
        carve: CarveSettings::NONE,
    };
}

/// A generated block, ready for chunks to be extracted from it.
#[derive(Debug)]
pub struct GeneratedBlock {
    /// Which block this is.
    pub coordinates: BlockCoordinates,
    /// The final terrain, depressions filled. What the bake resamples.
    pub terrain: BlockGrid,
    /// The final terrain **before** the last fill.
    ///
    /// `terrain - ground` is standing water: where the fill raised ground, that
    /// is a lake, and how far it raised it is how deep. S07 step 7's water body
    /// extents come from this difference and from nothing else.
    pub ground: BlockGrid,
    /// Drainage of the final surface.
    pub network: FlowNetwork,
    /// The generator that produced it, for the bake's detail noise.
    pub generator: ElevationGenerator,
    /// What each stage did.
    pub report: BlockReport,
}

impl GeneratedBlock {
    /// Standing water depth at a cell, in metres. Zero on dry ground.
    pub fn water_depth(&self, cell: crate::block::ErosionCell) -> f32 {
        (self.terrain.get(cell) - self.ground.get(cell)).max(0.0)
    }
}

/// What generating a block cost and did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockReport {
    /// Hydraulic erosion.
    pub erosion: crate::hydraulic::ErosionReport,
    /// Thermal relaxation.
    pub thermal: crate::thermal::ThermalReport,
    /// Channel carving.
    pub carve: crate::carve::CarveReport,
}

/// Generates one block — S07 stages 1 to 6.
///
/// A pure function of `(seed, block, settings)`, per `ADR-0006`. Nothing here
/// reads a clock, a thread id, or any state outside its arguments, which is what
/// makes generating a block on a background thread safe without a lock: two
/// blocks share nothing.
pub fn generate_block(seed: u64, block: BlockCoord, settings: WorldSettings) -> GeneratedBlock {
    let coordinates = BlockCoordinates::new(block);
    let generator = ElevationGenerator::with_world(seed, settings.terrain, settings.world);

    // 1. Base elevation on the continental surface.
    let elevation = BlockGrid::base_elevation(&generator, coordinates);

    // 2 and 3. Fill, route, and erode. `erode` runs the fill itself, because
    // erosion needs drainage and re-routes between rounds anyway.
    let (eroded, network, erosion) = crate::hydraulic::erode(elevation, settings.erosion);

    // 4. Talus relaxation.
    let (relaxed, thermal) = crate::thermal::relax(eroded, settings.thermal);

    // 5. Channel carving, which re-routes once more against the surface it cut.
    let carved = crate::carve::carve(relaxed, &network, settings.carve);

    GeneratedBlock {
        coordinates,
        terrain: carved.drained,
        ground: carved.ground,
        network: carved.network,
        generator,
        report: BlockReport {
            erosion,
            thermal,
            carve: carved.report,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{EDGE, ErosionCell};

    const SEED: u64 = 0x0BADC0DE;

    /// Enough of each stage to exercise it, few enough rounds to run in a test.
    ///
    /// Order independence does not depend on how many rounds run — it depends on
    /// nothing in the pipeline reading outside its arguments — so a short run
    /// tests the same property a long one would.
    fn fast() -> WorldSettings {
        WorldSettings {
            erosion: ErosionSettings {
                rounds: 1,
                ..ErosionSettings::DEFAULT
            },
            thermal: ThermalSettings {
                rounds: 1,
                ..ThermalSettings::DEFAULT
            },
            ..WorldSettings::default()
        }
    }

    fn hash(block: &GeneratedBlock) -> u64 {
        // FNV-1a over a stride of the terrain. A full hash of 26 million cells
        // is a second of work per block and this runs sixteen times; a stride of
        // 97 is a million samples, which no realistic difference survives.
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for index in (0..EDGE * EDGE).step_by(97) {
            let Some(cell) = ErosionCell::new(index % EDGE, index / EDGE) else {
                continue;
            };
            for byte in block.terrain.get(cell).to_bits().to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    /// Order independence, over a 2x2 area and few rounds.
    ///
    /// The full 4x4 that M2's exit criterion names lives in
    /// `tests/block_pipeline.rs`, because sixteen blocks generated twice is
    /// minutes of work and this runs on every `cargo test`. The *property* does
    /// not depend on area or on round count — it depends on nothing in the
    /// pipeline reading outside its arguments — so a small case tests the same
    /// thing and the large one confirms it at the size the criterion states.
    #[test]
    fn a_block_generates_the_same_whatever_ran_before_it() {
        let settings = fast();

        let target = BlockCoord::new(1, 1);
        let alone = hash(&generate_block(SEED, target, settings));

        // Generate the rest of a 2x2 first, plus an unrelated block, so anything
        // caching state between calls has had every chance to.
        for block in [
            BlockCoord::new(0, 0),
            BlockCoord::new(1, 0),
            BlockCoord::new(0, 1),
            BlockCoord::new(-9, 9),
        ] {
            let _ = generate_block(SEED, block, settings);
        }
        let after_neighbours = hash(&generate_block(SEED, target, settings));

        assert_eq!(
            alone, after_neighbours,
            "generating neighbours first changed this block, so generation is \
             not positional"
        );
    }

    /// Distinct blocks are distinct.
    ///
    /// Without this, the test above would pass against a generator that returned
    /// the same terrain everywhere.
    #[test]
    fn neighbouring_blocks_are_not_copies_of_each_other() {
        let settings = fast();
        let a = hash(&generate_block(SEED, BlockCoord::new(0, 0), settings));
        let b = hash(&generate_block(SEED, BlockCoord::new(1, 0), settings));
        let c = hash(&generate_block(SEED, BlockCoord::new(0, 1), settings));

        assert_ne!(a, b, "neighbours along X are identical");
        assert_ne!(a, c, "neighbours along Z are identical");
    }

    /// A different seed is a different world.
    #[test]
    fn the_seed_changes_the_world() {
        let settings = fast();
        let block = BlockCoord::new(2, 2);
        assert_ne!(
            hash(&generate_block(1, block, settings)),
            hash(&generate_block(2, block, settings))
        );
    }

    /// **S07's `no-erosion` profile is a valid world.**
    ///
    /// Every stage still runs — a world without erosion still needs drainage —
    /// so this checks the stages are no-ops rather than an untaken branch.
    #[test]
    fn the_no_erosion_profile_produces_a_valid_world() {
        let block = generate_block(SEED, BlockCoord::new(1, 1), WorldSettings::NO_EROSION);

        assert_eq!(block.report.erosion.rounds, 0);
        assert_eq!(block.report.thermal.rounds, 0);
        assert_eq!(block.report.carve.channel_cells, 0);

        // Still drained: the fill runs regardless, which is the point.
        assert_eq!(
            block.network.interior_sinks(),
            0,
            "a no-erosion world must still drain"
        );

        // And it is terrain, not a flat plane.
        let (low, high) = block.terrain.core_range();
        assert!(
            high - low > 50.0,
            "no-erosion terrain spans only {:.1} m",
            high - low
        );
    }

    /// **Water depth is computable**, which is what keeping `ground` is for.
    ///
    /// Before this, the pipeline returned only the filled surface and the
    /// information was gone — S07 step 7's water bodies would have needed a whole
    /// second fill to recover something that had just been computed and thrown
    /// away.
    #[test]
    fn standing_water_is_recoverable_from_the_two_surfaces() {
        let block = generate_block(SEED, BlockCoord::new(0, 0), fast());

        let mut wet = 0usize;
        let mut deepest = 0.0f32;
        let mut dry_but_negative = 0usize;

        for index in (0..EDGE * EDGE).step_by(31) {
            let Some(cell) = ErosionCell::new(index % EDGE, index / EDGE) else {
                continue;
            };
            let depth = block.water_depth(cell);
            if depth > 0.01 {
                wet += 1;
                deepest = deepest.max(depth);
            }
            // The fill never lowers ground, so depth is never negative before
            // the clamp. A negative here would mean the two surfaces had been
            // swapped.
            if block.terrain.get(cell) < block.ground.get(cell) - 0.001 {
                dry_but_negative += 1;
            }
        }

        assert_eq!(
            dry_but_negative, 0,
            "the filled surface is below the ground surface somewhere, so the \
             two are the wrong way round"
        );
        assert!(
            wet > 0,
            "no standing water anywhere on a block, which a third of a block \
             being closed basin makes implausible"
        );
        assert!(
            deepest > 0.5,
            "the deepest water is {deepest} m, which is a puddle not a lake"
        );
    }
}
