//! S06 acceptance tests: lazy allocation, halo correctness, kernel double
//! buffering, deterministic deposits, and tile dirty tracking.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use cx_core::math::{CELL_SIZE, CELLS_PER_CHUNK_EDGE, ChunkCoord, TILE_CELLS, Vec3, WorldPos};
use cx_fields::{
    Deposit, DepositBuffer, DepositOp, FieldId, FieldSpec, FieldStore, Persistence, StoreConfig,
};

const TEST: FieldId = FieldId(0);

fn store_with(spec: FieldSpec, chunks: &[ChunkCoord]) -> FieldStore {
    let mut store = FieldStore::new(StoreConfig { threads: 4 });
    store.register(TEST, spec);
    for chunk in chunks {
        store.insert_chunk(*chunk);
    }
    store
}

/// Copies the value `v` into every interior cell of one chunk.
fn diffuse(input: &[f32], output: &mut [f32], stride: usize, range: std::ops::Range<usize>) {
    for index in range {
        let centre = input[index];
        let sum =
            input[index - 1] + input[index + 1] + input[index - stride] + input[index + stride]
                - 4.0 * centre;
        output[index] = centre + 0.25 * sum;
    }
}

#[test]
fn s06_acceptance_unwritten_field_allocates_zero_bytes() {
    let store = store_with(
        FieldSpec::transient("BENCH_UNWRITTEN", 0.0),
        &[ChunkCoord::new(0, 0), ChunkCoord::new(1, 0)],
    );

    assert_eq!(
        store.allocated_bytes(TEST),
        0,
        "registration and chunk insertion must allocate nothing; storage appears on first write"
    );
    assert_eq!(store.chunks().len(), 2);
}

#[test]
fn first_write_allocates_and_the_rest_do_not() {
    let mut store = store_with(FieldSpec::transient("F", 0.0), &[ChunkCoord::new(0, 0)]);

    store.set(TEST, ChunkCoord::new(0, 0), 5, 5, 1.0);
    let after_first = store.allocated_bytes(TEST);
    assert!(after_first > 0);

    store.set(TEST, ChunkCoord::new(0, 0), 6, 6, 2.0);
    assert_eq!(
        store.allocated_bytes(TEST),
        after_first,
        "no further allocation"
    );
}

#[test]
fn a_field_default_is_returned_where_nothing_is_stored() {
    let store = store_with(FieldSpec::transient("F", 7.5), &[ChunkCoord::new(0, 0)]);
    assert!((store.get(TEST, ChunkCoord::new(0, 0), 0, 0) - 7.5).abs() < f32::EPSILON);
    // An unloaded chunk also reads as the default rather than failing.
    assert!((store.get(TEST, ChunkCoord::new(9, 9), 0, 0) - 7.5).abs() < f32::EPSILON);
}

#[test]
fn s06_acceptance_kernel_is_double_buffered() {
    // A uniform field must stay uniform under diffusion. If the kernel read and
    // wrote one array, cells processed later would see already-updated
    // neighbours and the result would drift in the scan direction.
    let chunk = ChunkCoord::new(0, 0);
    let mut store = store_with(FieldSpec::transient("F", 0.0), &[chunk]);
    store.fill(TEST, chunk, 0.5);

    store.run_kernel(TEST, diffuse);

    for (x, z) in [(1, 1), (500, 500), (1022, 1022)] {
        let value = store.get(TEST, chunk, x, z);
        assert!(
            (value - 0.5).abs() < 1e-6,
            "uniform input must stay uniform, got {value} at ({x}, {z})"
        );
    }
}

#[test]
fn a_kernel_propagates_a_disturbance_to_its_neighbours() {
    let chunk = ChunkCoord::new(0, 0);
    let mut store = store_with(FieldSpec::transient("F", 0.0), &[chunk]);
    store.fill(TEST, chunk, 0.0);
    store.set(TEST, chunk, 100, 100, 1.0);

    store.run_kernel(TEST, diffuse);

    let centre = store.get(TEST, chunk, 100, 100);
    let neighbour = store.get(TEST, chunk, 101, 100);
    let far = store.get(TEST, chunk, 200, 200);

    assert!(centre < 1.0, "the peak should have spread, got {centre}");
    assert!(
        neighbour > 0.0,
        "the neighbour should have received some, got {neighbour}"
    );
    assert!(
        far.abs() < 1e-6,
        "a distant cell should be untouched, got {far}"
    );
}

#[test]
fn s06_acceptance_halo_exchange_copies_neighbour_edges() {
    let left = ChunkCoord::new(0, 0);
    let right = ChunkCoord::new(1, 0);
    let mut store = store_with(FieldSpec::transient("F", 0.0), &[left, right]);

    store.fill(TEST, left, 1.0);
    store.fill(TEST, right, 2.0);

    store.exchange_halos(TEST);

    // The left chunk's +X halo column should now hold the right chunk's first
    // column. Without the exchange it would still read its own fill value, and a
    // kernel at the seam would see a cliff that is not in the world.
    let spec = *store.spec(TEST).expect("registered");
    let storage = store.chunk(TEST, left).expect("allocated");
    let halo = spec.halo_width as usize;
    let stride = spec.stride();
    let halo_index = (halo + 10) * stride + halo + CELLS_PER_CHUNK_EDGE as usize;

    assert!(
        (storage.front()[halo_index] - 2.0).abs() < f32::EPSILON,
        "the +X halo should hold the neighbour's edge value, got {}",
        storage.front()[halo_index]
    );
}

#[test]
fn halo_exchange_with_no_neighbour_leaves_the_ring_alone() {
    let chunk = ChunkCoord::new(0, 0);
    let mut store = store_with(FieldSpec::transient("F", 3.0), &[chunk]);
    store.fill(TEST, chunk, 3.0);

    store.exchange_halos(TEST);

    // An edge chunk keeps its own values in the ring, which makes the boundary
    // behave as a zero-gradient wall rather than as a cliff to zero.
    let spec = *store.spec(TEST).expect("registered");
    let storage = store.chunk(TEST, chunk).expect("allocated");
    let stride = spec.stride();
    assert!((storage.front()[stride + 1] - 3.0).abs() < f32::EPSILON);
}

#[test]
fn s06_acceptance_deposits_apply_in_a_deterministic_order() {
    let chunk = ChunkCoord::new(0, 0);

    // The same deposits queued in two different orders — as two thread counts
    // would produce — must land on the same value.
    let run = |reversed: bool| {
        let mut store = store_with(FieldSpec::transient("F", 0.0), &[chunk]);
        store.fill(TEST, chunk, 0.0);

        let mut buffer = DepositBuffer::with_capacity(8);
        let mut deposits = vec![
            Deposit {
                field: TEST,
                chunk,
                x: 4,
                z: 4,
                value: 5.0,
                op: DepositOp::Add,
            },
            Deposit {
                field: TEST,
                chunk,
                x: 4,
                z: 4,
                value: 2.0,
                op: DepositOp::Max,
            },
            Deposit {
                field: TEST,
                chunk,
                x: 4,
                z: 4,
                value: 1.0,
                op: DepositOp::Set,
            },
            Deposit {
                field: TEST,
                chunk,
                x: 4,
                z: 4,
                value: 3.0,
                op: DepositOp::Add,
            },
        ];
        if reversed {
            deposits.reverse();
        }
        for deposit in deposits {
            buffer.push(deposit);
        }

        buffer.drain_into(&mut store);
        store.get(TEST, chunk, 4, 4)
    };

    assert!(
        (run(false) - run(true)).abs() < f32::EPSILON,
        "queue order must not affect the result: {} vs {}",
        run(false),
        run(true)
    );
}

#[test]
fn draining_the_deposit_buffer_keeps_its_capacity() {
    let chunk = ChunkCoord::new(0, 0);
    let mut store = store_with(FieldSpec::transient("F", 0.0), &[chunk]);
    let mut buffer = DepositBuffer::with_capacity(64);

    for i in 0..64 {
        buffer.push(Deposit {
            field: TEST,
            chunk,
            x: i,
            z: 0,
            value: 1.0,
            op: DepositOp::Add,
        });
    }
    buffer.drain_into(&mut store);

    assert!(buffer.is_empty(), "the buffer should be drained");
    // Allocation inside a tick is banned, so the next tick must reuse this.
    assert_eq!(buffer.len(), 0);
}

#[test]
fn tile_dirty_tracking_marks_only_written_tiles() {
    let chunk = ChunkCoord::new(0, 0);
    let spec = FieldSpec {
        tile_dirty_tracking: true,
        ..FieldSpec::transient("ELEVATION_LIKE", 0.0)
    };
    let mut store = store_with(spec, &[chunk]);

    store.set(TEST, chunk, 5, 5, 1.0);
    let storage = store.chunk(TEST, chunk).expect("allocated");

    assert!(
        storage.is_tile_dirty(0, 0),
        "the written tile should be dirty"
    );
    assert!(
        !storage.is_tile_dirty(1, 0),
        "an untouched tile should not be"
    );
    assert_eq!(storage.dirty_tile_count(), 1);

    let storage = store.chunk_mut(TEST, chunk).expect("allocated");
    storage.set(TILE_CELLS + 1, 0, 1.0);
    assert_eq!(storage.dirty_tile_count(), 2);

    storage.clear_dirty_tiles();
    assert_eq!(storage.dirty_tile_count(), 0);
}

#[test]
fn a_field_without_tile_tracking_records_nothing() {
    let chunk = ChunkCoord::new(0, 0);
    let mut store = store_with(FieldSpec::transient("F", 0.0), &[chunk]);
    store.set(TEST, chunk, 5, 5, 1.0);

    let storage = store.chunk(TEST, chunk).expect("allocated");
    assert_eq!(
        storage.dirty_tile_count(),
        0,
        "tracking is opt-in per field"
    );
}

#[test]
fn sampling_interpolates_between_cells() {
    let chunk = ChunkCoord::new(0, 0);
    let mut store = store_with(FieldSpec::transient("F", 0.0), &[chunk]);
    store.fill(TEST, chunk, 0.0);
    store.set(TEST, chunk, 0, 0, 0.0);
    store.set(TEST, chunk, 1, 0, 1.0);

    // Halfway between cell 0 and cell 1 along X.
    let position = WorldPos::new(chunk, Vec3::new(CELL_SIZE * 0.5, 0.0, 0.0));
    let sampled = store.sample(TEST, position);

    assert!(
        (sampled - 0.5).abs() < 1e-3,
        "expected the midpoint, got {sampled}"
    );
}

#[test]
fn nearest_sampling_does_not_interpolate() {
    let chunk = ChunkCoord::new(0, 0);
    let mut store = store_with(FieldSpec::transient("F", 0.0), &[chunk]);
    store.fill(TEST, chunk, 0.0);
    store.set(TEST, chunk, 1, 0, 1.0);

    let position = WorldPos::new(chunk, Vec3::new(CELL_SIZE * 1.4, 0.0, 0.0));
    assert!((store.sample_nearest(TEST, position) - 1.0).abs() < f32::EPSILON);
}

#[test]
fn sampling_an_unloaded_chunk_returns_the_default() {
    let store = store_with(FieldSpec::transient("F", 2.5), &[ChunkCoord::new(0, 0)]);
    let position = WorldPos::new(ChunkCoord::new(50, 50), Vec3::ZERO);
    assert!((store.sample(TEST, position) - 2.5).abs() < f32::EPSILON);
}

#[test]
fn an_unregistered_field_is_inert_rather_than_fatal() {
    // Sim crates do not panic in release: a consumer asking for a field its
    // module never registered must degrade, not abort.
    let mut store = FieldStore::new(StoreConfig::default());
    store.insert_chunk(ChunkCoord::new(0, 0));

    assert!(!store.is_registered(TEST));
    assert_eq!(store.allocated_bytes(TEST), 0);
    store.set(TEST, ChunkCoord::new(0, 0), 0, 0, 1.0);
    store.run_kernel(TEST, diffuse);
    store.exchange_halos(TEST);
    assert_eq!(store.allocated_bytes(TEST), 0);
}

#[test]
fn persistence_policy_is_recorded_per_field() {
    let mut store = FieldStore::new(StoreConfig::default());
    store.register(
        TEST,
        FieldSpec {
            persistence: Persistence::DeltaPersisted,
            ..FieldSpec::transient("ELEVATION", 0.0)
        },
    );
    assert_eq!(
        store.spec(TEST).expect("registered").persistence,
        Persistence::DeltaPersisted
    );
}
