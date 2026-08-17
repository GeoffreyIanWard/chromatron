//! Loop drivers.
//!
//! Two drivers over one core (S03). The headless one lands at M0; the windowed
//! one at M1. They share [`crate::clock::TickClock`] entirely, which is what
//! makes the acceptance criterion "identical state hashes under both drivers"
//! achievable rather than aspirational — there is only one place ticks are
//! counted.

use cx_core::{Fixed, Tick};
use cx_ecs::{SimSchedule, SimWorld};

use crate::clock::{CatchUp, TickClock, TickRate};
use crate::control::TimeControl;

/// Why a headless run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The requested tick count was reached.
    Completed,
    /// A caller-supplied stop condition fired.
    ConditionMet,
}

/// What a headless run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunReport {
    /// Ticks actually executed.
    pub ticks: u64,
    /// The tick the clock finished on.
    pub final_tick: Tick,
    /// Why it stopped.
    pub reason: StopReason,
}

/// Runs ticks as fast as the machine allows, for batch runs and benchmarks.
///
/// Never constructs a view world (`ADR-0002`), and never consults wall-clock
/// time: a headless run of N ticks executes exactly N ticks regardless of how
/// long each takes.
#[derive(Debug, Default)]
pub struct HeadlessDriver {
    clock: TickClock,
}

impl HeadlessDriver {
    /// A driver at the given rate.
    pub const fn new(rate: TickRate) -> Self {
        Self {
            clock: TickClock::new(rate),
        }
    }

    /// The clock this driver advances.
    pub const fn clock(&self) -> &TickClock {
        &self.clock
    }

    /// Runs exactly `ticks` ticks.
    pub fn run(
        &mut self,
        world: &mut SimWorld,
        schedule: &mut SimSchedule,
        ticks: u64,
    ) -> RunReport {
        self.run_until(world, schedule, ticks, |_| false)
    }

    /// Runs up to `max_ticks`, stopping early when `stop` returns true.
    ///
    /// `stop` is called with the tick just completed. It must not consult
    /// anything outside the sim world, or the run stops being reproducible.
    pub fn run_until(
        &mut self,
        world: &mut SimWorld,
        schedule: &mut SimSchedule,
        max_ticks: u64,
        mut stop: impl FnMut(Tick) -> bool,
    ) -> RunReport {
        let mut executed = 0;

        while executed < max_ticks {
            schedule.run(world);
            self.clock.consume_tick();
            executed += 1;

            if stop(self.clock.tick()) {
                return RunReport {
                    ticks: executed,
                    final_tick: self.clock.tick(),
                    reason: StopReason::ConditionMet,
                };
            }
        }

        RunReport {
            ticks: executed,
            final_tick: self.clock.tick(),
            reason: StopReason::Completed,
        }
    }
}

/// Drives the sim from real elapsed time, as a windowed client does.
///
/// The full `WindowedDriver` with frame pacing is M1; this is the tick-counting
/// half, which is what M0 needs to prove the two drivers agree.
#[derive(Debug, Default)]
pub struct PacedDriver {
    clock: TickClock,
    control: TimeControl,
}

impl PacedDriver {
    /// A driver at the given rate, playing at 1x.
    pub const fn new(rate: TickRate) -> Self {
        Self {
            clock: TickClock::new(rate),
            control: TimeControl::Playing { multiplier: 1.0 },
        }
    }

    /// The clock.
    pub const fn clock(&self) -> &TickClock {
        &self.clock
    }

    /// The current control state.
    pub const fn control(&self) -> TimeControl {
        self.control
    }

    /// Sets pause, play, or step.
    pub const fn set_control(&mut self, control: TimeControl) {
        self.control = control;
    }

    /// Advances by one frame of real time, running whatever ticks are due.
    pub fn frame(
        &mut self,
        world: &mut SimWorld,
        schedule: &mut SimSchedule,
        real_delta: Fixed,
    ) -> CatchUp {
        let produced = self.clock.advance(real_delta, self.control);

        for _ in 0..produced.ticks {
            schedule.run(world);
            self.clock.consume_tick();
        }

        self.control = self.control.consume(produced.ticks);

        if produced.fell_behind {
            // A diagnostic rather than a silent slowdown: S03 is explicit that
            // hitting the cap must be visible, because "the game is in slow
            // motion" is otherwise unanswerable.
            tracing::warn!(
                tick = self.clock.tick().0,
                discarded_us = produced.discarded.as_micros(),
                "SimFallingBehind: catch-up cap reached, simulation time is being dropped"
            );
        }

        produced
    }
}
