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

    // Pinned against the constant rather than a literal, so a deliberate bump
    // updates one place. What must not change silently is the *major*, which is
    // what a viewer refuses on — asserted separately below.
    assert!(payload.contains(&format!("\"schema\": \"{}\"", cx_module::SCHEMA_VERSION)));
    assert!(
        payload.contains("\"schema\": \"1."),
        "a major bump breaks every existing viewer and is not something to do by accident"
    );
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

/// Where a declaration was made reaches the graph, and nothing else does.
///
/// S21 wants nodes to link to a file and line so a reader can open the
/// registration rather than go looking for it. The location is captured with
/// `#[track_caller]`, so it is the `registrar.system(...)` call inside the
/// module — not somewhere in the registrar's own internals, which would be the
/// same useless line for every system in the engine.
mod source_links {
    use super::*;

    /// Two modules with identical declarations, made on different lines.
    struct Early;
    struct Late;

    impl Module for Early {
        const ID: ModuleId = ModuleId("early");
        fn register(registrar: &mut Registrar) {
            registrar.system(Phase::AgentAct, "work", || {});
        }
    }

    impl Module for Late {
        const ID: ModuleId = ModuleId("early");

        fn register(registrar: &mut Registrar) {
            // Deliberately further down the file than `Early`'s, so the
            // captured location genuinely differs and the hash test below is
            // not vacuous.

            registrar.system(Phase::AgentAct, "work", || {});
        }
    }

    fn resolve<M: Module>() -> cx_module::Resolved {
        let mut registry = Registry::new();
        registry.register::<M>();
        registry.resolve().expect("the fixture should resolve")
    }

    fn only_system<M: Module>() -> cx_module::SystemRecord {
        *resolve::<M>()
            .modules()
            .flat_map(|record| record.systems.iter())
            .next()
            .expect("the fixture registers one system")
    }

    #[test]
    fn the_location_is_the_registration_not_the_registrar() {
        let system = only_system::<Early>();

        assert!(
            system.source.file.contains("graph.rs"),
            "the location should point at this test file, not into cx-module: {}",
            system.source.file
        );
        assert!(system.source.line > 0);
    }

    #[test]
    fn moving_a_registration_does_not_change_the_schedule_hash() {
        // The hash is world identity: it goes into saves and replays, and
        // ADR-0004 makes changing it a migration. Adding a comment above a
        // registration must not invalidate anyone's save.
        let early = only_system::<Early>();
        let late = only_system::<Late>();

        assert_ne!(
            early.source.line, late.source.line,
            "the fixtures must differ in line, or this proves nothing"
        );
        assert_eq!(
            resolve::<Early>().schedule_hash(),
            resolve::<Late>().schedule_hash(),
            "moving a registration down a file changed world identity"
        );
    }

    #[test]
    fn the_export_carries_the_location() {
        let payload = export(&resolve::<Early>());

        assert!(
            payload.contains("\"source\": \"") && payload.contains("graph.rs:"),
            "the exported graph should carry a source location:\n{payload}"
        );
    }

    #[test]
    fn the_export_is_still_byte_identical_and_carries_no_absolute_path() {
        // Paths come from the compiler and are relative to the workspace root,
        // so every machine building this commit emits the same string. An
        // absolute path would have broken `--baseline` diffing for anyone whose
        // checkout lived somewhere else — and would have leaked a home
        // directory into an artifact.
        let first = export(&resolve::<Early>());
        for _ in 0..5 {
            assert_eq!(export(&resolve::<Early>()), first);
        }

        assert!(
            !first.contains(env!("CARGO_MANIFEST_DIR")),
            "an absolute path reached the payload"
        );
    }
}
