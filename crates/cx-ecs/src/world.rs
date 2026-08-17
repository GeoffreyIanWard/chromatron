//! The simulation world: a thin, opinionated wrapper over `bevy_ecs`.
//!
//! The wrapper exists to enforce policy the raw library does not (`ADR-0001`,
//! S02): deterministic ordering, phase discipline, and deferred structural
//! change. Everything it adds is a constraint, not a feature.

use bevy_ecs::bundle::Bundle;
use bevy_ecs::entity::Entity;
use bevy_ecs::query::{QueryData, QueryFilter, QueryState};
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use bevy_tasks::{ComputeTaskPool, TaskPoolBuilder};

/// How a [`SimWorld`] is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldConfig {
    /// Worker threads for parallel system execution.
    ///
    /// From config, never `num_cpus` (`03-conventions.md`): a machine-varying
    /// thread count makes a reproducibility investigation harder than it needs
    /// to be. Results must not depend on this value — that is what the
    /// threads 1/4/16 determinism gate checks — but the *investigation* is much
    /// easier when the number is stated rather than discovered.
    pub threads: usize,

    /// Entity capacity hint, reserved when the first bulk spawn runs.
    ///
    /// Allocation inside a tick is banned, and archetype growth during a bulk
    /// spawn is the most common way to violate that accidentally.
    pub reserve_entities: usize,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            threads: 8,
            reserve_entities: 0,
        }
    }
}

/// The authoritative simulation world.
///
/// Holds the ECS world and nothing that knows rendering exists (`ADR-0002`).
#[derive(Debug)]
pub struct SimWorld {
    world: World,
    config: WorldConfig,
}

impl SimWorld {
    /// Builds a world and initializes the shared task pool.
    ///
    /// The task pool is process-global in `bevy_tasks`, so the first world built
    /// in a process fixes the thread count. That is documented rather than
    /// worked around: a process running two worlds with different thread counts
    /// is a benchmark harness, and the determinism gate covers exactly that case
    /// by running separate processes.
    pub fn new(config: WorldConfig) -> Self {
        ComputeTaskPool::get_or_init(|| {
            TaskPoolBuilder::new()
                .num_threads(config.threads.max(1))
                .thread_name("cx-sim".to_owned())
                .build()
        });

        Self {
            world: World::new(),
            config,
        }
    }

    /// The configuration this world was built with.
    pub const fn config(&self) -> &WorldConfig {
        &self.config
    }

    /// Spawns one entity.
    ///
    /// Correct for one-off setup and tests. In a tick, structural change goes
    /// through `SimCommands` and lands in `StructuralApply`; in bulk, use
    /// [`SimWorld::spawn_batch`], which is over 20x faster (an M0 gate).
    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
        self.world.spawn(bundle).id()
    }

    /// Spawns many entities of one archetype.
    ///
    /// The path agent spawning and chunk activation use. A `spawn` loop at this
    /// scale is a performance bug, not a style preference: each individual spawn
    /// can trigger an archetype move, and those dominate cost in an archetypal
    /// ECS.
    pub fn spawn_batch<I>(&mut self, bundles: I)
    where
        I: IntoIterator,
        I::Item: Bundle<Effect: bevy_ecs::bundle::NoBundleEffect>,
    {
        self.world.spawn_batch(bundles);
    }

    /// Despawns an entity, returning whether it existed.
    pub fn despawn(&mut self, entity: Entity) -> bool {
        self.world.despawn(entity)
    }

    /// Builds a query.
    ///
    /// The returned state is reusable and should be kept across ticks rather
    /// than rebuilt — building it walks the archetype list.
    ///
    /// Iteration order is **unspecified**. Callers must be order-independent; if
    /// order genuinely matters, use [`SimWorld::iter_deterministic`] and accept
    /// the sort.
    pub fn query<D: QueryData>(&mut self) -> QueryState<D> {
        self.world.query::<D>()
    }

    /// Builds a filtered query.
    pub fn query_filtered<D: QueryData, F: QueryFilter>(&mut self) -> QueryState<D, F> {
        self.world.query_filtered::<D, F>()
    }

    /// Entities matching a query, in `Entity` order.
    ///
    /// For the rare case where order genuinely affects results. It allocates and
    /// sorts, so it does not belong on a hot path — and a system that needs it
    /// every tick is usually a system that should be restructured read-then-write
    /// instead (S02).
    pub fn iter_deterministic<D: QueryData>(&mut self) -> Vec<Entity> {
        let mut entities: Vec<Entity> = self
            .world
            .query_filtered::<Entity, ()>()
            .iter(&self.world)
            .collect();
        entities.sort_unstable();
        entities.retain(|entity| self.world.get_entity(*entity).is_ok());
        entities
    }

    /// How many simulation entities are alive.
    ///
    /// Neither of the counts `bevy_ecs` offers means this, and both are wrong in
    /// ways that would quietly corrupt a memory report or an invariant:
    ///
    /// - `Entities::len()` counts *allocated indices*, including slots reserved
    ///   ahead of a bulk spawn — it reports 1024 after spawning 1000.
    /// - `Entities::count_spawned()` counts live entities, but `bevy_ecs` 0.19
    ///   stores **resources as entities** (`IsResource`), so inserting a resource
    ///   would inflate the population.
    ///
    /// So this walks and filters. It is O(entities) and belongs in diagnostics
    /// rather than a hot loop.
    pub fn entity_count(&self) -> usize {
        self.world
            .iter_entities()
            .filter(|entity| !entity.contains::<bevy_ecs::resource::IsResource>())
            .count()
    }

    /// Inserts a resource.
    pub fn insert_resource<R: Resource>(&mut self, resource: R) {
        self.world.insert_resource(resource);
    }

    /// Borrows a resource.
    pub fn resource<R: Resource>(&self) -> Option<&R> {
        self.world.get_resource::<R>()
    }

    /// Mutably borrows a resource.
    pub fn resource_mut<R: Resource<Mutability = bevy_ecs::component::Mutable>>(
        &mut self,
    ) -> Option<bevy_ecs::change_detection::Mut<'_, R>> {
        self.world.get_resource_mut::<R>()
    }

    /// The underlying `bevy_ecs` world.
    ///
    /// An escape hatch for exclusive systems and tooling. Reaching for it inside
    /// an ordinary system usually means bypassing the deferred-structural-change
    /// discipline, which is the one rule that keeps archetype churn bounded.
    pub const fn inner(&self) -> &World {
        &self.world
    }

    /// The underlying `bevy_ecs` world, mutably. See [`SimWorld::inner`].
    pub const fn inner_mut(&mut self) -> &mut World {
        &mut self.world
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::component::Component;

    #[derive(Component, Debug, Clone, Copy, PartialEq)]
    struct Position(f32);

    #[derive(Component, Debug, Clone, Copy)]
    struct Velocity(f32);

    #[test]
    fn spawn_batch_and_query_see_every_entity() {
        let mut world = SimWorld::new(WorldConfig::default());
        world.spawn_batch((0..1_000).map(|i| (Position(i as f32), Velocity(1.0))));

        assert_eq!(world.entity_count(), 1_000);

        let mut query = world.query::<&Position>();
        let count = query.iter(world.inner()).count();
        assert_eq!(count, 1_000);
    }

    #[test]
    fn deterministic_iteration_is_sorted_and_repeatable() {
        let mut world = SimWorld::new(WorldConfig::default());
        for i in 0..100 {
            world.spawn((Position(i as f32), Velocity(0.0)));
        }

        let first = world.iter_deterministic::<Entity>();
        let second = world.iter_deterministic::<Entity>();

        assert_eq!(first, second, "the same world must yield the same order");
        assert!(
            first.windows(2).all(|pair| pair[0] < pair[1]),
            "should be sorted"
        );
    }

    #[test]
    fn despawn_reports_whether_the_entity_existed() {
        let mut world = SimWorld::new(WorldConfig::default());
        let entity = world.spawn(Position(0.0));

        assert!(world.despawn(entity));
        assert!(
            !world.despawn(entity),
            "second despawn should report false, not panic"
        );
    }

    #[test]
    fn mutation_through_a_query_is_visible_afterwards() {
        let mut world = SimWorld::new(WorldConfig::default());
        world.spawn_batch((0..10).map(|i| (Position(i as f32), Velocity(2.0))));

        let mut query = world.query::<(&mut Position, &Velocity)>();
        for (mut position, velocity) in query.iter_mut(world.inner_mut()) {
            position.0 += velocity.0;
        }

        let mut check = world.query::<&Position>();
        let total: f32 = check.iter(world.inner()).map(|position| position.0).sum();
        assert!((total - (45.0 + 20.0)).abs() < f32::EPSILON, "got {total}");
    }
}
