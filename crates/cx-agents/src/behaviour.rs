//! Sensing, deciding, and acting (S10).
//!
//! Three systems in three phases, and the split between them is the whole
//! point:
//!
//! | Phase | System | Reads | Writes |
//! |---|---|---|---|
//! | `AgentDecide` | `decide_steering` | the spatial index, transforms | its own `Intent` |
//! | `AgentAct` | `resolve_claims` | every `Intent` | `Claimable` holders |
//! | `AgentAct` | `apply_intents` | its own `Intent` | its own `Transform` |
//!
//! Nothing reads another agent's intent, and nothing writes shared state before
//! every agent has decided. That is what makes the result independent of which
//! agent the scheduler happened to run first.
//!
//! # What is deliberately not here
//!
//! S10's flow fields, A* over the region graph, cost grids derived from field
//! data, utility scoring, and agent LOD. Those are M6. Local steering is the
//! bottom tier of S10's own navigation ladder and the only one that needs no
//! infrastructure that does not exist yet.

use cx_core::math::Vec3;
use cx_ecs::{Entity, Local, Query, Res, Transform};
use cx_spatial::{Found, SpatialIndex};

use crate::intent::{Agent, Claimable, Intent, SenseRadius};

/// Decides where each agent wants to move.
///
/// Separation: move away from whatever is nearby. It is the simplest behaviour
/// that actually needs the spatial index, which makes it the right one for
/// proving the sense path works before any of S10's real behaviours exist.
///
/// # Parallel-safe by construction
///
/// Takes `Res<SpatialIndex>`, not `ResMut`. Sensing has to be shareable — S05
/// and S10 both say so — and the query buffer is a [`Local`], so each parallel
/// instance of this system owns one and none of them allocate per call.
pub fn decide_steering(
    index: Res<SpatialIndex>,
    mut neighbours: Local<Vec<Found>>,
    mut agents: Query<(Entity, &Transform, &SenseRadius, &mut Intent)>,
) {
    for (entity, transform, radius, mut intent) in agents.iter_mut() {
        index
            .agents()
            .within_radius_into(transform.position, radius.0, &mut neighbours);

        let mut away = Vec3::ZERO;
        for found in neighbours.iter() {
            // An agent always finds itself. Steering away from your own
            // position is a zero vector in theory and a denormal in practice.
            if found.entity == entity {
                continue;
            }

            let offset = transform.position.delta(found.position);
            // Weighted by closeness, so a crowd pushes harder than a distant
            // neighbour. Guarded because two agents at the same position give a
            // zero distance, and the division would be infinite.
            let Some(direction) = offset.try_normalize() else {
                continue;
            };
            let weight = (radius.0 - found.distance).max(0.0);
            away += direction * weight;
        }

        *intent = Intent::steering(away);
    }
}

/// Settles contested claims.
///
/// **The tiebreak is the point.** When two agents want the same thing, the
/// holder is the one with the lower [`Entity`] — never whichever the scheduler
/// reached first. S10 makes that an acceptance criterion, and it is the kind of
/// rule that looks like an implementation detail right up until a replay
/// diverges at tick 50,000.
///
/// Runs in `AgentAct`, after every agent has decided, so no agent's decision
/// can depend on another's claim having already landed.
pub fn resolve_claims(
    intents: Query<(Entity, &Intent)>,
    mut claimables: Query<(Entity, &mut Claimable)>,
) {
    for (target, mut claimable) in claimables.iter_mut() {
        if claimable.is_held() {
            continue;
        }

        // The lowest-numbered claimant, found by scanning rather than by
        // sorting: the answer is a minimum, and computing it this way cannot
        // depend on the order the scan happens to visit.
        let winner = intents
            .iter()
            .filter(|(_, intent)| intent.claim == Some(target))
            .map(|(entity, _)| entity)
            .min();

        if let Some(winner) = winner {
            claimable.holder = Some(winner);
        }
    }
}

/// Turns intents into movement.
///
/// The only system here that writes a `Transform`, which is what `AgentAct`
/// means. Clears each intent as it consumes it: an intent surviving into the
/// next tick is an agent acting on a decision about a world that has moved.
pub fn apply_intents(
    mut agents: Query<(&Agent, &mut Intent, &mut Transform)>,
    tick_seconds: Res<TickSeconds>,
) {
    for (agent, mut intent, mut transform) in agents.iter_mut() {
        if let Some(steer) = intent.steer {
            transform.position = transform
                .position
                .offset(steer * agent.speed * tick_seconds.0);
        }

        *intent = Intent::default();
    }
}

/// Seconds of simulated time in one tick.
///
/// A resource rather than a constant so a test can run a tick without the tick
/// rate being compiled in — and, more importantly, so this is *simulated*
/// seconds. Reading a wall clock here would be the determinism violation
/// `ADR-0004` exists to prevent.
#[derive(bevy_ecs::resource::Resource, Debug, Clone, Copy)]
pub struct TickSeconds(pub f32);

impl Default for TickSeconds {
    fn default() -> Self {
        // 30 Hz, matching cx-time's default rate.
        Self(1.0 / 30.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::{ChunkCoord, WorldPos};
    use cx_ecs::{Phase, SimSchedule, SimWorld, WorldConfig};

    fn at(x: f32, z: f32) -> WorldPos {
        WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(x, 0.0, z))
    }

    /// A world with agents at the given positions, and the full three-phase
    /// schedule.
    fn agent_world(positions: &[WorldPos]) -> (SimWorld, SimSchedule, Vec<Entity>) {
        let mut world = SimWorld::new(WorldConfig::default());

        let entities: Vec<Entity> = positions
            .iter()
            .map(|position| {
                world.spawn((
                    Agent::default(),
                    SenseRadius::default(),
                    Intent::default(),
                    Transform::from_position(*position),
                ))
            })
            .collect();

        world.insert_resource(SpatialIndex::default());
        world.insert_resource(TickSeconds::default());

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::SpatialRebuild, cx_spatial::rebuild_index);
        schedule.add_system(Phase::AgentDecide, decide_steering);
        schedule.add_system(Phase::AgentAct, resolve_claims);
        schedule.add_system(Phase::AgentAct, apply_intents);

        (world, schedule, entities)
    }

    fn position_of(world: &SimWorld, entity: Entity) -> WorldPos {
        world
            .inner()
            .get::<Transform>(entity)
            .expect("the entity exists")
            .position
    }

    #[test]
    fn a_lone_agent_does_not_move() {
        // It senses only itself, and steering away from your own position is
        // the case that produces a denormal rather than a zero.
        let (mut world, mut schedule, entities) = agent_world(&[at(0.0, 0.0)]);
        schedule.run(&mut world);

        let position = position_of(&world, entities[0]);
        assert_eq!(position.local, Vec3::ZERO, "a lone agent should stay put");
    }

    #[test]
    fn two_agents_move_apart() {
        let (mut world, mut schedule, entities) = agent_world(&[at(-1.0, 0.0), at(1.0, 0.0)]);

        let before = SpatialIndexDistance::between(&world, entities[0], entities[1]);
        for _ in 0..5 {
            schedule.run(&mut world);
        }
        let after = SpatialIndexDistance::between(&world, entities[0], entities[1]);

        assert!(
            after > before,
            "separation should increase the gap: {before} then {after}"
        );
    }

    #[test]
    fn an_agent_never_steers_away_from_itself() {
        // The index returns the querying agent among its own neighbours. Not
        // skipping it produces a zero offset, a failed normalize, and — before
        // the guard — a NaN position that propagates everywhere.
        let (mut world, mut schedule, entities) = agent_world(&[at(3.0, 4.0)]);

        for _ in 0..3 {
            schedule.run(&mut world);
        }

        let position = position_of(&world, entities[0]);
        assert!(
            position.local.is_finite(),
            "position went non-finite: {:?}",
            position.local
        );
    }

    #[test]
    fn two_agents_at_the_same_position_do_not_produce_nan() {
        // Exactly coincident agents give a zero separation vector. It is a
        // spawn-on-the-same-tile case, not a contrived one.
        let (mut world, mut schedule, entities) = agent_world(&[at(0.0, 0.0), at(0.0, 0.0)]);

        for _ in 0..3 {
            schedule.run(&mut world);
        }

        for entity in entities {
            let position = position_of(&world, entity);
            assert!(
                position.local.is_finite(),
                "coincident agents produced {:?}",
                position.local
            );
        }
    }

    #[test]
    fn an_intent_does_not_survive_the_tick_that_made_it() {
        // A retained intent is an agent acting on a decision about a world that
        // has since moved — which looks like an agent ignoring what is in front
        // of it.
        let (mut world, mut schedule, entities) = agent_world(&[at(0.0, 0.0), at(0.5, 0.0)]);
        schedule.run(&mut world);

        for entity in entities {
            let intent = world
                .inner()
                .get::<Intent>(entity)
                .expect("the entity exists");
            assert!(
                intent.is_idle(),
                "an intent survived the tick that produced it: {intent:?}"
            );
        }
    }

    #[test]
    fn a_contested_claim_goes_to_the_lower_entity() {
        // S10's acceptance criterion. Without the tiebreak this is whichever
        // agent the scheduler reached first, which is a divergence rather than
        // an oddity.
        let mut world = SimWorld::new(WorldConfig::default());

        let first = world.spawn((Agent::default(), Intent::default()));
        let second = world.spawn((Agent::default(), Intent::default()));
        let prize = world.spawn(Claimable::default());

        // Both want it, and the *higher* entity is set up to be visited first
        // if anything iterated in insertion order.
        for agent in [second, first] {
            *world
                .inner_mut()
                .get_mut::<Intent>(agent)
                .expect("the agent exists") = Intent::claiming(prize);
        }

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::AgentAct, resolve_claims);
        schedule.run(&mut world);

        let holder = world
            .inner()
            .get::<Claimable>(prize)
            .expect("the prize exists")
            .holder;

        assert_eq!(
            holder,
            Some(first.min(second)),
            "the lower entity should win, not whichever was reached first"
        );
    }

    #[test]
    fn a_held_claim_is_not_taken_away() {
        let mut world = SimWorld::new(WorldConfig::default());

        let holder = world.spawn((Agent::default(), Intent::default()));
        let rival = world.spawn((Agent::default(), Intent::default()));
        let prize = world.spawn(Claimable {
            holder: Some(holder),
        });

        *world
            .inner_mut()
            .get_mut::<Intent>(rival)
            .expect("the rival exists") = Intent::claiming(prize);

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::AgentAct, resolve_claims);
        schedule.run(&mut world);

        assert_eq!(
            world
                .inner()
                .get::<Claimable>(prize)
                .expect("the prize exists")
                .holder,
            Some(holder),
            "an existing holder should keep it"
        );
    }

    #[test]
    fn the_same_scenario_runs_the_same_way_twice() {
        // The property all of the above is in service of. The ECS iterates in
        // an unspecified order, and none of it may reach the result.
        let positions: Vec<WorldPos> = (0..40)
            .map(|index| at((index % 8) as f32 * 1.1, (index / 8) as f32 * 1.3))
            .collect();

        let mut runs = Vec::new();
        for _ in 0..2 {
            let (mut world, mut schedule, entities) = agent_world(&positions);
            for _ in 0..20 {
                schedule.run(&mut world);
            }

            let final_positions: Vec<[f32; 3]> = entities
                .iter()
                .map(|entity| position_of(&world, *entity).local.to_array())
                .collect();
            runs.push(final_positions);
        }

        assert_eq!(runs[0], runs[1], "two identical runs diverged");
    }

    /// Distance between two entities, for readability in the tests above.
    struct SpatialIndexDistance;

    impl SpatialIndexDistance {
        fn between(world: &SimWorld, a: Entity, b: Entity) -> f32 {
            position_of(world, a).delta(position_of(world, b)).length()
        }
    }
}
