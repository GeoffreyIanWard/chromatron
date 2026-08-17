//! Capabilities — the indirection that makes disabling a subsystem safe.
//!
//! Modules never name each other (`ADR-0012`). Navigation does not depend on
//! hydrology; it optionally consumes [`cap::SURFACE_WATER`] and declares what it
//! does when nothing provides that.
//!
//! The point is not decoupling for its own sake. With direct module references,
//! disabling hydrology means every consumer either breaks or carries an
//! untested branch. With capabilities, each consumer has stated in advance what
//! it does without water, and CI runs that configuration.

use std::fmt;

/// A named interface a module provides or consumes.
///
/// A `&'static str` rather than an enum so that a module in a crate this one
/// has never heard of can declare its own. Comparison is by string content, so
/// two crates naming the same capability agree without a shared registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Capability(pub &'static str);

impl Capability {
    /// The capability's name.
    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// The capabilities the engine's own subsystems provide.
///
/// Listed here so the vocabulary is discoverable in one place; a module may
/// still declare a capability of its own without adding it here.
pub mod cap {
    use super::Capability;

    /// Terrain elevation exists and can be sampled (S07).
    pub const TERRAIN: Capability = Capability("terrain");
    /// Terrain can be modified by discrete edits (S19, `ADR-0011`).
    pub const TERRAIN_EDIT: Capability = Capability("terrain_edit");
    /// Temperature and precipitation fields exist (S08).
    pub const CLIMATE: Capability = Capability("climate");
    /// Surface water level and extent can be sampled (S08, `ADR-0009`).
    pub const SURFACE_WATER: Capability = Capability("surface_water");
    /// The river channel graph with discharge per edge (S08).
    pub const FLOW_NETWORK: Capability = Capability("flow_network");
    /// Biomass and vegetation state (S08).
    pub const ECOLOGY: Capability = Capability("ecology");
    /// Spatial queries: neighbours, raycasts (S05).
    pub const SPATIAL_INDEX: Capability = Capability("spatial_index");
    /// Agents exist and are stepped (S10).
    pub const AGENTS: Capability = Capability("agents");
    /// Navigation cost grids and pathfinding (S10).
    pub const NAVIGATION: Capability = Capability("navigation");
    /// Rigid body simulation (S11).
    pub const PHYSICS: Capability = Capability("physics");
    /// Simulation level-of-detail tiering and fast-forward (S09).
    pub const SIM_LOD: Capability = Capability("sim_lod");
    /// Dense field storage and kernels (S06).
    pub const FIELDS: Capability = Capability("fields");
    /// State hashing, metrics, invariants (S14).
    pub const DIAGNOSTICS: Capability = Capability("diagnostics");
}

/// What a module does when a capability it optionally consumes has no provider.
///
/// S20 requires this to be written down *before* the code exists: "it'll just be
/// zero" is a design decision. It is also what the S21 graph renders on an
/// absent-capability node, so an undeclared degradation is invisible twice over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Degradation {
    /// The capability that may be absent.
    pub capability: Capability,
    /// What the consumer does instead, in one sentence.
    pub behavior: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_compare_by_name() {
        assert_eq!(Capability("water"), Capability("water"));
        assert_ne!(cap::SURFACE_WATER, cap::FLOW_NETWORK);
    }

    #[test]
    fn engine_capability_names_are_unique() {
        let all = [
            cap::TERRAIN,
            cap::TERRAIN_EDIT,
            cap::CLIMATE,
            cap::SURFACE_WATER,
            cap::FLOW_NETWORK,
            cap::ECOLOGY,
            cap::SPATIAL_INDEX,
            cap::AGENTS,
            cap::NAVIGATION,
            cap::PHYSICS,
            cap::SIM_LOD,
            cap::FIELDS,
            cap::DIAGNOSTICS,
        ];
        let unique: std::collections::BTreeSet<&str> =
            all.iter().map(|capability| capability.name()).collect();
        assert_eq!(unique.len(), all.len(), "two capabilities share a name");
    }
}
