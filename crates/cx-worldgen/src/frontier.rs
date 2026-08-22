//! The generation frontier: which blocks to want, and in what order.
//!
//! Pure arithmetic, deliberately separated from the pool that acts on it. The
//! pool answers "how do blocks get made in the background"; this answers "which
//! blocks, first" — and keeping the second question a pure function of camera
//! state makes it testable without threads, timers, or a running app.
//!
//! # Looking ahead, not just around
//!
//! A frontier centred on where the camera *is* falls behind the moment the
//! camera moves: by the time a block finishes generating, the camera is closer
//! to the far edge of it. So the want-list is centred on where the camera **will
//! be** — position plus velocity times a lead time sized to how long a block
//! takes to make. Standing still, that is just "around me"; at speed, the
//! frontier stretches out ahead like headlights.
//!
//! # The one block that is never optional
//!
//! Wherever the camera currently stands comes first in every list, regardless
//! of velocity. A frontier that chased the look-ahead point so hard it dropped
//! the ground underfoot would be optimising exactly the wrong thing.

use cx_core::math::{BLOCK_SIZE, BlockCoord};

/// How the frontier sizes and aims itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrontierSettings {
    /// Seconds of travel to look ahead — roughly how long a block takes to
    /// make on the machine at hand. Cached blocks load in a few seconds;
    /// fresh ones take tens. Sizing this for the slow case is what "the
    /// player cannot outrun the frontier" (S07) means in practice.
    pub lead_seconds: f32,
    /// Blocks of radius to keep generated around the look-ahead point,
    /// whatever the speed. One means the 3x3 neighbourhood.
    pub radius_blocks: i32,
    /// Most blocks a single want-list may name. Caps the work a fast camera
    /// can queue; the nearest ones win.
    pub max_blocks: usize,
}

impl FrontierSettings {
    /// Defaults sized for ~44 s generation and current travel speeds.
    pub const DEFAULT: Self = Self {
        // 100 s of lead, not 50: a block takes ~24 s to make and the pool is
        // single-worker, so a request can wait a full block behind the one in
        // flight. Fifty seconds of lead lost that race exactly once per entry
        // into cold terrain at 200 m/s — measured, one ~25 s hole in the
        // ground per transition; a hundred covers request latency plus a
        // queued block with margin to spare.
        lead_seconds: 100.0,
        radius_blocks: 1,
        max_blocks: 24,
    };
}

impl Default for FrontierSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The blocks to have generated, highest priority first.
///
/// `position` and `velocity` are world metres and metres per second on the
/// ground plane. The camera's own block is always first; after it, blocks
/// sort by distance from the look-ahead point, with a fixed coordinate
/// tie-break so equal distances order the same way every call (`ADR-0004` —
/// the pool must see the same priorities on every machine).
pub fn wanted_blocks(
    position: (f32, f32),
    velocity: (f32, f32),
    settings: FrontierSettings,
) -> Vec<BlockCoord> {
    let here = block_containing(position.0, position.1);

    let ahead = (
        position.0 + velocity.0 * settings.lead_seconds,
        position.1 + velocity.1 * settings.lead_seconds,
    );
    let centre = block_containing(ahead.0, ahead.1);

    // Candidates: everything within the radius of the look-ahead centre, plus
    // everything along the straight line from here to there — at speed the
    // look-ahead point can be several blocks away, and skipping the middle
    // would generate the destination while the camera falls into the gap.
    let mut candidates: Vec<BlockCoord> = Vec::new();
    for dz in -settings.radius_blocks..=settings.radius_blocks {
        for dx in -settings.radius_blocks..=settings.radius_blocks {
            candidates.push(BlockCoord::new(centre.x + dx, centre.z + dz));
        }
    }
    let span = (centre.x - here.x).abs().max((centre.z - here.z).abs());
    for step in 0..=span {
        let t = if span == 0 {
            0.0
        } else {
            step as f32 / span as f32
        };
        let x = position.0 + (ahead.0 - position.0) * t;
        let z = position.1 + (ahead.1 - position.1) * t;
        candidates.push(block_containing(x, z));
    }

    // Priority follows the *path*, not the destination. Sorting by distance
    // to the look-ahead point built the far end of the journey first — at
    // 200 m/s with a 100 s lead the pool was generating terrain twenty
    // kilometres ahead while the camera flew off the edge of the ground in
    // front of it, measured as a hole under the camera every frame. So:
    // nearest the travel segment first (the line from the camera to the
    // look-ahead point), and along equal offsets, nearest the *camera* —
    // build the road, in driving order, before any of the scenery. Standing
    // still the segment is a point and this is exactly the old
    // nearest-the-camera order. Coordinates break ties so equal distances
    // order the same way every call (`ADR-0004` — the pool must see the same
    // priorities on every machine).
    let key = |block: &BlockCoord| {
        let centre_x = (block.x as f32 + 0.5) * BLOCK_SIZE;
        let centre_z = (block.z as f32 + 0.5) * BLOCK_SIZE;

        let seg_x = ahead.0 - position.0;
        let seg_z = ahead.1 - position.1;
        let to_x = centre_x - position.0;
        let to_z = centre_z - position.1;
        let length_squared = seg_x * seg_x + seg_z * seg_z;
        let t = if length_squared > 0.0 {
            ((to_x * seg_x + to_z * seg_z) / length_squared).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let off_x = to_x - seg_x * t;
        let off_z = to_z - seg_z * t;

        // Squared distances as ordered bits: exact, no float comparison quirks.
        (
            f64::from(off_x * off_x + off_z * off_z).to_bits(),
            f64::from(to_x * to_x + to_z * to_z).to_bits(),
            block.x,
            block.z,
        )
    };

    candidates.sort_by_key(key);
    candidates.dedup();

    // The ground underfoot leads, whatever the sort said.
    candidates.retain(|block| *block != here);
    let mut wanted = vec![here];
    wanted.extend(candidates);
    wanted.truncate(settings.max_blocks.max(1));
    wanted
}

/// The block containing a world position, floor-divided so negative
/// coordinates land in negative blocks rather than sharing block zero.
fn block_containing(x: f32, z: f32) -> BlockCoord {
    BlockCoord::new(
        (x / BLOCK_SIZE).floor() as i32,
        (z / BLOCK_SIZE).floor() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: FrontierSettings = FrontierSettings::DEFAULT;

    #[test]
    fn the_ground_underfoot_always_comes_first() {
        // Even at absurd speed away from it.
        let wanted = wanted_blocks((100.0, 100.0), (500.0, 0.0), SETTINGS);
        assert_eq!(wanted.first(), Some(&BlockCoord::new(0, 0)));
    }

    #[test]
    fn standing_still_wants_the_neighbourhood_nearest_first() {
        // Centre of block (0,0).
        let mid = cx_core::math::BLOCK_SIZE / 2.0;
        let wanted = wanted_blocks((mid, mid), (0.0, 0.0), SETTINGS);

        assert_eq!(wanted.first(), Some(&BlockCoord::new(0, 0)));
        // The 3x3 neighbourhood is all present.
        for dz in -1..=1 {
            for dx in -1..=1 {
                assert!(
                    wanted.contains(&BlockCoord::new(dx, dz)),
                    "missing neighbour ({dx}, {dz})"
                );
            }
        }
    }

    #[test]
    fn moving_east_pulls_the_frontier_east() {
        let mid = cx_core::math::BLOCK_SIZE / 2.0;
        // 200 m/s east, the exit criterion's speed: look-ahead reaches blocks
        // to the east, and nothing west of the camera should outrank them.
        let wanted = wanted_blocks((mid, mid), (200.0, 0.0), SETTINGS);

        let east_most = wanted.iter().map(|b| b.x).max().unwrap_or(0);
        assert!(
            east_most >= 1,
            "at 200 m/s east the frontier never looked east"
        );

        let position_of = |x: i32| wanted.iter().position(|b| *b == BlockCoord::new(x, 0));
        if let (Some(east), Some(west)) = (position_of(1), position_of(-1)) {
            assert!(east < west, "the block ahead ranks behind the block behind");
        }
    }

    #[test]
    fn fast_travel_fills_the_line_not_just_the_destination() {
        let mid = cx_core::math::BLOCK_SIZE / 2.0;
        // Fast enough that the look-ahead point is ~3 blocks east: every block
        // between here and there must be wanted, or the camera crosses ground
        // that was never generated on its way to ground that was.
        let speed = cx_core::math::BLOCK_SIZE * 3.0 / SETTINGS.lead_seconds;
        let wanted = wanted_blocks((mid, mid), (speed, 0.0), SETTINGS);

        for x in 0..=3 {
            assert!(
                wanted.contains(&BlockCoord::new(x, 0)),
                "block ({x}, 0) on the travel line is not wanted"
            );
        }
    }

    #[test]
    fn the_road_is_built_in_driving_order_before_the_scenery() {
        let mid = cx_core::math::BLOCK_SIZE / 2.0;
        // Look-ahead ~4 blocks east. The pool has one worker; if the
        // destination outranks the block directly ahead, the camera flies off
        // the edge of the generated world while its far future gets built —
        // which is exactly what the first ordering did, measured as a hole
        // under the camera every frame of a cold 200 m/s run.
        let speed = cx_core::math::BLOCK_SIZE * 4.0 / SETTINGS.lead_seconds;
        let wanted = wanted_blocks((mid, mid), (speed, 0.0), SETTINGS);

        let rank = |x: i32, z: i32| {
            wanted
                .iter()
                .position(|b| *b == BlockCoord::new(x, z))
                .unwrap_or(usize::MAX)
        };
        for x in 0..3 {
            assert!(
                rank(x, 0) < rank(x + 1, 0),
                "line block ({x}, 0) should be built before ({}, 0)",
                x + 1
            );
        }
        assert!(
            rank(1, 0) < rank(4, 1),
            "the next block on the road must outrank the destination's scenery"
        );
    }

    #[test]
    fn the_list_is_capped_and_deterministic() {
        let wanted = wanted_blocks((123.0, -456.0), (33.0, -71.0), SETTINGS);
        assert!(wanted.len() <= SETTINGS.max_blocks);

        let again = wanted_blocks((123.0, -456.0), (33.0, -71.0), SETTINGS);
        assert_eq!(wanted, again, "the same state produced a different list");

        // No duplicates: the pool treats the list as priorities, and a
        // duplicate would be a block racing itself.
        let mut sorted = wanted.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), wanted.len());
    }

    #[test]
    fn negative_positions_land_in_negative_blocks() {
        let wanted = wanted_blocks((-10.0, -10.0), (0.0, 0.0), SETTINGS);
        assert_eq!(wanted.first(), Some(&BlockCoord::new(-1, -1)));
    }
}
