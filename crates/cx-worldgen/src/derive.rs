//! Step 7 of S07's pipeline: derived static fields.
//!
//! Slope and aspect, computed once from baked `ELEVATION` and never recomputed
//! — `ADR-0008` removed continuous erosion precisely so that these could be
//! static. Nothing dirties them except a discrete terrain edit (S19).
//!
//! # Who consumes them
//!
//! Slope is the one with the most customers. Navigation cost grids are derived
//! from it (S10), physics uses it to decide what a body can rest on (S11), biome
//! assignment takes it as an input (S07 step 8), and scatter placement refuses
//! to put trees on cliffs (step 9). Aspect drives insolation, and through it
//! snow line, vegetation, and shading.
//!
//! # Quantised to a byte each
//!
//! `bench/memory-budget.md` requires it: one chunk is 1,048,576 cells, so a
//! single `f32` field is 4 MB per chunk and 10,000 resident chunks would be
//! 40 GB. At a byte each these are 1 MB per chunk, and the precision lost is
//! precision no consumer can use — 0.35 degrees of slope and 1.4 of aspect are
//! well below what a nav cost or a snow line can distinguish.
//!
//! The quantisation is **saturating, not wrapping**. A near-vertical face pins at
//! the maximum; it does not roll over and read as flat, which would put a
//! traversable route up a cliff.
//!
//! # Aspect on flat ground
//!
//! A flat cell has no aspect — not aspect zero, which is north. It gets
//! [`ASPECT_FLAT`], and consumers have to handle it. The alternative is a world
//! where every plain faces north and every consumer of aspect quietly believes
//! it, which is the kind of wrong that looks like a feature.

use cx_core::math::{CELL_SIZE, CELLS_PER_CHUNK, CELLS_PER_CHUNK_EDGE};

use crate::bake::ChunkElevation;

/// Aspect of a cell with no measurable slope.
///
/// Not a direction. `255` rather than `0`, because zero is north and a plain
/// that faces north is a plausible-looking lie.
pub const ASPECT_FLAT: u8 = u8::MAX;

/// Degrees of slope per quantisation step.
pub const SLOPE_STEP_DEGREES: f32 = 90.0 / 254.0;

/// Degrees of aspect per quantisation step.
///
/// 255 steps over the circle, because [`ASPECT_FLAT`] takes the 256th.
pub const ASPECT_STEP_DEGREES: f32 = 360.0 / 255.0;

/// The quantisation has to stay below what any consumer can distinguish, and
/// that is a property of the constants rather than of any run — so it is checked
/// at compile time. A later change to either step size that coarsened it past
/// what a nav cost or a snow line can use would fail the build rather than
/// quietly degrade every derived field in the world.
const _: () = assert!(
    SLOPE_STEP_DEGREES < 0.4,
    "slope quantisation is coarser than consumers can use"
);
const _: () = assert!(
    ASPECT_STEP_DEGREES < 1.5,
    "aspect quantisation is coarser than consumers can use"
);

/// The slope below which a cell is treated as having no aspect, in degrees.
///
/// Not zero. On a surface with metre-scale detail on it, a genuinely level
/// plain still has a gradient of a few hundredths of a degree from the noise,
/// and its direction is that noise's direction — which is to say, arbitrary.
/// Consumers reading it would see aspect flickering cell to cell across ground
/// that is flat.
pub const FLAT_SLOPE_DEGREES: f32 = 0.05;

/// Slope and aspect over one chunk, quantised.
#[derive(Debug, Clone)]
pub struct DerivedFields {
    slope: Vec<u8>,
    aspect: Vec<u8>,
}

impl DerivedFields {
    /// Quantised slope at a chunk-local cell. `0` is level, `254` is vertical.
    pub fn slope(&self, x: u32, z: u32) -> Option<u8> {
        self.slope.get(index(x, z)?).copied()
    }

    /// Quantised aspect, or [`ASPECT_FLAT`].
    pub fn aspect(&self, x: u32, z: u32) -> Option<u8> {
        self.aspect.get(index(x, z)?).copied()
    }

    /// Slope in degrees, dequantised.
    pub fn slope_degrees(&self, x: u32, z: u32) -> Option<f32> {
        Some(f32::from(self.slope(x, z)?) * SLOPE_STEP_DEGREES)
    }

    /// Aspect in degrees clockwise from north, or `None` on flat ground.
    ///
    /// Returns `None` for both "out of range" and "flat", which are different
    /// things — [`Self::aspect`] distinguishes them for a caller that needs to.
    pub fn aspect_degrees(&self, x: u32, z: u32) -> Option<f32> {
        let raw = self.aspect(x, z)?;
        if raw == ASPECT_FLAT {
            return None;
        }
        Some(f32::from(raw) * ASPECT_STEP_DEGREES)
    }

    /// The raw quantised slopes, row-major.
    pub fn slopes(&self) -> &[u8] {
        &self.slope
    }

    /// The raw quantised aspects, row-major.
    pub fn aspects(&self) -> &[u8] {
        &self.aspect
    }
}

/// Derives slope and aspect from a baked chunk.
///
/// # The edge problem, and why it is not solved here
///
/// A central difference needs a neighbour on each side, and cells on the chunk's
/// rim do not have one inside the chunk. This clamps, which makes a rim cell's
/// slope a one-sided difference — correct in direction, and up to half the true
/// magnitude on a curved surface.
///
/// The right fix is to bake with a one-cell halo, which `ELEVATION` is already
/// registered for (S07: *"`ELEVATION` is registered `DeltaPersisted` with a
/// one-cell halo"*). That plumbing does not exist yet, so this is recorded as a
/// known approximation on 4,092 of a chunk's 1,048,576 cells — 0.4% — rather
/// than papered over. The symptom if it were ignored would be a faint seam of
/// under-reported slope along every chunk boundary, which navigation would read
/// as a slightly cheaper route around the edge of every chunk.
pub fn derive_fields(elevation: &ChunkElevation) -> DerivedFields {
    let mut slope = vec![0u8; CELLS_PER_CHUNK as usize];
    let mut aspect = vec![ASPECT_FLAT; CELLS_PER_CHUNK as usize];

    let at = |x: u32, z: u32| -> f32 {
        elevation
            .get(
                x.min(CELLS_PER_CHUNK_EDGE - 1),
                z.min(CELLS_PER_CHUNK_EDGE - 1),
            )
            .unwrap_or(0.0)
    };

    // Row-parallel, in one pass over an interleaved pair per cell that is
    // split afterwards. Two separate passes would be simpler but would compute
    // every gradient twice; interleaving keeps the arithmetic single-pass and
    // still gives each band a disjoint output slice. Derivation sits on the
    // chunk-promotion path now, where a million atan calls single-threaded
    // showed up directly in the worst frame time (22.5 ms against a 20 ms
    // target on the traversal exercise).
    let mut pairs = vec![[0u8, ASPECT_FLAT]; CELLS_PER_CHUNK as usize];
    crate::parallel::fill_rows(&mut pairs, CELLS_PER_CHUNK_EDGE as usize, |z, row| {
        for (x, out) in row.iter_mut().enumerate() {
            let x = x as u32;
            // Central differences, clamped at the rim. The span is two cells
            // wide except on the rim, where it is one — so the divisor has to
            // follow, or edge cells report double their real gradient.
            let (west, east) = (x.saturating_sub(1), (x + 1).min(CELLS_PER_CHUNK_EDGE - 1));
            let (north, south) = (z.saturating_sub(1), (z + 1).min(CELLS_PER_CHUNK_EDGE - 1));

            let run_x = (east - west) as f32 * CELL_SIZE;
            let run_z = (south - north) as f32 * CELL_SIZE;

            let dx = if run_x > 0.0 {
                (at(east, z) - at(west, z)) / run_x
            } else {
                0.0
            };
            let dz = if run_z > 0.0 {
                (at(x, south) - at(x, north)) / run_z
            } else {
                0.0
            };

            let magnitude = (dx * dx + dz * dz).sqrt();
            let degrees = magnitude.atan().to_degrees();

            // Saturating: a near-vertical face pins at the top rather than
            // wrapping to zero and reading as level ground.
            out[0] = ((degrees / SLOPE_STEP_DEGREES).round().clamp(0.0, 254.0)) as u8;

            if degrees < FLAT_SLOPE_DEGREES {
                continue;
            }

            // Aspect is the compass direction the slope **faces** — the way
            // water would run — measured clockwise from north. North is -Z, per
            // `03-conventions.md`'s right-handed Y-up frame, so the downhill
            // direction is the negated gradient.
            let bearing = (-dx).atan2(dz).to_degrees();
            let bearing = if bearing < 0.0 {
                bearing + 360.0
            } else {
                bearing
            };
            out[1] = ((bearing / ASPECT_STEP_DEGREES).round() as u32 % 255) as u8;
        }
    });

    for (index, [s, a]) in pairs.iter().enumerate() {
        if let Some(slot) = slope.get_mut(index) {
            *slot = *s;
        }
        if let Some(slot) = aspect.get_mut(index) {
            *slot = *a;
        }
    }

    DerivedFields { slope, aspect }
}

fn index(x: u32, z: u32) -> Option<usize> {
    if x >= CELLS_PER_CHUNK_EDGE || z >= CELLS_PER_CHUNK_EDGE {
        return None;
    }
    Some((z * CELLS_PER_CHUNK_EDGE + x) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bake::{BakeSettings, bake_chunk};
    use crate::block::{BlockCoordinates, BlockGrid, ErosionCell};
    use crate::elevation::{ElevationGenerator, TerrainShape};
    use crate::flow::FlowNetwork;
    use cx_core::math::{BlockCoord, ChunkCoord, EROSION_CELL_SIZE};

    /// A block sloping at a known angle, so slope has a right answer.
    ///
    /// `fall` is metres of drop per metre travelled along +X.
    fn ramp(fall: f32) -> BlockGrid {
        let mut grid = BlockGrid::filled(0.0);
        for z in 0..crate::block::EDGE {
            for x in 0..crate::block::EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                grid.set(cell, 500.0 - x as f32 * EROSION_CELL_SIZE * fall);
            }
        }
        grid
    }

    fn fields_for(grid: &BlockGrid) -> DerivedFields {
        let network = FlowNetwork::build(grid.clone());
        let generator = ElevationGenerator::new(7, TerrainShape::default());
        let baked = bake_chunk(
            grid,
            &network,
            &generator,
            BlockCoordinates::new(BlockCoord::new(0, 0)),
            ChunkCoord::new(4, 4),
            // No detail: this is testing the derivation, and texture would put
            // a fraction of a degree of noise on every answer.
            BakeSettings::SMOOTH,
        )
        .expect("in this block");
        derive_fields(&baked)
    }

    /// **A known slope reads back as that slope.**
    #[test]
    fn slope_matches_the_angle_it_was_built_from() {
        // tan(30°) = 0.5774, tan(10°) = 0.1763.
        for (fall, expected) in [(0.1763f32, 10.0f32), (0.5774, 30.0), (1.0, 45.0)] {
            let fields = fields_for(&ramp(fall));
            let measured = fields.slope_degrees(512, 512).expect("in range");
            assert!(
                (measured - expected).abs() < 0.5,
                "a {expected}-degree ramp read back as {measured} degrees"
            );
        }
    }

    /// **Flat ground is flat, and has no aspect.**
    ///
    /// Not aspect zero, which is north — a world where every plain faces north
    /// is the kind of wrong that looks like a feature.
    #[test]
    fn level_ground_has_no_aspect() {
        let fields = fields_for(&BlockGrid::filled(120.0));

        assert_eq!(fields.slope(512, 512), Some(0));
        assert_eq!(fields.aspect(512, 512), Some(ASPECT_FLAT));
        assert_eq!(fields.aspect_degrees(512, 512), None);
    }

    /// **Aspect points the way water runs.**
    ///
    /// The ramp falls along +X, so downhill is east — 90 degrees clockwise from
    /// north. Getting this backwards is the classic aspect bug: it produces a
    /// world lit from the wrong side, and nothing but a rendered hillshade
    /// notices.
    #[test]
    fn aspect_faces_downhill_not_uphill() {
        let fields = fields_for(&ramp(0.5));
        let bearing = fields.aspect_degrees(512, 512).expect("not flat");

        assert!(
            (bearing - 90.0).abs() < 3.0,
            "a slope falling east reads as bearing {bearing}, not 90 degrees"
        );
    }

    /// And the other axis, so a transposed gradient cannot pass.
    #[test]
    fn a_slope_falling_south_reads_as_south() {
        let mut grid = BlockGrid::filled(0.0);
        for z in 0..crate::block::EDGE {
            for x in 0..crate::block::EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                grid.set(cell, 500.0 - z as f32 * EROSION_CELL_SIZE * 0.5);
            }
        }

        let fields = fields_for(&grid);
        let bearing = fields.aspect_degrees(512, 512).expect("not flat");

        // +Z is south, so downhill along +Z is a bearing of 180.
        assert!(
            (bearing - 180.0).abs() < 3.0,
            "a slope falling south reads as bearing {bearing}, not 180 degrees"
        );
    }

    /// **Quantisation saturates rather than wrapping.**
    ///
    /// A cliff that wrapped to zero would read as level ground, and navigation
    /// would happily route a path up it.
    #[test]
    fn a_cliff_pins_at_the_maximum_rather_than_wrapping() {
        // 100 m of drop per 2 m cell — about 89 degrees.
        let fields = fields_for(&ramp(50.0));
        let raw = fields.slope(512, 512).expect("in range");

        assert!(
            raw >= 250,
            "a near-vertical face quantised to {raw}, which is not near the top"
        );
        assert!(raw <= 254, "slope must stay within its range, got {raw}");
    }

    /// Both fields fit the byte-per-cell budget.
    ///
    /// The resolution claim is a compile-time assertion on the constants
    /// themselves — see the `const _` blocks above — because it is a property of
    /// the constants and not of any run. What this checks is the thing that
    /// *does* depend on the code: that a chunk's two derived fields together
    /// cost 2 MB rather than the 8 MB they would at `f32`, which is the figure
    /// `bench/memory-budget.md` uses to make field quantization mandatory.
    #[test]
    fn a_chunk_of_derived_fields_fits_the_memory_budget() {
        let fields = fields_for(&ramp(0.2));

        let bytes = size_of_val(fields.slopes()) + size_of_val(fields.aspects());
        let as_floats = (CELLS_PER_CHUNK as usize) * 2 * size_of::<f32>();

        assert_eq!(bytes, 2 * CELLS_PER_CHUNK as usize);
        assert_eq!(
            as_floats / bytes,
            4,
            "quantisation should be saving a factor of four"
        );
    }

    #[test]
    fn out_of_range_cells_report_none() {
        let fields = fields_for(&ramp(0.2));
        assert!(fields.slope(CELLS_PER_CHUNK_EDGE, 0).is_none());
        assert!(fields.aspect(0, CELLS_PER_CHUNK_EDGE).is_none());
    }

    /// The whole chunk is covered, not just the middle.
    #[test]
    fn every_cell_of_the_chunk_is_derived() {
        let fields = fields_for(&ramp(0.3));

        assert_eq!(fields.slopes().len(), CELLS_PER_CHUNK as usize);
        assert_eq!(fields.aspects().len(), CELLS_PER_CHUNK as usize);

        // On a uniform ramp every interior cell should read the same slope. The
        // rim is a documented one-sided difference and is excluded — see
        // `derive_fields`.
        let middle = fields.slope(512, 512).expect("in range");
        let mut differing = 0;
        for z in 1..CELLS_PER_CHUNK_EDGE - 1 {
            for x in 1..CELLS_PER_CHUNK_EDGE - 1 {
                if fields.slope(x, z) != Some(middle) {
                    differing += 1;
                }
            }
        }
        assert_eq!(
            differing, 0,
            "{differing} interior cells of a uniform ramp disagree about its slope"
        );
    }

    /// Deriving twice agrees (`ADR-0006`).
    #[test]
    fn derivation_is_reproducible() {
        let grid = ramp(0.4);
        let first = fields_for(&grid);
        let second = fields_for(&grid);

        assert_eq!(first.slopes(), second.slopes());
        assert_eq!(first.aspects(), second.aspects());
    }
}
