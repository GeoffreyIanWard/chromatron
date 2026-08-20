//! Gravity and ground contact.
//!
//! # This is not rapier, and does not pretend to be
//!
//! S11 says *adopt, do not write*: `rapier3d` at M8, behind the facade this
//! crate is. What is here is the one case that needs no solver at all — a body
//! falling until it meets the terrain — and it is named for exactly that. There
//! are no rigid bodies, no contacts between entities, no constraints, and no
//! broad phase.
//!
//! Building it now is worth doing for three reasons, none of which is "physics":
//!
//! - It makes `cx-physics` a real module with real dependencies, which is what
//!   the S21 graph needs and what M2 asked for.
//! - It is the **first reader** of `ELEVATION`. Until now the field-access layer
//!   had one edge, a write, and the read path was untested by anything.
//! - The participation rule — that the overwhelming majority of entities must
//!   never touch physics — is a property worth establishing before there is a
//!   million-entity population to retrofit it onto.
//!
//! Naming it `FallingBody` rather than `RigidBody` is deliberate. A type that
//! claims more than it does is how a placeholder survives into a release.
//!
//! # Fixed timestep, always
//!
//! S11 is explicit: the same fixed timestep as the tick, never a variable one.
//! Variable-timestep integration gives a different trajectory at a different
//! frame rate, which is a divergence between two machines running the same seed.

use bevy_ecs::component::Component;
use bevy_ecs::resource::Resource;
use cx_core::math::Vec3;
use cx_ecs::Transform;

/// A body that falls and rests on the terrain.
///
/// **Participation is by having this component.** S11 requires that the
/// overwhelming majority of a million-entity simulation never touches physics,
/// and the cheapest way to guarantee that is for the query not to match them.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct FallingBody {
    /// Vertical speed in metres per second. Negative is downward.
    pub velocity: f32,
    /// Whether the body is resting on the ground.
    ///
    /// Tracked rather than derived from velocity: a body exactly at rest has
    /// zero velocity, and so does one at the top of its arc.
    pub grounded: bool,
}

/// How far above the terrain a body counts as resting on it, in metres.
///
/// Without a tolerance a body oscillates: it lands exactly on the surface, the
/// next tick applies gravity, and it is fractionally below and gets pushed back.
/// The visible symptom is a jitter of a few millimetres that never settles.
const GROUND_TOLERANCE: f32 = 0.01;

/// Simulation constants.
#[derive(Resource, Debug, Clone, Copy)]
pub struct PhysicsConfig {
    /// Downward acceleration in metres per second squared.
    pub gravity: f32,
    /// Seconds of simulated time per tick.
    ///
    /// Simulated, never wall-clock: reading a real clock here is the
    /// determinism violation `ADR-0004` exists to prevent, and it would also
    /// make a body's trajectory depend on the frame rate.
    pub tick_seconds: f32,
}

impl Default for PhysicsConfig {
    fn default() -> Self {
        Self {
            gravity: -9.81,
            // 30 Hz, matching cx-time's default rate.
            tick_seconds: 1.0 / 30.0,
        }
    }
}

/// What one step did to one body.
///
/// Returned rather than applied so the arithmetic can be tested without an ECS
/// world — which is where the cases that matter live: landing, resting, and
/// falling past the surface in a single step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    /// The body's new state.
    pub body: FallingBody,
    /// Its new height, in metres.
    pub height: f32,
}

/// Advances one body by one tick against a ground height.
///
/// Pure: same inputs, same outputs, no clock and no shared state.
pub fn step(body: FallingBody, height: f32, ground: f32, config: &PhysicsConfig) -> Step {
    // A body already resting stays put. Re-integrating it every tick is what
    // produces the millimetre jitter the tolerance exists to prevent.
    if body.grounded && (height - ground).abs() <= GROUND_TOLERANCE {
        return Step {
            body: FallingBody {
                velocity: 0.0,
                grounded: true,
            },
            height: ground,
        };
    }

    let velocity = body.velocity + config.gravity * config.tick_seconds;
    let next = height + velocity * config.tick_seconds;

    // Landed. Checked against the *destination* rather than the origin, so a
    // body moving fast enough to cross the surface within one step still lands
    // — the tunnelling case, which at 30 Hz starts around 30 m/s and is
    // therefore reachable by anything falling for a second.
    if next <= ground {
        return Step {
            body: FallingBody {
                velocity: 0.0,
                grounded: true,
            },
            height: ground,
        };
    }

    Step {
        body: FallingBody {
            velocity,
            grounded: false,
        },
        height: next,
    }
}

/// Applies `Step` to a transform.
pub fn place(transform: &mut Transform, height: f32) {
    let local = transform.position.local;
    transform.position = cx_core::math::WorldPos {
        chunk: transform.position.chunk,
        local: Vec3::new(local.x, height, local.z),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PhysicsConfig {
        PhysicsConfig::default()
    }

    fn falling() -> FallingBody {
        FallingBody::default()
    }

    #[test]
    fn a_body_accelerates_downward() {
        let first = step(falling(), 100.0, 0.0, &config());
        assert!(first.body.velocity < 0.0);
        assert!(first.height < 100.0);

        let second = step(first.body, first.height, 0.0, &config());
        assert!(
            second.body.velocity < first.body.velocity,
            "gravity should keep accelerating it"
        );
    }

    #[test]
    fn a_body_lands_on_the_ground_rather_than_passing_through() {
        let mut state = Step {
            body: falling(),
            height: 5.0,
        };

        for _ in 0..100 {
            state = step(state.body, state.height, 2.0, &config());
        }

        assert!(state.body.grounded);
        assert!(
            (state.height - 2.0).abs() < 1e-5,
            "rested at {}",
            state.height
        );
    }

    #[test]
    fn a_fast_body_does_not_tunnel_through_the_ground() {
        // At 30 Hz a body moving faster than about 30 m/s covers more than a
        // metre per step. Checking the origin against the ground rather than the
        // destination lets it pass straight through, which reads as an object
        // vanishing into the terrain.
        let fast = FallingBody {
            velocity: -500.0,
            grounded: false,
        };
        let landed = step(fast, 10.0, 9.9, &config());

        assert!(landed.body.grounded, "a fast body should still land");
        assert!((landed.height - 9.9).abs() < 1e-5);
    }

    #[test]
    fn a_resting_body_does_not_jitter() {
        // The oscillation this is written against: land exactly on the surface,
        // apply gravity next tick, end up fractionally below, get pushed back.
        // A few millimetres, forever.
        let mut state = Step {
            body: FallingBody {
                velocity: 0.0,
                grounded: true,
            },
            height: 12.0,
        };

        for tick in 0..50 {
            let next = step(state.body, state.height, 12.0, &config());
            assert_eq!(
                next.height, state.height,
                "a resting body moved on tick {tick}"
            );
            assert_eq!(next.body.velocity, 0.0);
            state = next;
        }
    }

    #[test]
    fn a_body_whose_ground_rises_beneath_it_is_pushed_up() {
        // Terrain edits move the ground. A body left below the new surface
        // would be inside the terrain, which is worse than being moved.
        let resting = FallingBody {
            velocity: 0.0,
            grounded: true,
        };
        let raised = step(resting, 10.0, 15.0, &config());

        assert!((raised.height - 15.0).abs() < 1e-5, "got {}", raised.height);
        assert!(raised.body.grounded);
    }

    #[test]
    fn a_body_whose_ground_falls_away_starts_falling_again() {
        let resting = FallingBody {
            velocity: 0.0,
            grounded: true,
        };
        let dropped = step(resting, 10.0, 0.0, &config());

        assert!(!dropped.body.grounded, "it should be airborne again");
        assert!(dropped.body.velocity < 0.0);
        assert!(dropped.height < 10.0);
    }

    #[test]
    fn the_same_fall_runs_the_same_way_twice() {
        // A fixed timestep and no shared state, so this is exact rather than
        // approximate — which is the property S11's determinism criterion wants
        // and the reason the timestep is not variable.
        let trajectory = |_: ()| {
            let mut state = Step {
                body: falling(),
                height: 200.0,
            };
            let mut heights = Vec::new();
            for _ in 0..200 {
                state = step(state.body, state.height, 0.0, &config());
                heights.push(state.height);
            }
            heights
        };

        assert_eq!(trajectory(()), trajectory(()));
    }

    #[test]
    fn a_body_starting_below_the_ground_is_placed_on_it() {
        // Spawning inside terrain is a content mistake, not a physics one, and
        // the physics should resolve it rather than integrate from inside a
        // hill.
        let below = step(falling(), -50.0, 0.0, &config());
        assert!(below.body.grounded);
        assert!((below.height - 0.0).abs() < 1e-5);
    }
}
