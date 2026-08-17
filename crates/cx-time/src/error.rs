//! Time configuration failures.

/// Why a time configuration was rejected.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TimeError {
    /// A tick rate outside the supported range.
    #[error(
        "tick rate {hz} Hz is outside the supported range {}-{} Hz (S03). The rate is part of \
         world identity, so it is validated at startup rather than clamped silently — a save \
         made at an unsupported rate could not be replayed.",
        crate::clock::MIN_TICK_HZ,
        crate::clock::MAX_TICK_HZ
    )]
    UnsupportedTickRate {
        /// The rejected rate.
        hz: u64,
    },

    /// A speed multiplier outside the supported range.
    #[error("time multiplier {multiplier}x is outside the supported range 0.1x-10000x (S03)")]
    UnsupportedMultiplier {
        /// The rejected multiplier.
        multiplier: f32,
    },
}
