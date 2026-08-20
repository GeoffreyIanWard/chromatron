//! Step 3 of S07's pipeline: hydraulic erosion.
//!
//! The stage the whole block-granularity decision exists for. A cell's final
//! height depends on its neighbours over many rounds and on how much of the
//! landscape drains through it, so it cannot be a pure function of one
//! coordinate — which is why `ADR-0006` raised generation from the chunk to the
//! block, and why blocks carry a halo.
//!
//! # Stream power
//!
//! `dz/dt = -K · A^m · S^n` — a cell lowers in proportion to how much drains
//! through it (`A`, the accumulation from step 2) and how steeply it falls
//! (`S`). That is the standard detachment-limited incision law, and the reason
//! it produces landscapes rather than noise is that both terms are feedbacks:
//! a channel that captures more drainage cuts faster, cutting faster captures
//! more drainage, and valleys emerge from a surface that had none.
//!
//! # Implicit, not explicit
//!
//! The obvious scheme is `z -= dt · K · A^m · S^n`, evaluated everywhere from
//! last round's heights. It is also a trap. Explicit stream power is only stable
//! below a timestep that shrinks as `A` grows, and `A` here spans one cell to
//! eight million — so the stable step is set by the largest river in the block
//! and every hillside is integrated with a step thousands of times smaller than
//! it needs. Exceed it and a cell cuts below its own outlet, which reverses the
//! gradient, which cuts harder: the channel drills a hole and the surface goes
//! to pieces around it.
//!
//! So this uses the implicit scheme of Braun & Willett (2013). Each cell is
//! solved against its receiver's **already-updated** height:
//!
//! ```text
//! z' = (z + dt·K·A^m·z_r'/dx) / (1 + dt·K·A^m/dx)
//! ```
//!
//! which is a weighted average of the cell's old height and its receiver's new
//! one. Being an average, the result is between them — a cell can approach its
//! receiver's height but never cross it, at any timestep. The scheme is
//! unconditionally stable, so the timestep becomes a knob for *how much erosion*
//! rather than a stability constraint to respect.
//!
//! It needs each cell solved after its receiver, which is exactly
//! [`crate::flow::FlowNetwork::drainage_order`] reversed. One pass, no
//! convergence loop, and therefore no iteration count that silently changes the
//! terrain when someone tunes it.
//!
//! # Why `n = 1`
//!
//! The closed form above holds only for a slope exponent of one. Other values
//! need a Newton solve per cell per round, and the visual difference is a
//! change in how concave channel profiles are — worth having eventually, not
//! worth 26 million Newton iterations a round to get at M2.
//!
//! # Known artifact: D8 grid bias in the incised surface
//!
//! Erosion incises each cell towards **one** receiver, and D8 offers only eight
//! of them. On a smooth surface every groove therefore snaps to a multiple of 45
//! degrees, and because incision is a feedback — a groove that captures more
//! cuts deeper, and cutting deeper captures more — the bias compounds over
//! rounds rather than averaging out. Rendered, an eroded block shows a
//! herringbone texture over its hillsides and channels running in hard diagonal
//! and orthogonal segments.
//!
//! This is visible and it is not fixed here. What *was* tried: flow accumulation
//! now splits across every downslope neighbour rather than following D8
//! ([`crate::flow`]'s `accumulate`), on the theory that the bias entered through
//! the `A^m` term. It made the channel network noticeably more natural and left
//! the surface striping essentially unchanged — so the diagnosis was wrong, and
//! the bias comes from the single-receiver *incision*, not from the area.
//!
//! The honest options from here, in order of preference:
//!
//! 1. **Thermal erosion (step 4)** relaxes slopes past a talus angle, which is
//!    exactly what these grooves exceed. It is the next stage and may remove
//!    most of this as a side effect of doing its own job. Worth measuring before
//!    building anything more complicated.
//! 2. **Multi-receiver incision** — lower each cell towards a weighted average
//!    of its receivers instead of one. The implicit scheme survives it, since a
//!    convex combination is still bounded by its inputs, but it is a real change
//!    to the solve.
//!
//! Recorded rather than quietly shipped, because every assertion in this module
//! passes against the biased surface: "channels cut more than hillslopes" is
//! perfectly true when the channels are straight lines.
//!
//! # Re-routing
//!
//! Erosion changes the surface, so the drainage network computed from the
//! original surface stops describing it. Between rounds the network is rebuilt.
//! That is most of the cost, and it is not optional: without it, channels stay
//! wherever the *unerodedated* noise happened to put them and never capture each
//! other, which is precisely the process that turns a fractal surface into a
//! landscape.

use crate::block::{BlockGrid, ErosionCell};
use crate::flow::FlowNetwork;

/// How erosion is shaped.
///
/// Named parameters rather than constants in the loop: these are what a world
/// preset turns, and `no-erosion` in S07's profile list is this struct with
/// [`Self::rounds`] at zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ErosionSettings {
    /// Erodibility, `K`. How fast rock yields, in the units the law implies.
    ///
    /// Physically this is a rock property; here it is the master intensity
    /// knob, because it multiplies the whole term.
    pub erodibility: f32,
    /// Drainage-area exponent, `m`. 0.5 is the conventional value and the one
    /// that makes channel concavity look like real rivers.
    pub area_exponent: f32,
    /// Timestep per round. Unconditionally stable, so this sets how far the
    /// landscape evolves rather than whether the solve survives.
    pub timestep: f32,
    /// Erosion rounds. Each is one implicit solve plus one re-route.
    ///
    /// Zero is the `no-erosion` profile: the surface comes back untouched, and
    /// S07 requires that to still be a valid world.
    pub rounds: u32,
}

impl Default for ErosionSettings {
    fn default() -> Self {
        Self {
            erodibility: 4.0e-5,
            area_exponent: 0.5,
            timestep: 2.0e4,
            rounds: 12,
        }
    }
}

impl ErosionSettings {
    /// The `no-erosion` profile (S07). A valid world, differing from `full-sim`
    /// only in terrain shape.
    pub const NONE: Self = Self {
        erodibility: 0.0,
        area_exponent: 0.5,
        timestep: 0.0,
        rounds: 0,
    };
}

/// What one run of erosion did, for recording rather than for gating.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ErosionReport {
    /// Rounds actually run.
    pub rounds: u32,
    /// Mean height lost over the core, in metres. Signed: erosion only ever
    /// lowers, so a positive number here would be a bug.
    pub mean_lowering: f32,
    /// The deepest single cell incision over the core, in metres.
    pub deepest: f32,
    /// Interior sinks in the final network. Must be zero, or the surface that
    /// comes out of erosion cannot be drained by the stages after it.
    pub interior_sinks: usize,
}

/// Erodes a surface, returning it and the flow network that describes it.
///
/// Takes elevation by value: the uneroded surface has no further use, and
/// holding both would double the largest allocation in the pipeline.
///
/// The network comes back because step 5 (channel carving) and step 7 (static
/// field derivation) both need the drainage of the *final* surface, and
/// recomputing it would repeat the most expensive part of this function.
pub fn erode(
    elevation: BlockGrid,
    settings: ErosionSettings,
) -> (BlockGrid, FlowNetwork, ErosionReport) {
    let mut network = FlowNetwork::build(elevation);

    // The baseline is the **filled** surface, not the raw one. Step 2 runs
    // either way — a world with no erosion still needs drainage — so measuring
    // against raw elevation would report the fill's basin-raising as erosion,
    // and in a basin it would report it as erosion *raising ground*.
    let before = network.filled().clone();

    for _ in 0..settings.rounds {
        let eroded = incise(&network, settings);

        // Re-route against the surface erosion just produced. Skipping this
        // freezes channels where the unerodedated noise put them; capture — one
        // channel cutting back into another's catchment — is the process that
        // makes a fractal surface into a landscape, and capture cannot happen
        // if the network never changes.
        network = FlowNetwork::build(eroded);
    }

    let report = measure(&before, &network, settings.rounds);
    let filled = network.filled().clone();

    (filled, network, report)
}

/// One implicit stream-power solve over the whole grid.
fn incise(network: &FlowNetwork, settings: ErosionSettings) -> BlockGrid {
    let mut surface = network.filled().clone();

    // Receivers before donors. `drainage_order` is donors-first by
    // construction, so this walks it backwards — and every cell then reads a
    // receiver height that has already been updated this round, which is the
    // whole basis of the implicit scheme.
    for index in network.drainage_order().iter().rev() {
        let Some(cell) = FlowNetwork::cell_at(*index) else {
            continue;
        };
        let Some(receiver) = network.downstream(cell) else {
            // An outlet. Held fixed: it is where the block's water leaves, and
            // lowering it would let the whole block erode away downwards with
            // nothing to erode *towards*.
            continue;
        };
        let Some(distance) = network.distance_downstream(cell) else {
            continue;
        };

        let here = surface.get(cell);
        let there = surface.get(receiver);

        // Already at or below its receiver — inside a filled flat, where there
        // is no gradient to erode along. Stream power is zero there anyway;
        // skipping avoids spending the arithmetic to prove it.
        if here <= there {
            continue;
        }

        // Drainage *area*, not cell count: the law is in metres squared, and a
        // count would make the result depend on the erosion grid's resolution.
        // With `ADR-0015` free to change that resolution, an area keeps the
        // same landscape at any cell size.
        let area = f64::from(network.accumulation(cell))
            * f64::from(cx_core::math::EROSION_CELL_SIZE).powi(2);

        let factor = f64::from(settings.timestep)
            * f64::from(settings.erodibility)
            * area.powf(f64::from(settings.area_exponent))
            / f64::from(distance);

        // The weighted average. `factor` is non-negative, so the result lies
        // between `there` and `here` however large the timestep is — which is
        // the unconditional stability, visible in one line.
        let updated = (f64::from(here) + factor * f64::from(there)) / (1.0 + factor);

        surface.set(cell, updated as f32);
    }

    surface
}

/// Measures what erosion did, over the core only.
///
/// Core only for the reason [`ErosionCell::is_core`] gives: halo cells are
/// computed with less surrounding context, and folding them in would make the
/// measurement partly a statement about the halo.
fn measure(before: &BlockGrid, network: &FlowNetwork, rounds: u32) -> ErosionReport {
    use cx_core::math::EROSION_CELLS_PER_BLOCK_EDGE;

    let low = crate::block::HALO_CELLS;
    let high = low + EROSION_CELLS_PER_BLOCK_EDGE;

    let mut total = 0.0f64;
    let mut counted = 0u64;
    let mut deepest = 0.0f32;

    for z in low..high {
        for x in low..high {
            let Some(cell) = ErosionCell::new(x, z) else {
                continue;
            };
            let lost = before.get(cell) - network.filled().get(cell);
            total += f64::from(lost);
            counted += 1;
            deepest = deepest.max(lost);
        }
    }

    ErosionReport {
        rounds,
        mean_lowering: if counted == 0 {
            0.0
        } else {
            (total / counted as f64) as f32
        },
        deepest,
        interior_sinks: network.interior_sinks(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::EDGE;

    /// Fewer rounds than the default, because every round re-routes a
    /// 26-million-cell grid and these run on every `cargo test`.
    const TEST_SETTINGS: ErosionSettings = ErosionSettings {
        erodibility: 4.0e-5,
        area_exponent: 0.5,
        timestep: 2.0e4,
        rounds: 4,
    };

    /// A tilted plane with noise on it, small enough to erode quickly.
    fn rough_slope() -> BlockGrid {
        let mut grid = BlockGrid::filled(0.0);
        for z in 0..EDGE {
            for x in 0..EDGE {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                // A slope plus a deterministic ripple, so channels have
                // something to organise around.
                let base = 200.0 - x as f32 * 0.02;
                let ripple = ((x as f32 * 0.11).sin() + (z as f32 * 0.17).sin()) * 1.5;
                grid.set(cell, base + ripple);
            }
        }
        grid
    }

    /// **`no-erosion` is a valid profile, not a broken one** (S07).
    ///
    /// Compared against the *filled* surface rather than the raw one, because
    /// step 2 runs either way — a world without erosion still needs drainage.
    /// Comparing against raw would call the fill's basin-raising a difference.
    #[test]
    fn zero_rounds_changes_nothing_the_fill_did_not() {
        let filled = FlowNetwork::build(rough_slope()).filled().clone();
        let (after, _, report) = erode(rough_slope(), ErosionSettings::NONE);

        assert_eq!(report.rounds, 0);
        assert_eq!(report.mean_lowering, 0.0, "no-erosion removed material");

        for z in (0..EDGE).step_by(97) {
            for x in (0..EDGE).step_by(97) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                assert_eq!(
                    after.get(cell),
                    filled.get(cell),
                    "no-erosion changed ({x}, {z}), so it is not the identity it \
                     claims to be"
                );
            }
        }
    }

    /// Erosion only ever lowers ground.
    #[test]
    fn nothing_is_ever_raised_by_erosion() {
        let filled = FlowNetwork::build(rough_slope()).filled().clone();
        let (after, _, report) = erode(rough_slope(), TEST_SETTINGS);

        assert!(
            report.mean_lowering > 0.0,
            "erosion removed nothing at all: mean lowering {}",
            report.mean_lowering
        );

        // Against the filled input, so a basin the fill raised does not read as
        // erosion raising ground. Erosion itself must never add material.
        for z in (0..EDGE).step_by(37) {
            for x in (0..EDGE).step_by(37) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                let raised = after.get(cell) - filled.get(cell);
                assert!(
                    raised <= 0.001,
                    "({x}, {z}) ended {raised} m above the surface erosion was \
                     given, which erosion cannot do"
                );
            }
        }
    }

    /// **The stability claim, tested where an explicit scheme would explode.**
    ///
    /// A thousand-fold timestep. Explicit stream power would drive cells below
    /// their receivers, reverse the gradient, and tear the surface apart. The
    /// implicit form is a weighted average, so the worst it can do is flatten
    /// everything to the outlet height.
    #[test]
    fn an_absurd_timestep_flattens_rather_than_exploding() {
        let settings = ErosionSettings {
            timestep: 2.0e7,
            rounds: 3,
            ..TEST_SETTINGS
        };
        let (after, network, report) = erode(rough_slope(), settings);

        assert_eq!(
            report.interior_sinks, 0,
            "a large timestep produced terrain that cannot be drained"
        );

        for z in (0..EDGE).step_by(53) {
            for x in (0..EDGE).step_by(53) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                let height = after.get(cell);
                assert!(
                    height.is_finite(),
                    "({x}, {z}) is {height} — the solve diverged"
                );
                assert!(
                    (-1_000.0..=1_000.0).contains(&height),
                    "({x}, {z}) reached {height} m, far outside the 200 m the \
                     surface started at: the scheme is not stable"
                );
            }
        }

        // And flow still runs downhill on whatever is left.
        let cell = ErosionCell::new(2_000, 2_000).expect("in range");
        if let Some(next) = network.downstream(cell) {
            assert!(network.filled().get(next) <= network.filled().get(cell));
        }
    }

    /// More erosion removes more material, monotonically.
    ///
    /// Without this, `erodibility` could be wired to nothing and every other
    /// test here would still pass — the surface would still be filled, still
    /// drain, still never rise.
    #[test]
    fn a_larger_erodibility_removes_more() {
        let gentle = ErosionSettings {
            erodibility: 1.0e-5,
            ..TEST_SETTINGS
        };
        let fierce = ErosionSettings {
            erodibility: 8.0e-5,
            ..TEST_SETTINGS
        };

        let (_, _, gentle_report) = erode(rough_slope(), gentle);
        let (_, _, fierce_report) = erode(rough_slope(), fierce);

        assert!(
            fierce_report.mean_lowering > gentle_report.mean_lowering * 1.5,
            "eight times the erodibility removed {} m against {} m, so the \
             parameter is barely connected to the result",
            fierce_report.mean_lowering,
            gentle_report.mean_lowering
        );
    }

    /// Erosion concentrates where drainage does.
    ///
    /// The whole point of `A^m`: channels cut and hillslopes do not. A run that
    /// lowered everything evenly would pass every test above while producing a
    /// surface that is merely shorter, not carved.
    #[test]
    fn incision_follows_drainage_rather_than_being_uniform() {
        let before = FlowNetwork::build(rough_slope()).filled().clone();
        let (after, network, _) = erode(rough_slope(), TEST_SETTINGS);

        let mut channel_loss = (0.0f64, 0u32);
        let mut slope_loss = (0.0f64, 0u32);

        for z in (0..EDGE).step_by(11) {
            for x in (0..EDGE).step_by(11) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                let lost = f64::from(before.get(cell) - after.get(cell));
                let area = network.accumulation(cell);

                if area > 10_000.0 {
                    channel_loss = (channel_loss.0 + lost, channel_loss.1 + 1);
                } else if area < 10.0 {
                    slope_loss = (slope_loss.0 + lost, slope_loss.1 + 1);
                }
            }
        }

        assert!(
            channel_loss.1 > 0 && slope_loss.1 > 0,
            "the fixture produced no channels ({} sampled) or no hillslopes ({})",
            channel_loss.1,
            slope_loss.1
        );

        let channel_mean = channel_loss.0 / f64::from(channel_loss.1);
        let slope_mean = slope_loss.0 / f64::from(slope_loss.1);

        assert!(
            channel_mean > slope_mean * 2.0,
            "channels lost {channel_mean:.3} m and hillslopes {slope_mean:.3} m — \
             erosion is uniform, so the drainage-area term is not doing anything"
        );
    }
}
