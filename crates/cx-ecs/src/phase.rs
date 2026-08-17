//! The fixed tick phases.
//!
//! From `02-architecture.md`: these are **not composable**. Modules insert
//! systems into phases; they never add, remove, or reorder them. That is
//! deliberate — the phase list is the ordering contract that makes parallel
//! execution safe and results order-independent. If phases were composable,
//! determinism would depend on module load order (`ADR-0012`).
//!
//! The read-then-write separation is the part to understand before adding a
//! system anywhere. `AgentSense` reads fields and neighbours and writes nothing;
//! `AgentDecide` produces intents; `AgentAct` applies them. A system that both
//! reads shared neighbour state and writes shared state within one phase makes
//! the result depend on execution order, which is the bug this structure exists
//! to prevent.

use bevy_ecs::schedule::SystemSet;

/// A tick phase. Systems run inside exactly one.
///
/// The discriminants are not meaningful; [`Phase::ORDER`] is the contract.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// 1 — apply buffered player and script commands.
    IntakeCommands,
    /// 2 — activate, demote, and dormant-ize chunks; fast-forward loads.
    ChunkLifecycle,
    /// 2b — apply `EditCommand`s, mark dirty tiles, repair drainage (S19).
    ///
    /// Numbered `2b` in `02-architecture.md` because it was added after the
    /// original twelve were numbered, and renumbering would have invalidated
    /// every reference to a phase number in the doc set.
    TerrainEdit,
    /// 3 — climate, then hydrology, then ecology, in that fixed order. No
    /// erosion: that is a generation stage (`ADR-0008`).
    FieldSolve,
    /// 4 — rebuild the spatial index from last tick's positions.
    SpatialRebuild,
    /// 5 — read fields and neighbours. **Writes nothing.**
    AgentSense,
    /// 6 — behaviour. Produces intents only.
    AgentDecide,
    /// 7 — apply intents; movement integration.
    AgentAct,
    /// 8 — rapier step.
    Physics,
    /// 9 — apply queued entity-to-field writes from the deposit buffer.
    FieldDeposit,
    /// 10 — drain and dispatch double-buffered events.
    Events,
    /// 11 — apply command buffers: spawn, despawn, insert, remove.
    ///
    /// This is where every structural change lands. Archetype moves are the
    /// dominant cost in an archetypal ECS (`ADR-0001`), so they are batched here
    /// rather than scattered through the tick.
    StructuralApply,
    /// 12 — metrics, invariants, state hash.
    Diagnostics,
}

impl Phase {
    /// Every phase, in execution order. This slice *is* the ordering contract.
    pub const ORDER: [Phase; 13] = [
        Phase::IntakeCommands,
        Phase::ChunkLifecycle,
        Phase::TerrainEdit,
        Phase::FieldSolve,
        Phase::SpatialRebuild,
        Phase::AgentSense,
        Phase::AgentDecide,
        Phase::AgentAct,
        Phase::Physics,
        Phase::FieldDeposit,
        Phase::Events,
        Phase::StructuralApply,
        Phase::Diagnostics,
    ];

    /// Position in the tick, from 0.
    pub fn index(self) -> usize {
        Phase::ORDER
            .iter()
            .position(|phase| *phase == self)
            .unwrap_or(0)
    }

    /// The name used in diagnostics, the S21 graph, and profiling spans.
    pub const fn name(self) -> &'static str {
        match self {
            Phase::IntakeCommands => "IntakeCommands",
            Phase::ChunkLifecycle => "ChunkLifecycle",
            Phase::TerrainEdit => "TerrainEdit",
            Phase::FieldSolve => "FieldSolve",
            Phase::SpatialRebuild => "SpatialRebuild",
            Phase::AgentSense => "AgentSense",
            Phase::AgentDecide => "AgentDecide",
            Phase::AgentAct => "AgentAct",
            Phase::Physics => "Physics",
            Phase::FieldDeposit => "FieldDeposit",
            Phase::Events => "Events",
            Phase::StructuralApply => "StructuralApply",
            Phase::Diagnostics => "Diagnostics",
        }
    }

    /// Whether systems in this phase may write shared state.
    ///
    /// `AgentSense` is read-only by contract, and a system registered there that
    /// takes a mutable parameter is a design error rather than a compile error —
    /// this is what `cx-diag` checks against at registration time.
    pub const fn is_read_only(self) -> bool {
        matches!(self, Phase::AgentSense)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_contains_every_phase_exactly_once() {
        let mut seen = std::collections::BTreeSet::new();
        for phase in Phase::ORDER {
            assert!(seen.insert(phase), "{phase:?} appears twice in ORDER");
        }
        assert_eq!(seen.len(), Phase::ORDER.len());
    }

    #[test]
    fn indices_are_strictly_increasing_in_declared_order() {
        for pair in Phase::ORDER.windows(2) {
            let (first, second) = (pair[0], pair[1]);
            assert!(
                first.index() < second.index(),
                "{first:?} should precede {second:?}"
            );
        }
    }

    #[test]
    fn read_then_write_phases_are_in_the_right_relative_order() {
        // The property this ordering exists for: sense before decide before act,
        // and deposits after everything that might produce them.
        assert!(Phase::AgentSense.index() < Phase::AgentDecide.index());
        assert!(Phase::AgentDecide.index() < Phase::AgentAct.index());
        assert!(Phase::AgentAct.index() < Phase::FieldDeposit.index());
        assert!(Phase::FieldDeposit.index() < Phase::StructuralApply.index());
        assert!(Phase::StructuralApply.index() < Phase::Diagnostics.index());
    }
}
