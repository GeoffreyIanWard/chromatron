//! The simulation clock and its delta type.
//!
//! Two rules from `03-conventions.md` shape this module:
//!
//! - Tick duration is `u64` microseconds, **never a float**. Accumulating a
//!   float dt drifts, and drift in a deterministic simulation is divergence.
//! - Sim systems receive [`Fixed`], not `f32`, so that using frame time inside
//!   the sim is visible in review rather than silently compiling.

use std::fmt;

/// Default tick duration in microseconds — 30 Hz.
///
/// The *default*, not the rate. S03 makes the tick rate configurable within
/// 10–120 Hz, and `TickClock` takes its duration from config rather than from
/// this constant. Everything here is rate-agnostic integer arithmetic, so a
/// different rate needs no changes in this module — but the chosen rate is part
/// of world identity and is recorded in saves and replays (S13).
pub const TICK_US: u64 = 33_333;

/// The canonical simulation clock.
///
/// Wall-clock time never enters sim logic; this is the only notion of "when"
/// the sim has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Tick(pub u64);

impl Tick {
    /// The tick before the simulation starts.
    pub const ZERO: Tick = Tick(0);

    /// The next tick.
    pub const fn next(self) -> Tick {
        Tick(self.0.saturating_add(1))
    }

    /// Ticks elapsed from `earlier` to `self`, saturating at zero.
    pub const fn since(self, earlier: Tick) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    /// Elapsed simulated time at a given tick duration.
    pub const fn elapsed(self, tick_us: u64) -> Fixed {
        Fixed::from_micros(self.0.saturating_mul(tick_us))
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tick {}", self.0)
    }
}

/// A simulation-side duration, stored as whole microseconds.
///
/// Deliberately awkward to mix with render-rate `f32` seconds: the conversion is
/// [`Fixed::as_secs_f32`], named so that a reviewer can see frame time entering
/// sim logic. There is no `From<f32>`, because that is the direction that would
/// be a bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Fixed(u64);

impl Fixed {
    /// Zero duration.
    pub const ZERO: Fixed = Fixed(0);

    /// One default tick at 30 Hz.
    pub const TICK: Fixed = Fixed(TICK_US);

    /// A duration from whole microseconds.
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }

    /// A duration from whole milliseconds.
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis.saturating_mul(1_000))
    }

    /// Whole microseconds.
    pub const fn as_micros(self) -> u64 {
        self.0
    }

    /// Seconds as `f32`, for the few places that genuinely need it — physics
    /// integration and render-side interpolation.
    ///
    /// Every call site is a place to ask whether the value is about to be
    /// accumulated. Accumulating these instead of counting ticks reintroduces
    /// the drift the integer representation exists to prevent.
    pub fn as_secs_f32(self) -> f32 {
        self.0 as f32 / 1_000_000.0
    }

    /// Seconds as `f64`, for diagnostics and reporting.
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }

    /// Sum, saturating rather than wrapping.
    pub const fn saturating_add(self, other: Fixed) -> Fixed {
        Fixed(self.0.saturating_add(other.0))
    }

    /// Difference, saturating at zero.
    pub const fn saturating_sub(self, other: Fixed) -> Fixed {
        Fixed(self.0.saturating_sub(other.0))
    }

    /// Scaled by an integer factor.
    pub const fn saturating_mul(self, factor: u64) -> Fixed {
        Fixed(self.0.saturating_mul(factor))
    }

    /// How many whole `step`s fit in this duration, and what is left over.
    ///
    /// This is the fixed-timestep accumulator in one function: integer division,
    /// no float remainder, so a long-running session cannot drift.
    pub const fn divide(self, step: Fixed) -> (u64, Fixed) {
        if step.0 == 0 {
            return (0, self);
        }
        (self.0 / step.0, Fixed(self.0 % step.0))
    }

    /// Position within `step` as a fraction in `[0, 1)`, for render
    /// interpolation (`alpha` in S03).
    pub fn fraction_of(self, step: Fixed) -> f32 {
        if step.0 == 0 {
            return 0.0;
        }
        (self.0 % step.0) as f32 / step.0 as f32
    }
}

impl fmt::Display for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3} ms", self.0 as f64 / 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tick_is_30hz() {
        assert_eq!(TICK_US, 33_333);
        // 30 Hz is 33,333.33 µs. The third of a microsecond is dropped
        // deliberately: an integer tick that is stable forever beats an exact
        // rate that drifts.
        assert!((1_000_000.0 / TICK_US as f64 - 30.0).abs() < 0.001);
    }

    #[test]
    fn accumulator_does_not_drift_over_a_long_session() {
        // Ten hours at 30 Hz. A f32 accumulator would have visibly drifted by
        // here; integer division cannot.
        const TICKS: u64 = 30 * 60 * 60 * 10;

        let mut accumulator = Fixed::ZERO;
        let mut ticks_run = 0u64;
        for _ in 0..TICKS {
            accumulator = accumulator.saturating_add(Fixed::TICK);
            let (whole, remainder) = accumulator.divide(Fixed::TICK);
            ticks_run += whole;
            accumulator = remainder;
        }

        assert_eq!(ticks_run, TICKS);
        assert_eq!(accumulator, Fixed::ZERO, "no residue should accumulate");
    }

    #[test]
    fn divide_yields_whole_steps_and_remainder() {
        let elapsed = Fixed::from_micros(TICK_US * 3 + 100);
        let (steps, remainder) = elapsed.divide(Fixed::TICK);
        assert_eq!(steps, 3);
        assert_eq!(remainder, Fixed::from_micros(100));
    }

    #[test]
    fn divide_by_zero_step_yields_no_steps_rather_than_panicking() {
        // Sim crates do not panic in release (03-conventions.md), and a
        // misconfigured tick rate of zero must degrade rather than abort.
        let (steps, remainder) = Fixed::from_micros(500).divide(Fixed::ZERO);
        assert_eq!(steps, 0);
        assert_eq!(remainder, Fixed::from_micros(500));
    }

    #[test]
    fn interpolation_fraction_is_within_unit_range() {
        let half = Fixed::from_micros(TICK_US / 2);
        assert!((half.fraction_of(Fixed::TICK) - 0.5).abs() < 0.001);
        assert!((Fixed::ZERO.fraction_of(Fixed::TICK)).abs() < f32::EPSILON);
    }

    #[test]
    fn tick_elapsed_matches_tick_count() {
        assert_eq!(
            Tick(100).elapsed(TICK_US),
            Fixed::from_micros(TICK_US * 100)
        );
        assert_eq!(Tick(10).since(Tick(4)), 6);
        assert_eq!(Tick(4).since(Tick(10)), 0, "should saturate, not wrap");
    }
}
