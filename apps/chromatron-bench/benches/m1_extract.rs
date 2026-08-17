//! M1 extract gate — `extract_100k_instances`, < 2 ms.
//!
//! Extract runs once per rendered frame, so its budget is a slice of a frame
//! rather than of a tick. At 144 fps a frame is 6.9 ms; spending 2 ms of that
//! copying sim state to the view world is already a third of it, which is why
//! the gate is where it is.
//!
//! This is the only M1 benchmark that needs no GPU, and therefore the only one
//! that can gate on a standard CI runner. The rendering gates need a decision
//! about how to run them — see `docs/milestones/M1-loop-and-pixels.md`.

use chromatron_bench::{gate, targets};
use criterion::{Criterion, criterion_group, criterion_main};
use cx_core::math::{ChunkCoord, Vec3, WorldPos};
use cx_ecs::{PreviousTransform, SimWorld, Transform, WorldConfig};
use cx_view::{ViewWorld, extract};

const INSTANCE_COUNT: usize = 100_000;

fn build_world() -> SimWorld {
    let mut world = SimWorld::new(WorldConfig::default());

    // Spread across chunks rather than piled into one: extract rebases per
    // instance against the origin chunk, and a single-chunk scene would skip
    // the arithmetic the real case pays for.
    world.spawn_batch((0..INSTANCE_COUNT).map(|i| {
        let chunk = ChunkCoord::new((i % 16) as i32, (i / 16 % 16) as i32);
        let local = Vec3::new((i % 500) as f32, 0.0, (i % 313) as f32);
        let current = Transform::from_position(WorldPos::new(chunk, local));
        let previous =
            Transform::from_position(WorldPos::new(chunk, local + Vec3::new(0.1, 0.0, 0.1)));
        (current, PreviousTransform(previous))
    }));

    world
}

fn bench_extract_100k_instances(c: &mut Criterion) {
    let mut world = build_world();
    // Sized up front. Extract is on the frame path, and growing this vector
    // during it would put an allocation there.
    let mut view = ViewWorld::with_capacity(INSTANCE_COUNT);

    let mut group = c.benchmark_group("extract_100k_instances");
    group.bench_function("interpolated", |b| {
        b.iter(|| {
            // A mid-tick alpha: the interpolation path is the one that runs
            // every frame, and benchmarking at 0 or 1 would measure a case the
            // renderer almost never sees.
            extract(&mut world, &mut view, 0.5, ChunkCoord::new(8, 8));
        });
    });
    group.finish();

    assert_eq!(
        view.len(),
        INSTANCE_COUNT,
        "every instance should have been extracted"
    );

    gate::assert_within(
        "extract_100k_instances",
        gate::measured_mean("extract_100k_instances/interpolated"),
        targets::EXTRACT_100K_INSTANCES,
    );
}

criterion_group!(m1_extract, bench_extract_100k_instances);
criterion_main!(m1_extract);
