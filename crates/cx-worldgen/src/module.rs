//! `cx-worldgen` as a module (S20).
//!
//! The second engine subsystem to declare itself, and the first with a
//! *dependency*: it requires `fields` and would fail resolution without it. Up
//! to now every profile resolved to one module, one capability, and one system,
//! which made the S21 graph a single box.
//!
//! # This module owns `ELEVATION`
//!
//! `cx-fields` provides storage and owns no data (`ADR-0012`); the field belongs
//! to whoever generates it. That is what makes disabling worldgen free the
//! memory rather than merely stop writing to it — and it is what the graph's
//! field-access layer draws.
//!
//! # Two writers, permanently
//!
//! `ADR-0011` permits exactly two writers to `ELEVATION`: generation here, and
//! terrain edits in S19. A third is a defect rather than a change, and it is the
//! one thing the S21 graph diff hard-fails on.

use bevy_ecs::resource::Resource;
use cx_ecs::{Phase, Res, ResMut};
use cx_fields::{FieldId, FieldSpec, Fields, Persistence};
use cx_module::{Access, Capability, Module, ModuleId, Registrar, Version, cap};

use crate::elevation::{ElevationGenerator, TerrainShape};

/// Ground height in metres. Owned by this module.
pub const ELEVATION: FieldId = FieldId(1);

/// The generator, as a sim resource.
///
/// Holds the seed and the shape, which together are the entire world: two
/// worlds with the same pair are the same world, and nothing else here is state.
#[derive(Resource, Debug)]
pub struct Worldgen {
    generator: ElevationGenerator,
}

impl Worldgen {
    /// A generator for `seed`.
    pub const fn new(seed: u64, shape: TerrainShape) -> Self {
        Self {
            generator: ElevationGenerator::new(seed, shape),
        }
    }

    /// The elevation generator.
    pub const fn generator(&self) -> &ElevationGenerator {
        &self.generator
    }
}

impl Default for Worldgen {
    fn default() -> Self {
        Self::new(0, TerrainShape::default())
    }
}

/// Generates terrain.
pub struct WorldgenModule;

impl Module for WorldgenModule {
    const ID: ModuleId = ModuleId("worldgen");
    const VERSION: Version = Version::new(0, 1);

    fn provides() -> &'static [Capability] {
        &[cap::TERRAIN]
    }

    fn requires() -> &'static [Capability] {
        // Hard, not optional: terrain with nowhere to store it is not a
        // degraded world, it is a startup error (S20).
        &[cap::FIELDS]
    }

    fn register(registrar: &mut Registrar) {
        registrar.field("ELEVATION", size_of::<f32>());

        // ChunkLifecycle, not TerrainEdit: this fills chunks as they come into
        // existence. Edits are a separate phase and a separate writer
        // (`ADR-0011`).
        registrar.system(
            Phase::ChunkLifecycle,
            "generate_elevation",
            generate_elevation,
        );

        // Declared rather than derived. S21's resolved open question: a write
        // that goes through the deposit buffer has no system parameter to infer
        // it from, and a graph that quietly omits an ELEVATION writer is worse
        // than no graph.
        registrar.access("generate_elevation", "ELEVATION", Access::Write);
    }
}

/// Fills newly loaded chunks with base elevation.
///
/// Every registered chunk whose elevation has never been written. "Never
/// written" is the field's own default sentinel rather than a separate flag: two
/// records of the same fact drift, and this one cannot.
fn generate_elevation(mut fields: ResMut<Fields>, worldgen: Res<Worldgen>) {
    let store = fields.store_mut();

    if !store.is_registered(ELEVATION) {
        store.register(
            ELEVATION,
            FieldSpec {
                name: "ELEVATION",
                default: UNGENERATED,
                // Delta-persisted: an untouched chunk costs zero bytes because it
                // can be regenerated from the seed, and only edits are saved
                // (`ADR-0011`, S13).
                persistence: Persistence::DeltaPersisted,
                // One cell, for the slope and flow kernels that read a 5-point
                // stencil across chunk seams.
                halo_width: 1,
                // Edits to elevation must dirty the tiles that mesh, collide,
                // and navigate over it (`ADR-0011`).
                tile_dirty_tracking: true,
            },
        );
    }

    let chunks: Vec<cx_core::math::ChunkCoord> = store.chunks().to_vec();
    let generator = *worldgen.generator();

    for chunk in chunks {
        if generated(store, chunk) {
            continue;
        }

        for z in 0..cx_core::math::CELLS_PER_CHUNK_EDGE {
            for x in 0..cx_core::math::CELLS_PER_CHUNK_EDGE {
                store.set(
                    ELEVATION,
                    chunk,
                    x,
                    z,
                    generator.chunk_elevation(chunk, x, z),
                );
            }
        }

        tracing::debug!(?chunk, "generated base elevation");
    }
}

/// What an ungenerated cell reads as.
///
/// Far below any terrain a preset can produce, so it cannot be mistaken for a
/// real height — and low enough that anything accidentally rendered at this
/// elevation is obviously wrong rather than subtly so.
pub const UNGENERATED: f32 = -100_000.0;

/// Whether a chunk has already been generated.
fn generated(store: &cx_fields::FieldStore, chunk: cx_core::math::ChunkCoord) -> bool {
    // One cell is enough: generation fills a chunk completely or not at all,
    // because it runs in one pass with no yield point inside it.
    store.get(ELEVATION, chunk, 0, 0) > UNGENERATED
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::{CELLS_PER_CHUNK_EDGE, ChunkCoord};
    use cx_ecs::{SimSchedule, SimWorld, WorldConfig};
    use cx_fields::FieldsModule;
    use cx_module::Registry;

    fn world_with_chunks(chunks: &[ChunkCoord]) -> (SimWorld, SimSchedule) {
        let mut world = SimWorld::new(WorldConfig::default());

        let mut fields = Fields::default();
        for chunk in chunks {
            fields.store_mut().insert_chunk(*chunk);
        }
        world.insert_resource(fields);
        world.insert_resource(Worldgen::new(1234, TerrainShape::default()));

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::ChunkLifecycle, generate_elevation);

        (world, schedule)
    }

    #[test]
    fn the_module_resolves_with_its_dependency() {
        // S20's per-module smoke profile: a module plus its *declared*
        // dependencies only, which is what catches one quietly relying on
        // something it never declared.
        let mut registry = Registry::new();
        registry.register::<FieldsModule>();
        registry.register::<WorldgenModule>();

        let resolved = registry
            .resolve()
            .expect("worldgen plus fields should resolve");

        assert_eq!(resolved.modules().count(), 2);
        assert_eq!(resolved.systems().count(), 2);
    }

    #[test]
    fn it_does_not_resolve_without_storage() {
        // The point of `requires` rather than `consumes_optional`: terrain with
        // nowhere to put it is a startup error, not a degraded world.
        let mut registry = Registry::new();
        registry.register::<WorldgenModule>();

        assert!(
            registry.resolve().is_err(),
            "worldgen requires fields and must refuse to resolve without it"
        );
    }

    #[test]
    fn it_owns_elevation() {
        let mut registry = Registry::new();
        registry.register::<FieldsModule>();
        registry.register::<WorldgenModule>();
        let resolved = registry.resolve().expect("resolves");

        let owner = resolved
            .modules()
            .find(|record| record.fields.iter().any(|field| field.name == "ELEVATION"))
            .map(|record| record.id);

        assert_eq!(
            owner,
            Some(ModuleId("worldgen")),
            "ELEVATION belongs to whoever generates it, not to storage"
        );
    }

    #[test]
    fn generation_fills_a_chunk() {
        let chunk = ChunkCoord::new(0, 0);
        let (mut world, mut schedule) = world_with_chunks(&[chunk]);

        schedule.run(&mut world);

        let fields = world
            .inner()
            .get_resource::<Fields>()
            .expect("fields resource exists");

        for (x, z) in [
            (0, 0),
            (CELLS_PER_CHUNK_EDGE - 1, CELLS_PER_CHUNK_EDGE - 1),
            (CELLS_PER_CHUNK_EDGE / 2, CELLS_PER_CHUNK_EDGE / 2),
        ] {
            let height = fields.store().get(ELEVATION, chunk, x, z);
            assert!(
                height > UNGENERATED,
                "cell ({x}, {z}) was never written: {height}"
            );
        }
    }

    #[test]
    fn generating_twice_changes_nothing() {
        // Regeneration has to be idempotent: a chunk that reloads and
        // regenerates must land on the same terrain, or a save reloaded twice is
        // two different worlds.
        let chunk = ChunkCoord::new(-4, 7);
        let (mut world, mut schedule) = world_with_chunks(&[chunk]);

        schedule.run(&mut world);
        let after_first: Vec<f32> = sample(&world, chunk);

        schedule.run(&mut world);
        let after_second: Vec<f32> = sample(&world, chunk);

        assert_eq!(after_first, after_second);
    }

    #[test]
    fn chunk_order_does_not_affect_the_result() {
        // ADR-0006's positional guarantee, at the level the module works at.
        let chunks = [
            ChunkCoord::new(0, 0),
            ChunkCoord::new(3, 1),
            ChunkCoord::new(-2, 5),
        ];

        let (mut forwards_world, mut forwards) = world_with_chunks(&chunks);
        forwards.run(&mut forwards_world);

        let mut reversed = chunks;
        reversed.reverse();
        let (mut backwards_world, mut backwards) = world_with_chunks(&reversed);
        backwards.run(&mut backwards_world);

        for chunk in chunks {
            assert_eq!(
                sample(&forwards_world, chunk),
                sample(&backwards_world, chunk),
                "chunk {chunk:?} differs depending on generation order"
            );
        }
    }

    #[test]
    fn an_ungenerated_chunk_reads_as_obviously_wrong() {
        // The sentinel is not zero on purpose: zero is a plausible sea-level
        // height, so a chunk that failed to generate would look like flat ground
        // rather than like a failure.
        let (world, _) = world_with_chunks(&[ChunkCoord::new(0, 0)]);
        let fields = world
            .inner()
            .get_resource::<Fields>()
            .expect("fields resource exists");

        assert!(
            !fields.store().is_registered(ELEVATION) || UNGENERATED < -1_000.0,
            "the ungenerated sentinel must be unmistakable"
        );
    }

    fn sample(world: &SimWorld, chunk: ChunkCoord) -> Vec<f32> {
        let fields = world
            .inner()
            .get_resource::<Fields>()
            .expect("fields resource exists");

        (0..16)
            .map(|index| fields.store().get(ELEVATION, chunk, index * 37, index * 11))
            .collect()
    }
}
