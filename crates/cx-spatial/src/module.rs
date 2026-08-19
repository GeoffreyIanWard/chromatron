//! `cx-spatial` as a module (S20).
//!
//! # Rebuilt in `SpatialRebuild`, and stale everywhere after it
//!
//! The index is rebuilt once per tick, in its own phase, from the positions as
//! they stand at that moment. Systems in `AgentSense` query it freely. Systems
//! in `AgentAct` may still query it, and what they get is **one phase stale** —
//! because everything sensing happened before anything moved.
//!
//! That staleness is deliberate and is what makes the phase order mean
//! something: if `AgentAct` saw movement made by other agents in the same tick,
//! the result would depend on which agent ran first, which is the property the
//! phases exist to remove (`ADR-0001`, `ADR-0004`).
//!
//! # It owns no fields
//!
//! The index is derived from `Transform`, which `cx-ecs` owns. Nothing here is
//! authoritative state: a lost index is rebuilt next tick from positions that
//! were never in it.

use bevy_ecs::resource::Resource;
use cx_ecs::{Entity, Phase, Query, ResMut, Transform};
use cx_module::{Capability, Module, ModuleId, Registrar, Version, cap};

use crate::grid::SpatialGrid;

/// Default cell size for the agent index, in metres.
///
/// S05 wants a cell per entity class — 4 m for creatures, 128 m for buildings.
/// One index exists so far and it is the agent one, so this is the creature
/// figure. The second index arrives with the entities that need it, rather than
/// as an empty structure waiting for them.
const AGENT_CELL_SIZE: f32 = 4.0;

/// The agent spatial index, as a sim resource.
#[derive(Resource, Debug)]
pub struct SpatialIndex {
    agents: SpatialGrid,
}

impl SpatialIndex {
    /// An empty index.
    pub fn new(cell_size: f32) -> Self {
        Self {
            agents: SpatialGrid::new(cell_size),
        }
    }

    /// The agent index.
    pub const fn agents(&self) -> &SpatialGrid {
        &self.agents
    }

    /// The agent index, mutably.
    ///
    /// Queries need `&mut` because results come from a reused buffer — the
    /// alternative is allocating per query, which S05 rules out.
    pub const fn agents_mut(&mut self) -> &mut SpatialGrid {
        &mut self.agents
    }
}

impl Default for SpatialIndex {
    fn default() -> Self {
        Self::new(AGENT_CELL_SIZE)
    }
}

/// Provides neighbour queries over sparse entities.
pub struct SpatialModule;

impl Module for SpatialModule {
    const ID: ModuleId = ModuleId("spatial");
    const VERSION: Version = Version::new(0, 1);

    fn provides() -> &'static [Capability] {
        &[cap::SPATIAL_INDEX]
    }

    fn requires() -> &'static [Capability] {
        // Nothing. The index is built from `Transform`, which every world has:
        // it is an ECS component, not a capability another module provides.
        &[]
    }

    fn register(registrar: &mut Registrar) {
        registrar.system(
            Phase::SpatialRebuild,
            "rebuild_spatial_index",
            rebuild_index,
        );
    }
}

/// Rebuilds the agent index from current positions.
///
/// Public so that a test or a caller assembling its own schedule can register
/// the same system the module does, rather than a copy of it that drifts.
pub fn rebuild_index(mut index: ResMut<SpatialIndex>, query: Query<(Entity, &Transform)>) {
    // Collected before rebuilding rather than streamed, because `rebuild` takes
    // an iterator and the borrow checker will not have the query alive across a
    // `&mut self` call on the resource. The vector is the one allocation on this
    // path and is the obvious thing to pool when the population gets large.
    let positions: Vec<(Entity, cx_core::math::WorldPos)> = query
        .iter()
        .map(|(entity, transform)| (entity, transform.position))
        .collect();

    index.agents_mut().rebuild(positions);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::{ChunkCoord, Vec3, WorldPos};
    use cx_ecs::{SimSchedule, SimWorld, WorldConfig};
    use cx_module::Registry;

    fn at(x: f32, z: f32) -> WorldPos {
        WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(x, 0.0, z))
    }

    fn world_with(positions: &[WorldPos]) -> (SimWorld, SimSchedule) {
        let mut world = SimWorld::new(WorldConfig::default());
        for position in positions {
            world.spawn(Transform::from_position(*position));
        }
        world.insert_resource(SpatialIndex::default());

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::SpatialRebuild, rebuild_index);

        (world, schedule)
    }

    #[test]
    fn the_module_resolves_on_its_own() {
        // S20's per-module smoke profile. `spatial` declares no dependencies, so
        // it must resolve entirely alone — and this is what catches it quietly
        // relying on something it never declared.
        let mut registry = Registry::new();
        registry.register::<SpatialModule>();

        let resolved = registry.resolve().expect("spatial should resolve alone");
        assert_eq!(resolved.modules().count(), 1);
        assert_eq!(resolved.systems().count(), 1);
    }

    #[test]
    fn it_provides_the_spatial_index_capability() {
        assert!(SpatialModule::provides().contains(&cap::SPATIAL_INDEX));
        assert!(SpatialModule::requires().is_empty());
    }

    #[test]
    fn it_owns_no_fields() {
        // The index is derived from Transform. Nothing here is authoritative
        // state: a lost index is rebuilt next tick from positions that were
        // never in it.
        let mut registry = Registry::new();
        registry.register::<SpatialModule>();
        let resolved = registry.resolve().expect("resolves");

        let owned: usize = resolved.modules().map(|record| record.fields.len()).sum();
        assert_eq!(owned, 0);
    }

    #[test]
    fn a_tick_indexes_every_entity() {
        let (mut world, mut schedule) = world_with(&[at(0.0, 0.0), at(5.0, 0.0), at(100.0, 0.0)]);

        schedule.run(&mut world);

        let index = world
            .inner()
            .get_resource::<SpatialIndex>()
            .expect("the resource exists");
        assert_eq!(index.agents().len(), 3);
    }

    #[test]
    fn the_index_follows_the_entities_that_moved() {
        // A rebuild that appended rather than replaced would leave every entity
        // at every position it had ever occupied, which reads as agents sensing
        // ghosts.
        let (mut world, mut schedule) = world_with(&[at(0.0, 0.0)]);
        schedule.run(&mut world);

        {
            let mut query = world.inner_mut().query::<&mut Transform>();
            let inner = world.inner_mut();
            for mut transform in query.iter_mut(inner) {
                transform.position = at(500.0, 0.0);
            }
        }
        schedule.run(&mut world);

        let index = world
            .inner_mut()
            .get_resource_mut::<SpatialIndex>()
            .expect("the resource exists")
            .into_inner();

        assert_eq!(index.agents().len(), 1, "one entity, not two");
        assert!(
            index
                .agents_mut()
                .within_radius(at(0.0, 0.0), 10.0)
                .is_empty(),
            "nothing should remain at the old position"
        );
        assert_eq!(
            index.agents_mut().within_radius(at(500.0, 0.0), 10.0).len(),
            1
        );
    }

    #[test]
    fn two_runs_of_the_same_world_index_identically() {
        // The determinism property, at the level the module works at. The ECS
        // hands entities out in an unspecified iteration order, and the index
        // must not carry that order into its results.
        let positions: Vec<WorldPos> = (0..64)
            .map(|index| at(index as f32 * 0.9, (index % 7) as f32 * 1.3))
            .collect();

        let query_at = at(10.0, 3.0);
        let mut results = Vec::new();

        for _ in 0..2 {
            let (mut world, mut schedule) = world_with(&positions);
            schedule.run(&mut world);

            let index = world
                .inner_mut()
                .get_resource_mut::<SpatialIndex>()
                .expect("the resource exists")
                .into_inner();

            let found: Vec<Entity> = index
                .agents_mut()
                .within_radius(query_at, 12.0)
                .iter()
                .map(|found| found.entity)
                .collect();

            assert!(!found.is_empty(), "the query should find something");
            results.push(found);
        }

        assert_eq!(results[0], results[1]);
    }
}
