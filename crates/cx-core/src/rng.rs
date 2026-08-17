//! Per-system random number streams.
//!
//! There is no global RNG anywhere in this engine, and adding one would break
//! determinism in a way that is very hard to find later. The reason is
//! ordering: a global generator makes every system's output depend on how many
//! draws every *other* system made first, so a change in scheduling — or a
//! parallel run — silently changes results.
//!
//! Instead each system constructs its own stream from
//! `(world_seed, StreamId, tick)`. Two systems drawing on the same tick cannot
//! influence each other, and a stream can be reconstructed exactly when
//! replaying a bug (`ADR-0004`).
//!
//! One rule this cannot enforce mechanically, from `03-conventions.md`: no
//! system may consume a *variable* number of draws based on data another system
//! could reorder.

use crate::hash::{combine, mix64, reduce, unit_f32};
use crate::time::Tick;

/// Identifies which system a stream belongs to.
///
/// An enum rather than a string or an integer so that two systems cannot
/// accidentally share a stream, and so that adding a new consumer of randomness
/// is a visible change in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum StreamId {
    /// Terrain generation: base elevation, ridges, warping.
    Terrain,
    /// Hydraulic and thermal erosion, at generation time only (`ADR-0008`).
    Erosion,
    /// Biome assignment and boundary jitter.
    Biome,
    /// Scatter placement: trees, rocks, detail props.
    Scatter,
    /// Climate variation and weather events.
    Climate,
    /// Ecology: growth, spread, mortality.
    Ecology,
    /// Agent decision noise and behaviour tie-breaking.
    AgentDecision,
    /// Agent spawning and population composition.
    AgentSpawn,
    /// Physics jitter and contact tie-breaking.
    Physics,
    /// Reserved for tests and benchmarks; never used by shipped systems.
    Test,
}

impl StreamId {
    /// A stable numeric tag, independent of declaration order.
    ///
    /// Explicit constants rather than `as u32` on the enum: reordering variants
    /// must never change generated worlds, and a derived discriminant would let
    /// exactly that happen during an innocuous tidy-up.
    pub const fn tag(self) -> u32 {
        match self {
            StreamId::Terrain => 0x0001,
            StreamId::Erosion => 0x0002,
            StreamId::Biome => 0x0003,
            StreamId::Scatter => 0x0004,
            StreamId::Climate => 0x0005,
            StreamId::Ecology => 0x0006,
            StreamId::AgentDecision => 0x0007,
            StreamId::AgentSpawn => 0x0008,
            StreamId::Physics => 0x0009,
            StreamId::Test => 0xffff,
        }
    }
}

/// A reproducible stream of pseudorandom values.
///
/// SplitMix64: fast, small state, and adequate for simulation noise. Not
/// cryptographic, and nothing here should be used for anything that needs to be
/// unpredictable rather than merely varied.
#[derive(Debug, Clone)]
pub struct RngStream {
    state: u64,
}

impl RngStream {
    /// A stream for one system on one tick.
    ///
    /// The same three arguments always produce the same sequence — that is the
    /// entire point, and it is what makes a replay reproduce a bug.
    pub fn new(world_seed: u64, stream: StreamId, tick: Tick) -> Self {
        let mut state = mix64(world_seed);
        state = combine(state, stream.tag() as u64);
        state = combine(state, tick.0);
        Self { state }
    }

    /// A stream seeded by position rather than by tick, for generation work
    /// (`ADR-0006`).
    ///
    /// Generation must not depend on when or in what order it runs, so a
    /// generation-time stream takes a positional hash instead of a clock.
    pub fn from_hash(hash: u64) -> Self {
        Self { state: mix64(hash) }
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix64(self.state)
    }

    /// The next 32 bits.
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// A uniform `f32` in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        unit_f32(self.next_u64())
    }

    /// A uniform value in `0..range`, or 0 when `range` is 0.
    ///
    /// Note that this consumes exactly one draw regardless of `range`. A
    /// rejection-sampling implementation would consume a variable number, which
    /// is precisely the pattern `03-conventions.md` bans.
    pub fn next_range(&mut self, range: u32) -> u32 {
        reduce(self.next_u64(), range)
    }

    /// A uniform `f32` in `[low, high)`.
    pub fn next_range_f32(&mut self, low: f32, high: f32) -> f32 {
        low + self.next_f32() * (high - low)
    }

    /// `true` with the given probability.
    pub fn chance(&mut self, probability: f32) -> bool {
        self.next_f32() < probability
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s01_acceptance_identical_construction_yields_identical_sequences() {
        let draw = |()| {
            let mut rng = RngStream::new(12345, StreamId::Ecology, Tick(99));
            (0..64).map(|_| rng.next_u64()).collect::<Vec<_>>()
        };
        assert_eq!(draw(()), draw(()));
    }

    #[test]
    fn s01_acceptance_streams_are_uncorrelated() {
        // Chi-squared over the joint distribution of two streams drawing on the
        // same seed and tick. If the streams were correlated — the failure mode
        // where a stream id barely perturbs the state — pairs would cluster on
        // the diagonal and the statistic would blow up.
        const DRAWS: usize = 1_000_000;
        const BUCKETS: usize = 8;

        let mut a = RngStream::new(7, StreamId::Terrain, Tick(1));
        let mut b = RngStream::new(7, StreamId::Erosion, Tick(1));

        let mut joint = [[0u32; BUCKETS]; BUCKETS];
        for _ in 0..DRAWS {
            let i = a.next_range(BUCKETS as u32) as usize;
            let j = b.next_range(BUCKETS as u32) as usize;
            if let Some(row) = joint.get_mut(i)
                && let Some(cell) = row.get_mut(j)
            {
                *cell += 1;
            }
        }

        let expected = DRAWS as f64 / (BUCKETS * BUCKETS) as f64;
        let chi_squared: f64 = joint
            .iter()
            .flatten()
            .map(|&observed| {
                let diff = observed as f64 - expected;
                diff * diff / expected
            })
            .sum();

        // 63 degrees of freedom; the 99.9th percentile is about 103. A value
        // far above that means the streams are not independent.
        assert!(
            chi_squared < 103.0,
            "chi-squared {chi_squared:.1} over {DRAWS} draws suggests the streams are correlated"
        );
    }

    #[test]
    fn different_ticks_produce_different_sequences() {
        let first = RngStream::new(1, StreamId::Climate, Tick(1)).next_u64();
        let second = RngStream::new(1, StreamId::Climate, Tick(2)).next_u64();
        assert_ne!(first, second);
    }

    #[test]
    fn stream_tags_are_stable_and_unique() {
        // Reordering the enum must not change any generated world.
        let ids = [
            StreamId::Terrain,
            StreamId::Erosion,
            StreamId::Biome,
            StreamId::Scatter,
            StreamId::Climate,
            StreamId::Ecology,
            StreamId::AgentDecision,
            StreamId::AgentSpawn,
            StreamId::Physics,
            StreamId::Test,
        ];
        let tags: std::collections::BTreeSet<u32> = ids.iter().map(|id| id.tag()).collect();
        assert_eq!(tags.len(), ids.len(), "stream tags must be unique");
        assert_eq!(
            StreamId::Terrain.tag(),
            0x0001,
            "tags are part of world identity"
        );
    }

    #[test]
    fn range_draws_consume_exactly_one_step_regardless_of_range() {
        // A variable number of draws per call is the determinism hazard this
        // implementation avoids; this pins the property.
        let mut narrow = RngStream::new(3, StreamId::Test, Tick(0));
        let mut wide = RngStream::new(3, StreamId::Test, Tick(0));

        let _ = narrow.next_range(2);
        let _ = wide.next_range(1_000_000);

        assert_eq!(
            narrow.next_u64(),
            wide.next_u64(),
            "both should have advanced once"
        );
    }

    #[test]
    fn chance_respects_its_probability() {
        let mut rng = RngStream::new(11, StreamId::Test, Tick(0));
        let hits = (0..100_000).filter(|_| rng.chance(0.25)).count();
        assert!(
            (24_000..26_000).contains(&hits),
            "got {hits} hits, expected about 25,000"
        );
    }
}
