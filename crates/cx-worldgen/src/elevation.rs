//! Base elevation: step 1 of S07's generation pipeline.
//!
//! A pure function of `(world_seed, position)`. Nothing accumulates, nothing is
//! sequential, and generating chunk (3, 1) before (0, 0) produces exactly the
//! same terrain as the reverse — which is what `ADR-0006` requires of everything
//! in this crate, and what makes the rest of the pipeline safe to parallelise
//! later.
//!
//! # What is deliberately not here
//!
//! Steps 2–9 of S07: depression fill, flow routing, hydraulic and thermal
//! erosion, channel carving, biomes, scatter. Those are **block**-granular and
//! iterative — a cell's final height depends on its neighbours over hundreds of
//! passes — and they are M2 work. This is the surface they all start from.
//!
//! Writing a plausible-looking placeholder instead would have been quicker and
//! would have made the difference invisible, which is the opposite of useful:
//! terrain from this module is smooth because it has not been eroded yet, and
//! that should be obvious when you look at it.
//!
//! # Value noise, not gradient noise
//!
//! Value noise interpolated from a positional hash, rather than Perlin or
//! simplex. It needs no permutation table, no gradient vectors, and no
//! initialisation — so it is a pure function of the seed in the strictest sense,
//! with nothing to get out of sync between a generation run and a later
//! regeneration of the same block. The visual difference is invisible under
//! erosion, which is the only place it would matter.

use cx_core::hash::{mix64, unit_f32};
use cx_core::math::{CELL_SIZE, CHUNK_SIZE, ChunkCoord};

/// Which quantity a hash is for, so elevation and later fields at the same cell
/// are uncorrelated (`cx_core::hash::hash_position`'s `field` parameter).
const FIELD_ELEVATION: u32 = 1;

/// How elevation is shaped.
///
/// Named parameters rather than magic numbers in the noise function: these are
/// the knobs a world preset turns, and every one of them changes what the
/// terrain *is* rather than how it is computed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerrainShape {
    /// Metres of elevation between the lowest and highest ground.
    pub relief: f32,
    /// Elevation the noise varies around, in metres.
    pub base: f32,
    /// Horizontal size of the largest features, in metres.
    pub feature_size: f32,
    /// Noise octaves. Each is half the wavelength and roughly half the
    /// amplitude of the last.
    pub octaves: u32,
    /// How much of the previous octave's amplitude each next one keeps.
    pub persistence: f32,
}

impl Default for TerrainShape {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TerrainShape {
    /// The default, as a constant so callers can stay `const`.
    pub const DEFAULT: Self = Self {
        relief: 120.0,
        base: 40.0,
        // A kilometre: large enough that a chunk is a piece of a hill rather
        // than a hill, which is what makes the seams between chunks
        // uninteresting — the point of positional generation.
        feature_size: 1_024.0,
        octaves: 5,
        persistence: 0.5,
    };
}

impl TerrainShape {
    /// No *local* relief — a block with no hills of its own.
    ///
    /// This is not by itself a flat world: the continental surface the block
    /// sits on is still there, so heights will be smooth but not level. For
    /// genuinely flat ground use [`ElevationGenerator::flat`].
    pub const fn flat(height: f32) -> Self {
        Self {
            relief: 0.0,
            base: height,
            feature_size: 1_024.0,
            octaves: 1,
            persistence: 0.5,
        }
    }
}

/// Generates base elevation.
#[derive(Debug, Clone, Copy)]
pub struct ElevationGenerator {
    seed: u64,
    shape: TerrainShape,
    /// Continental structure this terrain sits on (S07's world map).
    ///
    /// Not optional, and not cosmetic. Without a regional gradient a third of
    /// every block is closed basin, and the geometric drainage that flat
    /// resolution gives those basins is what erosion carves into a herringbone.
    /// See [`crate::worldmap`].
    world: crate::worldmap::WorldMap,
}

impl ElevationGenerator {
    /// A generator for a world.
    /// A generator for a world, with default continental structure.
    pub const fn new(seed: u64, shape: TerrainShape) -> Self {
        Self {
            seed,
            shape,
            world: crate::worldmap::WorldMap::new(seed, crate::worldmap::WorldMapSettings::DEFAULT),
        }
    }

    /// A generator with explicit continental structure.
    ///
    /// [`crate::worldmap::WorldMapSettings::FLAT`] reproduces the bare-noise
    /// terrain this crate produced before the world map existed, which is what
    /// makes the difference demonstrable rather than asserted.
    pub const fn with_world(
        seed: u64,
        shape: TerrainShape,
        settings: crate::worldmap::WorldMapSettings,
    ) -> Self {
        Self {
            seed,
            shape,
            world: crate::worldmap::WorldMap::new(seed, settings),
        }
    }

    /// A genuinely flat world at `height`.
    ///
    /// [`TerrainShape::flat`] removes a block's *local* relief; it does not
    /// remove the continental surface underneath, so on its own it produces
    /// terrain that is smooth but still hundreds of metres from level. A test
    /// fixture that asks for flat ground and silently gets a continental slope
    /// is a trap — this is the constructor that means what it says.
    pub const fn flat(seed: u64, height: f32) -> Self {
        Self::with_world(
            seed,
            TerrainShape::flat(height),
            crate::worldmap::WorldMapSettings::FLAT,
        )
    }

    /// The continental surface this terrain sits on.
    pub const fn world(&self) -> crate::worldmap::WorldMap {
        self.world
    }

    /// The seed this generator was built with.
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// The shape this generator was built with.
    pub const fn shape(&self) -> TerrainShape {
        self.shape
    }

    /// Elevation in metres at an absolute world position.
    ///
    /// Takes absolute metres rather than a chunk-local cell because noise is
    /// continuous across chunk boundaries; the caller converts, and
    /// [`ElevationGenerator::chunk_elevation`] is the convenience that does it
    /// correctly.
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        // The continental surface, plus this block's own relief on top of it.
        // Added rather than blended: a block keeps its own shape wherever it
        // sits, and only its altitude and the direction it drains change.
        let continental = self.world.elevation_at(x, z);

        if self.shape.octaves == 0 || self.shape.relief == 0.0 {
            return continental + self.shape.base;
        }

        let mut total = 0.0;
        let mut amplitude = 1.0;
        let mut normaliser = 0.0;
        let mut wavelength = self.shape.feature_size.max(CELL_SIZE);

        for octave in 0..self.shape.octaves {
            total += amplitude * self.octave(x / wavelength, z / wavelength, octave);
            normaliser += amplitude;
            amplitude *= self.shape.persistence;
            wavelength *= 0.5;
        }

        // Normalised so that changing the octave count changes the *detail* and
        // not the overall height. Without this, adding an octave raises the
        // whole world.
        let unit = if normaliser > 0.0 {
            total / normaliser
        } else {
            0.5
        };

        continental + self.shape.base + (unit - 0.5) * self.shape.relief
    }

    /// One octave of value noise at grid coordinates, in `0..1`.
    fn octave(&self, x: f32, z: f32, octave: u32) -> f32 {
        let x0 = x.floor();
        let z0 = z.floor();

        // Smoothstep on the fractional part. Linear interpolation between
        // lattice values leaves visible creases along every grid line, which
        // read as a square grid pressed into the terrain.
        let tx = smoothstep(x - x0);
        let tz = smoothstep(z - z0);

        let corner = |ix: f32, iz: f32| self.lattice(ix as i32, iz as i32, octave);

        let top = lerp(corner(x0, z0), corner(x0 + 1.0, z0), tx);
        let bottom = lerp(corner(x0, z0 + 1.0), corner(x0 + 1.0, z0 + 1.0), tx);
        lerp(top, bottom, tz)
    }

    /// The value at one lattice point, in `0..1`.
    ///
    /// Hashed from the coordinate rather than looked up, so there is no table to
    /// build and the world is unbounded in every direction.
    fn lattice(&self, x: i32, z: i32, octave: u32) -> f32 {
        let packed = ((x as u32 as u64) << 32) | (z as u32 as u64);
        let mut hash = mix64(self.seed ^ u64::from(FIELD_ELEVATION));
        hash = cx_core::hash::combine(hash, packed);
        hash = cx_core::hash::combine(hash, u64::from(octave));
        unit_f32(hash)
    }

    /// Elevation at a cell within a chunk.
    ///
    /// The conversion from chunk-local to absolute is here rather than at each
    /// call site because getting it wrong produces terrain that tiles — every
    /// chunk identical — which looks like a seed problem rather than an
    /// arithmetic one.
    pub fn chunk_elevation(&self, chunk: ChunkCoord, cell_x: u32, cell_z: u32) -> f32 {
        let origin_x = chunk.x as f32 * CHUNK_SIZE;
        let origin_z = chunk.z as f32 * CHUNK_SIZE;

        // Cell centres, not corners: a cell is an area, and sampling its corner
        // biases every chunk half a cell towards its origin.
        let x = origin_x + (cell_x as f32 + 0.5) * CELL_SIZE;
        let z = origin_z + (cell_z as f32 + 0.5) * CELL_SIZE;

        self.height_at(x, z)
    }
}

/// Hermite smoothstep on `0..1`.
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
    use cx_core::math::CELLS_PER_CHUNK_EDGE;

    fn generator() -> ElevationGenerator {
        ElevationGenerator::new(0xC0FFEE, TerrainShape::default())
    }

    #[test]
    fn the_same_position_always_gives_the_same_height() {
        // The property the whole crate rests on (ADR-0006): generation order
        // must not be observable.
        let generator = generator();
        for (x, z) in [(0.0, 0.0), (1_234.5, -9_876.25), (-0.5, 0.5)] {
            let first = generator.height_at(x, z);
            for _ in 0..5 {
                assert_eq!(
                    generator.height_at(x, z),
                    first,
                    "height at ({x}, {z}) changed between calls"
                );
            }
        }
    }

    #[test]
    fn chunks_generated_in_any_order_agree() {
        // Generating a chunk's cells forwards and backwards must produce the
        // same column of heights. A generator with any accumulated state fails
        // here, and nowhere else until a save is reloaded.
        let generator = generator();
        let chunk = ChunkCoord::new(2, -3);

        let forwards: Vec<f32> = (0..32)
            .map(|index| generator.chunk_elevation(chunk, index, index))
            .collect();
        let backwards: Vec<f32> = (0..32)
            .rev()
            .map(|index| generator.chunk_elevation(chunk, index, index))
            .collect();

        assert_eq!(forwards, backwards.into_iter().rev().collect::<Vec<f32>>());
    }

    #[test]
    fn a_different_seed_gives_different_terrain() {
        let a = ElevationGenerator::new(1, TerrainShape::default());
        let b = ElevationGenerator::new(2, TerrainShape::default());

        let differing = (0..64)
            .filter(|index| {
                let x = *index as f32 * 37.0;
                (a.height_at(x, 0.0) - b.height_at(x, 0.0)).abs() > 0.01
            })
            .count();

        assert!(
            differing > 50,
            "two seeds should produce mostly different terrain, only {differing}/64 differed"
        );
    }

    #[test]
    fn terrain_is_continuous_across_a_chunk_boundary() {
        // The bug this catches: a chunk-local coordinate used as though it were
        // absolute makes every chunk identical, with a cliff at every seam.
        let generator = generator();

        let last_of_first = generator.chunk_elevation(
            ChunkCoord::new(0, 0),
            CELLS_PER_CHUNK_EDGE - 1,
            CELLS_PER_CHUNK_EDGE / 2,
        );
        let first_of_next =
            generator.chunk_elevation(ChunkCoord::new(1, 0), 0, CELLS_PER_CHUNK_EDGE / 2);

        assert!(
            (last_of_first - first_of_next).abs() < 1.0,
            "adjacent cells across a chunk seam differ by {} m",
            (last_of_first - first_of_next).abs()
        );
    }

    #[test]
    fn adjacent_chunks_are_not_identical() {
        // The other half of the same bug: continuity is trivially satisfied by a
        // generator that returns the same value everywhere.
        let generator = generator();
        let here = generator.chunk_elevation(ChunkCoord::new(0, 0), 10, 10);
        let far = generator.chunk_elevation(ChunkCoord::new(40, -25), 10, 10);

        assert!(
            (here - far).abs() > 0.5,
            "distant chunks should differ, got {here} and {far}"
        );
    }

    /// A block's **own** relief is what its shape says, wherever it sits.
    ///
    /// Measured against the continental surface rather than against zero. The
    /// world map moves terrain up and down by hundreds of metres, and the claim
    /// worth making is that it changes a block's *altitude* and not its shape —
    /// a block on a plateau has the same 100 m of relief as one in a basin.
    /// Asserting on absolute height would just be asserting the world map's
    /// range, which `worldmap.rs` already covers.
    #[test]
    fn local_relief_is_what_the_shape_says_wherever_the_block_sits() {
        let shape = TerrainShape {
            relief: 100.0,
            base: 50.0,
            ..TerrainShape::default()
        };
        let generator = ElevationGenerator::new(7, shape);
        let world = generator.world();

        for index in 0..4_000 {
            let x = index as f32 * 13.7;
            let z = index as f32 * -7.3;

            let local = generator.height_at(x, z) - world.elevation_at(x, z);
            assert!(
                (0.0..=100.0).contains(&local),
                "local relief {local} at ({x}, {z}) escaped base ± relief/2"
            );
        }
    }

    /// And a flat generator is genuinely flat, continental surface included.
    #[test]
    fn a_flat_generator_is_level_everywhere() {
        let generator = ElevationGenerator::flat(7, 42.0);
        for index in 0..1_000 {
            let x = index as f32 * 911.0;
            assert_eq!(generator.height_at(x, -x), 42.0);
        }
    }

    #[test]
    fn the_octave_count_changes_detail_and_not_altitude() {
        // Unnormalised octave summing raises the whole world every time an
        // octave is added, which shows up as "the terrain preset broke" the
        // first time someone tunes it.
        let mean_at = |octaves: u32| {
            let generator = ElevationGenerator::new(
                99,
                TerrainShape {
                    octaves,
                    ..TerrainShape::default()
                },
            );
            let total: f32 = (0..2_000)
                .map(|index| generator.height_at(index as f32 * 11.0, index as f32 * 5.0))
                .sum();
            total / 2_000.0
        };

        let sparse = mean_at(1);
        let dense = mean_at(6);
        assert!(
            (sparse - dense).abs() < 8.0,
            "mean altitude moved from {sparse} to {dense} when octaves changed"
        );
    }

    #[test]
    fn a_flat_shape_is_actually_flat() {
        let generator = ElevationGenerator::flat(5, 12.5);
        for index in 0..100 {
            let height = generator.height_at(index as f32 * 100.0, index as f32 * -50.0);
            assert!((height - 12.5).abs() < f32::EPSILON, "got {height}");
        }
    }

    #[test]
    fn terrain_is_smooth_at_cell_scale() {
        // Neighbouring cells 0.5 m apart should not differ by metres. Noise
        // sampled at the wrong scale produces a surface no mesh or collider can
        // represent, and it is not obvious from a screenshot at a distance.
        let generator = generator();
        let mut worst: f32 = 0.0;

        for index in 0..2_000 {
            let x = index as f32 * CELL_SIZE;
            let step =
                (generator.height_at(x, 0.0) - generator.height_at(x + CELL_SIZE, 0.0)).abs();
            worst = worst.max(step);
        }

        assert!(
            worst < 2.0,
            "the steepest half-metre step is {worst} m, which no collider will like"
        );
    }
}
