//! Step 5 of S07's pipeline: channel carving.
//!
//! Erosion lowers ground in proportion to how much drains through it, which
//! produces valleys. It does not produce **channels** — a river is a metres-wide
//! trench in the floor of a valley that may be hundreds of metres across, and at
//! a 2 m erosion grid the stream-power term simply does not resolve one. So the
//! flow network is incised into the eroded surface directly, with a geometry
//! taken from discharge.
//!
//! # Hydraulic geometry
//!
//! S08 fixes the exponents: **width ∝ Q^0.5, depth ∝ Q^0.4** (Leopold & Maddock
//! 1953), with the constants content-defined. Discharge is catchment area times
//! effective precipitation; there is no precipitation field yet, so area stands
//! in for it — a uniform-rainfall world, which is the right default and the one a
//! precipitation field later scales rather than replaces.
//!
//! Both exponents are well under 1, which is the thing worth noticing. A river
//! draining a hundred times the catchment is ten times as wide and six times as
//! deep, not a hundred times either. That is why a drainage network looks like it
//! does: tributaries stay comparable to their trunk instead of vanishing beside
//! it.
//!
//! # Width is quantised to the erosion grid
//!
//! `ADR-0015` runs this stage at 2 m, so a channel narrower than that has no
//! width to represent and is carried by depth alone. That is not a loss:
//! sub-2 m channels are streams, and a stream is a line on a map rather than a
//! shape.
//!
//! # Carving must not create sinks
//!
//! Cutting a trench into a surface is exactly how to make water disappear into
//! one. Two things prevent it, and both are load-bearing:
//!
//! - **Depth grows with discharge, and discharge grows downstream.** A channel
//!   bed therefore falls at least as fast as the surface it is cut into, so the
//!   trench cannot rise along its own length.
//! - **The banks are a profile, not a step.** Cells beside the channel are
//!   lowered by a fraction that falls to zero at the channel's edge, so there is
//!   no wall for water to be trapped behind.
//!
//! Neither argument is worth trusting on its own, so the network is rebuilt after
//! carving and the interior sinks are counted.

use crate::block::{BlockGrid, CELLS, EDGE, ErosionCell};
use crate::flow::FlowNetwork;

/// How channels are shaped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CarveSettings {
    /// Catchment area, in square metres, at which a channel begins.
    ///
    /// Below this the flow is a hillslope rill and gets no trench. Setting it too
    /// low carves the entire drainage network including every headwater thread,
    /// which reads as a crazed surface rather than as rivers.
    pub channel_threshold: f32,
    /// Metres of depth at unit discharge. The master depth knob.
    pub depth_coefficient: f32,
    /// Metres of width at unit discharge. The master width knob.
    pub width_coefficient: f32,
    /// Deepest a channel may be cut, in metres.
    ///
    /// A cap, not a target. The power law has no upper bound, and a block
    /// containing an unusually large catchment would otherwise get a canyon that
    /// is a property of the block boundary rather than of the landscape.
    pub max_depth: f32,
    /// Widest a channel may be cut, in metres. Bounds the per-cell work, which
    /// is quadratic in width.
    pub max_width: f32,
}

impl Default for CarveSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl CarveSettings {
    /// The default, as a constant so callers can stay `const`.
    pub const DEFAULT: Self = Self {
        // 50,000 m² — five hectares. Small enough that a landscape has a
        // visible network, large enough that it is a network and not a rash.
        channel_threshold: 50_000.0,
        depth_coefficient: 0.008,
        width_coefficient: 0.009,
        max_depth: 12.0,
        max_width: 60.0,
    };
}

impl CarveSettings {
    /// No carving. A valid profile, per S07's `no-erosion`.
    pub const NONE: Self = Self {
        channel_threshold: f32::INFINITY,
        depth_coefficient: 0.0,
        width_coefficient: 0.0,
        max_depth: 0.0,
        max_width: 0.0,
    };

    /// Channel depth in metres for a catchment area in square metres.
    pub fn depth_for(&self, area: f32) -> f32 {
        if area < self.channel_threshold {
            return 0.0;
        }
        (self.depth_coefficient * area.powf(0.4)).min(self.max_depth)
    }

    /// Channel width in metres for a catchment area in square metres.
    pub fn width_for(&self, area: f32) -> f32 {
        if area < self.channel_threshold {
            return 0.0;
        }
        (self.width_coefficient * area.powf(0.5)).min(self.max_width)
    }
}

/// What carving did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CarveReport {
    /// Cells that are channel centreline — above the discharge threshold.
    pub channel_cells: usize,
    /// Cells lowered at all, banks included.
    pub carved_cells: usize,
    /// The deepest cut, in metres.
    pub deepest: f32,
    /// Interior sinks after re-routing the carved surface. **Must be zero.**
    pub interior_sinks: usize,
}

/// What carving produced.
///
/// Three surfaces rather than one, because they answer different questions and
/// only [`Self::drained`] is the terrain. Returning a single grid was the earlier
/// shape and it silently discarded [`Self::ground`], which is the only thing that
/// makes water depth computable — see that field.
#[derive(Debug)]
pub struct Carved {
    /// The carved surface **with depressions filled**. This is the terrain, and
    /// what the bake resamples.
    pub drained: BlockGrid,
    /// The carved surface **before** the final fill.
    ///
    /// The difference between this and [`Self::drained`] is standing water: where
    /// the fill had to raise ground, that is a lake, and how far it raised it is
    /// how deep. S07 step 7 derives water body extents and surface levels from
    /// exactly this difference, so discarding it would mean either recomputing a
    /// whole fill later or having no lakes.
    pub ground: BlockGrid,
    /// Drainage of the carved surface.
    pub network: FlowNetwork,
    /// What carving did.
    pub report: CarveReport,
}

/// Cuts the flow network into a surface.
///
/// The network is rebuilt afterwards, and that is not optional bookkeeping:
/// carving moves channels by metres, so every later stage that asks where the
/// water is would otherwise be reading the drainage of a surface that no longer
/// exists.
pub fn carve(
    surface: BlockGrid,
    network: &FlowNetwork,
    settings: CarveSettings,
    seal: &crate::flow::BlockBoundary,
) -> Carved {
    let cell_size = cx_core::math::EROSION_CELL_SIZE;
    let cell_area = cell_size * cell_size;

    // Read-then-write, as everywhere in this pipeline: the cut is computed for
    // every cell against the *old* surface and applied afterwards. A cell can be
    // reached by more than one channel, and taking the **deepest** rather than
    // the sum is what stops a confluence being cut twice as deep as either of
    // the channels meeting there.
    let mut cut = vec![0.0f32; CELLS];

    // The widest any stamp can reach, in rows. Bounds how far outside its own
    // rows a band must look for sources whose trench lands inside them.
    let max_reach = ((settings.max_width / 2.0) / cell_size).ceil() as u32;

    // Band-parallel, gather style: each band owns its rows of `cut` and scans
    // every source within reach — including sources in neighbouring bands —
    // keeping only the parts of each trench that land on its own rows. A
    // boundary source gets its geometry recomputed by both bands, which costs a
    // little redundant arithmetic and buys a race-free, order-independent
    // result: "deepest cut wins" is a max, and a max is the same whatever order
    // it meets its inputs in.
    let channel_counts = crate::parallel::fill_bands_map(&mut cut, |start_z, band| {
        band.fill(0.0);
        let band_rows = (band.len() / EDGE as usize) as u32;
        let mut channel_cells = 0usize;

        let scan_from = start_z.saturating_sub(max_reach);
        let scan_to = (start_z + band_rows + max_reach).min(EDGE);

        for z in scan_from..scan_to {
            for x in 0..EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };

                let area = network.accumulation(cell) * cell_area;
                let depth = settings.depth_for(area);
                if depth <= 0.0 {
                    continue;
                }
                // Counted once, by the band that owns the source's own row.
                if z >= start_z && z < start_z + band_rows {
                    channel_cells += 1;
                }

                let half_width = (settings.width_for(area) / 2.0).max(cell_size / 2.0);
                let reach = (half_width / cell_size).ceil() as i32;

                for dz in -reach..=reach {
                    let Some(tz) = z.checked_add_signed(dz) else {
                        continue;
                    };
                    // Only rows this band owns.
                    if tz < start_z || tz >= start_z + band_rows || tz >= EDGE {
                        continue;
                    }
                    for dx in -reach..=reach {
                        let Some(tx) = x.checked_add_signed(dx) else {
                            continue;
                        };
                        if tx >= EDGE {
                            continue;
                        }

                        let distance = ((dx * dx + dz * dz) as f32).sqrt() * cell_size;
                        if distance > half_width {
                            continue;
                        }

                        // A parabolic cross-section: full depth at the
                        // centreline, tapering to nothing at the bank. A
                        // flat-bottomed trench with vertical sides would be a
                        // wall for water to pond behind, which is the failure
                        // this stage has to avoid.
                        let across = distance / half_width;
                        let here = depth * (1.0 - across * across);

                        let index = (tz - start_z) as usize * EDGE as usize + tx as usize;
                        if let Some(slot) = band.get_mut(index)
                            && here > *slot
                        {
                            *slot = here;
                        }
                    }
                }
            }
        }

        channel_cells
    });
    let channel_cells: usize = channel_counts.iter().sum();

    let mut surface = surface;
    let mut carved_cells = 0usize;
    let mut deepest = 0.0f32;

    for (index, depth) in cut.iter().enumerate() {
        if *depth <= 0.0 {
            continue;
        }
        let Some(cell) = unflat(index as u32) else {
            continue;
        };
        surface.set(cell, surface.get(cell) - *depth);
        carved_cells += 1;
        deepest = deepest.max(*depth);
    }

    // Kept before the fill consumes it. This is the ground; what comes back
    // from the fill is the ground plus whatever standing water sits on it.
    let ground = surface.clone();

    let rebuilt = FlowNetwork::build_sealed(surface, seal);
    let report = CarveReport {
        channel_cells,
        carved_cells,
        deepest,
        interior_sinks: rebuilt.interior_sinks(),
    };

    Carved {
        drained: rebuilt.filled().clone(),
        ground,
        network: rebuilt,
        report,
    }
}

fn unflat(index: u32) -> Option<ErosionCell> {
    ErosionCell::new(index % EDGE, index / EDGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slope with a converging valley, so accumulation actually builds.
    fn valley() -> BlockGrid {
        let mut grid = BlockGrid::filled(0.0);
        let centre = (EDGE / 2) as f32;

        for z in 0..EDGE {
            for x in 0..EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                // Falls along +Z, with a V pressed into it along x = centre so
                // flow collects into one trunk.
                let along = 400.0 - z as f32 * 0.05;
                let across = (x as f32 - centre).abs() * 0.02;
                grid.set(cell, along + across);
            }
        }
        grid
    }

    fn carved(settings: CarveSettings) -> (BlockGrid, BlockGrid, CarveReport) {
        let before = FlowNetwork::build(valley());
        let baseline = before.filled().clone();
        let carved = carve(
            baseline.clone(),
            &before,
            settings,
            &crate::flow::BlockBoundary::open(),
        );
        (baseline, carved.drained, carved.report)
    }

    /// **`no-erosion` leaves the surface alone** (S07).
    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn carving_nothing_changes_nothing() {
        let (before, after, report) = carved(CarveSettings::NONE);

        assert_eq!(report.channel_cells, 0);
        assert_eq!(report.carved_cells, 0);

        for z in (0..EDGE).step_by(97) {
            for x in (0..EDGE).step_by(97) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                assert_eq!(after.get(cell), before.get(cell));
            }
        }
    }

    /// **The hydraulic geometry S08 specifies**, as arithmetic.
    ///
    /// Exponents well under one are the whole reason a drainage network looks
    /// like a network: a hundredfold catchment is ten times as wide, not a
    /// hundred. Wiring these to the wrong exponent produces a trunk river that
    /// dwarfs everything feeding it.
    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn width_and_depth_follow_the_specified_power_laws() {
        let settings = CarveSettings {
            max_depth: f32::INFINITY,
            max_width: f32::INFINITY,
            ..CarveSettings::default()
        };

        let small = 1.0e6f32;
        let large = small * 100.0;

        let width_ratio = settings.width_for(large) / settings.width_for(small);
        let depth_ratio = settings.depth_for(large) / settings.depth_for(small);

        // 100^0.5 = 10, 100^0.4 = 6.31.
        assert!(
            (width_ratio - 10.0).abs() < 0.1,
            "width scaled by {width_ratio} for 100x the catchment; S08 says Q^0.5"
        );
        assert!(
            (depth_ratio - 6.31).abs() < 0.1,
            "depth scaled by {depth_ratio} for 100x the catchment; S08 says Q^0.4"
        );
    }

    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn nothing_below_the_threshold_is_carved() {
        let settings = CarveSettings::default();
        assert_eq!(settings.depth_for(settings.channel_threshold - 1.0), 0.0);
        assert_eq!(settings.width_for(settings.channel_threshold - 1.0), 0.0);
        assert!(settings.depth_for(settings.channel_threshold * 10.0) > 0.0);
    }

    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn the_caps_bound_an_unbounded_power_law() {
        let settings = CarveSettings::default();
        // A catchment far larger than any block could hold.
        let vast = 1.0e12f32;
        assert_eq!(settings.depth_for(vast), settings.max_depth);
        assert_eq!(settings.width_for(vast), settings.max_width);
    }

    /// **Carving only ever lowers**, and only near channels.
    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn carving_lowers_channels_and_leaves_hillslopes_alone() {
        let (before, after, report) = carved(CarveSettings::default());

        assert!(
            report.channel_cells > 0,
            "the fixture produced no channels, so this test asserts nothing"
        );
        assert!(report.carved_cells >= report.channel_cells);

        let mut lowered = 0usize;
        for z in (0..EDGE).step_by(13) {
            for x in (0..EDGE).step_by(13) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                let change = after.get(cell) - before.get(cell);
                assert!(
                    change <= 0.001,
                    "({x}, {z}) was raised by {change} m, which carving cannot do"
                );
                if change < -0.001 {
                    lowered += 1;
                }
            }
        }

        assert!(lowered > 0, "nothing was lowered at all");
    }

    /// **The property carving most easily breaks.**
    ///
    /// Cutting a trench into a surface is exactly how to make water disappear
    /// into one. Counted rather than argued.
    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn carving_leaves_no_water_trapped() {
        let (_, _, report) = carved(CarveSettings::default());
        assert_eq!(
            report.interior_sinks, 0,
            "carving left {} cells with nowhere to drain",
            report.interior_sinks
        );
    }

    /// A deeper setting cuts deeper. Without this, the coefficient could be
    /// wired to nothing and every test above would still pass.
    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn a_larger_coefficient_cuts_deeper() {
        let (_, _, shallow) = carved(CarveSettings {
            depth_coefficient: 0.004,
            ..CarveSettings::default()
        });
        let (_, _, deep) = carved(CarveSettings {
            depth_coefficient: 0.016,
            ..CarveSettings::default()
        });

        assert!(
            deep.deepest > shallow.deepest * 1.5,
            "four times the coefficient cut {} m against {} m",
            deep.deepest,
            shallow.deepest
        );
    }

    /// The same surface carves the same way twice (`ADR-0006`).
    #[test]
    #[ignore = "block-scale; the worldgen gate runs every ignored test in release"]
    fn carving_is_reproducible() {
        let (_, first, first_report) = carved(CarveSettings::default());
        let (_, second, second_report) = carved(CarveSettings::default());

        assert_eq!(first_report, second_report);
        for z in (0..EDGE).step_by(37) {
            for x in (0..EDGE).step_by(37) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                assert_eq!(first.get(cell), second.get(cell));
            }
        }
    }
}
