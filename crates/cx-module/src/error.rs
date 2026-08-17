//! Startup validation failures.
//!
//! S20 requires each of these to fail at startup with a message naming the
//! module and the problem — never at tick 50,000. The messages are written for
//! someone who did not write the module and is reading a CI log.

use crate::capability::Capability;
use crate::module::ModuleId;

/// Why a module set could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ModuleError {
    /// A required capability has no provider in this module set.
    #[error(
        "module `{module}` requires capability `{capability}`, which no enabled module provides. \
         Either enable a module that provides it, or make the dependency optional and declare \
         what `{module}` does without it (S20)."
    )]
    MissingCapability {
        /// The module with the unmet requirement.
        module: ModuleId,
        /// The capability nothing provides.
        capability: Capability,
    },

    /// Two modules both claim to provide the same capability.
    #[error(
        "modules `{first}` and `{second}` both provide capability `{capability}`. Capability \
         providers are add-only and exclusive (S20): a capability has exactly one provider, so \
         consumers cannot silently depend on which one won."
    )]
    DuplicateProvider {
        /// The contested capability.
        capability: Capability,
        /// The module registered first, by id order.
        first: ModuleId,
        /// The other module.
        second: ModuleId,
    },

    /// The same module id was registered twice.
    #[error(
        "module `{module}` is registered twice; module ids are unique and part of world identity"
    )]
    DuplicateModule {
        /// The repeated id.
        module: ModuleId,
    },

    /// Two modules registered a field of the same name.
    #[error(
        "modules `{first}` and `{second}` both register field `{field}`. Field names are global \
         (S06), because a field is addressed by name across module boundaries."
    )]
    DuplicateField {
        /// The contested field name.
        field: &'static str,
        /// The module registered first, by id order.
        first: ModuleId,
        /// The other module.
        second: ModuleId,
    },

    /// A module consumes a capability optionally without saying what it does
    /// when that capability is absent.
    #[error(
        "module `{module}` optionally consumes `{capability}` but declares no behaviour for its \
         absence. \"It'll just be zero\" is a design decision and gets written down \
         (03-conventions.md) — add a Degradation entry saying what happens."
    )]
    UndeclaredDegradation {
        /// The module missing a declaration.
        module: ModuleId,
        /// The capability whose absence is undeclared.
        capability: Capability,
    },

    /// The dependency graph contains a cycle.
    #[error(
        "dependency cycle between modules: {}. Capabilities are a directed graph; a cycle means \
         two modules each need the other resolved first, which has no valid schedule.",
        .modules.iter().map(|module| module.name()).collect::<Vec<_>>().join(" -> ")
    )]
    DependencyCycle {
        /// The modules that could not be ordered.
        modules: Vec<ModuleId>,
    },
}
