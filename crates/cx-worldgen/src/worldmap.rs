//! The world map: continental structure under the block pipeline (S07).
//!
//! M2's first deliverable, and it was built out of order for a reason worth
//! stating. Erosion on bare fractal noise produced terrain that looked like a
//! circuit board — a herringbone over every hillside, channels in hard
//! 45-degree runs. Three candidate fixes inside the erosion stages were tried
//! and all three failed. The cause is that **scale-free noise does not drain**:
//! with no regional gradient, roughly a third of every block is closed basin,
//! and the geometric gradient that flat resolution gives those basins is what
//! erosion carves.
//!
//! A regional tilt removed the artifact completely. This is where that tilt
//! comes from, in a form that works in every direction rather than one.
//!
//! # What "map" means here, and what it does not
//!
//! Not a stored grid. The world is effectively infinite, so there is no finite
//! array to hold and no global drainage network to route across it — a routed
//! network needs a boundary, and there is not one.
//!
//! Instead this is **very long wavelength positional noise**: the same technique
//! as base elevation, an order of magnitude coarser and an order of magnitude
//! taller. Continents tens of kilometres across, relief measured in hundreds of
//! metres. It is a pure function of `(seed, position)` like everything else in
//! this crate (`ADR-0006`), so it needs no storage, no seams, and no bounds.
//!
//! Global drainage in S07's sense — every river reaching a named ocean — is not
//! what this provides and is not what the pipeline needs. What the pipeline
//! needs is that **a block has somewhere downhill to send its water**, and a
//! regional gradient several times larger than block-scale relief supplies that
//! everywhere at once.
//!
//! # Why the amplitude matters more than the shape
//!
//! Basins form where local relief overwhelms the regional gradient. Base terrain
//! has 120 m of relief across a block; for the regional slope to dominate it has
//! to fall by appreciably more than that over the same 8 km. That sets the
//! relationship between [`WorldMapSettings::relief`] and
//! [`WorldMapSettings::wavelength`], and it is the one thing here that cannot be
//! tuned by eye — [`WorldMapSettings::typical_gradient`] computes it so the
//! trade is visible rather than implicit.

use cx_core::hash::{mix64, unit_f32};

/// Which quantity a hash is for, so the world map and base elevation at the same
/// place are uncorrelated rather than scaled copies of each other.
const FIELD_CONTINENT: u32 = 2;

/// How continental structure is shaped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldMapSettings {
    /// Metres between the lowest and highest continental elevation.
    pub relief: f32,
    /// Elevation the continents vary around, in metres. Sea level is zero, so a
    /// positive value here puts most of the world above water.
    pub base: f32,
    /// Horizontal size of the largest continental features, in metres.
    pub wavelength: f32,
    /// Octaves. Few, deliberately: this is the shape a block sits on, and detail
    /// here is detail base elevation would supply better and cheaper.
    pub octaves: u32,
    /// Amplitude each octave keeps from the last.
    pub persistence: f32,
}

impl Default for WorldMapSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl WorldMapSettings {
    /// The default, as a constant so callers can stay `const`.
    ///
    /// Duplicating this as both a `const` and a `Default` impl is not ideal, but
    /// `Default::default` cannot be called from a `const fn`, and
    /// [`crate::ElevationGenerator::new`] is const because every caller in the
    /// pipeline builds one.
    pub const DEFAULT: Self = Self {
        // 1,400 m over 64 km is a typical gradient near 44 m/km — comfortably
        // above the 40 m/km that was measured to remove the basin artifact,
        // and above base terrain's 120 m of block-scale relief by enough that
        // the regional slope decides where water goes.
        relief: 1_400.0,
        base: 220.0,
        wavelength: 64_000.0,
        octaves: 3,
        persistence: 0.45,
    };

    /// A world with no continental structure. Base elevation only.
    ///
    /// The shape everything looked like before this module existed, kept so the
    /// difference stays demonstrable rather than becoming a claim in a comment.
    pub const FLAT: Self = Self {
        relief: 0.0,
        base: 0.0,
        wavelength: 64_000.0,
        octaves: 1,
        persistence: 0.5,
    };

    /// Roughly how steeply the continental surface falls, in metres per
    /// kilometre.
    ///
    /// Half a wavelength is the distance from a ridge to the trough beside it,
    /// so the relief divided by that is the slope between them. Approximate on
    /// purpose — the point is the order of magnitude, which is what decides
    /// whether a block drains.
    ///
    /// **This is the number to look at when tuning.** Below base terrain's own
    /// relief across a block, the regional slope stops deciding where water goes
    /// and blocks start ponding again.
    pub fn typical_gradient(&self) -> f32 {
        if self.wavelength <= 0.0 {
            return 0.0;
        }
        self.relief / (self.wavelength / 2.0) * 1_000.0
    }
}

/// Continental elevation, as a pure function of position.
#[derive(Debug, Clone, Copy)]
pub struct WorldMap {
    seed: u64,
    settings: WorldMapSettings,
}

impl WorldMap {
    /// A world map for a seed.
    pub const fn new(seed: u64, settings: WorldMapSettings) -> Self {
        Self { seed, settings }
    }

    /// The settings this was built with.
    pub const fn settings(&self) -> WorldMapSettings {
        self.settings
    }

    /// Continental elevation in metres at an absolute world position.
    ///
    /// Added to base elevation rather than multiplied into it: a block should sit
    /// *on* the continental surface with its own relief intact, not have its
    /// relief scaled by where it happens to be. Multiplying would make lowlands
    /// flat and highlands jagged, which is a real thing about real terrain but
    /// not one that belongs here — it is a job for erodibility varying by rock
    /// type, which is content rather than shape.
    pub fn elevation_at(&self, x: f32, z: f32) -> f32 {
        if self.settings.octaves == 0 || self.settings.relief == 0.0 {
            return self.settings.base;
        }

        let mut total = 0.0;
        let mut amplitude = 1.0;
        let mut normaliser = 0.0;
        let mut wavelength = self.settings.wavelength.max(1.0);

        for octave in 0..self.settings.octaves {
            total += amplitude * self.octave(x / wavelength, z / wavelength, octave);
            normaliser += amplitude;
            amplitude *= self.settings.persistence;
            wavelength *= 0.5;
        }

        let unit = if normaliser > 0.0 {
            total / normaliser
        } else {
            0.5
        };

        self.settings.base + (unit - 0.5) * self.settings.relief
    }

    /// Tectonic uplift rate at a position, in `0..=1`.
    ///
    /// Nothing consumes this yet. It is here because stream power's full form is
    /// `dz/dt = U - K·A^m·S^n`, and without the `U` term a landscape only ever
    /// wears down — mountains cannot be sustained, and a long enough run flattens
    /// everything to its outlets. Erosion currently runs for few enough rounds
    /// that it does not matter; when it does not, this is what the term reads.
    ///
    /// Derived from the continental surface rather than hashed separately, so
    /// uplift is high where the land is high. That is the causality backwards —
    /// real elevation is high *because* uplift is — but the correlation is what
    /// matters for the shape, and inverting it would need a tectonic model this
    /// project has no use for.
    pub fn uplift_at(&self, x: f32, z: f32) -> f32 {
        if self.settings.relief <= 0.0 {
            return 0.0;
        }
        let above = self.elevation_at(x, z) - self.settings.base;
        (above / self.settings.relief * 2.0).clamp(0.0, 1.0)
    }

    /// One octave of value noise, in `0..1`.
    fn octave(&self, x: f32, z: f32, octave: u32) -> f32 {
        let x0 = x.floor();
        let z0 = z.floor();

        let tx = smoothstep(x - x0);
        let tz = smoothstep(z - z0);

        let corner = |ix: f32, iz: f32| self.lattice(ix as i32, iz as i32, octave);

        let top = lerp(corner(x0, z0), corner(x0 + 1.0, z0), tx);
        let bottom = lerp(corner(x0, z0 + 1.0), corner(x0 + 1.0, z0 + 1.0), tx);
        lerp(top, bottom, tz)
    }

    fn lattice(&self, x: i32, z: i32, octave: u32) -> f32 {
        let packed = ((x as u32 as u64) << 32) | (z as u32 as u64);
        let mut hash = mix64(self.seed ^ u64::from(FIELD_CONTINENT));
        hash = cx_core::hash::combine(hash, packed);
        hash = cx_core::hash::combine(hash, u64::from(octave));
        unit_f32(hash)
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::BLOCK_SIZE;

    const SEED: u64 = 0x0BADC0DE;

    fn map() -> WorldMap {
        WorldMap::new(SEED, WorldMapSettings::default())
    }

    /// **The number the whole module exists to produce.**
    ///
    /// A block's own relief is 120 m over 8 km. For the regional slope to decide
    /// where water goes rather than local noise, the continental surface has to
    /// fall by appreciably more than that over the same distance. A 40 m/km tilt
    /// was measured to remove the basin artifact entirely, so the default must
    /// clear it.
    #[test]
    fn the_default_gradient_clears_what_removed_the_artifact() {
        let gradient = WorldMapSettings::default().typical_gradient();

        assert!(
            gradient >= 40.0,
            "the continental gradient is {gradient:.1} m/km, below the 40 m/km \
             that was measured to remove the basin artifact — blocks will pond \
             again"
        );

        // And the drop across one block, stated in the units the comparison is
        // actually in.
        let across_a_block = gradient * BLOCK_SIZE / 1_000.0;
        assert!(
            across_a_block > 120.0 * 2.0,
            "the continental surface falls {across_a_block:.0} m across a block \
             against 120 m of block-scale relief, which is not enough for the \
             regional slope to dominate"
        );
    }

    /// `ADR-0006`: a pure function of position.
    #[test]
    fn the_same_position_always_gives_the_same_elevation() {
        let map = map();
        for (x, z) in [(0.0, 0.0), (12_345.0, -6_789.0), (1.0e6, 2.0e6)] {
            assert_eq!(map.elevation_at(x, z), map.elevation_at(x, z));
        }
    }

    #[test]
    fn a_different_seed_gives_a_different_world() {
        let a = WorldMap::new(1, WorldMapSettings::default());
        let b = WorldMap::new(2, WorldMapSettings::default());

        let differing = (0..64)
            .filter(|i| {
                let p = *i as f32 * 3_000.0;
                (a.elevation_at(p, p) - b.elevation_at(p, p)).abs() > 1.0
            })
            .count();

        assert!(differing > 50, "only {differing} of 64 samples differed");
    }

    /// The map is smooth at block scale — that is the entire point.
    ///
    /// If it varied appreciably *within* a block it would be adding relief
    /// rather than a gradient, and blocks would pond exactly as before.
    #[test]
    fn the_map_varies_between_blocks_and_not_within_one() {
        let map = map();

        // Across one block: a consistent slope, so the difference should be
        // close to the gradient times the distance rather than noise.
        let here = map.elevation_at(0.0, 0.0);
        let next_block = map.elevation_at(BLOCK_SIZE, 0.0);
        let far = map.elevation_at(BLOCK_SIZE * 8.0, 0.0);

        assert!(
            (here - next_block).abs() < (here - far).abs() * 2.0,
            "the map changes as much across one block as across eight, so it is \
             noise at block scale rather than continental structure"
        );

        // Within a block, the surface should be close to linear: sampling the
        // midpoint should land near the average of the ends.
        let midpoint = map.elevation_at(BLOCK_SIZE / 2.0, 0.0);
        let average = (here + next_block) / 2.0;
        assert!(
            (midpoint - average).abs() < 60.0,
            "the midpoint of a block is {midpoint:.0} m against an average of \
             {average:.0} m, so the map is not smooth at block scale"
        );
    }

    #[test]
    fn a_flat_map_adds_nothing() {
        let map = WorldMap::new(SEED, WorldMapSettings::FLAT);
        assert_eq!(map.elevation_at(0.0, 0.0), 0.0);
        assert_eq!(map.elevation_at(1.0e6, -1.0e6), 0.0);
        assert_eq!(map.settings().typical_gradient(), 0.0);
    }

    #[test]
    fn elevation_stays_within_the_relief_it_was_given() {
        let map = map();
        let settings = WorldMapSettings::default();

        for i in 0..2_000 {
            let x = i as f32 * 137.0;
            let z = i as f32 * -211.0;
            let height = map.elevation_at(x, z);
            assert!(
                height >= settings.base - settings.relief
                    && height <= settings.base + settings.relief,
                "({x}, {z}) is {height} m, outside the relief it was given"
            );
        }
    }

    #[test]
    fn uplift_is_high_where_the_land_is_high() {
        let map = map();
        let settings = WorldMapSettings::default();

        let mut highest = (f32::NEG_INFINITY, 0.0);
        let mut lowest = (f32::INFINITY, 0.0);

        for i in 0..2_000 {
            let x = i as f32 * 173.0;
            let height = map.elevation_at(x, 0.0);
            let uplift = map.uplift_at(x, 0.0);
            if height > highest.0 {
                highest = (height, uplift);
            }
            if height < lowest.0 {
                lowest = (height, uplift);
            }
        }

        assert!(
            highest.1 > lowest.1,
            "uplift is {} at {} m and {} at {} m",
            highest.1,
            highest.0,
            lowest.1,
            lowest.0
        );
        assert!((0.0..=1.0).contains(&map.uplift_at(0.0, 0.0)));
        let _ = settings;
    }
}
