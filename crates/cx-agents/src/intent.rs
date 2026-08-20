//! Agent state and the intents that carry a decision into an action (S10).
//!
//! # Why an intent exists at all
//!
//! `02-architecture.md` splits every tick into read-then-write phases. An agent
//! **decides** in `AgentDecide`, reading the world and writing only its own
//! intent, and **acts** in `AgentAct`, where the intent becomes movement.
//!
//! Collapsing those into one system would work, right up until two agents
//! interacted: the second would read a world the first had already changed, and
//! the result would depend on which ran first. Every system in a phase runs in
//! unspecified order (`ADR-0001`), so that is a divergence between two runs of
//! the same seed, not merely an oddity.
//!
//! The intent is what makes the split possible. It is per-agent, so writing it
//! is not a shared write, and nothing reads another agent's intent.

use bevy_ecs::component::Component;
use cx_core::math::Vec3;
use cx_ecs::Entity;

/// Marks an entity as an agent, with how fast it moves.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Agent {
    /// Movement speed in metres per second.
    pub speed: f32,
}

impl Default for Agent {
    fn default() -> Self {
        Self { speed: 2.0 }
    }
}

/// How far an agent senses, in metres.
///
/// A component rather than a constant because S10 tiers behaviour by scale, and
/// the radius is the first thing that differs between a `Full` agent and a
/// `Coarse` one.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct SenseRadius(pub f32);

impl Default for SenseRadius {
    fn default() -> Self {
        // Inside S10's "local" tier, which is steering against the spatial
        // index rather than pathfinding.
        Self(6.0)
    }
}

/// What an agent decided to do this tick.
///
/// Written in `AgentDecide`, consumed in `AgentAct`, and cleared as it is
/// consumed — an intent that survived into the next tick would be an agent
/// acting on a decision it made about a world that has since moved.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct Intent {
    /// Direction to move, already normalized. `None` means stay put.
    pub steer: Option<Vec3>,
    /// What this agent decided to claim, if anything.
    pub claim: Option<Entity>,
}

impl Intent {
    /// An intent to move in `direction`.
    ///
    /// A direction that cannot be normalized becomes "stay put" rather than a
    /// zero vector: a zero-length steer and a decision not to move are the same
    /// thing, and representing them differently invites a caller to distinguish
    /// them.
    pub fn steering(direction: Vec3) -> Self {
        let steer = direction.try_normalize();
        Self { steer, claim: None }
    }

    /// An intent to claim `target`.
    pub const fn claiming(target: Entity) -> Self {
        Self {
            steer: None,
            claim: Some(target),
        }
    }

    /// Whether this intent asks for anything.
    pub const fn is_idle(&self) -> bool {
        self.steer.is_none() && self.claim.is_none()
    }
}

/// Something an agent can claim, and who holds it.
///
/// One holder at a time. The interesting part is not the claim but how contests
/// are settled — see `crate::behaviour::resolve_claims`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct Claimable {
    /// The agent holding this, if any.
    pub holder: Option<Entity>,
}

impl Claimable {
    /// Whether anyone holds this.
    pub const fn is_held(&self) -> bool {
        self.holder.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_direction_becomes_no_steer() {
        // A zero-length steer and "stay put" are the same decision. Two
        // representations of one state is an invitation to branch on the
        // difference.
        assert_eq!(Intent::steering(Vec3::ZERO).steer, None);
        assert!(Intent::steering(Vec3::ZERO).is_idle());
    }

    #[test]
    fn a_steer_is_normalized() {
        // Speed belongs to the agent, not to the decision. An unnormalized
        // steer would make an agent's speed depend on how far away whatever it
        // was avoiding happened to be.
        let intent = Intent::steering(Vec3::new(30.0, 0.0, 40.0));
        let steer = intent.steer.expect("a real direction");

        assert!((steer.length() - 1.0).abs() < 1e-5);
        assert!((steer.x - 0.6).abs() < 1e-5);
        assert!((steer.z - 0.8).abs() < 1e-5);
    }

    #[test]
    fn a_direction_too_small_to_normalize_is_not_a_direction() {
        // Two agents at almost exactly the same position produce a separation
        // vector near zero. Normalizing it yields infinities, and an agent
        // teleports.
        let intent = Intent::steering(Vec3::new(1e-30, 0.0, 1e-30));
        assert_eq!(intent.steer, None, "a denormal offset is not a direction");
    }

    #[test]
    fn an_idle_intent_asks_for_nothing() {
        assert!(Intent::default().is_idle());
        assert!(!Intent::steering(Vec3::X).is_idle());
    }
}
