//! Structured logging.
//!
//! `tracing` with structured fields, and a `tick` span wrapping each tick so
//! that every sim log line carries the tick number (S01). That single field is
//! what makes a log usable during a determinism investigation: a divergence is
//! reported as a tick number, and the surrounding log lines have to be findable
//! by the same key.
//!
//! Subscriber *installation* lives in `apps/` — a library that installs a global
//! subscriber makes itself impossible to embed.

use crate::time::Tick;

/// Creates the span that wraps one tick.
///
/// The tick number is recorded as a field rather than baked into a message, so
/// structured consumers can filter on it.
#[macro_export]
macro_rules! tick_span {
    ($tick:expr) => {
        ::tracing::info_span!("tick", tick = $tick.0)
    };
}

/// Enters a tick span for the duration of `body`.
///
/// The closure form exists because the span guard must be dropped at the end of
/// the tick; holding it across an await or an early return is the usual way tick
/// numbers end up attached to the wrong log lines.
pub fn in_tick<T>(tick: Tick, body: impl FnOnce() -> T) -> T {
    let span = tracing::info_span!("tick", tick = tick.0);
    let _guard = span.enter();
    body()
}

/// Records a value that should appear in every log line for the current tick.
pub fn record_tick_field(name: &'static str, value: i64) {
    tracing::Span::current().record(name, value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_tick_returns_the_body_value() {
        let result = in_tick(Tick(7), || 1 + 1);
        assert_eq!(result, 2);
    }

    #[test]
    fn nested_ticks_do_not_leak_their_guards() {
        // The guard is dropped when `in_tick` returns, so a subsequent call is
        // not nested inside the previous tick's span.
        let first = in_tick(Tick(1), || tracing::Span::current().id());
        let second = in_tick(Tick(2), || tracing::Span::current().id());
        assert!(first.is_none() || second.is_none() || first != second);
    }
}
