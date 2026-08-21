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
//! # The grid-bias artifact, and where it actually came from
//!
//! Eroding a block of bare noise produced a herringbone over every hillside and
//! channels in hard 45-degree runs — terrain that looked like a circuit board.
//! Three fixes were tried. **All three were wrong**, and they are recorded
//! because each is the kind of thing that sounds obviously right:
//!
//! 1. **Split the accumulation** across all downslope neighbours instead of
//!    following D8, on the theory that the bias entered through `A^m`. The
//!    channel network improved; the surface striping did not change.
//! 2. **Let thermal erosion plane it off** (step 4) — the grooves exceed the
//!    talus angle, so relaxation ought to remove them. It moved 12.5 million
//!    metres of material and the herringbone survived.
//! 3. **Multi-receiver incision** — lower each cell towards a weighted average
//!    of its receivers rather than the single steepest, so incision is no longer
//!    confined to eight rays. No visible change either.
//!
//! The actual cause is **filled basins**. Base elevation is scale-free fractal
//! noise with no regional drainage, so roughly a third of a block is closed
//! basin. Flat resolution gives those basins a BFS-distance gradient, which is
//! geometric — parallel combs at 45 degrees, visible in the accumulation before
//! any erosion runs at all. Erosion then carves exactly those combs, and the
//! feedback amplifies them into the landscape.
//!
//! Confirmed by removing the basins rather than by reasoning: with a regional
//! tilt of 40 m/km, the same code on the same seed produces dendritic,
//! organic-looking valleys and **no herringbone at all**.
//!
//! So the fix is the **world map** — M2's first deliverable, supplying the
//! coarse elevation and drainage that make a block drain somewhere instead of
//! ponding. Not a change to this module. An earlier check had dismissed that
//! possibility, but it measured *basin fraction* (32% to 17% under tilt) rather
//! than the artifact, and concluded the wrong thing from the right number.
//!
//! None of this was visible in a test. Every assertion here passes against the
//! biased surface, because "channels cut more than hillslopes" is perfectly true
//! when the channels are straight lines. It took four renders.
//!
//! Incision is multi-receiver regardless — see [`incise`]. It did not fix the
//! artifact, but with the accumulation already split it is the consistent form,
//! and splitting the area while confining the incision to one ray is not a
//! position worth defending.
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
    /// How rock hardness varies by place ([`crate::hardness`]).
    ///
    /// `HardnessSettings::UNIFORM` reproduces the old single-constant world.
    pub hardness: crate::hardness::HardnessSettings,
    /// Erosion rounds. Each is one implicit solve plus one re-route.
    ///
    /// Zero is the `no-erosion` profile: the surface comes back untouched, and
    /// S07 requires that to still be a valid world.
    pub rounds: u32,
}

impl Default for ErosionSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl ErosionSettings {
    /// The default, as a constant so callers can stay `const`.
    pub const DEFAULT: Self = Self {
        erodibility: 4.0e-5,
        area_exponent: 0.5,
        timestep: 2.0e4,
        hardness: crate::hardness::HardnessSettings::DEFAULT,
        rounds: 12,
    };
}

impl ErosionSettings {
    /// The `no-erosion` profile (S07). A valid world, differing from `full-sim`
    /// only in terrain shape.
    pub const NONE: Self = Self {
        erodibility: 0.0,
        area_exponent: 0.5,
        timestep: 0.0,
        hardness: crate::hardness::HardnessSettings::UNIFORM,
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
    seed: u64,
    block: crate::block::BlockCoordinates,
    settings: ErosionSettings,
) -> (BlockGrid, FlowNetwork, ErosionReport) {
    // Sampled once and reused every round: hardness is a property of the rock,
    // not of the eroding surface, so it never needs recomputing mid-run.
    let hardness = crate::hardness::HardnessMap::for_block(seed, block, settings.hardness);

    let mut network = FlowNetwork::build(elevation);

    // The baseline is the **filled** surface, not the raw one. Step 2 runs
    // either way — a world with no erosion still needs drainage — so measuring
    // against raw elevation would report the fill's basin-raising as erosion,
    // and in a basin it would report it as erosion *raising ground*.
    let before = network.filled().clone();

    for _ in 0..settings.rounds {
        let eroded = incise(&network, &hardness, settings);

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
///
/// Multi-receiver: a cell is lowered towards a weighted average of **all** the
/// cells it drains to, not towards the single steepest one. For a cell with
/// receivers `r_j` carrying shares `w_j`:
///
/// ```text
/// z' = (z + Σ f_j·z_rj') / (1 + Σ f_j),   f_j = dt·K·A^m·w_j / dx_j
/// ```
///
/// Still a convex combination of the cell's old height and its receivers' new
/// ones, so it is still bounded by them and still unconditionally stable — the
/// single-receiver form is just this with one term.
///
/// # Why it is not the single-receiver form
///
/// Because that form prints the grid into the terrain. D8 offers eight
/// directions, so every groove snaps to a multiple of 45 degrees, and incision
/// is a feedback: a groove that captures more cuts deeper, which captures more.
/// Twelve rounds turn a landscape into a herringbone with channels in hard
/// diagonal runs.
///
/// This was built to remove that bias and **did not**. The cause turned out to
/// be filled basins rather than the incision — see the module docs. It is kept
/// because the accumulation is already split across receivers, and splitting the
/// area while confining the incision to a single ray is not a coherent pair.
///
/// The receivers are already solved when a cell is reached, because
/// [`crate::flow::FlowNetwork::drainage_order`] is a topological order over the
/// same multi-receiver graph the shares come from.
fn incise(
    network: &FlowNetwork,
    hardness: &crate::hardness::HardnessMap,
    settings: ErosionSettings,
) -> BlockGrid {
    let mut surface = network.filled().clone();

    for index in network.drainage_order().iter().rev() {
        let Some(cell) = FlowNetwork::cell_at(*index) else {
            continue;
        };

        let here = surface.get(cell);

        // Drainage *area*, not cell count: the law is in metres squared, and a
        // count would tie the result to the erosion grid's resolution, which
        // `ADR-0015` is free to change.
        let area = f64::from(network.accumulation(cell))
            * f64::from(cx_core::math::EROSION_CELL_SIZE).powi(2);
        // Erodibility scaled by the rock under this cell. Soft ground gives
        // way faster, hard ground resists — this one factor is where valley
        // width, cliff bands, and (eventually) waterfall steps come from.
        let common = f64::from(settings.timestep)
            * f64::from(settings.erodibility)
            * f64::from(hardness.multiplier(cell))
            * area.powf(f64::from(settings.area_exponent));

        let mut weight_sum = 0.0f64;
        let mut weighted_heights = 0.0f64;

        network.for_each_receiver(cell, |receiver, share| {
            let there = surface.get(receiver);

            // Nothing to cut towards. Inside a filled flat the receiver is at
            // the same height, and stream power is zero there anyway.
            if here <= there {
                return;
            }

            let distance = FlowNetwork::distance_to(cell, receiver);
            let factor = common * f64::from(share) / f64::from(distance);

            weight_sum += factor;
            weighted_heights += factor * f64::from(there);
        });

        if weight_sum <= 0.0 {
            // An outlet, or a cell with nothing below it. Held fixed: the
            // outlet is where the block's water leaves, and lowering it would
            // let the whole block erode downwards with nothing to erode
            // *towards*.
            continue;
        }

        let updated = (f64::from(here) + weighted_heights) / (1.0 + weight_sum);
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
        // Uniform, so these tests keep making the claims they were written to
        // make — about the solve, not about rock variation. Hardness has its
        // own test below and its own module tests.
        hardness: crate::hardness::HardnessSettings::UNIFORM,
        rounds: 4,
    };

    /// Every test erodes the same nominal block.
    fn at_origin() -> crate::block::BlockCoordinates {
        crate::block::BlockCoordinates::new(cx_core::math::BlockCoord::new(0, 0))
    }

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
        let (after, _, report) = erode(rough_slope(), 7, at_origin(), ErosionSettings::NONE);

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
        let (after, _, report) = erode(rough_slope(), 7, at_origin(), TEST_SETTINGS);

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
        let (after, network, report) = erode(rough_slope(), 7, at_origin(), settings);

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

        let (_, _, gentle_report) = erode(rough_slope(), 7, at_origin(), gentle);
        let (_, _, fierce_report) = erode(rough_slope(), 7, at_origin(), fierce);

        assert!(
            fierce_report.mean_lowering > gentle_report.mean_lowering * 1.5,
            "eight times the erodibility removed {} m against {} m, so the \
             parameter is barely connected to the result",
            fierce_report.mean_lowering,
            gentle_report.mean_lowering
        );
    }

    /// Soft rock loses more material than hard rock on the same terrain.
    ///
    /// This is the test that proves the hardness map is actually wired into
    /// the solve. Every other test here uses UNIFORM hardness, so without this
    /// one, the multiplier could be dropped on the floor and nothing would
    /// notice.
    #[test]
    fn softer_ground_erodes_faster_than_harder_ground() {
        let settings = ErosionSettings {
            hardness: crate::hardness::HardnessSettings {
                contrast: 16.0,
                ..crate::hardness::HardnessSettings::DEFAULT
            },
            ..TEST_SETTINGS
        };

        let before = FlowNetwork::build(rough_slope()).filled().clone();
        let (after, _, _) = erode(rough_slope(), 7, at_origin(), settings);

        let map = crate::hardness::HardnessMap::for_block(7, at_origin(), settings.hardness);

        // Split sampled cells into the softest and hardest thirds and compare
        // average material lost. The terrain fixture is the same everywhere,
        // so any systematic difference between the groups is the rock.
        let mut soft = (0.0f64, 0u32);
        let mut hard = (0.0f64, 0u32);

        for z in (0..EDGE).step_by(17) {
            for x in (0..EDGE).step_by(17) {
                let Some(cell) = ErosionCell::new(x, z) else {
                    continue;
                };
                let lost = f64::from(before.get(cell) - after.get(cell));
                match map.hardness(cell) {
                    0..=85 => soft = (soft.0 + lost, soft.1 + 1),
                    170..=255 => hard = (hard.0 + lost, hard.1 + 1),
                    _ => {}
                }
            }
        }

        assert!(
            soft.1 > 100 && hard.1 > 100,
            "not enough of each rock type sampled"
        );
        let soft_mean = soft.0 / f64::from(soft.1);
        let hard_mean = hard.0 / f64::from(hard.1);

        assert!(
            soft_mean > hard_mean * 1.5,
            "soft ground lost {soft_mean:.3} m against hard ground's {hard_mean:.3} m \
             — the hardness map is not reaching the solve"
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
        let (after, network, _) = erode(rough_slope(), 7, at_origin(), TEST_SETTINGS);

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
