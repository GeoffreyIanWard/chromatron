//! `cx-agents` as a module (S20).
//!
//! The first module with a *module* dependency that is not storage: it requires
//! `spatial_index`, so resolution has a real chain to order — spatial before
//! agents, both after nothing.
//!
//! # Three systems, three phases, on purpose
//!
//! `decide_steering` in `AgentDecide`, then `resolve_claims` and `apply_intents`
//! in `AgentAct`. The split is not organisational: it is what stops one agent's
//! decision from depending on another agent's action having already happened,
//! which within a phase is scheduler order and therefore not reproducible
//! (`ADR-0001`, `ADR-0004`).

use cx_ecs::Phase;
use cx_module::{Capability, Module, ModuleId, Registrar, Version, cap};

use crate::behaviour::{apply_intents, decide_steering, resolve_claims};

/// Agents that sense, decide, and act.
pub struct AgentsModule;

impl Module for AgentsModule {
    const ID: ModuleId = ModuleId("agents");
    const VERSION: Version = Version::new(0, 1);

    fn provides() -> &'static [Capability] {
        &[cap::AGENTS]
    }

    fn requires() -> &'static [Capability] {
        // Hard. Sensing is neighbour queries, and an agent that cannot sense is
        // not a degraded agent — it is a moving object with no behaviour, which
        // is a different thing and should not be reached by accident.
        &[cap::SPATIAL_INDEX]
    }

    fn register(registrar: &mut Registrar) {
        registrar.system(Phase::AgentDecide, "decide_steering", decide_steering);

        // Claims settle before movement. An agent that moved first would be
        // acting on a claim it might not win, which is visible as an agent
        // walking towards something and then turning around.
        registrar.system(Phase::AgentAct, "resolve_claims", resolve_claims);
        registrar.system(Phase::AgentAct, "apply_intents", apply_intents);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_module::Registry;
    use cx_spatial::SpatialModule;

    #[test]
    fn the_module_resolves_with_its_dependency() {
        // S20's per-module smoke profile: the module plus its *declared*
        // dependencies only. What this catches is a module quietly relying on
        // something it never declared.
        let mut registry = Registry::new();
        registry.register::<SpatialModule>();
        registry.register::<AgentsModule>();

        let resolved = registry
            .resolve()
            .expect("agents plus spatial should resolve");

        assert_eq!(resolved.modules().count(), 2);
        assert_eq!(resolved.systems().count(), 4);
    }

    #[test]
    fn it_does_not_resolve_without_a_spatial_index() {
        // `requires`, not `consumes_optional`. An agent that cannot sense is not
        // a degraded agent; it is a moving object with no behaviour.
        let mut registry = Registry::new();
        registry.register::<AgentsModule>();

        assert!(
            registry.resolve().is_err(),
            "agents require a spatial index and must refuse to resolve without one"
        );
    }

    #[test]
    fn deciding_and_acting_are_in_different_phases() {
        // The structural claim the whole crate rests on. If these ever collapse
        // into one phase, agents start reading a world other agents have already
        // changed, and the tick stops being reproducible.
        let mut registry = Registry::new();
        registry.register::<SpatialModule>();
        registry.register::<AgentsModule>();
        let resolved = registry.resolve().expect("resolves");

        let phase_of = |name: &str| {
            resolved
                .modules()
                .flat_map(|record| record.systems.iter())
                .find(|system| system.name == name)
                .map(|system| system.phase)
        };

        assert_eq!(phase_of("decide_steering"), Some(Phase::AgentDecide));
        assert_eq!(phase_of("apply_intents"), Some(Phase::AgentAct));
        assert_ne!(
            phase_of("decide_steering"),
            phase_of("apply_intents"),
            "deciding and acting must not share a phase"
        );
    }

    #[test]
    fn claims_settle_in_the_same_phase_as_movement() {
        // Both in AgentAct: a claim resolved in a later phase than the movement
        // it justifies would let an agent move toward something it then loses.
        let mut registry = Registry::new();
        registry.register::<SpatialModule>();
        registry.register::<AgentsModule>();
        let resolved = registry.resolve().expect("resolves");

        let phases: Vec<Phase> = resolved
            .modules()
            .flat_map(|record| record.systems.iter())
            .filter(|system| matches!(system.name, "resolve_claims" | "apply_intents"))
            .map(|system| system.phase)
            .collect();

        assert_eq!(phases.len(), 2);
        assert!(phases.iter().all(|phase| *phase == Phase::AgentAct));
    }

    #[test]
    fn it_owns_no_fields() {
        // Agents are entities, not field data. Nothing here belongs in the
        // dense store.
        let mut registry = Registry::new();
        registry.register::<SpatialModule>();
        registry.register::<AgentsModule>();
        let resolved = registry.resolve().expect("resolves");

        let owned: usize = resolved.modules().map(|record| record.fields.len()).sum();
        assert_eq!(owned, 0);
    }
}
