//! M0 module gates (S20) — `module_resolution_order_independence`,
//! `disabled_module_zero_cost`.
//!
//! Both are correctness claims measured like benchmarks, because the second one
//! is only meaningful as a measurement: `ADR-0012` promises a disabled module
//! costs *zero* ticks and *zero* bytes, not that it costs little. The difference
//! between "does not run" and "runs a branch that does nothing" is invisible in a
//! unit test and obvious on a stopwatch.
//!
//! The modules here are stand-ins. The engine's real ones do not exist yet, and
//! the mechanism is what these gates are about — a resolution test against real
//! hydrology would fail for reasons belonging to hydrology.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use cx_ecs::{Phase, ResMut, Resource, SimSchedule, SimWorld, WorldConfig};
use cx_module::{Capability, Degradation, Module, ModuleId, Profile, Registrar, cap};

#[derive(Resource, Default, Debug)]
struct TickLog(u32);

fn terrain_system(mut log: ResMut<TickLog>) {
    log.0 += 1;
}

fn erosion_system(mut log: ResMut<TickLog>) {
    log.0 += 100;
}

struct TerrainModule;
impl Module for TerrainModule {
    const ID: ModuleId = ModuleId("terrain");
    fn provides() -> &'static [Capability] {
        &[cap::TERRAIN]
    }
    fn register(registrar: &mut Registrar) {
        registrar.field("ELEVATION", 4).system(
            Phase::ChunkLifecycle,
            "generate_terrain",
            terrain_system,
        );
    }
}

/// Stands in for the erosion generation stage that `no-erosion` switches off.
struct ErosionModule;
impl Module for ErosionModule {
    const ID: ModuleId = ModuleId("erosion");
    fn requires() -> &'static [Capability] {
        &[cap::TERRAIN]
    }
    fn register(registrar: &mut Registrar) {
        registrar.field("FLOW_ACCUM", 2).system(
            Phase::ChunkLifecycle,
            "apply_erosion",
            erosion_system,
        );
    }
}

struct HydrologyModule;
impl Module for HydrologyModule {
    const ID: ModuleId = ModuleId("hydrology");
    fn provides() -> &'static [Capability] {
        &[cap::SURFACE_WATER]
    }
    fn requires() -> &'static [Capability] {
        &[cap::TERRAIN]
    }
    fn consumes_optional() -> &'static [Capability] {
        &[cap::TERRAIN_EDIT]
    }
    fn degradations() -> &'static [Degradation] {
        &[Degradation {
            capability: cap::TERRAIN_EDIT,
            behavior: "drainage computed once at generation, never repaired",
        }]
    }
    fn register(registrar: &mut Registrar) {
        registrar.field("WATER_DEPTH", 2);
    }
}

fn full_sim() -> Profile {
    Profile::new("full-sim")
        .with::<TerrainModule>()
        .with::<ErosionModule>()
        .with::<HydrologyModule>()
}

fn no_erosion() -> Profile {
    Profile::new("no-erosion")
        .with::<TerrainModule>()
        .with::<HydrologyModule>()
}

/// `module_resolution_order_independence` — identical schedule hash across ten
/// permuted registration orders.
///
/// If this fails, every state hash in the project is suspect: two machines that
/// registered modules in different orders would diverge while both believing
/// they ran the same simulation.
fn bench_resolution_order_independence(c: &mut Criterion) {
    let baseline = full_sim()
        .build()
        .resolve()
        .expect("full-sim should resolve");

    let mut distinct_orders = std::collections::BTreeSet::new();
    for permutation in 0..10 {
        let registry = full_sim().build_permuted(permutation);
        distinct_orders.insert(
            registry
                .registration_order()
                .iter()
                .map(|id| id.name())
                .collect::<Vec<_>>(),
        );

        let resolved = registry
            .resolve()
            .unwrap_or_else(|error| panic!("permutation {permutation} failed to resolve: {error}"));

        assert_eq!(
            resolved.schedule_hash(),
            baseline.schedule_hash(),
            "gate module_resolution_order_independence: permutation {permutation} produced a \
             different resolved schedule than permutation 0.\n\n\
             S20 requires a topological sort with a stable ModuleId tiebreak. An order-dependent \
             schedule means state hashes are not comparable between runs, which invalidates the \
             determinism gates and every golden test built on them."
        );
    }

    assert!(
        distinct_orders.len() > 2,
        "gate module_resolution_order_independence is not testing anything: the permutations \
         produced only {} distinct registration orders",
        distinct_orders.len()
    );

    let mut group = c.benchmark_group("module_resolution");
    group.bench_function("resolve_full_sim", |b| {
        b.iter(|| black_box(full_sim().build().resolve()));
    });
    group.finish();
}

/// `disabled_module_zero_cost` — a disabled module contributes no tick time and
/// no field allocations.
fn bench_disabled_module_zero_cost(_c: &mut Criterion) {
    let full = full_sim().build().resolve().expect("full-sim resolves");
    let reduced = no_erosion().build().resolve().expect("no-erosion resolves");

    assert!(full.contains_system("apply_erosion"));
    assert!(
        !reduced.contains_system("apply_erosion"),
        "gate disabled_module_zero_cost: a disabled module's system is still scheduled.\n\n\
         ADR-0012: degradation resolves at schedule-build time. The system must not be \
         scheduled at all — not scheduled behind a branch that returns early."
    );

    assert_eq!(
        full.field_bytes_per_cell() - reduced.field_bytes_per_cell(),
        2,
        "gate disabled_module_zero_cost: disabling erosion must free FLOW_ACCUM's bytes, not \
         merely stop stepping them (S20, docs/bench/memory-budget.md)"
    );

    // And the same claim measured on the tick rather than in the registry: the
    // disabled module's system must contribute nothing to what actually runs.
    let mut world = SimWorld::new(WorldConfig::default());
    world.insert_resource(TickLog::default());

    let mut schedule = SimSchedule::new();
    no_erosion()
        .build()
        .build_schedule(&mut schedule)
        .expect("resolves");
    schedule.run(&mut world);

    let log = world.resource::<TickLog>().expect("inserted");
    assert_eq!(
        log.0, 1,
        "gate disabled_module_zero_cost: erosion contributed to the tick despite being disabled"
    );
}

/// Every optionally-consumed capability must declare what happens when it is
/// absent — the mechanical half of a rule S20 states in prose.
fn bench_optional_capabilities_declare_degradation(_c: &mut Criterion) {
    let resolved = no_erosion().build().resolve().expect("resolves");

    for record in resolved.modules() {
        for capability in record.consumes_optional {
            assert!(
                record.degradation_for(*capability).is_some(),
                "gate: module `{}` optionally consumes `{capability}` without declaring what it \
                 does when that is absent. \"It'll just be zero\" is a design decision and gets \
                 written down (03-conventions.md).",
                record.id
            );
        }
    }

    // TERRAIN_EDIT has no provider in either profile, so hydrology's declared
    // degradation should surface as an absent capability rather than silence.
    let absent = resolved.absent_capabilities();
    assert!(
        absent
            .iter()
            .any(|degradation| degradation.capability == cap::TERRAIN_EDIT),
        "an optional capability with no provider should be reported, got {absent:?}"
    );
}

criterion_group!(
    m0_module,
    bench_resolution_order_independence,
    bench_disabled_module_zero_cost,
    bench_optional_capabilities_declare_degradation
);
criterion_main!(m0_module);
