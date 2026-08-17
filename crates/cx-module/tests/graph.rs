//! S21 acceptance tests for the architecture graph export.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use cx_ecs::Phase;
use cx_module::graph::{export, writers_of};
use cx_module::{
    Access, Capability, Degradation, Module, ModuleId, Registrar, Registry, Version, cap,
};

fn noop() {}

/// Provides terrain, and owns `ELEVATION` — one of its two permitted writers
/// (`ADR-0011`).
struct TerrainModule;

impl Module for TerrainModule {
    const ID: ModuleId = ModuleId("terrain");
    const VERSION: Version = Version::new(1, 0);

    fn provides() -> &'static [Capability] {
        &[cap::TERRAIN]
    }

    fn register(registrar: &mut Registrar) {
        registrar.field("ELEVATION", 4);
        registrar.system(Phase::ChunkLifecycle, "generate_terrain", noop);
        registrar.access("generate_terrain", "ELEVATION", Access::Write);
    }
}

/// The second permitted `ELEVATION` writer: discrete edits (S19).
struct EditModule;

impl Module for EditModule {
    const ID: ModuleId = ModuleId("edit");

    fn provides() -> &'static [Capability] {
        &[cap::TERRAIN_EDIT]
    }

    fn requires() -> &'static [Capability] {
        &[cap::TERRAIN]
    }

    fn register(registrar: &mut Registrar) {
        registrar.system(Phase::TerrainEdit, "apply_edits", noop);
        registrar.access("apply_edits", "ELEVATION", Access::Write);
    }
}

/// Consumes surface water optionally, so the absent-capability path is exercised.
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
        registrar.system(Phase::AgentSense, "read_nav_grid", noop);
        registrar.access("read_nav_grid", "ELEVATION", Access::Read);
    }
}

fn resolve_in(order: usize) -> cx_module::Resolved {
    let mut registry = Registry::new();

    // Three registration orders over the same set. Which one is used must not
    // reach the exported bytes.
    match order % 3 {
        0 => {
            registry.register::<TerrainModule>();
            registry.register::<EditModule>();
            registry.register::<NavigationModule>();
        }
        1 => {
            registry.register::<NavigationModule>();
            registry.register::<TerrainModule>();
            registry.register::<EditModule>();
        }
        _ => {
            registry.register::<EditModule>();
            registry.register::<NavigationModule>();
            registry.register::<TerrainModule>();
        }
    }

    registry.resolve().expect("the module set should resolve")
}

#[test]
fn s21_acceptance_export_is_byte_identical_across_registration_orders() {
    let baseline = export(&resolve_in(0));

    for order in 1..10 {
        assert_eq!(
            export(&resolve_in(order)),
            baseline,
            "registration order {order} changed the exported bytes; the graph must be a \
             projection of the resolved set, not of how it was registered"
        );
    }
}

#[test]
fn exporting_the_same_set_twice_is_byte_identical() {
    // The property that makes --baseline diffing meaningful.
    assert_eq!(export(&resolve_in(0)), export(&resolve_in(0)));
}

#[test]
fn s21_acceptance_elevation_has_exactly_two_writers() {
    let resolved = resolve_in(0);
    let writers = writers_of(&resolved, "ELEVATION");

    assert_eq!(
        writers,
        vec!["apply_edits", "generate_terrain"],
        "ADR-0011 permits exactly two ELEVATION writers: worldgen and edit application. \
         A third is a defect, which is why this assertion hard-fails rather than annotating."
    );
}

#[test]
fn a_third_elevation_writer_is_detected() {
    // The assertion above is only worth something if it can fail.
    struct RogueModule;
    impl Module for RogueModule {
        const ID: ModuleId = ModuleId("rogue");
        fn register(registrar: &mut Registrar) {
            registrar.system(Phase::FieldSolve, "erode_continuously", noop);
            registrar.access("erode_continuously", "ELEVATION", Access::Write);
        }
    }

    let mut registry = Registry::new();
    registry.register::<TerrainModule>();
    registry.register::<EditModule>();
    registry.register::<RogueModule>();
    let resolved = registry.resolve().expect("resolves");

    let writers = writers_of(&resolved, "ELEVATION");
    assert_eq!(
        writers.len(),
        3,
        "the third writer must be visible: {writers:?}"
    );
    assert!(writers.contains(&"erode_continuously"));
}

#[test]
fn s21_acceptance_absent_capabilities_are_drawn_with_their_degradation() {
    // Navigation optionally consumes SURFACE_WATER; nothing provides it here.
    let payload = export(&resolve_in(0));

    assert!(
        payload.contains("\"name\": \"surface_water\""),
        "an absent capability must still appear as a node:\n{payload}"
    );
    assert!(
        payload.contains("\"present\": false"),
        "it must be marked absent rather than omitted:\n{payload}"
    );
    assert!(
        payload.contains("nav cost omits its water component"),
        "and must carry the declared degraded behaviour:\n{payload}"
    );
}

#[test]
fn systems_carry_their_phase() {
    let payload = export(&resolve_in(0));

    assert!(payload.contains("\"name\": \"apply_edits\""));
    assert!(payload.contains("\"phase\": \"TerrainEdit\""));
    assert!(payload.contains("\"phase\": \"AgentSense\""));
}

#[test]
fn the_payload_carries_its_schema_version_and_schedule_hash() {
    let resolved = resolve_in(0);
    let payload = export(&resolved);

    assert!(payload.contains("\"schema\": \"1.0\""));
    assert!(
        payload.contains(&format!("{:016x}", resolved.schedule_hash())),
        "the payload must be matchable against the save or replay it describes"
    );
}

#[test]
fn moving_a_system_between_phases_changes_the_schedule_hash() {
    // The property behind graph diff catching a phase move in review rather than
    // as a determinism failure at tick 50,000.
    struct EarlyModule;
    impl Module for EarlyModule {
        const ID: ModuleId = ModuleId("mover");
        fn register(registrar: &mut Registrar) {
            registrar.system(Phase::AgentSense, "observe", noop);
        }
    }

    struct LateModule;
    impl Module for LateModule {
        const ID: ModuleId = ModuleId("mover");
        fn register(registrar: &mut Registrar) {
            registrar.system(Phase::AgentAct, "observe", noop);
        }
    }

    let mut early = Registry::new();
    early.register::<EarlyModule>();
    let mut late = Registry::new();
    late.register::<LateModule>();

    assert_ne!(
        early.resolve().expect("resolves").schedule_hash(),
        late.resolve().expect("resolves").schedule_hash(),
        "the phase a system runs in is part of schedule identity"
    );
}

#[test]
fn degraded_behaviour_text_is_json_escaped() {
    struct QuotingModule;
    impl Module for QuotingModule {
        const ID: ModuleId = ModuleId("quoting");
        fn consumes_optional() -> &'static [Capability] {
            &[cap::ECOLOGY]
        }
        fn degradations() -> &'static [Degradation] {
            &[Degradation {
                capability: cap::ECOLOGY,
                behavior: "foraging reports \"no resources\" and is unscheduled",
            }]
        }
        fn register(_registrar: &mut Registrar) {}
    }

    let mut registry = Registry::new();
    registry.register::<QuotingModule>();
    let payload = export(&registry.resolve().expect("resolves"));

    assert!(
        payload.contains("\\\"no resources\\\""),
        "prose written by a module author contains quotes, and must be escaped:\n{payload}"
    );
}
