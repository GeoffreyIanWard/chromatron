//! `cx-fields` as a module (S20).
//!
//! The first engine subsystem to declare itself. Until now every module in the
//! codebase was a test fixture, which meant the module system was exercised but
//! never *used* — profiles resolved to nothing and the S21 graph exported an
//! empty diagram.
//!
//! What this module provides is **storage**, not data. It owns no fields:
//! `ELEVATION` belongs to worldgen (S07), `BIOMASS` to ecology (S08). Each
//! registers its own, which is what makes disabling ecology genuinely free its
//! memory rather than merely stop stepping it (`ADR-0012`).

use bevy_ecs::resource::Resource;
use cx_ecs::{Phase, ResMut};
use cx_module::{Capability, Module, ModuleId, Registrar, Version, cap};

use crate::store::{FieldStore, StoreConfig};

/// The field store, as a sim resource.
///
/// A newtype rather than implementing `Resource` on [`FieldStore`] directly, so
/// that `cx-fields` stays usable — and testable — without an ECS world. The
/// storage layer has no reason to require one.
#[derive(Resource, Debug)]
pub struct Fields(pub FieldStore);

impl Fields {
    /// A resource wrapping a store built from `config`.
    pub fn new(config: StoreConfig) -> Self {
        Self(FieldStore::new(config))
    }

    /// The store.
    pub const fn store(&self) -> &FieldStore {
        &self.0
    }

    /// The store, mutably.
    pub const fn store_mut(&mut self) -> &mut FieldStore {
        &mut self.0
    }
}

impl Default for Fields {
    fn default() -> Self {
        Self::new(StoreConfig::default())
    }
}

/// Provides dense field storage: chunked SoA arrays, halos, kernels, sampling.
pub struct FieldsModule;

impl Module for FieldsModule {
    const ID: ModuleId = ModuleId("fields");
    const VERSION: Version = Version::new(1, 0);

    fn provides() -> &'static [Capability] {
        &[cap::FIELDS]
    }

    fn register(registrar: &mut Registrar) {
        // Halo exchange is its own sub-phase within FieldSolve (S06), and runs
        // before any solver: a kernel reading a stale halo sees a cliff at the
        // chunk seam that does not exist in the world.
        registrar.system(Phase::FieldSolve, "exchange_halos", exchange_halos);
    }
}

/// Refreshes every registered field's halo ring from its neighbours.
///
/// Exchanges all registered fields rather than taking a list: a field whose halo
/// is stale is a bug that shows up as a seam much later, and the cost is
/// proportional to loaded chunks rather than to cells.
fn exchange_halos(mut fields: ResMut<Fields>) {
    let registered: Vec<crate::store::FieldId> = fields.store().registered_fields().collect();
    for field in registered {
        fields.store_mut().exchange_halos(field);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_module::Registry;

    #[test]
    fn the_module_resolves_on_its_own() {
        // S20 requires each module to pass a smoke profile of itself plus its
        // declared dependencies only — which is what catches a module quietly
        // relying on something it never declared. `fields` declares none.
        let mut registry = Registry::new();
        registry.register::<FieldsModule>();

        let resolved = registry.resolve().expect("fields should resolve alone");

        assert_eq!(resolved.modules().count(), 1);
        assert_eq!(resolved.systems().count(), 1);
    }

    #[test]
    fn it_provides_the_fields_capability() {
        assert!(FieldsModule::provides().contains(&cap::FIELDS));
        assert!(
            FieldsModule::requires().is_empty(),
            "storage depends on nothing"
        );
    }

    #[test]
    fn the_module_owns_no_fields() {
        // Storage, not data. ELEVATION belongs to worldgen and BIOMASS to
        // ecology; each registers its own so that disabling one frees its memory.
        let mut registry = Registry::new();
        registry.register::<FieldsModule>();
        let resolved = registry.resolve().expect("resolves");

        let owned: usize = resolved.modules().map(|record| record.fields.len()).sum();
        assert_eq!(owned, 0);
    }
}
