//! The M0 memory gate — `memory_16_chunks_1m_entities`.
//!
//! `bench/memory-budget.md` calls min-spec the binding constraint: 16 GB is a
//! realistic desktop minimum and a Steam Deck is 16 GB shared, so **8 GB for the
//! process** is the number that decides whether the architecture fits.
//!
//! This builds the M0 exit configuration — 16 chunks of dense fields plus a
//! million entities — and measures peak RSS. It is the last M0 exit criterion,
//! and the one that would most obviously invalidate the design if it failed:
//! unlike a timing gate, memory cannot be recovered by optimising a loop.
//!
//! Measured on Linux only, which is where CI gates (see `chromatron_bench::rss`).
//! Elsewhere the benchmark builds the same world, reports what it can, and says
//! it did not measure — rather than passing quietly and implying it did.

use chromatron_bench::{BENCH_THREADS, rss};
use criterion::{Criterion, criterion_group, criterion_main};
use cx_core::glam::Vec3;
use cx_core::math::ChunkCoord;
use cx_ecs::{Component, SimWorld, WorldConfig};
use cx_fields::{FieldId, FieldSpec, FieldStore, Persistence, StoreConfig};

/// The min-spec budget from `bench/memory-budget.md`.
const MIN_SPEC_BUDGET_BYTES: u64 = 8 * 1024 * 1024 * 1024;

const ENTITY_COUNT: usize = 1_000_000;
const CHUNK_EDGE: i32 = 4;

/// Four `f32` fields across 16 chunks: 16 × 1,048,576 cells × 4 B × 4 fields,
/// double-buffered. About 537 MB of field storage before halos.
///
/// Deliberately the *unquantized* case. `memory-budget.md` says quantization is
/// mandatory and S06 has not implemented it yet, so this measures the worst
/// version of the workload — which is the honest thing to gate on until `u8` and
/// `u16` fields exist.
const FIELDS: [(FieldId, &str); 4] = [
    (FieldId(0), "ELEVATION"),
    (FieldId(1), "SOIL_MOISTURE"),
    (FieldId(2), "TEMPERATURE"),
    (FieldId(3), "BIOMASS"),
];

#[derive(Component, Clone, Copy)]
struct Position(Vec3);

#[derive(Component, Clone, Copy)]
struct Velocity(Vec3);

#[derive(Component, Clone, Copy)]
struct Energy(f32);

#[derive(Component, Clone, Copy)]
struct Age(u32);

fn build_fields() -> FieldStore {
    let mut store = FieldStore::new(StoreConfig {
        threads: BENCH_THREADS,
    });

    for (id, name) in FIELDS {
        store.register(
            id,
            FieldSpec {
                name,
                default: 0.0,
                persistence: Persistence::Transient,
                halo_width: 1,
                tile_dirty_tracking: false,
            },
        );
    }

    for x in 0..CHUNK_EDGE {
        for z in 0..CHUNK_EDGE {
            let chunk = ChunkCoord::new(x, z);
            store.insert_chunk(chunk);
            // Fill, because storage is lazy: a registered-but-unwritten field
            // allocates nothing, and measuring that would measure nothing.
            for (id, _) in FIELDS {
                store.fill(id, chunk, 0.5);
            }
        }
    }

    store
}

fn build_entities() -> SimWorld {
    let mut world = SimWorld::new(WorldConfig {
        threads: BENCH_THREADS,
        ..WorldConfig::default()
    });
    world.spawn_batch((0..ENTITY_COUNT).map(|i| {
        let f = i as f32;
        (
            Position(Vec3::new(f, 0.0, f)),
            Velocity(Vec3::Y),
            Energy(50.0),
            Age(0),
        )
    }));
    world
}

fn bench_memory_16_chunks_1m_entities(_c: &mut Criterion) {
    let baseline = rss::peak_rss_bytes();

    let store = build_fields();
    let world = build_entities();

    // Touch both so nothing is optimised away before the measurement.
    let field_bytes: usize = FIELDS
        .iter()
        .map(|(id, _)| store.allocated_bytes(*id))
        .sum();
    let entities = world.entity_count();

    assert_eq!(
        entities, ENTITY_COUNT,
        "the world should hold the entities it was given"
    );
    assert!(field_bytes > 0, "field storage should be allocated");

    let Some(peak) = rss::peak_rss_bytes() else {
        // Not a silent pass: the benchmark reports that it could not measure,
        // and on the platform that gates (Linux) this branch is unreachable.
        println!(
            "memory_16_chunks_1m_entities: peak RSS unavailable on this platform; \
             built {} entities and {:.0} MB of field storage but measured nothing. \
             The gate runs on Linux — see chromatron_bench::rss.",
            entities,
            field_bytes as f64 / (1024.0 * 1024.0)
        );
        return;
    };

    println!(
        "memory_16_chunks_1m_entities: peak RSS {:.2} GiB (budget {:.0} GiB), \
         field storage {:.0} MB, entities {entities}, baseline before build {:.2} GiB",
        rss::as_gib(peak),
        rss::as_gib(MIN_SPEC_BUDGET_BYTES),
        field_bytes as f64 / (1024.0 * 1024.0),
        baseline.map(rss::as_gib).unwrap_or(0.0),
    );

    assert!(
        peak <= MIN_SPEC_BUDGET_BYTES,
        "gate memory_16_chunks_1m_entities: peak RSS {:.2} GiB exceeds the {:.0} GiB min-spec \
         budget (docs/bench/memory-budget.md).\n\n\
         Min-spec is the binding constraint, not the desktop 12 GB: a Steam Deck is 16 GB \
         shared. Note that field quantization is not implemented yet (S06), so this measures \
         the unquantized worst case — the budget assumes u8/u16/f16 element types, and \
         implementing them is the first lever, ahead of reducing CELLS_PER_CHUNK_EDGE.",
        rss::as_gib(peak),
        rss::as_gib(MIN_SPEC_BUDGET_BYTES)
    );

    drop(store);
    drop(world);
}

criterion_group!(m0_memory, bench_memory_16_chunks_1m_entities);
criterion_main!(m0_memory);
