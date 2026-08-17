//! Turning a criterion measurement into a pass/fail CI gate.
//!
//! `docs/bench/baselines.md` says every number there is a gate, and the
//! milestone rule is that a milestone is not complete until its section passes
//! in CI. Criterion on its own reports and regresses against its *own* previous
//! run, which is a different question — it will happily report a stable 40 ms
//! for a benchmark whose documented budget is 33 ms.
//!
//! So the gate is asserted separately, against the mean criterion just measured.

use std::path::PathBuf;
use std::time::Duration;

/// Reads back the mean estimate criterion recorded for `id`, e.g.
/// `"ecs_tick_1m_3systems/8_threads"`.
///
/// Reading the saved estimate rather than timing a second run matters: the gate
/// then asserts on the same measurement that was reported, so a failure message
/// and criterion's own output can never disagree.
pub fn measured_mean(id: &str) -> Duration {
    let path = estimates_path(id);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "gate {id}: could not read criterion estimates at {}: {error}.\n\n\
             This usually means the benchmark did not run, or ran under a different id than \
             the gate is looking up. The gate must not be skipped silently — a gate that \
             cannot measure is a failing gate.",
            path.display()
        )
    });

    let nanos = point_estimate_nanos(&raw).unwrap_or_else(|| {
        panic!(
            "gate {id}: criterion estimates at {} did not contain a mean point estimate. \
             The criterion output format may have changed.",
            path.display()
        )
    });

    Duration::from_nanos(nanos as u64)
}

fn estimates_path(id: &str) -> PathBuf {
    // Anchored to this crate's manifest rather than the working directory:
    // `cargo bench` runs the benchmark binary with the *package* directory as
    // CWD, so a relative "target" would resolve to
    // apps/chromatron-bench/target, which does not exist.
    let target = std::env::var("CARGO_TARGET_DIR")
        .unwrap_or_else(|_| format!("{}/../../target", env!("CARGO_MANIFEST_DIR")));
    PathBuf::from(target)
        .join("criterion")
        .join(id)
        .join("new")
        .join("estimates.json")
}

/// Pulls `mean.point_estimate` out of criterion's estimates file.
///
/// Hand-parsed rather than pulled in via serde: this is one number out of a file
/// with a stable shape, and the benchmark crate having no runtime dependencies
/// keeps the allocation gate honest.
fn point_estimate_nanos(raw: &str) -> Option<f64> {
    let mean = raw.find("\"mean\"")?;
    let point = raw[mean..].find("\"point_estimate\"")?;
    let after_colon = raw[mean + point..].find(':')?;
    let tail = &raw[mean + point + after_colon + 1..];

    let value: String = tail
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == 'e' || *c == '-' || *c == '+')
        .collect();

    value.parse().ok()
}

/// Asserts a measured duration against its documented budget.
///
/// The message names the gate, both numbers, and where the budget is written
/// down, because the person reading it in CI output six months from now did not
/// write this code.
#[track_caller]
pub fn assert_within(gate: &str, measured: Duration, budget: Duration) {
    assert!(
        measured <= budget,
        "gate {gate}: measured {measured:?}, budget {budget:?} \
         (docs/bench/baselines.md#m0).\n\n\
         M0 exists to try to break the architecture before building on it. A failure here is \
         a signal to stop and revise — see the 'If it fails' section of \
         docs/milestones/M0-dual-scale-proof.md — not a benchmark to be tuned until it passes."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_criterion_estimates_payload() {
        let raw = r#"{"mean":{"confidence_interval":{"confidence_level":0.95,
            "lower_bound":2812345.6,"upper_bound":2912345.6},"point_estimate":2856789.1,
            "standard_error":25000.0},"median":{"point_estimate":2850000.0}}"#;
        let nanos = point_estimate_nanos(raw).expect("mean should parse");
        assert!((nanos - 2_856_789.1).abs() < 1.0, "got {nanos}");
    }

    #[test]
    fn reads_the_mean_not_the_first_point_estimate_in_the_file() {
        // `slope` precedes `mean` in criterion's real output. Picking the first
        // `point_estimate` in the file would silently gate on the wrong number.
        let raw = r#"{"slope":{"point_estimate":999999.0},
            "mean":{"point_estimate":1234.0}}"#;
        assert_eq!(point_estimate_nanos(raw), Some(1234.0));
    }
}
