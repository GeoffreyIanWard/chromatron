//! M0 field gates (S06) — `field_stencil_16m_cells`, `field_halo_exchange_16_chunks`.
//!
//! S06 calls itself the spec most likely to be underestimated, and says the
//! scale claim is won or lost here because field cells outnumber entities by two
//! orders of magnitude. These two gates are the direct test of that claim.
//!
//! As the first caller of `cx-fields`, this file also pins down the kernel
//! contract from S06: flat slices, explicit stride, an index range per work
//! item, and double buffering — kernels never read and write the same array.

use std::hint::black_box;

use chromatron_bench::{BENCH_THREADS, gate, targets};
use criterion::{Criterion, criterion_group, criterion_main};
use cx_core::ChunkCoord;
use cx_fields::{FieldId, FieldSpec, FieldStore, Persistence, StoreConfig};

/// 16 chunks of 1024x1024 cells = 16,777,216 cells, which is the 16M figure in
/// `01-scope.md` and the M0 gate.
const CHUNK_COUNT: i32 = 16;

/// Chunks are laid out square rather than in a strip: halo exchange cost depends
/// on how many neighbours each chunk has, and a 16x1 strip would understate it
/// by giving most chunks two neighbours instead of four.
const CHUNK_EDGE: i32 = 4;
const _: () = assert!(CHUNK_EDGE * CHUNK_EDGE == CHUNK_COUNT);

/// A test field standing in for `SOIL_MOISTURE`, the real per-tick stencil
/// workload once ecology lands at M4 (`ADR-0008` removed erosion from the tick,
/// so diffusion is what remains).
const TEST_FIELD: FieldId = FieldId(0);

fn store_with_chunks(threads: usize) -> FieldStore {
    let mut store = FieldStore::new(StoreConfig { threads });

    store.register(
        TEST_FIELD,
        FieldSpec {
            name: "BENCH_DIFFUSION",
            default: 0.0,
            persistence: Persistence::Transient,
            halo_width: 1,
            tile_dirty_tracking: false,
        },
    );

    for x in 0..CHUNK_EDGE {
        for z in 0..CHUNK_EDGE {
            let chunk = ChunkCoord { x, z };
            store.insert_chunk(chunk);
            // Force allocation. S06 says a never-written field allocates zero
            // bytes, so a benchmark that skipped this would measure nothing.
            store.fill(TEST_FIELD, chunk, 0.5);
        }
    }

    store
}

/// The 5-point stencil from the gate: a plain diffusion step.
///
/// Written to the S06 contract — no branches in the inner loop, no bounds
/// checks, neighbours reached through the halo ring rather than through
/// conditionals at the edges. If this needs an `if`, the halo is wrong.
fn diffuse(input: &[f32], output: &mut [f32], stride: usize, range: std::ops::Range<usize>) {
    const RATE: f32 = 0.25;

    for index in range {
        let centre = input[index];
        let sum =
            input[index - 1] + input[index + 1] + input[index - stride] + input[index + stride]
                - 4.0 * centre;
        output[index] = centre + RATE * sum;
    }
}

/// `field_stencil_16m_cells` — < 12 ms on 8 threads.
fn bench_stencil_16m_cells(c: &mut Criterion) {
    let mut store = store_with_chunks(BENCH_THREADS);

    let mut group = c.benchmark_group("field_stencil_16m_cells");
    group.sample_size(30);
    group.bench_function("8_threads", |b| {
        b.iter(|| {
            // Parallelises by chunk, then by row band within a chunk — never by
            // cell (03-conventions.md).
            store.run_kernel(TEST_FIELD, diffuse);
            black_box(&store);
        });
    });
    group.finish();

    gate::assert_within(
        "field_stencil_16m_cells",
        gate::measured_mean("field_stencil_16m_cells/8_threads"),
        targets::FIELD_STENCIL_16M,
    );
}

/// `field_halo_exchange_16_chunks` — < 1 ms.
///
/// Measured separately from the stencil rather than folded into it. Halo
/// exchange is its own sub-phase (S06) and is the part that scales with chunk
/// *count* rather than cell count, so a regression here would otherwise hide
/// inside a stencil number twelve times its size.
fn bench_halo_exchange(c: &mut Criterion) {
    let mut store = store_with_chunks(BENCH_THREADS);

    let mut group = c.benchmark_group("field_halo_exchange_16_chunks");
    group.bench_function("exchange", |b| {
        b.iter(|| {
            store.exchange_halos(TEST_FIELD);
            black_box(&store);
        });
    });
    group.finish();

    gate::assert_within(
        "field_halo_exchange_16_chunks",
        gate::measured_mean("field_halo_exchange_16_chunks/exchange"),
        targets::FIELD_HALO_16_CHUNKS,
    );
}

/// Not a timing gate, but it belongs next to these: S06 requires a field that
/// has never been written to allocate zero bytes, and that is a claim about the
/// same code path the benchmarks above exercise.
///
/// It sits in the benchmark rather than in a unit test because it is measured
/// with the same memory reporting the M0 memory gate uses.
fn bench_unwritten_field_allocates_nothing(_c: &mut Criterion) {
    let mut store = FieldStore::new(StoreConfig::default());
    store.register(
        TEST_FIELD,
        FieldSpec {
            name: "BENCH_UNWRITTEN",
            default: 0.0,
            persistence: Persistence::Transient,
            halo_width: 1,
            tile_dirty_tracking: false,
        },
    );
    for x in 0..CHUNK_EDGE {
        for z in 0..CHUNK_EDGE {
            store.insert_chunk(ChunkCoord { x, z });
        }
    }

    let bytes = store.allocated_bytes(TEST_FIELD);
    assert_eq!(
        bytes, 0,
        "S06: a field that has never been written must allocate zero bytes, but 16 inserted \
         chunks reported {bytes} bytes. Lazy-on-first-write is what makes the memory budget \
         in docs/bench/memory-budget.md achievable — a 1024x1024 f32 field is 4 MB per chunk."
    );
}

criterion_group!(
    m0_fields,
    bench_stencil_16m_cells,
    bench_halo_exchange,
    bench_unwritten_field_allocates_nothing
);
criterion_main!(m0_fields);
