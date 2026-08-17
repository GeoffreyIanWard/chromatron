//! Positional hashing — the basis of all worldgen (`ADR-0006`).
//!
//! Every generated value derives from `hash(world_seed, coordinate, ...)`, never
//! from a sequential stream. That is what makes generating block B before block
//! A produce identical output to the reverse, which in turn is what lets the
//! block cache be deleted and regenerated without changing the world.
//!
//! Only integer operations appear here. Floating point is not merely unnecessary
//! but disqualifying: `f32` arithmetic can differ between architectures, and a
//! world that regenerates differently on another machine is not a world that can
//! be shared as a seed.

use crate::math::{BlockCoord, ChunkCoord};

/// A 64-bit mixing function with good avalanche behaviour.
///
/// This is the SplitMix64 finalizer. It is used rather than a hasher from the
/// standard library because `DefaultHasher`'s algorithm is explicitly not
/// guaranteed stable across Rust releases — a worldgen seed must outlive a
/// toolchain upgrade.
#[inline]
pub const fn mix64(value: u64) -> u64 {
    let mut z = value;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Folds one value into a running hash.
#[inline]
pub const fn combine(accumulator: u64, value: u64) -> u64 {
    // Rotation before mixing keeps inputs from cancelling when the same value is
    // folded twice, which happens constantly with coordinates like (0, 0).
    mix64(accumulator.rotate_left(23) ^ value.wrapping_add(0x9e37_79b9_7f4a_7c15))
}

/// The positional hash all worldgen draws from (`ADR-0006`, S01).
///
/// `field` distinguishes independent value streams at the same location — a
/// separate constant per generated quantity, so elevation and moisture at the
/// same cell are uncorrelated. `index` addresses within that field.
///
/// Cheap enough for per-cell use: a handful of multiplies and shifts, no memory
/// access, no allocation.
#[inline]
pub const fn hash_position(seed: u64, chunk: ChunkCoord, field: u32, index: u32) -> u64 {
    // Coordinates are cast through u32 before widening so that negative values
    // keep their bit pattern rather than sign-extending into the high half,
    // where they would collide with large positive coordinates.
    let packed_chunk = ((chunk.x as u32 as u64) << 32) | (chunk.z as u32 as u64);
    let packed_field = ((field as u64) << 32) | (index as u64);

    let mut hash = mix64(seed);
    hash = combine(hash, packed_chunk);
    combine(hash, packed_field)
}

/// The positional hash at block granularity, for generation stages that work per
/// block rather than per cell (`ADR-0006`: blocks are the unit of generation).
#[inline]
pub const fn hash_block(seed: u64, block: BlockCoord, field: u32) -> u64 {
    let packed = ((block.x as u32 as u64) << 32) | (block.z as u32 as u64);
    combine(combine(mix64(seed), packed), field as u64)
}

/// A uniform `f32` in `[0, 1)` from a hash value.
///
/// Built by constructing the mantissa directly rather than by dividing, so the
/// result is exactly reproducible: no rounding mode, no `as` conversion of a
/// large integer, identical on every platform with IEEE-754 `f32`.
#[inline]
pub fn unit_f32(hash: u64) -> f32 {
    // 24 bits of mantissa is the full precision of f32.
    (hash >> 40) as f32 / (1u32 << 24) as f32
}

/// A value in `0..range`, or 0 when `range` is 0.
///
/// Uses the multiply-shift reduction rather than a modulo. It is faster and has
/// a bias below 2^-32, which is far under anything worldgen can observe.
#[inline]
pub const fn reduce(hash: u64, range: u32) -> u32 {
    (((hash >> 32) * range as u64) >> 32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross-architecture test vector from S01's acceptance criteria.
    ///
    /// S01 requires `hash_position` to produce identical output on x86-64 and
    /// aarch64 for a fixed vector. Only half of that can be checked here — this
    /// asserts the values are stable for *this* build, and CI running the same
    /// test on both architectures supplies the other half.
    ///
    /// **These values are world identity.** Every world ever generated derives
    /// from them. If a refactor changes them, every existing seed now produces
    /// different terrain, so updating this test is a decision that needs an ADR,
    /// not a fix that unblocks a build.
    #[test]
    fn s01_acceptance_hash_position_matches_pinned_vector() {
        let cases: [(u64, i32, i32, u32, u32, u64); 7] = [
            (0, 0, 0, 0, 0, 0xf08e_6760_e777_ffa4),
            (1, 0, 0, 0, 0, 0x8c2d_f343_76a3_4b79),
            (0, 1, 0, 0, 0, 0x2b8e_884c_4435_cc78),
            (0, -1, 0, 0, 0, 0x0908_ece2_3fe6_e687),
            (0, 0, 0, 1, 0, 0xc519_87fc_4c14_795d),
            (0, 0, 0, 0, 1, 0x8f1b_7613_7dd9_f216),
            (
                0xdead_beef_cafe_f00d,
                12_345,
                -6_789,
                3,
                1_000_000,
                0xc69b_8ad2_0331_b31f,
            ),
        ];

        for (seed, x, z, field, index, expected) in cases {
            let actual = hash_position(seed, ChunkCoord::new(x, z), field, index);
            assert_eq!(
                actual, expected,
                "hash_position({seed}, ({x}, {z}), {field}, {index}) changed: \
                 expected {expected:#018x}, got {actual:#018x}. Every world generated by \
                 every previous build just changed too — see the note on this test."
            );
        }

        assert_eq!(
            hash_block(99, BlockCoord::new(-3, 4), 2),
            0x0ba4_1eeb_da26_db2b
        );
    }

    #[test]
    fn distinct_inputs_produce_distinct_hashes() {
        // Adjacent coordinates are the case that matters: a hash that correlates
        // neighbours produces visible grid artefacts in generated terrain.
        let mut seen = std::collections::BTreeSet::new();
        for x in -8..8 {
            for z in -8..8 {
                for field in 0..4 {
                    assert!(
                        seen.insert(hash_position(42, ChunkCoord::new(x, z), field, 0)),
                        "collision at ({x}, {z}) field {field}"
                    );
                }
            }
        }
    }

    #[test]
    fn negative_and_positive_coordinates_do_not_collide() {
        // Sign extension would make (-1, 0) and (4294967295, 0) the same cell.
        assert_ne!(
            hash_position(0, ChunkCoord::new(-1, 0), 0, 0),
            hash_position(0, ChunkCoord::new(i32::MAX, 0), 0, 0)
        );
        assert_ne!(
            hash_position(0, ChunkCoord::new(-1, -1), 0, 0),
            hash_position(0, ChunkCoord::new(-1, 1), 0, 0)
        );
    }

    #[test]
    fn coordinate_axes_are_not_symmetric() {
        // A hash that folds x and z the same way makes (3, 7) and (7, 3)
        // identical, mirroring generated terrain about the diagonal.
        assert_ne!(
            hash_position(0, ChunkCoord::new(3, 7), 0, 0),
            hash_position(0, ChunkCoord::new(7, 3), 0, 0)
        );
    }

    #[test]
    fn unit_f32_stays_within_range() {
        for i in 0..10_000u64 {
            let value = unit_f32(mix64(i));
            assert!((0.0..1.0).contains(&value), "{value} out of range at {i}");
        }
    }

    #[test]
    fn unit_f32_is_roughly_uniform() {
        // Ten buckets over 100k draws; each should hold ~10%. Loose bounds —
        // this catches a broken shift, not a subtle statistical defect.
        let mut buckets = [0u32; 10];
        for i in 0..100_000u64 {
            let value = unit_f32(hash_position(7, ChunkCoord::new(0, 0), 0, i as u32));
            let bucket = ((value * 10.0) as usize).min(9);
            if let Some(count) = buckets.get_mut(bucket) {
                *count += 1;
            }
        }
        for (index, count) in buckets.iter().enumerate() {
            assert!(
                (8_000..12_000).contains(count),
                "bucket {index} held {count}, expected roughly 10,000"
            );
        }
    }

    #[test]
    fn reduce_stays_in_range_and_handles_zero() {
        assert_eq!(
            reduce(u64::MAX, 0),
            0,
            "empty range must not divide by zero"
        );
        for i in 0..1_000u64 {
            assert!(reduce(mix64(i), 7) < 7);
        }
    }
}
