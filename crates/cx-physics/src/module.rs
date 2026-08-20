//! `cx-physics` as a module (S20).
//!
//! # The facade is the deliverable, not the physics
//!
//! S11 says *adopt, do not write*: `rapier3d` at M8, behind a facade so that
//! physics types never leak into gameplay and so the dependency stays
//! replaceable. This module is that facade, with the one case that needs no
//! solver behind it — see [`crate::falling`], which is named for what it does
//! rather than for what will eventually live there.
//!
//! When rapier arrives, what changes is the body of `step_bodies`. The module
//! declaration, the phase, the participation rule, and the `ELEVATION` read do
//! not.
//!
//! # It reads `ELEVATION`, and declares that it does
//!
//! The first *reader* of the field — until now the graph's field-access layer
//! had a single edge, a write, so the read path was drawn by nothing and
//! exercised by nothing.
//!
//! Reading, never writing. `ADR-0011` permits exactly two writers to
//! `ELEVATION`, generation and terrain edits, and physics is neither: a body
//! resting on the ground does not reshape it.

use cx_ecs::{Phase, Query, Res, Transform};
use cx_fields::Fields;
use cx_module::{Access, Capability, Module, ModuleId, Registrar, Version, cap};
use cx_worldgen::ELEVATION;

use crate::falling::{FallingBody, PhysicsConfig, place, step};

/// Bodies that fall and rest on terrain.
pub struct PhysicsModule;

impl Module for PhysicsModule {
    const ID: ModuleId = ModuleId("physics");
    const VERSION: Version = Version::new(0, 1);

    fn provides() -> &'static [Capability] {
        &[cap::PHYSICS]
    }

    fn requires() -> &'static [Capability] {
        // Both hard. Ground contact reads `ELEVATION`, which needs the field
        // store to hold it and worldgen to have generated it. A physics module
        // with no terrain is not degraded physics — every body falls forever,
        // which is worse than refusing to start.
        &[cap::FIELDS, cap::TERRAIN]
    }

    fn register(registrar: &mut Registrar) {
        // Its own phase, after `AgentAct`, per S11. Agents decide and move
        // first; physics then resolves where that leaves them.
        registrar.system(Phase::Physics, "step_bodies", step_bodies);

        // Declared, and declared as a *read*. ADR-0011 permits exactly two
        // writers of ELEVATION and physics is neither of them.
        registrar.access("step_bodies", "ELEVATION", Access::Read);
    }
}

/// Advances every falling body by one tick.
///
/// Only entities with a [`FallingBody`] participate. S11 requires that the
/// overwhelming majority of a million-entity simulation never touches physics,
/// and a query that does not match them is the cheapest possible guarantee of
/// that — there is no per-entity check to forget.
pub fn step_bodies(
    fields: Res<Fields>,
    config: Res<PhysicsConfig>,
    mut bodies: Query<(&mut FallingBody, &mut Transform)>,
) {
    let store = fields.store();

    // Nothing to fall onto at all. Asked once rather than per body, and asked
    // *structurally*: an unregistered field samples to zero, and zero is a
    // perfectly plausible ground height, so a value check here would let every
    // body settle at sea level in a world with no terrain and look correct
    // doing it.
    if !store.is_registered(ELEVATION) {
        return;
    }

    for (mut body, mut transform) in bodies.iter_mut() {
        let position = transform.position.normalized();

        // The chunk this body is over may not be loaded, which is again a
        // structural question rather than a question about the sampled value.
        if store.chunk(ELEVATION, position.chunk).is_none() {
            continue;
        }

        let ground = store.sample(ELEVATION, position);

        // Loaded but not yet generated: the cell still holds worldgen's
        // sentinel, a hundred kilometres down. Integrating towards it drops the
        // body out of the world and never brings it back.
        if ground <= cx_worldgen::UNGENERATED {
            continue;
        }

        let stepped = step(*body, transform.position.local.y, ground, &config);
        *body = stepped.body;
        place(&mut transform, stepped.height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::{ChunkCoord, Vec3, WorldPos};
    use cx_ecs::{SimSchedule, SimWorld, WorldConfig};
    use cx_fields::FieldsModule;
    use cx_module::Registry;
    use cx_worldgen::{TerrainShape, Worldgen, WorldgenModule};

    /// A world with generated terrain and the physics schedule.
    fn physics_world(heights: f32) -> (SimWorld, SimSchedule) {
        let mut world = SimWorld::new(WorldConfig::default());

        let mut fields = Fields::default();
        fields.store_mut().insert_chunk(ChunkCoord::new(0, 0));
        world.insert_resource(fields);
        world.insert_resource(Worldgen::new(0, TerrainShape::flat(heights)));
        world.insert_resource(PhysicsConfig::default());

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::ChunkLifecycle, cx_worldgen::generate_elevation);
        schedule.add_system(Phase::Physics, step_bodies);

        (world, schedule)
    }

    fn at(x: f32, y: f32, z: f32) -> WorldPos {
        WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(x, y, z))
    }

    #[test]
    fn the_module_resolves_with_its_dependencies() {
        // S20's per-module smoke profile: the module plus its *declared*
        // dependencies only, which catches one quietly relying on something it
        // never declared.
        let mut registry = Registry::new();
        registry.register::<FieldsModule>();
        registry.register::<WorldgenModule>();
        registry.register::<PhysicsModule>();

        let resolved = registry.resolve().expect("physics should resolve");
        assert_eq!(resolved.modules().count(), 3);
    }

    #[test]
    fn it_does_not_resolve_without_terrain() {
        // A physics module with no terrain is not degraded physics — every body
        // falls forever, which is worse than refusing to start.
        let mut registry = Registry::new();
        registry.register::<FieldsModule>();
        registry.register::<PhysicsModule>();

        assert!(
            registry.resolve().is_err(),
            "physics requires terrain and must refuse to resolve without it"
        );
    }

    #[test]
    fn it_reads_elevation_and_does_not_write_it() {
        // ADR-0011 permits exactly two writers of ELEVATION. Physics is
        // neither: a body resting on the ground does not reshape it.
        let mut registry = Registry::new();
        registry.register::<FieldsModule>();
        registry.register::<WorldgenModule>();
        registry.register::<PhysicsModule>();
        let resolved = registry.resolve().expect("resolves");

        let physics_access: Vec<Access> = resolved
            .modules()
            .filter(|record| record.id == ModuleId("physics"))
            .flat_map(|record| record.accesses.iter())
            .filter(|access| access.field == "ELEVATION")
            .map(|access| access.access)
            .collect();

        assert_eq!(physics_access, vec![Access::Read]);
        assert_eq!(
            cx_module::writers_of(&resolved, "ELEVATION"),
            vec!["generate_elevation"],
            "physics must not appear as a writer"
        );
    }

    #[test]
    fn a_body_falls_and_rests_on_generated_terrain() {
        let (mut world, mut schedule) = physics_world(20.0);
        let body = world.spawn((
            FallingBody::default(),
            Transform::from_position(at(10.0, 60.0, 10.0)),
        ));

        for _ in 0..200 {
            schedule.run(&mut world);
        }

        let height = world
            .inner()
            .get::<Transform>(body)
            .expect("the body exists")
            .position
            .local
            .y;

        assert!(
            (height - 20.0).abs() < 0.1,
            "the body should rest on terrain at 20 m, got {height}"
        );
    }

    #[test]
    fn an_entity_without_a_body_is_never_touched() {
        // S11's participation rule. The majority of a million-entity simulation
        // must never reach physics, and a query that does not match them is the
        // cheapest possible guarantee — there is no per-entity check to forget.
        let (mut world, mut schedule) = physics_world(20.0);

        let start = at(0.0, 500.0, 0.0);
        let inert = world.spawn(Transform::from_position(start));

        for _ in 0..50 {
            schedule.run(&mut world);
        }

        let position = world
            .inner()
            .get::<Transform>(inert)
            .expect("the entity exists")
            .position;

        assert_eq!(
            position.local, start.local,
            "an entity with no FallingBody should not have moved"
        );
    }

    #[test]
    fn a_body_with_no_terrain_at_all_waits_where_it_is() {
        // The case that caught a bug: an *unregistered* field samples to zero,
        // not to worldgen's sentinel, and zero is a plausible ground height. A
        // value check here would have let every body settle at sea level in a
        // world with no terrain and look entirely correct doing it.
        let mut world = SimWorld::new(WorldConfig::default());
        world.insert_resource(Fields::default());
        world.insert_resource(PhysicsConfig::default());

        let start = at(0.0, 50.0, 0.0);
        let body = world.spawn((FallingBody::default(), Transform::from_position(start)));

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::Physics, step_bodies);

        for _ in 0..20 {
            schedule.run(&mut world);
        }

        let height = world
            .inner()
            .get::<Transform>(body)
            .expect("the body exists")
            .position
            .local
            .y;

        assert_eq!(
            height, 50.0,
            "it should have waited, not fallen to {height}"
        );
    }

    #[test]
    fn two_runs_of_the_same_scene_end_identically() {
        // S11's determinism criterion, at the scale that exists. A fixed
        // timestep and no shared state, so this is exact rather than
        // approximate.
        let heights = |()| {
            let (mut world, mut schedule) = physics_world(15.0);
            let bodies: Vec<_> = (0..20)
                .map(|index| {
                    world.spawn((
                        FallingBody::default(),
                        Transform::from_position(at(
                            index as f32 * 2.0,
                            40.0 + index as f32,
                            index as f32,
                        )),
                    ))
                })
                .collect();

            for _ in 0..60 {
                schedule.run(&mut world);
            }

            bodies
                .iter()
                .map(|entity| {
                    world
                        .inner()
                        .get::<Transform>(*entity)
                        .expect("the body exists")
                        .position
                        .local
                        .y
                })
                .collect::<Vec<f32>>()
        };

        assert_eq!(heights(()), heights(()));
    }

    #[test]
    fn a_body_over_an_unloaded_chunk_waits() {
        // Terrain exists, but not here. Structural again: the chunk is absent
        // rather than holding a particular value.
        let (mut world, mut schedule) = physics_world(20.0);

        let far = WorldPos::new(ChunkCoord::new(40, -30), Vec3::new(5.0, 100.0, 5.0));
        let body = world.spawn((FallingBody::default(), Transform::from_position(far)));

        for _ in 0..30 {
            schedule.run(&mut world);
        }

        let height = world
            .inner()
            .get::<Transform>(body)
            .expect("the body exists")
            .position
            .local
            .y;

        assert_eq!(
            height, 100.0,
            "it should have waited, not fallen to {height}"
        );
    }

    #[test]
    fn a_body_over_a_loaded_but_ungenerated_chunk_waits() {
        // The third and last "no ground here" case, and the only one the
        // sentinel actually covers: the chunk is loaded, the field registered,
        // and generation has not run.
        let mut world = SimWorld::new(WorldConfig::default());

        let mut fields = Fields::default();
        fields.store_mut().insert_chunk(ChunkCoord::new(0, 0));
        world.insert_resource(fields);
        world.insert_resource(Worldgen::new(0, TerrainShape::flat(10.0)));
        world.insert_resource(PhysicsConfig::default());

        let body = world.spawn((
            FallingBody::default(),
            Transform::from_position(at(1.0, 80.0, 1.0)),
        ));

        // Registers ELEVATION and fills the chunk, then is removed so nothing
        // regenerates: what remains is a registered field over a chunk whose
        // cells were reset to the sentinel.
        let mut register_only = SimSchedule::new();
        register_only.add_system(Phase::ChunkLifecycle, cx_worldgen::generate_elevation);
        register_only.run(&mut world);

        {
            let mut fields = world
                .inner_mut()
                .get_resource_mut::<Fields>()
                .expect("the resource exists");
            fields
                .store_mut()
                .fill(ELEVATION, ChunkCoord::new(0, 0), cx_worldgen::UNGENERATED);
        }

        let mut physics_only = SimSchedule::new();
        physics_only.add_system(Phase::Physics, step_bodies);
        for _ in 0..30 {
            physics_only.run(&mut world);
        }

        let height = world
            .inner()
            .get::<Transform>(body)
            .expect("the body exists")
            .position
            .local
            .y;

        assert_eq!(
            height, 80.0,
            "it should have waited, not fallen to {height}"
        );
    }
}
