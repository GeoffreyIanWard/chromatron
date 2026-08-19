//! Time controls, decided without a window.
//!
//! S03 asks for pause, single-step, and speed. What those keys *do* is pure
//! state transition on a [`TimeControl`], so it lives here and is unit-tested,
//! and the key table in `cx-app::window` is left with nothing but "this key means
//! that action". The overlay's buttons emit the same [`Action`]s, so there is one
//! definition of what pausing does rather than one per input device.
//!
//! The split matters more than it looks: "space pauses" is trivially correct and
//! trivially checked, whereas "stepping while already stepping" and "speeding up
//! from paused" are the cases that produce a confused loop, and neither needs a
//! display server to get wrong.

use cx_time::{MAX_MULTIPLIER, MIN_MULTIPLIER, TimeControl};

/// Something the player asked the loop to do.
///
/// Named for intent rather than for a key, so the binding can move and the
/// meaning cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Close the window and end the run.
    Quit,
    /// Pause if running, resume if paused.
    TogglePause,
    /// Run exactly one tick, then pause.
    Step,
    /// Double the speed, up to [`MAX_MULTIPLIER`].
    Faster,
    /// Halve the speed, down to [`MIN_MULTIPLIER`].
    Slower,
    /// Return to 1x.
    NormalSpeed,
}

/// What the loop should do about an [`Action`].
///
/// Deliberately *not* `#[non_exhaustive]`, unlike [`Action`]. Actions are an
/// open taxonomy of things a player might ask for and will grow; responses are
/// the two things the loop can do about one. Marking this open would force every
/// caller in every other crate to write a catch-all arm, which is exactly how a
/// third response would get silently ignored if one were ever added.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Response {
    /// Adopt this control state.
    Time(TimeControl),
    /// Shut down.
    Quit,
}

/// How much one press of [`Action::Faster`] or [`Action::Slower`] moves.
///
/// A factor rather than an increment: the useful range spans four orders of
/// magnitude (`0.1` to `10000`), so a fixed step would be either uselessly fine
/// at the top or unusably coarse at the bottom.
const SPEED_STEP: f32 = 2.0;

/// Applies an action to the current control state.
///
/// Total: every action has a defined result from every state, because the
/// alternative is a key that does nothing in a situation nobody thought about.
pub fn respond(control: TimeControl, action: Action) -> Response {
    let next = match action {
        Action::Quit => return Response::Quit,

        // Resuming always returns to 1x rather than restoring the speed in
        // effect before the pause. `Paused` carries no multiplier, so the
        // alternative means keeping shadow state purely so an unpause can
        // surprise someone with 8x.
        Action::TogglePause => {
            if control.is_running() {
                TimeControl::Paused
            } else {
                TimeControl::default()
            }
        }

        // One tick, from any state — including mid-step. Pressing step twice
        // quickly should advance two ticks, not queue up a second run of
        // whatever the first one was.
        Action::Step => TimeControl::Stepping { remaining: 1 },

        Action::Faster => scaled(control, SPEED_STEP),
        Action::Slower => scaled(control, 1.0 / SPEED_STEP),
        Action::NormalSpeed => TimeControl::default(),
    };

    Response::Time(next)
}

/// The control state `control` scaled by `factor`, clamped to the valid range.
///
/// Adjusting speed while paused *resumes*: pressing a speed key is an intent to
/// watch it run, and since `Paused` holds no multiplier there is nothing else
/// the press could reasonably mean.
fn scaled(control: TimeControl, factor: f32) -> TimeControl {
    let current = match control {
        TimeControl::Paused => 1.0,
        other => other.multiplier(),
    };

    TimeControl::Playing {
        multiplier: (current * factor).clamp(MIN_MULTIPLIER, MAX_MULTIPLIER),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(response: Response) -> TimeControl {
        match response {
            Response::Time(control) => control,
            Response::Quit => panic!("expected a time control, got a quit"),
        }
    }

    #[test]
    fn pause_toggles_both_ways() {
        let paused = control(respond(TimeControl::default(), Action::TogglePause));
        assert_eq!(paused, TimeControl::Paused);

        let resumed = control(respond(paused, Action::TogglePause));
        assert!(resumed.is_running());
        assert!((resumed.multiplier() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn resuming_from_a_step_run_pauses_first() {
        // Stepping counts as running, so the first press stops it. Otherwise the
        // key would appear dead during a multi-tick step.
        let stepping = TimeControl::Stepping { remaining: 4 };
        assert_eq!(
            control(respond(stepping, Action::TogglePause)),
            TimeControl::Paused
        );
    }

    #[test]
    fn step_always_asks_for_exactly_one_tick() {
        for from in [
            TimeControl::Paused,
            TimeControl::default(),
            TimeControl::Stepping { remaining: 9 },
        ] {
            assert_eq!(
                control(respond(from, Action::Step)),
                TimeControl::Stepping { remaining: 1 },
                "stepping from {from:?} should owe exactly one tick"
            );
        }
    }

    #[test]
    fn speed_doubles_and_halves() {
        let fast = control(respond(TimeControl::default(), Action::Faster));
        assert!((fast.multiplier() - 2.0).abs() < f32::EPSILON);

        let back = control(respond(fast, Action::Slower));
        assert!((back.multiplier() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn speed_stays_inside_what_the_clock_accepts() {
        // The clamp is the point: TimeControl::playing rejects out-of-range
        // multipliers, so an unclamped key press would be a way to construct a
        // state the validated constructor refuses.
        let mut control_state = TimeControl::default();
        for _ in 0..40 {
            control_state = control(respond(control_state, Action::Faster));
        }
        assert!((control_state.multiplier() - MAX_MULTIPLIER).abs() < f32::EPSILON);
        assert!(TimeControl::playing(control_state.multiplier()).is_ok());

        for _ in 0..80 {
            control_state = control(respond(control_state, Action::Slower));
        }
        assert!((control_state.multiplier() - MIN_MULTIPLIER).abs() < f32::EPSILON);
        assert!(TimeControl::playing(control_state.multiplier()).is_ok());
    }

    #[test]
    fn a_speed_key_while_paused_resumes() {
        let resumed = control(respond(TimeControl::Paused, Action::Faster));
        assert!(
            resumed.is_running(),
            "a speed key means 'show me it running'"
        );
        assert!((resumed.multiplier() - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn normal_speed_recovers_from_anything() {
        for from in [
            TimeControl::Paused,
            TimeControl::Playing { multiplier: 512.0 },
            TimeControl::Stepping { remaining: 3 },
        ] {
            let reset = control(respond(from, Action::NormalSpeed));
            assert_eq!(reset, TimeControl::default());
        }
    }

    #[test]
    fn quit_is_never_a_time_change() {
        for from in [TimeControl::Paused, TimeControl::default()] {
            assert_eq!(respond(from, Action::Quit), Response::Quit);
        }
    }
}
