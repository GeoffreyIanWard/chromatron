//! S20 acceptance tests: resolution, order independence, degradation, and the
//! startup validation table.
//!
//! These use stand-in modules rather than the engine's real ones, which do not
//! exist yet. That is the right test subject either way — what is being asserted
//! is the *mechanism*, and a mechanism tested against real modules would fail
//! for reasons belonging to hydrology.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use cx_ecs::{Phase, Query, ResMut, Resource, SimSchedule, SimWorld, WorldConfig};
use cx_module::{
    Capability, Degradation, Module, ModuleError, ModuleId, Profile, Registrar, Version, cap,
};

#[derive(Resource, Default, Debug)]
struct RunLog(Vec<&'static str>);

fn terrain_system(mut log: ResMut<RunLog>) {
    log.0.push("terrain");
}

fn hydrology_system(mut log: ResMut<RunLog>) {
    log.0.push("hydrology");
}

fn navigation_system(mut log: ResMut<RunLog>) {
    log.0.push("navigation");
}

/// Provides terrain. Depends on nothing.
struct TerrainModule;

impl Module for TerrainModule {
    const ID: ModuleId = ModuleId("terrain");
    const VERSION: Version = Version::new(1, 0);

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

/// Provides climate. Depends on nothing.
struct ClimateModule;

impl Module for ClimateModule {
    const ID: ModuleId = ModuleId("climate");

    fn provides() -> &'static [Capability] {
        &[cap::CLIMATE]
    }

    fn register(registrar: &mut Registrar) {
        registrar.field("TEMPERATURE", 2);
    }
}

/// Requires terrain and climate; provides water. The spec's own example.
struct HydrologyModule;

impl Module for HydrologyModule {
    const ID: ModuleId = ModuleId("hydrology");

    fn provides() -> &'static [Capability] {
        &[cap::SURFACE_WATER, cap::FLOW_NETWORK]
    }

    fn requires() -> &'static [Capability] {
        &[cap::TERRAIN, cap::CLIMATE]
    }

    fn consumes_optional() -> &'static [Capability] {
        &[cap::TERRAIN_EDIT]
    }

    fn degradations() -> &'static [Degradation] {
        &[Degradation {
            capability: cap::TERRAIN_EDIT,
            behavior: "drainage is computed once at generation and never repaired",
        }]
    }

    fn register(registrar: &mut Registrar) {
        registrar.field("WATER_DEPTH", 2).system(
            Phase::FieldSolve,
            "route_discharge",
            hydrology_system,
        );
    }
}

/// Optionally consumes water — the degradation case from S20's table.
struct NavigationModule;

impl Module for NavigationModule {
    const ID: ModuleId = ModuleId("navigation");

    fn provides() -> &'static [Capability] {
        &[cap::NAVIGATION]
    }

    fn requires() -> &'static [Capability] {
        &[cap::TERRAIN]
    }

    fn consumes_optional() -> &'static [Capability] {
        &[cap::SURFACE_WATER]
    }

    fn degradations() -> &'static [Degradation] {
        &[Degradation {
            capability: cap::SURFACE_WATER,
            behavior: "nav cost omits its water component; traversability from slope only",
        }]
    }

    fn register(registrar: &mut Registrar) {
        registrar.field("TRAVERSABILITY", 1).system(
            Phase::AgentSense,
            "rebuild_nav_cost",
            navigation_system,
        );
    }
}

fn full_sim() -> Profile {
    Profile::new("full-sim")
        .with::<TerrainModule>()
        .with::<ClimateModule>()
        .with::<HydrologyModule>()
        .with::<NavigationModule>()
}

fn no_hydrology() -> Profile {
    Profile::new("no-hydrology")
        .with::<TerrainModule>()
        .with::<ClimateModule>()
        .with::<NavigationModule>()
}

#[test]
fn s20_acceptance_resolution_is_order_independent() {
    let baseline = full_sim()
        .build()
        .resolve()
        .expect("full-sim should resolve");

    // First prove the permutations permute. Without this, a build_permuted that
    // silently returned registration order unchanged would make the assertion
    // below pass for the wrong reason.
    let orders: std::collections::BTreeSet<Vec<&str>> = (0..10)
        .map(|permutation| {
            full_sim()
                .build_permuted(permutation)
                .registration_order()
                .iter()
                .map(|id| id.name())
                .collect()
        })
        .collect();
    assert!(
        orders.len() > 2,
        "the permutations must actually reorder registration; got {} distinct orders",
        orders.len()
    );

    for permutation in 1..10 {
        let resolved = full_sim()
            .build_permuted(permutation)
            .resolve()
            .unwrap_or_else(|error| panic!("permutation {permutation} failed: {error}"));

        assert_eq!(
            resolved.schedule_hash(),
            baseline.schedule_hash(),
            "permutation {permutation} produced a different resolved schedule"
        );
        assert_eq!(resolved.world_identity(), baseline.world_identity());
    }
}

#[test]
fn s20_acceptance_dependencies_resolve_before_their_dependents() {
    let resolved = full_sim().build().resolve().expect("resolves");
    let order: Vec<&str> = resolved.modules().map(|record| record.id.name()).collect();

    let position = |name: &str| order.iter().position(|id| *id == name).expect("present");

    assert!(position("terrain") < position("hydrology"));
    assert!(position("climate") < position("hydrology"));
    assert!(position("terrain") < position("navigation"));
    // hydrology provides SURFACE_WATER, which navigation optionally consumes, so
    // it must still be ordered first when present.
    assert!(position("hydrology") < position("navigation"));
}

#[test]
fn s20_acceptance_disabling_a_module_removes_its_systems_and_fields() {
    let full = full_sim().build().resolve().expect("resolves");
    let reduced = no_hydrology().build().resolve().expect("resolves");

    assert!(full.contains_system("route_discharge"));
    assert!(
        !reduced.contains_system("route_discharge"),
        "a disabled module's system must not be scheduled at all — not scheduled behind a \
         branch that returns early (ADR-0012)"
    );

    assert!(
        reduced.field_bytes_per_cell() < full.field_bytes_per_cell(),
        "disabling a module must free its fields, not merely stop stepping them: {} vs {}",
        reduced.field_bytes_per_cell(),
        full.field_bytes_per_cell()
    );
    assert_eq!(
        full.field_bytes_per_cell() - reduced.field_bytes_per_cell(),
        2,
        "exactly WATER_DEPTH's two bytes per cell should disappear"
    );
}

#[test]
fn s20_acceptance_absent_capability_reports_its_declared_degradation() {
    let reduced = no_hydrology().build().resolve().expect("resolves");
    let absent = reduced.absent_capabilities();

    assert_eq!(absent.len(), 1, "only SURFACE_WATER should be missing");
    assert_eq!(absent[0].capability, cap::SURFACE_WATER);
    assert!(
        absent[0].behavior.contains("slope"),
        "the degradation must say what happens instead, got {:?}",
        absent[0].behavior
    );
}

#[test]
fn s20_acceptance_missing_required_capability_names_module_and_capability() {
    // Hydrology without climate: a hard dependency with no provider.
    let error = Profile::new("broken")
        .with::<TerrainModule>()
        .with::<HydrologyModule>()
        .build()
        .resolve()
        .expect_err("should fail");

    let message = error.to_string();
    assert!(message.contains("hydrology"), "{message}");
    assert!(message.contains("climate"), "{message}");
    assert!(matches!(error, ModuleError::MissingCapability { .. }));
}

#[test]
fn s20_acceptance_two_providers_of_one_capability_is_an_error() {
    struct SecondTerrain;
    impl Module for SecondTerrain {
        const ID: ModuleId = ModuleId("terrain_alt");
        fn provides() -> &'static [Capability] {
            &[cap::TERRAIN]
        }
        fn register(_: &mut Registrar) {}
    }

    let error = Profile::new("two-terrains")
        .with::<TerrainModule>()
        .with::<SecondTerrain>()
        .build()
        .resolve()
        .expect_err("should fail");

    let message = error.to_string();
    assert!(
        message.contains("terrain") && message.contains("terrain_alt"),
        "{message}"
    );
    assert!(matches!(error, ModuleError::DuplicateProvider { .. }));
}

#[test]
fn s20_acceptance_duplicate_field_name_names_both_modules() {
    struct FieldSquatter;
    impl Module for FieldSquatter {
        const ID: ModuleId = ModuleId("squatter");
        fn register(registrar: &mut Registrar) {
            registrar.field("ELEVATION", 4);
        }
    }

    let error = Profile::new("clash")
        .with::<TerrainModule>()
        .with::<FieldSquatter>()
        .build()
        .resolve()
        .expect_err("should fail");

    assert!(
        matches!(
            error,
            ModuleError::DuplicateField {
                field: "ELEVATION",
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn s20_acceptance_undeclared_degradation_is_rejected() {
    struct Careless;
    impl Module for Careless {
        const ID: ModuleId = ModuleId("careless");
        fn consumes_optional() -> &'static [Capability] {
            &[cap::SURFACE_WATER]
        }
        // No degradations(): "it'll just be zero" left unwritten.
        fn register(_: &mut Registrar) {}
    }

    let error = Profile::new("careless")
        .with::<Careless>()
        .build()
        .resolve()
        .expect_err("fails");
    assert!(
        matches!(error, ModuleError::UndeclaredDegradation { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("written down"), "{error}");
}

#[test]
fn a_dependency_cycle_is_reported_rather_than_hanging() {
    struct A;
    impl Module for A {
        const ID: ModuleId = ModuleId("a");
        fn provides() -> &'static [Capability] {
            &[cap::TERRAIN]
        }
        fn requires() -> &'static [Capability] {
            &[cap::CLIMATE]
        }
        fn register(_: &mut Registrar) {}
    }
    struct B;
    impl Module for B {
        const ID: ModuleId = ModuleId("b");
        fn provides() -> &'static [Capability] {
            &[cap::CLIMATE]
        }
        fn requires() -> &'static [Capability] {
            &[cap::TERRAIN]
        }
        fn register(_: &mut Registrar) {}
    }

    let error = Profile::new("cycle")
        .with::<A>()
        .with::<B>()
        .build()
        .resolve()
        .expect_err("fails");
    assert!(
        matches!(error, ModuleError::DependencyCycle { .. }),
        "{error}"
    );
}

#[test]
fn resolved_modules_install_their_systems_into_the_schedule() {
    let mut world = SimWorld::new(WorldConfig::default());
    world.insert_resource(RunLog::default());

    let mut schedule = SimSchedule::new();
    let resolved = full_sim()
        .build()
        .build_schedule(&mut schedule)
        .expect("full-sim should resolve");

    assert_eq!(schedule.system_count(), 3);
    assert_eq!(resolved.module_count(), 4);

    schedule.run(&mut world);

    let log = world.resource::<RunLog>().expect("inserted");
    // Phase order decides execution, not module resolution order: ChunkLifecycle
    // precedes FieldSolve precedes AgentSense.
    assert_eq!(log.0, vec!["terrain", "hydrology", "navigation"]);
}

#[test]
fn a_disabled_module_costs_no_tick_time() {
    let mut world = SimWorld::new(WorldConfig::default());
    world.insert_resource(RunLog::default());

    let mut schedule = SimSchedule::new();
    no_hydrology()
        .build()
        .build_schedule(&mut schedule)
        .expect("resolves");

    schedule.run(&mut world);

    let log = world.resource::<RunLog>().expect("inserted");
    assert!(
        !log.0.contains(&"hydrology"),
        "a disabled module must contribute zero systems to the tick"
    );
    assert_eq!(schedule.system_count(), 2);
}

#[test]
fn unused_query_import_is_exercised() {
    // Keeps the Query import honest: a module system with a real query shape.
    fn probe(_query: Query<&RunLog>) {}
    let mut schedule = SimSchedule::new();
    schedule.add_system(Phase::Diagnostics, probe);
    assert_eq!(schedule.system_count(), 1);
}
