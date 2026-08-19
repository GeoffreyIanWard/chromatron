//! The curated profiles from S20.
//!
//! Profiles live here rather than in `cx-module` because a profile names actual
//! modules, and `cx-module` must not depend on the subsystems that implement its
//! trait — that is the dependency direction `ADR-0012` exists to prevent. The
//! facade is the layer that can see both.
//!
//! # Why most of these are still thin
//!
//! `cx-fields`, `cx-worldgen`, `cx-spatial`, and `cx-agents` are modules; the
//! solvers and physics register nothing yet. Each fills in as its crate becomes
//! a module, and the profile names exist now so that the CLI, the gates, and
//! S20's profile rule have a stable surface to build against.
//!
//! `spatial` and `agents` are in `full-sim` and `game` but not in `terrain` or
//! `hydro`: an index over sparse entities is worth nothing in a profile with no
//! agents, and a profile that carries a module it cannot use is a profile whose
//! name has stopped describing it.
//!
//! They also go in together. `agents` *requires* `spatial_index`, so a profile
//! with one and not the other does not resolve at all — which is the module
//! system doing its job rather than a constraint to work around.
//!
//! A profile that is thinner than its documentation is worth having anyway: the
//! *mechanism* is what M0 needed to prove, and a named set that resolves, hashes,
//! and exports is that proof. What `terrain` adds is the first profile that is
//! genuinely *different* from `minimal` — two modules, a `requires` edge between
//! them, and a field with an owner and a declared writer, which is the first
//! graph with anything to look at.

use cx_agents::AgentsModule;
use cx_fields::FieldsModule;
use cx_module::Profile;
use cx_spatial::SpatialModule;
use cx_worldgen::WorldgenModule;

/// Core, ECS, time, fields — the M0 benchmark set.
pub fn minimal() -> Profile {
    Profile::new("minimal").with::<FieldsModule>()
}

/// `minimal` plus worldgen, erosion, and rendering (S20).
///
/// Erosion is still M2; what exists is base elevation (S07 step 1).
pub fn terrain() -> Profile {
    Profile::new("terrain")
        .with::<FieldsModule>()
        .with::<WorldgenModule>()
}

/// `terrain` plus climate and hydrology (S20). Solvers land at M4.
pub fn hydro() -> Profile {
    Profile::new("hydro")
        .with::<FieldsModule>()
        .with::<WorldgenModule>()
}

/// Every simulation module, headless (S20).
pub fn full_sim() -> Profile {
    Profile::new("full-sim")
        .with::<FieldsModule>()
        .with::<WorldgenModule>()
        .with::<SpatialModule>()
        .with::<AgentsModule>()
}

/// `full-sim` minus the erosion generation stage (S20).
///
/// The profile that proves the toggle works end to end. It cannot do that yet —
/// erosion is an M2 generation stage — but the name resolves so nothing has to
/// be renamed later.
pub fn no_erosion() -> Profile {
    Profile::new("no-erosion")
        .with::<FieldsModule>()
        .with::<WorldgenModule>()
        .with::<SpatialModule>()
        .with::<AgentsModule>()
}

/// Everything, including presentation (S20).
pub fn game() -> Profile {
    Profile::new("game")
        .with::<FieldsModule>()
        .with::<WorldgenModule>()
        .with::<SpatialModule>()
        .with::<AgentsModule>()
}

/// A profile by name, or `None` if unknown.
///
/// Returns `None` rather than falling back to a default: a typo in `--profile`
/// silently resolving to something else is how a benchmark ends up measuring a
/// configuration nobody chose.
pub fn by_name(name: &str) -> Option<Profile> {
    match name {
        "minimal" => Some(minimal()),
        "terrain" => Some(terrain()),
        "hydro" => Some(hydro()),
        "full-sim" => Some(full_sim()),
        "no-erosion" => Some(no_erosion()),
        "game" => Some(game()),
        _ => None,
    }
}

/// Every profile name, for help text and for the CI matrix.
pub const NAMES: &[&str] = &[
    "minimal",
    "terrain",
    "hydro",
    "full-sim",
    "no-erosion",
    "game",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_profile_resolves() {
        for name in NAMES {
            let profile = by_name(name).unwrap_or_else(|| panic!("{name} should exist"));
            profile
                .build()
                .resolve()
                .unwrap_or_else(|error| panic!("profile {name} failed to resolve: {error}"));
        }
    }

    #[test]
    fn an_unknown_profile_is_rejected_rather_than_defaulted() {
        assert!(
            by_name("mnimal").is_none(),
            "a typo must not resolve to something else"
        );
    }

    #[test]
    fn profiles_resolve_to_the_same_schedule_hash_across_registration_orders() {
        // The S20 property, now over a real module rather than a test fixture.
        let baseline = minimal()
            .build()
            .resolve()
            .expect("resolves")
            .schedule_hash();

        for permutation in 0..10 {
            let hash = minimal()
                .build_permuted(permutation)
                .resolve()
                .expect("resolves")
                .schedule_hash();
            assert_eq!(
                hash, baseline,
                "permutation {permutation} changed the schedule hash"
            );
        }
    }

    #[test]
    fn the_minimal_profile_actually_contains_something() {
        // The regression this whole change exists to prevent: profiles that
        // resolve to nothing look healthy and prove nothing.
        let resolved = minimal().build().resolve().expect("resolves");
        assert!(resolved.modules().count() > 0, "minimal must not be empty");
        assert!(
            resolved.systems().count() > 0,
            "minimal must schedule something"
        );
    }
}
