//! **M2's headline exit criterion**, at the size it is stated (S07/M2).
//!
//! *"A 4x4 block area generated in two different orders produces identical field
//! hashes."*
//!
//! This is the property the entire design rests on. `ADR-0006` raised generation
//! granularity to the block so erosion could be non-local while staying
//! positional; if order mattered, the block cache would be unsound, replays would
//! diverge, and "an unmodified chunk costs zero bytes" — the claim that makes an
//! infinite world's save file finite — would be false.
//!
//! # Why it is `#[ignore]`d
//!
//! Sixteen blocks, generated twice, plus one unrelated block to give anything
//! caching state a chance to leak: **33 block generations**. Even
//! at one erosion round that is minutes, and `cargo test` runs on every commit.
//!
//! `pipeline.rs` keeps a fast 2x2 version in its unit tests, because the property
//! does not depend on area — it depends on nothing in the pipeline reading
//! outside its arguments. This confirms it at the size the criterion states.
//!
//! Run it with:
//!
//! ```text
//! cargo test -p cx-worldgen --test block_pipeline --release -- --ignored --nocapture
//! ```

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::disallowed_methods
)]

use std::time::Instant;

use cx_core::math::BlockCoord;
use cx_worldgen::block::{EDGE, ErosionCell};
use cx_worldgen::{
    ErosionSettings, GeneratedBlock, ThermalSettings, WorldSettings, generate_block,
};

const SEED: u64 = 0x0BADC0DE;

/// One erosion round rather than twelve. Order independence is a property of
/// what the code reads, not of how long it runs — see the module docs.
fn settings() -> WorldSettings {
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

/// FNV-1a over the terrain, at a stride.
///
/// A stride of 97 over 26 million cells is 270,000 samples per block. A
/// difference that survives that is a difference in fewer than one cell in
/// 270,000, which no order dependence in this pipeline could produce — every
/// stage sweeps the whole grid.
fn hash(block: &GeneratedBlock) -> u64 {
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

#[test]
#[ignore = "33 block generations; run explicitly with --ignored"]
fn a_four_by_four_area_generates_the_same_in_any_order() {
    let settings = settings();
    let started = Instant::now();

    let area: Vec<BlockCoord> = (0..4)
        .flat_map(|z| (0..4).map(move |x| BlockCoord::new(x, z)))
        .collect();
    assert_eq!(area.len(), 16);

    let forward: Vec<u64> = area
        .iter()
        .map(|block| hash(&generate_block(SEED, *block, settings)))
        .collect();

    // Backwards, with one unrelated block generated first. Reversing alone
    // would only show the pipeline does not depend on *direction*; the intruder
    // shows it does not depend on what ran before it. One intruder, not one per
    // pair: every regeneration in this pass already acts as "unrelated work"
    // for the one after it, so per-pair intruders re-proved the same claim
    // sixteen times — and pushed the CI job over its time limit doing it.
    let _ = generate_block(SEED, BlockCoord::new(-9, 9), settings);
    let mut backward: Vec<u64> = area
        .iter()
        .rev()
        .map(|block| hash(&generate_block(SEED, *block, settings)))
        .collect();
    backward.reverse();

    println!(
        "4x4 order independence: 33 block generations in {:?}",
        started.elapsed()
    );

    let differing = forward
        .iter()
        .zip(&backward)
        .zip(&area)
        .filter(|((a, b), _)| a != b)
        .map(|((_, _), block)| *block)
        .collect::<Vec<_>>();

    assert!(
        differing.is_empty(),
        "{} of 16 blocks differ when the area is generated in a different \
         order: {differing:?}. Generation is not positional, which makes the \
         block cache unsound and replays divergent.",
        differing.len()
    );

    // And the sixteen are not all the same block, which would make the check
    // above vacuous.
    let mut distinct = forward.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        16,
        "only {} of 16 blocks are distinct, so the comparison above proves \
         nothing",
        distinct.len()
    );
}
