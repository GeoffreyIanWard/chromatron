//! Step 4 of S07's pipeline: thermal erosion.
//!
//! Rock does not stand at arbitrary angles. Above a material's **talus angle** —
//! around 35 degrees for loose scree — a slope sheds material downhill until it
//! is back at that angle. This is the process that puts a fan of debris at the
//! bottom of a cliff, rounds a ridge line, and stops a landscape from holding
//! slopes it physically could not.
//!
//! # What it does here
//!
//! Every cell compares itself with its eight neighbours. Where the drop to a
//! neighbour exceeds what the talus angle allows over that distance, some of the
//! excess moves — from this cell to that neighbour, split across whichever
//! neighbours are too far below.
//!
//! # Read-then-write, and why that is not optional
//!
//! Deltas are computed for every cell from the **old** surface, and applied
//! afterwards. Updating in place as the sweep goes would make a cell's result
//! depend on whether its neighbours had been visited yet — which is row-major
//! order, which is an implementation detail, which `ADR-0006` forbids from
//! reaching terrain. The same read-then-write separation the tick phases use
//! (`03-conventions.md`), for the same reason.
//!
//! # Mass is conserved
//!
//! Unlike hydraulic erosion, which carries material out of the block entirely,
//! thermal erosion only *moves* it: every metre a cell loses, a neighbour gains.
//! That is a property worth having as a test rather than an intention, because
//! the failure mode — a stray factor that makes each round quietly add or remove
//! material — looks like nothing at all until a landscape has slowly inflated.

use cx_core::math::EROSION_CELL_SIZE;

use crate::block::{BlockGrid, CELLS, EDGE, ErosionCell};

/// The eight neighbours, in a fixed order.
///
/// Fixed for reproducibility: floating-point addition is not associative, so
/// summing a cell's contributions in a different order is a different number,
/// and `ADR-0006` promises the same block generates identically every time.
const NEIGHBOURS: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// How thermal erosion is shaped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalSettings {
    /// Talus angle in degrees — the steepest slope the material holds.
    ///
    /// ~35 degrees is loose scree. Lower makes a softer, more rounded
    /// landscape; higher lets cliffs stand.
    pub talus_degrees: f32,
    /// Fraction of the excess moved per round, in `0..=1`.
    ///
    /// Not 1.0 by default. Moving the entire excess in one step lets a cell
    /// overshoot below the neighbour it was shedding to, which reverses the
    /// slope and sheds it straight back — the surface then oscillates instead of
    /// settling, and more rounds make it worse rather than better.
    pub strength: f32,
    /// Relaxation rounds.
    pub rounds: u32,
}

impl Default for ThermalSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl ThermalSettings {
    /// The default, as a constant so callers can stay `const`.
    pub const DEFAULT: Self = Self {
        talus_degrees: 35.0,
        strength: 0.5,
        rounds: 8,
    };
}

impl ThermalSettings {
    /// No thermal erosion. A valid profile, per S07's `no-erosion`.
    pub const NONE: Self = Self {
        talus_degrees: 90.0,
        strength: 0.0,
        rounds: 0,
    };
}

/// What one run of thermal erosion did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalReport {
    /// Rounds actually run.
    pub rounds: u32,
    /// Total material moved, in metres summed over cells. A magnitude, not a
    /// net: the net is zero by construction and is checked separately.
    pub moved: f64,
    /// Net change in total height. Should be ~0 — see the module docs.
    pub net_change: f64,
    /// Cells still steeper than the talus angle after the last round.
    ///
    /// Not required to be zero, and **not monotonic in rounds**: as an
    /// over-steep peak sheds, its debris apron spreads outward and the apron's
    /// leading edge is itself a new steep front. The count can therefore rise
    /// slightly while the surface is genuinely settling, which is what
    /// [`Self::excess`] measures instead. Kept because "how much of the block is
    /// still steep" is a useful thing to know, not because it is a progress
    /// metric.
    pub over_steep: usize,
    /// Total steepness past the talus angle, in metres summed over the core.
    ///
    /// **This** is the settling measure. Unlike the cell count it falls
    /// monotonically with rounds, because it measures how far over the limit the
    /// surface is rather than how many places are over it at all.
    pub excess: f64,
}

/// Relaxes slopes past the talus angle.
///
/// Takes the surface by value and returns it, matching [`crate::hydraulic::erode`]:
/// the pipeline hands one grid from stage to stage rather than keeping copies of
/// each intermediate, because the grid is the largest allocation there is.
pub fn relax(mut surface: BlockGrid, settings: ThermalSettings) -> (BlockGrid, ThermalReport) {
    let mut delta = vec![0.0f32; CELLS];
    let mut moved = 0.0f64;

    // Thresholds precomputed per neighbour: the maximum drop the talus angle
    // permits over that neighbour's distance. Diagonals are 1.414 cells away and
    // so allow a proportionally larger drop; treating them as one cell apart
    // would relax them 41% too aggressively, and the result is a surface with
    // its diagonals planed flatter than its axes — the grid, printed into the
    // terrain, which is the artifact this stage is partly here to remove.
    let tangent = settings.talus_degrees.to_radians().tan();
    let mut threshold = [0.0f32; 8];
    for (index, (dx, dz)) in NEIGHBOURS.iter().enumerate() {
        let distance = if dx.abs() + dz.abs() == 2 {
            EROSION_CELL_SIZE * std::f32::consts::SQRT_2
        } else {
            EROSION_CELL_SIZE
        };
        if let Some(slot) = threshold.get_mut(index) {
            *slot = tangent * distance;
        }
    }

    for _ in 0..settings.rounds {
        // Each band zeroes and fills its own rows of `delta` directly. Material
        // a band's edge rows shed into the neighbouring band goes into two
        // spill rows returned to the caller, merged below in band order — so
        // every float is added in the same sequence on any thread count
        // (`crate::parallel`'s rule). Within a band the sweep is the same
        // read-then-write scatter as the serial version.
        let spills = crate::parallel::fill_bands_map(&mut delta, |start_z, band| {
            band.fill(0.0);
            let band_rows = band.len() / EDGE as usize;
            let mut spill_up = vec![0.0f32; EDGE as usize];
            let mut spill_down = vec![0.0f32; EDGE as usize];
            let mut band_moved = 0.0f64;

            for z in start_z..start_z + band_rows as u32 {
                for x in 0..EDGE {
                    let Some(cell) = ErosionCell::new(x, z) else {
                        continue;
                    };
                    let height = surface.get(cell);

                    // First pass over the neighbours: how much excess is there,
                    // and how is it distributed? Read only — nothing is written
                    // until every cell has been measured against the same
                    // surface.
                    let mut excess_total = 0.0f32;
                    let mut deepest_excess = 0.0f32;
                    let mut excesses = [0.0f32; 8];

                    for (index, (dx, dz)) in NEIGHBOURS.iter().enumerate() {
                        let Some(next) = offset(cell, *dx, *dz) else {
                            continue;
                        };
                        let allowed = threshold.get(index).copied().unwrap_or(f32::INFINITY);
                        let excess = (height - surface.get(next)) - allowed;
                        if excess <= 0.0 {
                            continue;
                        }
                        if let Some(slot) = excesses.get_mut(index) {
                            *slot = excess;
                        }
                        excess_total += excess;
                        deepest_excess = deepest_excess.max(excess);
                    }

                    if excess_total <= 0.0 {
                        continue;
                    }

                    // Halved because the neighbour is coming *up* by the same
                    // amount this cell goes down: to close a gap of `d`, each
                    // side moves `d/2`. Moving the full excess from each side
                    // overshoots by exactly a factor of two, and the surface
                    // rings.
                    let budget = settings.strength * deepest_excess * 0.5;

                    // Writes a delta to a cell that may be just outside this
                    // band's rows.
                    let mut add = |tx: u32, tz: u32, amount: f32| {
                        let column = tx as usize;
                        if tz + 1 == start_z {
                            if let Some(slot) = spill_up.get_mut(column) {
                                *slot += amount;
                            }
                        } else if tz == start_z + band_rows as u32 {
                            if let Some(slot) = spill_down.get_mut(column) {
                                *slot += amount;
                            }
                        } else {
                            let index = (tz - start_z) as usize * EDGE as usize + column;
                            if let Some(slot) = band.get_mut(index) {
                                *slot += amount;
                            }
                        }
                    };

                    for (index, (dx, dz)) in NEIGHBOURS.iter().enumerate() {
                        let share = excesses.get(index).copied().unwrap_or(0.0);
                        if share <= 0.0 {
                            continue;
                        }
                        let Some(next) = offset(cell, *dx, *dz) else {
                            continue;
                        };

                        // Proportional to how far over the limit each neighbour
                        // is, so the steepest face takes the most. An even
                        // split would send as much material to a neighbour
                        // barely over the angle as to a cliff.
                        let amount = budget * (share / excess_total);

                        add(cell.x(), cell.z(), -amount);
                        add(next.x(), next.z(), amount);
                        band_moved += f64::from(amount);
                    }
                }
            }

            (start_z, band_rows, spill_up, spill_down, band_moved)
        });

        // Merge the spill rows and the moved totals, in band order.
        for (start_z, band_rows, spill_up, spill_down, band_moved) in spills {
            if start_z > 0 {
                for (x, amount) in spill_up.iter().enumerate() {
                    if *amount != 0.0
                        && let Some(slot) =
                            delta.get_mut((start_z as usize - 1) * EDGE as usize + x)
                    {
                        *slot += amount;
                    }
                }
            }
            let below = start_z as usize + band_rows;
            if below < EDGE as usize {
                for (x, amount) in spill_down.iter().enumerate() {
                    if *amount != 0.0
                        && let Some(slot) = delta.get_mut(below * EDGE as usize + x)
                    {
                        *slot += amount;
                    }
                }
            }
            moved += band_moved;
        }

        // Apply, row-parallel: each cell adds its own delta, nothing crosses.
        let delta_ref = &delta;
        crate::parallel::fill_grid(surface.as_mut_slice(), |z, row| {
            for (x, cell) in row.iter_mut().enumerate() {
                let change = delta_ref
                    .get(z as usize * EDGE as usize + x)
                    .copied()
                    .unwrap_or(0.0);
                if change != 0.0 {
                    *cell += change;
                }
            }
        });
    }

    let report = measure(&surface, &threshold, settings.rounds, moved);
    (surface, report)
}

/// Counts what is left over the talus angle, and the net height change.
fn measure(surface: &BlockGrid, threshold: &[f32; 8], rounds: u32, moved: f64) -> ThermalReport {
    use cx_core::math::EROSION_CELLS_PER_BLOCK_EDGE;

    let low = crate::block::HALO_CELLS;
    let high = low + EROSION_CELLS_PER_BLOCK_EDGE;

    let mut over_steep = 0usize;
    let mut excess = 0.0f64;

    for z in low..high {
        for x in low..high {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };
            let height = surface.get(cell);

            let mut worst = 0.0f32;
            for (index, (dx, dz)) in NEIGHBOURS.iter().enumerate() {
                let Some(next) = offset(cell, *dx, *dz) else {
                    continue;
                };
                let allowed = threshold.get(index).copied().unwrap_or(f32::MAX);
                worst = worst.max((height - surface.get(next)) - allowed);
            }

            if worst > 0.0 {
                over_steep += 1;
                excess += f64::from(worst);
            }
        }
    }

    ThermalReport {
        rounds,
        moved,
        // Filled in by the caller that has the "before" surface; zero here is
        // the honest value for a function that cannot see one.
        net_change: 0.0,
        over_steep,
        excess,
    }
}

/// Total height over the whole grid, for a mass-conservation check.
///
/// `f64`, because summing 26 million `f32` values in `f32` loses the low bits
/// long before the end and would make any conservation test a test of
/// accumulated rounding.
pub fn total_height(surface: &BlockGrid) -> f64 {
    surface.as_slice().iter().map(|h| f64::from(*h)).sum()
}

fn offset(cell: ErosionCell, dx: i32, dz: i32) -> Option<ErosionCell> {
    let x = cell.x().checked_add_signed(dx)?;
    let z = cell.z().checked_add_signed(dz)?;
    ErosionCell::new(x, z)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fewer rounds than the default: every round sweeps 26 million cells.
    const TEST_SETTINGS: ThermalSettings = ThermalSettings {
        talus_degrees: 35.0,
        strength: 0.5,
        rounds: 4,
    };

    /// A cone far steeper than any talus angle.
    fn steep_cone() -> BlockGrid {
        let mut grid = BlockGrid::filled(0.0);
        let centre = 2_560.0f32;

        for z in 0..EDGE {
            for x in 0..EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                let dx = x as f32 - centre;
                let dz = z as f32 - centre;
                let distance = (dx * dx + dz * dz).sqrt();
                // Drops 4 m per cell — about 63 degrees, far past scree.
                grid.set(cell, (400.0 - distance * 4.0).max(0.0));
            }
        }
        grid
    }

    /// A gentle slope well under the talus angle.
    fn gentle_slope() -> BlockGrid {
        let mut grid = BlockGrid::filled(0.0);
        for z in 0..EDGE {
            for x in 0..EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                // 0.2 m per 2 m cell — about 6 degrees.
                grid.set(cell, 100.0 - x as f32 * 0.2);
            }
        }
        grid
    }

    #[test]
    fn zero_rounds_is_the_identity() {
        let before = gentle_slope();
        let (after, report) = relax(gentle_slope(), ThermalSettings::NONE);

        assert_eq!(report.rounds, 0);
        assert_eq!(report.moved, 0.0);

        for z in (0..EDGE).step_by(101) {
            for x in (0..EDGE).step_by(101) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                assert_eq!(after.get(cell), before.get(cell));
            }
        }
    }

    /// **A slope already within the angle is left alone.**
    ///
    /// Without this, a version that relaxed everything unconditionally would
    /// pass every other test here while flattening the world.
    #[test]
    fn a_slope_under_the_talus_angle_is_untouched() {
        let before = gentle_slope();
        let (after, report) = relax(gentle_slope(), TEST_SETTINGS);

        assert_eq!(
            report.moved, 0.0,
            "material moved on a 6-degree slope, which is nowhere near the \
             35-degree talus angle"
        );

        for z in (0..EDGE).step_by(101) {
            for x in (0..EDGE).step_by(101) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                assert_eq!(after.get(cell), before.get(cell));
            }
        }
    }

    /// **The claim the stage exists to make:** over-steep ground gets less so.
    #[test]
    fn an_over_steep_cone_relaxes_towards_the_talus_angle() {
        let (_, before) = relax(steep_cone(), ThermalSettings::NONE);
        let (_, after) = relax(steep_cone(), TEST_SETTINGS);

        assert!(
            before.over_steep > 0,
            "the fixture is not over-steep, so this test asserts nothing"
        );
        assert!(
            after.excess < before.excess * 0.9,
            "excess steepness went from {:.0} m to {:.0} m, so relaxation is not \
             relaxing",
            before.excess,
            after.excess
        );
        assert!(after.moved > 0.0, "no material moved on a 63-degree cone");
    }

    /// **Mass is conserved.** Material moves; it is never created or destroyed.
    ///
    /// The failure this guards against is silent: a stray factor that makes each
    /// round add or remove a little looks like nothing until a landscape has
    /// slowly inflated or worn away over many rounds.
    #[test]
    fn material_moves_rather_than_appearing_or_vanishing() {
        let before = total_height(&steep_cone());
        let (after_grid, report) = relax(steep_cone(), TEST_SETTINGS);
        let after = total_height(&after_grid);

        assert!(report.moved > 0.0, "nothing moved, so this proves nothing");

        // Relative to the material actually moved, not to the total height —
        // against a 26-million-cell sum, almost any error is small in relative
        // terms and the test would pass regardless.
        let drift = (after - before).abs();
        assert!(
            drift < report.moved * 1e-3,
            "total height changed by {drift} m while moving {} m of material, so \
             thermal erosion is not conservative",
            report.moved
        );
    }

    /// The same input relaxes the same way twice.
    ///
    /// Float addition is not associative, so a cell's result depends on the
    /// order its neighbours contribute. Fixed order, or `ADR-0006`'s promise
    /// that a block regenerates identically does not hold.
    #[test]
    fn relaxation_is_reproducible() {
        let (first, first_report) = relax(steep_cone(), TEST_SETTINGS);
        let (second, second_report) = relax(steep_cone(), TEST_SETTINGS);

        assert_eq!(first_report, second_report);

        for z in (0..EDGE).step_by(37) {
            for x in (0..EDGE).step_by(37) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                assert_eq!(
                    first.get(cell),
                    second.get(cell),
                    "({x}, {z}) relaxed two different ways"
                );
            }
        }
    }

    /// More rounds settle further, and do not oscillate.
    ///
    /// The overshoot failure the `0.5` factor in `budget` exists to prevent
    /// shows up exactly here: a surface that rings would get *worse* with more
    /// rounds, not better.
    #[test]
    fn more_rounds_settle_further_rather_than_ringing() {
        let few = ThermalSettings {
            rounds: 2,
            ..TEST_SETTINGS
        };
        let many = ThermalSettings {
            rounds: 10,
            ..TEST_SETTINGS
        };

        let (_, few_report) = relax(steep_cone(), few);
        let (_, many_report) = relax(steep_cone(), many);

        // Excess steepness, not the over-steep *count*. The count is not a
        // progress metric — an apron spreading outward creates new steep front
        // as fast as the peak behind it settles — and asserting on it failed
        // against a surface that was settling perfectly well.
        assert!(
            many_report.excess < few_report.excess,
            "ten rounds left {:.0} m of excess steepness against two rounds' \
             {:.0} m, so the surface is oscillating instead of settling",
            many_report.excess,
            few_report.excess
        );
    }
}
