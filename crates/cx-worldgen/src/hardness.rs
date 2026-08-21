//! Rock hardness: how easily the ground gives way to water, varied by place.
//!
//! Before this existed, one erodibility constant applied to the entire world,
//! and every stretch of every river carved at the same rate. The result looked
//! believable in the small and monotonous in the large — uniform valleys,
//! uniform channels, no surprises. Real landscapes get their variety in large
//! part from *what the water is cutting through*: soft rock opens into wide
//! valleys, hard rock resists and stands as ridges, cliffs, and the knickpoints
//! that become rapids and waterfalls.
//!
//! This module supplies that variation as a per-cell multiplier on erodibility.
//! It is not a full material system — no named rock types, no per-material
//! talus angles yet — just the one number erosion actually consumes, varied
//! smoothly by position. If a richer material model arrives later (sandstone,
//! granite, clay as content-defined types), it replaces the *source* of this
//! multiplier without changing how erosion uses it.
//!
//! # Same rules as every other generation input
//!
//! Hardness is positional noise: a pure function of `(seed, world position)`,
//! like base elevation and the world map (`ADR-0006`). Two neighbouring blocks
//! sample the same hardness at the same ground, so a hard band crossing a block
//! seam stays continuous. A separate noise field id keeps hardness uncorrelated
//! with elevation — soft rock on a mountaintop and hard rock in a valley are
//! both allowed, which is where perched lakes and waterfall steps will
//! eventually come from.
//!
//! # Stored as one byte per cell
//!
//! The map is sampled once per block and kept, because erosion reads it every
//! round. A byte per cell is 26 MB against the block budget's remaining
//! headroom; storing it as `f32` would be 105 MB and push the working set over
//! budget, and a 1/255 step in hardness is far below anything the eye can see
//! in the output. The byte-to-multiplier conversion goes through a 256-entry
//! table built once per erosion run, so the per-cell cost in the hot loop is an
//! array lookup rather than a `powf`.

use cx_core::hash::{combine, mix64, unit_f32};

use crate::block::{BlockCoordinates, CELLS, EDGE, ErosionCell};

/// Which quantity the hash is for, so hardness is uncorrelated with elevation
/// (field 1) and the continental map (field 2) at the same position.
const FIELD_HARDNESS: u32 = 3;

/// Octaves of hardness noise. Two is deliberate: hardness should read as
/// regions and bands, not as texture. Fine detail in *hardness* would fight the
/// erosion detail it is supposed to shape.
const OCTAVES: u32 = 2;

/// How rock hardness varies across a block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HardnessSettings {
    /// Size of the largest soft/hard regions, in metres.
    ///
    /// Several hundred metres up to a couple of kilometres reads as geology —
    /// bands of different rock a valley can cross. Much smaller reads as noise;
    /// much larger and a whole block lands in one band and the world is
    /// uniform again, just uniform-per-block.
    pub wavelength: f32,
    /// How much faster the softest rock erodes than the hardest.
    ///
    /// `1.0` means no variation at all — exactly the old single-constant
    /// behaviour, which keeps `no-erosion` and every existing test meaningful.
    /// `8.0` means the softest ground gives way eight times faster than the
    /// hardest. The multiplier is centred so that average ground erodes at the
    /// configured erodibility: it ranges from `1/sqrt(contrast)` to
    /// `sqrt(contrast)`.
    pub contrast: f32,
}

impl HardnessSettings {
    /// The default: kilometre-scale bands, softest rock ~8x faster than
    /// hardest.
    pub const DEFAULT: Self = Self {
        wavelength: 1_100.0,
        contrast: 8.0,
    };

    /// No variation — the single-constant world this module replaced.
    pub const UNIFORM: Self = Self {
        wavelength: 1_100.0,
        contrast: 1.0,
    };
}

impl Default for HardnessSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Rock hardness over one block, one byte per cell.
///
/// `0` is the softest rock in the world, `255` the hardest.
#[derive(Debug, Clone)]
pub struct HardnessMap {
    cells: Vec<u8>,
    /// Byte value to erodibility multiplier, precomputed so the erosion loop
    /// does a lookup instead of a `powf` per cell per round.
    multiplier: [f32; 256],
}

impl HardnessMap {
    /// Samples hardness for a whole block, halo included.
    pub fn for_block(seed: u64, block: BlockCoordinates, settings: HardnessSettings) -> Self {
        // Row-parallel, same shape as base elevation: pure per-cell sampling.
        let mut cells = vec![0u8; CELLS];
        crate::parallel::fill_grid(&mut cells, |z, row| {
            for (x, cell) in row.iter_mut().enumerate() {
                let (world_x, world_z) = block.cell_centre(x as u32, z);
                let hardness = sample(seed, world_x, world_z, settings.wavelength);
                *cell = (hardness * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        });

        // Softest (byte 0) erodes sqrt(contrast) faster than average; hardest
        // (byte 255) erodes sqrt(contrast) slower. Geometric, so doubling the
        // contrast stretches both ends equally rather than only the soft end.
        let mut multiplier = [1.0f32; 256];
        if settings.contrast > 0.0 {
            for (byte, slot) in multiplier.iter_mut().enumerate() {
                let hardness = byte as f32 / 255.0;
                *slot = settings.contrast.powf(0.5 - hardness);
            }
        }

        Self { cells, multiplier }
    }

    /// The erodibility multiplier at a cell. `1.0` everywhere when contrast is 1.
    pub fn multiplier(&self, cell: ErosionCell) -> f32 {
        let index = (cell.z() as usize) * (EDGE as usize) + (cell.x() as usize);
        let byte = self.cells.get(index).copied().unwrap_or(128);
        self.multiplier.get(byte as usize).copied().unwrap_or(1.0)
    }

    /// Raw hardness at a cell, `0..=255`. For rendering and later stages —
    /// thermal erosion's talus angle and carving's channel shape are natural
    /// future consumers.
    pub fn hardness(&self, cell: ErosionCell) -> u8 {
        let index = (cell.z() as usize) * (EDGE as usize) + (cell.x() as usize);
        self.cells.get(index).copied().unwrap_or(128)
    }
}

/// Hardness in `0..1` at a world position. Two octaves of value noise.
fn sample(seed: u64, x: f32, z: f32, wavelength: f32) -> f32 {
    let wavelength = wavelength.max(1.0);
    let mut total = 0.0;
    let mut amplitude = 1.0;
    let mut normaliser = 0.0;
    let mut scale = wavelength;

    for octave in 0..OCTAVES {
        total += amplitude * octave_noise(seed, x / scale, z / scale, octave);
        normaliser += amplitude;
        amplitude *= 0.5;
        scale *= 0.5;
    }

    if normaliser > 0.0 {
        total / normaliser
    } else {
        0.5
    }
}

fn octave_noise(seed: u64, x: f32, z: f32, octave: u32) -> f32 {
    let x0 = x.floor();
    let z0 = z.floor();
    let tx = smoothstep(x - x0);
    let tz = smoothstep(z - z0);

    let corner = |ix: f32, iz: f32| lattice(seed, ix as i32, iz as i32, octave);

    let top = lerp(corner(x0, z0), corner(x0 + 1.0, z0), tx);
    let bottom = lerp(corner(x0, z0 + 1.0), corner(x0 + 1.0, z0 + 1.0), tx);
    lerp(top, bottom, tz)
}

fn lattice(seed: u64, x: i32, z: i32, octave: u32) -> f32 {
    let packed = ((x as u32 as u64) << 32) | (z as u32 as u64);
    let mut hash = mix64(seed ^ u64::from(FIELD_HARDNESS));
    hash = combine(hash, packed);
    hash = combine(hash, u64::from(octave));
    unit_f32(hash)
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
    use cx_core::math::BlockCoord;

    const SEED: u64 = 0x0BADC0DE;

    fn map(settings: HardnessSettings) -> HardnessMap {
        HardnessMap::for_block(SEED, BlockCoordinates::new(BlockCoord::new(0, 0)), settings)
    }

    /// Contrast 1 must reproduce the single-constant world exactly.
    ///
    /// This is what keeps every pre-hardness test and the `no-erosion` profile
    /// meaningful: with contrast 1 the multiplier is exactly 1.0 everywhere,
    /// and multiplying by exactly 1.0 leaves erosion bit-identical to the code
    /// this module replaced.
    #[test]
    fn contrast_one_multiplies_by_exactly_one() {
        let map = map(HardnessSettings::UNIFORM);
        for (x, z) in [(0u32, 0u32), (100, 2_000), (5_000, 5_000)] {
            let cell = ErosionCell::new(x, z).expect("in range");
            assert_eq!(map.multiplier(cell), 1.0);
        }
    }

    /// Soft rock erodes faster than average, hard rock slower, centred on 1.
    #[test]
    fn the_multiplier_is_centred_and_spans_the_contrast() {
        let map = map(HardnessSettings {
            contrast: 9.0,
            ..HardnessSettings::DEFAULT
        });

        // The table's ends: byte 0 is sqrt(9) = 3x, byte 255 is 1/3x.
        assert!((map.multiplier[0] - 3.0).abs() < 0.01);
        assert!((map.multiplier[255] - 1.0 / 3.0).abs() < 0.01);
        // And the middle is ~1: average rock erodes at the configured rate.
        assert!((map.multiplier[128] - 1.0).abs() < 0.02);
    }

    /// Hardness actually varies across a block at the default wavelength.
    #[test]
    fn a_block_contains_both_softer_and_harder_ground() {
        let map = map(HardnessSettings::DEFAULT);

        let mut low = u8::MAX;
        let mut high = u8::MIN;
        for z in (0..EDGE).step_by(41) {
            for x in (0..EDGE).step_by(41) {
                let cell = ErosionCell::new(x, z).expect("in range");
                low = low.min(map.hardness(cell));
                high = high.max(map.hardness(cell));
            }
        }

        assert!(
            high - low > 100,
            "hardness spans only {low}..{high} across a whole block, which is \
             too uniform to shape anything"
        );
    }

    /// Hardness agrees across a block seam, like every positional input.
    ///
    /// A hard band crossing the boundary between two blocks must be the same
    /// rock on both sides, or erosion would treat the two halves of one ridge
    /// differently and the seam would show in the terrain.
    #[test]
    fn neighbouring_blocks_agree_about_shared_ground() {
        use cx_core::math::EROSION_CELLS_PER_BLOCK_EDGE;
        let left = HardnessMap::for_block(
            SEED,
            BlockCoordinates::new(BlockCoord::new(0, 0)),
            HardnessSettings::DEFAULT,
        );
        let right = HardnessMap::for_block(
            SEED,
            BlockCoordinates::new(BlockCoord::new(1, 0)),
            HardnessSettings::DEFAULT,
        );

        // The right block's first halo column covers the same ground as a
        // column inside the left block's core (same geometry as the elevation
        // halo test).
        for z in (0..EDGE).step_by(97) {
            let in_right = ErosionCell::new(0, z).expect("in range");
            let in_left = ErosionCell::new(EROSION_CELLS_PER_BLOCK_EDGE, z).expect("in range");
            assert_eq!(
                right.hardness(in_right),
                left.hardness(in_left),
                "hardness disagrees about the same ground at z={z}"
            );
        }
    }

    /// Same seed, same map. Different seed, different map.
    #[test]
    fn hardness_is_seeded() {
        let a = HardnessMap::for_block(
            1,
            BlockCoordinates::new(BlockCoord::new(0, 0)),
            HardnessSettings::DEFAULT,
        );
        let b = HardnessMap::for_block(
            2,
            BlockCoordinates::new(BlockCoord::new(0, 0)),
            HardnessSettings::DEFAULT,
        );

        let differing = (0..EDGE)
            .step_by(131)
            .filter(|i| {
                let cell = ErosionCell::new(*i, *i).expect("in range");
                a.hardness(cell) != b.hardness(cell)
            })
            .count();
        assert!(
            differing > 20,
            "two seeds produced nearly identical hardness"
        );
    }
}
