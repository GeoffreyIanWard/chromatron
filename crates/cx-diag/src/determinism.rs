//! The determinism harness.
//!
//! `ADR-0004` targets bit-exact reproducibility for one build on any machine,
//! any thread count. That claim is only worth what it is checked by, so the
//! checks exist from M0 rather than M7 — a determinism bug introduced now is
//! cheapest to find now, and nearly impossible to find later.
//!
//! Two checks, testing different failure modes:
//!
//! - **Thread counts 1 / 4 / 16.** Catches a system whose result depends on
//!   iteration order or on parallel float accumulation order.
//! - **In-process versus subprocess.** Catches state that leaks between runs in
//!   one process: a `static`, a lazily-initialised global, a cached allocation
//!   that happens to be reused. Running the same scenario twice in one process
//!   would pass while a fresh process diverged.

use cx_core::Tick;
use cx_ecs::{SimSchedule, SimWorld, WorldConfig};

use crate::hash::{StateHash, StateHasher};

/// A scenario the harness can run repeatedly.
///
/// Deliberately a pair of closures rather than a trait: the harness must be able
/// to build a *fresh* world for each run, and a trait object holding state would
/// invite exactly the leakage the subprocess check exists to catch.
pub struct Scenario<B, S> {
    /// Populates a fresh world.
    pub build: B,
    /// Registers the systems.
    pub schedule: S,
    /// Ticks to run.
    pub ticks: u64,
}

/// The hash sequence a run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashSequence {
    /// One hash per tick, in tick order.
    pub hashes: Vec<StateHash>,
    /// Thread count the run used.
    pub threads: usize,
}

impl HashSequence {
    /// The first tick where two sequences differ, if any.
    ///
    /// This is the divergence detector in miniature: report the *first*
    /// disagreement, because every later one is a consequence.
    pub fn first_divergence(&self, other: &HashSequence) -> Option<Tick> {
        self.hashes
            .iter()
            .zip(other.hashes.iter())
            .position(|(a, b)| a != b)
            .map(|index| Tick(index as u64))
            .or_else(|| {
                (self.hashes.len() != other.hashes.len())
                    .then(|| Tick(self.hashes.len().min(other.hashes.len()) as u64))
            })
    }

    /// The final hash, or `None` for an empty run.
    pub fn last(&self) -> Option<StateHash> {
        self.hashes.last().copied()
    }
}

/// Runs a scenario and records a hash per tick.
pub fn run_scenario<B, S>(
    scenario: &Scenario<B, S>,
    hasher: &StateHasher,
    threads: usize,
) -> HashSequence
where
    B: Fn(&mut SimWorld),
    S: Fn(&mut SimSchedule),
{
    let mut world = SimWorld::new(WorldConfig {
        threads,
        ..WorldConfig::default()
    });
    (scenario.build)(&mut world);

    let mut schedule = SimSchedule::new();
    (scenario.schedule)(&mut schedule);

    let mut hashes = Vec::with_capacity(scenario.ticks as usize);
    for _ in 0..scenario.ticks {
        schedule.run(&mut world);
        hashes.push(hasher.hash_world(&mut world));
    }

    HashSequence { hashes, threads }
}

/// Runs a scenario at several thread counts and compares.
///
/// Returns the first disagreement, or `None` when every run matched.
pub fn compare_thread_counts<B, S>(
    scenario: &Scenario<B, S>,
    hasher: &StateHasher,
    thread_counts: &[usize],
) -> Option<Divergence>
where
    B: Fn(&mut SimWorld),
    S: Fn(&mut SimSchedule),
{
    let mut baseline: Option<HashSequence> = None;

    for threads in thread_counts {
        let run = run_scenario(scenario, hasher, *threads);

        match &baseline {
            None => baseline = Some(run),
            Some(reference) => {
                if let Some(tick) = reference.first_divergence(&run) {
                    return Some(Divergence {
                        tick,
                        left_threads: reference.threads,
                        right_threads: run.threads,
                        left: reference.hashes.get(tick.0 as usize).copied(),
                        right: run.hashes.get(tick.0 as usize).copied(),
                    });
                }
            }
        }
    }

    None
}

/// Where two runs stopped agreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divergence {
    /// First tick whose hashes differ.
    pub tick: Tick,
    /// Thread count of the reference run.
    pub left_threads: usize,
    /// Thread count of the diverging run.
    pub right_threads: usize,
    /// Reference hash at that tick.
    pub left: Option<StateHash>,
    /// Diverging hash at that tick.
    pub right: Option<StateHash>,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "diverged at {} — {} threads gave {}, {} threads gave {}",
            self.tick,
            self.left_threads,
            self.left
                .map(|hash| hash.to_string())
                .unwrap_or_else(|| "<none>".to_owned()),
            self.right_threads,
            self.right
                .map(|hash| hash.to_string())
                .unwrap_or_else(|| "<none>".to_owned()),
        )
    }
}
