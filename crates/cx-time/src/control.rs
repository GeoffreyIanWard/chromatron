//! Speed, pause, and single-stepping.

use crate::error::TimeError;

/// Slowest supported multiplier.
pub const MIN_MULTIPLIER: f32 = 0.1;

/// Fastest supported multiplier.
///
/// Above roughly 20x this stops meaning "run more ticks" and starts meaning
/// "do less per tick" — S09 shifts LOD tiers instead, which is why the clock
/// exposes the multiplier rather than keeping it private.
pub const MAX_MULTIPLIER: f32 = 10_000.0;

/// How time is currently advancing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimeControl {
    /// No ticks run. Real time is not banked.
    Paused,
    /// Ticks run, scaled by `multiplier`.
    Playing {
        /// Speed multiplier, `0.1..=10000`.
        multiplier: f32,
    },
    /// A fixed number of ticks run, then the caller is expected to pause again.
    Stepping {
        /// Ticks still owed.
        remaining: u64,
    },
}

impl Default for TimeControl {
    fn default() -> Self {
        Self::Playing { multiplier: 1.0 }
    }
}

impl TimeControl {
    /// Playing at a validated multiplier.
    pub fn playing(multiplier: f32) -> Result<Self, TimeError> {
        if !(MIN_MULTIPLIER..=MAX_MULTIPLIER).contains(&multiplier) {
            return Err(TimeError::UnsupportedMultiplier { multiplier });
        }
        Ok(Self::Playing { multiplier })
    }

    /// The current multiplier, or zero when paused.
    ///
    /// Read by S09 to decide LOD tiers: past about 20x the answer to "go faster"
    /// is to simulate less, not to run more ticks per frame.
    pub fn multiplier(self) -> f32 {
        match self {
            TimeControl::Paused => 0.0,
            TimeControl::Playing { multiplier } => multiplier,
            TimeControl::Stepping { .. } => 1.0,
        }
    }

    /// Whether the simulation is advancing at all.
    pub fn is_running(self) -> bool {
        !matches!(self, TimeControl::Paused)
    }

    /// Records that `ticks` steps were consumed, retiring a finished step run.
    pub fn consume(self, ticks: u64) -> Self {
        match self {
            TimeControl::Stepping { remaining } => {
                let left = remaining.saturating_sub(ticks);
                if left == 0 {
                    TimeControl::Paused
                } else {
                    TimeControl::Stepping { remaining: left }
                }
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipliers_are_validated() {
        assert!(TimeControl::playing(1.0).is_ok());
        assert!(TimeControl::playing(10_000.0).is_ok());
        assert!(TimeControl::playing(0.0).is_err());
        assert!(TimeControl::playing(20_000.0).is_err());
    }

    #[test]
    fn stepping_retires_to_paused_when_exhausted() {
        let control = TimeControl::Stepping { remaining: 3 };
        assert_eq!(control.consume(1), TimeControl::Stepping { remaining: 2 });
        assert_eq!(control.consume(3), TimeControl::Paused);
        assert_eq!(
            control.consume(99),
            TimeControl::Paused,
            "should saturate, not wrap"
        );
    }

    #[test]
    fn paused_reports_zero_multiplier_for_s09() {
        assert!(TimeControl::Paused.multiplier().abs() < f32::EPSILON);
        assert!(!TimeControl::Paused.is_running());
        assert!(TimeControl::default().is_running());
    }
}
