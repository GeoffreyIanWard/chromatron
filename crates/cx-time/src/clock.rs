//! The tick clock and its accumulator.
//!
//! One loop serves three masters: a windowed game at 60+ fps, a debug session
//! stepping one tick at a time, and a headless batch run at 10,000x. They are
//! the same code path with different drivers, and this is the part they share.
//!
//! Everything here is integer microseconds. A float accumulator drifts, and
//! drift in a deterministic simulation is divergence (`03-conventions.md`).

use cx_core::{Fixed, Tick};

use crate::control::TimeControl;
use crate::error::TimeError;

/// Longest real-time step the clock will honour, in microseconds.
///
/// A frame that took longer — a breakpoint, a stalled disk, a laptop lid — is
/// clamped rather than believed. Without this, one 30-second pause would try to
/// run 900 ticks in the next frame and the sim would never catch up.
pub const MAX_FRAME_DELTA_US: u64 = 250_000;

/// Most catch-up ticks the clock will report for a single frame.
///
/// 7, not a rounder number: `MAX_FRAME_DELTA_US` (250 ms) already limits a frame
/// to 7.5 ticks at the default 30 Hz rate, so the frame clamp is the guard that
/// actually binds and a cap of 8 could never be reached. This constant is set to
/// the number the clamp can actually produce, so it describes real behaviour
/// instead of an unreachable ceiling. At a faster configured rate the clamp
/// admits more ticks per frame and this cap becomes the real limit instead.
pub const MAX_CATCHUP: u64 = 7;

/// Slowest supported tick rate, in Hz.
pub const MIN_TICK_HZ: u64 = 10;

/// Fastest supported tick rate, in Hz.
pub const MAX_TICK_HZ: u64 = 120;

/// A validated tick rate.
///
/// Part of world identity (S03, `ADR-0012`): the same command stream at 10 Hz
/// and 30 Hz is not the same run, so this is recorded in saves and replays and a
/// mismatch on load refuses rather than diverging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TickRate {
    micros: u64,
}

impl Default for TickRate {
    fn default() -> Self {
        Self {
            micros: cx_core::TICK_US,
        }
    }
}

impl TickRate {
    /// A rate from whole hertz, validated against the supported range.
    pub const fn from_hz(hz: u64) -> Result<Self, TimeError> {
        if hz < MIN_TICK_HZ || hz > MAX_TICK_HZ {
            return Err(TimeError::UnsupportedTickRate { hz });
        }
        Ok(Self {
            micros: 1_000_000 / hz,
        })
    }

    /// The 30 Hz default.
    pub const fn default_rate() -> Self {
        Self {
            micros: cx_core::TICK_US,
        }
    }

    /// Tick duration.
    pub const fn step(self) -> Fixed {
        Fixed::from_micros(self.micros)
    }

    /// Tick duration in microseconds.
    pub const fn micros(self) -> u64 {
        self.micros
    }

    /// Approximate rate in hertz, for display.
    pub fn hz(self) -> f64 {
        1_000_000.0 / self.micros as f64
    }
}

/// What one call to [`TickClock::advance`] produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CatchUp {
    /// Ticks the caller should now run.
    pub ticks: u64,
    /// Whether simulated time was dropped, by either guard.
    ///
    /// Reported rather than absorbed: silently slowing the simulation is what
    /// makes "why is my game running in slow motion" unanswerable (S03 calls for
    /// a `SimFallingBehind` diagnostic).
    ///
    /// At the default 30 Hz rate the frame clamp is the guard that actually
    /// binds — 250 ms of clamped delta is 7.5 ticks, which is why
    /// `MAX_CATCHUP` is 7. Both guards set this flag regardless of which one
    /// fired, since either way simulated time was dropped.
    pub fell_behind: bool,
    /// Real time discarded by the frame-delta clamp.
    pub discarded: Fixed,
}

/// The simulation clock.
#[derive(Debug, Clone)]
pub struct TickClock {
    tick: Tick,
    accumulator: Fixed,
    rate: TickRate,
}

impl Default for TickClock {
    fn default() -> Self {
        Self::new(TickRate::default())
    }
}

impl TickClock {
    /// A clock at tick zero.
    pub const fn new(rate: TickRate) -> Self {
        Self {
            tick: Tick::ZERO,
            accumulator: Fixed::ZERO,
            rate,
        }
    }

    /// The current tick.
    pub const fn tick(&self) -> Tick {
        self.tick
    }

    /// The configured rate.
    pub const fn rate(&self) -> TickRate {
        self.rate
    }

    /// Unconsumed time, less than one tick.
    pub const fn accumulator(&self) -> Fixed {
        self.accumulator
    }

    /// Interpolation factor for the render frame, in `[0, 1)`.
    ///
    /// `alpha = accumulator / step` (S03). Extract blends the previous and
    /// current transforms by this; without it, a 30 Hz sim rendered at 144 Hz
    /// looks obviously wrong.
    pub fn alpha(&self) -> f32 {
        self.accumulator.fraction_of(self.rate.step())
    }

    /// Records that a tick has been run.
    pub const fn consume_tick(&mut self) {
        self.tick = self.tick.next();
    }

    /// Feeds real elapsed time in and reports how many ticks to run.
    ///
    /// The clamp, the multiplier, and the catch-up cap all apply here so that
    /// every driver gets the same behaviour rather than each reimplementing it.
    pub fn advance(&mut self, real_delta: Fixed, control: TimeControl) -> CatchUp {
        let clamped = Fixed::from_micros(real_delta.as_micros().min(MAX_FRAME_DELTA_US));
        let discarded = real_delta.saturating_sub(clamped);

        match control {
            TimeControl::Paused => CatchUp {
                ticks: 0,
                fell_behind: false,
                discarded,
            },
            TimeControl::Stepping { remaining } => {
                // Stepping ignores real time entirely: N steps means N ticks,
                // which is what makes "pause, step 5, resume" reproduce a
                // continuous 5-tick run exactly.
                CatchUp {
                    ticks: remaining.min(MAX_CATCHUP),
                    fell_behind: false,
                    discarded,
                }
            }
            TimeControl::Playing { multiplier } => {
                let scaled = scale(clamped, multiplier);
                self.accumulator = self.accumulator.saturating_add(scaled);

                let (available, remainder) = self.accumulator.divide(self.rate.step());
                let ticks = available.min(MAX_CATCHUP);

                // The backlog beyond the cap is dropped, not carried: keeping it
                // would guarantee the next frame is also over budget, which is
                // the spiral the cap exists to break.
                self.accumulator = remainder;

                let capped = available > MAX_CATCHUP;
                let clamped_away = discarded.as_micros() > 0;

                CatchUp {
                    ticks,
                    fell_behind: capped || clamped_away,
                    discarded,
                }
            }
        }
    }
}

/// Scales a duration by a speed multiplier, in integer microseconds.
///
/// The multiplier is a float because it is a user-facing setting, but the
/// arithmetic lands back on integers immediately — a scaled float accumulated
/// across a long session is exactly the drift this module exists to avoid.
fn scale(delta: Fixed, multiplier: f32) -> Fixed {
    if multiplier <= 0.0 {
        return Fixed::ZERO;
    }
    let scaled = (delta.as_micros() as f64 * multiplier as f64).round();
    Fixed::from_micros(scaled.max(0.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_rate_validates_its_range() {
        assert!(TickRate::from_hz(30).is_ok());
        assert!(TickRate::from_hz(10).is_ok());
        assert!(TickRate::from_hz(120).is_ok());
        assert!(matches!(
            TickRate::from_hz(5),
            Err(TimeError::UnsupportedTickRate { hz: 5 })
        ));
        assert!(TickRate::from_hz(240).is_err());
    }

    #[test]
    fn s03_acceptance_a_declared_tick_count_runs_exactly() {
        // Real time arrives in awkward slices; the tick count must not care.
        let mut clock = TickClock::default();
        let mut run = 0u64;

        for _ in 0..1_000 {
            let produced = clock.advance(
                Fixed::from_micros(17_000),
                TimeControl::Playing { multiplier: 1.0 },
            );
            for _ in 0..produced.ticks {
                clock.consume_tick();
                run += 1;
            }
        }

        // 1000 frames x 17 ms = 17 s of real time; at 30 Hz that is 510 ticks.
        assert_eq!(run, 510);
        assert_eq!(clock.tick(), cx_core::Tick(510));
    }

    #[test]
    fn s03_acceptance_a_long_stall_is_clamped_and_reported() {
        let mut clock = TickClock::default();

        let produced = clock.advance(
            Fixed::from_micros(2_000_000),
            TimeControl::Playing { multiplier: 1.0 },
        );

        // 2 s clamps to 250 ms, which at 30 Hz is 7.5 ticks — so the *clamp*
        // binds here, not the catch-up cap. Both guards report falling behind.
        assert_eq!(produced.ticks, 7);
        assert!(
            produced.ticks <= MAX_CATCHUP,
            "the catch-up cap must still hold"
        );
        assert!(
            produced.fell_behind,
            "falling behind must be reported, not absorbed"
        );
        assert!(
            produced.discarded.as_micros() == 1_750_000,
            "the clamp should report the real time it dropped, got {}",
            produced.discarded
        );
    }

    #[test]
    fn a_paused_clock_produces_no_ticks_and_does_not_bank_time() {
        let mut clock = TickClock::default();

        for _ in 0..100 {
            let produced = clock.advance(Fixed::from_micros(16_000), TimeControl::Paused);
            assert_eq!(produced.ticks, 0);
        }

        // Resuming must not release 1.6 seconds of banked time in one frame.
        let produced = clock.advance(
            Fixed::from_micros(16_000),
            TimeControl::Playing { multiplier: 1.0 },
        );
        assert_eq!(
            produced.ticks, 0,
            "no backlog should have accumulated while paused"
        );
    }

    #[test]
    fn stepping_produces_exactly_the_requested_ticks() {
        let mut clock = TickClock::default();
        let produced = clock.advance(Fixed::ZERO, TimeControl::Stepping { remaining: 5 });
        assert_eq!(produced.ticks, 5, "stepping ignores real time");
    }

    #[test]
    fn the_multiplier_scales_produced_ticks() {
        let mut clock = TickClock::default();
        let one_frame = Fixed::from_micros(33_333);

        let normal = clock.advance(one_frame, TimeControl::Playing { multiplier: 1.0 });
        assert_eq!(normal.ticks, 1);

        let mut fast = TickClock::default();
        let accelerated = fast.advance(one_frame, TimeControl::Playing { multiplier: 4.0 });
        assert_eq!(accelerated.ticks, 4);
    }

    #[test]
    fn alpha_stays_within_the_unit_interval() {
        let mut clock = TickClock::default();
        for step in 1..200u64 {
            clock.advance(
                Fixed::from_micros(step * 997),
                TimeControl::Playing { multiplier: 1.0 },
            );
            let alpha = clock.alpha();
            assert!((0.0..1.0).contains(&alpha), "alpha {alpha} out of range");
        }
    }

    #[test]
    fn a_slower_rate_produces_proportionally_fewer_ticks() {
        let rate = TickRate::from_hz(10).expect("10 Hz is supported");
        let mut clock = TickClock::new(rate);

        // One second of real time clamps to 250 ms, which at 10 Hz is 2.5 ticks.
        let produced = clock.advance(
            Fixed::from_micros(1_000_000),
            TimeControl::Playing { multiplier: 1.0 },
        );
        assert_eq!(produced.ticks, 2);
        assert!(produced.fell_behind, "clamped time is dropped time");
    }

    #[test]
    fn a_zero_or_negative_multiplier_stops_time_rather_than_reversing_it() {
        let mut clock = TickClock::default();
        let produced = clock.advance(
            Fixed::from_micros(33_333),
            TimeControl::Playing { multiplier: -1.0 },
        );
        assert_eq!(produced.ticks, 0);
        assert_eq!(clock.tick(), cx_core::Tick::ZERO);
    }
}
