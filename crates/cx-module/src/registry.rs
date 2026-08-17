//! Registration, validation, and resolution.
//!
//! Resolution is a topological sort with a stable [`ModuleId`] tiebreak
//! (S20). Registration order must not affect the result — if it did, two
//! machines that registered in different orders would diverge while both
//! believing they ran the same simulation, and every state hash in the project
//! would be suspect.

use std::collections::{BTreeMap, BTreeSet};

use cx_core::hash::{combine, mix64};

use crate::capability::{Capability, Degradation};
use crate::error::ModuleError;
use crate::module::{FieldDecl, Module, ModuleId, Registrar, SystemDecl, Version};
use crate::resolved::{ModuleRecord, Resolved};

/// One registered module, before resolution.
struct Entry {
    id: ModuleId,
    version: Version,
    provides: &'static [Capability],
    requires: &'static [Capability],
    consumes_optional: &'static [Capability],
    degradations: &'static [Degradation],
    systems: Vec<SystemDecl>,
    fields: Vec<FieldDecl>,
}

/// Collects modules, then resolves them into a schedule.
#[derive(Default)]
pub struct Registry {
    entries: Vec<Entry>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("modules", &self.entries.len())
            .finish()
    }
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Registers a module.
    ///
    /// Order does not matter, and the shuffled-registration test exists to keep
    /// it that way.
    pub fn register<M: Module>(&mut self) -> &mut Self {
        let mut registrar = Registrar {
            module: Some(M::ID),
            ..Registrar::default()
        };
        M::register(&mut registrar);

        self.entries.push(Entry {
            id: M::ID,
            version: M::VERSION,
            provides: M::provides(),
            requires: M::requires(),
            consumes_optional: M::consumes_optional(),
            degradations: M::degradations(),
            systems: registrar.systems,
            fields: registrar.fields,
        });
        self
    }

    /// Module ids in the order they were registered.
    ///
    /// Exposed so the order-independence test can assert it is actually testing
    /// something: a permutation that happened not to permute would make that
    /// gate pass for the wrong reason.
    pub fn registration_order(&self) -> Vec<ModuleId> {
        self.entries.iter().map(|entry| entry.id).collect()
    }

    /// How many modules are registered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no modules are registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Validates and orders the module set.
    ///
    /// Every failure here is a startup failure that names the module and the
    /// problem — S20's whole point is that these surface now rather than at tick
    /// 50,000.
    pub fn resolve(self) -> Result<Resolved, ModuleError> {
        let entries = self.validated_entries()?;
        let providers = Self::provider_map(&entries)?;

        self.check_requirements(&entries, &providers)?;
        Self::check_degradations_declared(&entries)?;

        let order = Self::topological_order(&entries, &providers)?;
        Ok(Self::build(entries, order, &providers))
    }

    fn validated_entries(&self) -> Result<Vec<&Entry>, ModuleError> {
        let mut by_id: BTreeMap<ModuleId, &Entry> = BTreeMap::new();
        for entry in &self.entries {
            if by_id.insert(entry.id, entry).is_some() {
                return Err(ModuleError::DuplicateModule { module: entry.id });
            }
        }

        // Field names are global: two modules cannot both own ELEVATION.
        let mut field_owner: BTreeMap<&'static str, ModuleId> = BTreeMap::new();
        for entry in by_id.values() {
            for field in &entry.fields {
                if let Some(existing) = field_owner.insert(field.name, entry.id) {
                    return Err(ModuleError::DuplicateField {
                        field: field.name,
                        first: existing,
                        second: entry.id,
                    });
                }
            }
        }

        // Sorted by ModuleId, so everything downstream is order-independent by
        // construction rather than by remembering to sort later.
        Ok(by_id.into_values().collect())
    }

    fn provider_map(entries: &[&Entry]) -> Result<BTreeMap<Capability, ModuleId>, ModuleError> {
        let mut providers = BTreeMap::new();
        for entry in entries {
            for capability in entry.provides {
                if let Some(existing) = providers.insert(*capability, entry.id) {
                    // Add-only, with exclusivity enforced (S20's resolved open
                    // question). Replacement is more powerful and more dangerous;
                    // nothing wants it yet.
                    return Err(ModuleError::DuplicateProvider {
                        capability: *capability,
                        first: existing,
                        second: entry.id,
                    });
                }
            }
        }
        Ok(providers)
    }

    fn check_requirements(
        &self,
        entries: &[&Entry],
        providers: &BTreeMap<Capability, ModuleId>,
    ) -> Result<(), ModuleError> {
        for entry in entries {
            for capability in entry.requires {
                if !providers.contains_key(capability) {
                    return Err(ModuleError::MissingCapability {
                        module: entry.id,
                        capability: *capability,
                    });
                }
            }
        }
        Ok(())
    }

    fn check_degradations_declared(entries: &[&Entry]) -> Result<(), ModuleError> {
        for entry in entries {
            for capability in entry.consumes_optional {
                let declared = entry
                    .degradations
                    .iter()
                    .any(|degradation| degradation.capability == *capability);
                if !declared {
                    return Err(ModuleError::UndeclaredDegradation {
                        module: entry.id,
                        capability: *capability,
                    });
                }
            }
        }
        Ok(())
    }

    /// Kahn's algorithm with the ready set held in a `BTreeSet`, so ties break by
    /// `ModuleId` rather than by insertion order.
    fn topological_order(
        entries: &[&Entry],
        providers: &BTreeMap<Capability, ModuleId>,
    ) -> Result<Vec<ModuleId>, ModuleError> {
        let mut dependencies: BTreeMap<ModuleId, BTreeSet<ModuleId>> = BTreeMap::new();
        let mut dependents: BTreeMap<ModuleId, BTreeSet<ModuleId>> = BTreeMap::new();

        for entry in entries {
            dependencies.entry(entry.id).or_default();
            dependents.entry(entry.id).or_default();
        }

        for entry in entries {
            // Optional dependencies order the schedule when present, exactly as
            // required ones do — a consumer must still run after its provider.
            let needed = entry.requires.iter().chain(entry.consumes_optional.iter());
            for capability in needed {
                if let Some(provider) = providers.get(capability)
                    && *provider != entry.id
                {
                    dependencies.entry(entry.id).or_default().insert(*provider);
                    dependents.entry(*provider).or_default().insert(entry.id);
                }
            }
        }

        let mut ready: BTreeSet<ModuleId> = dependencies
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(id, _)| *id)
            .collect();

        let mut order = Vec::with_capacity(entries.len());
        while let Some(id) = ready.iter().next().copied() {
            ready.remove(&id);
            order.push(id);

            let downstream = dependents.get(&id).cloned().unwrap_or_default();
            for dependent in downstream {
                if let Some(deps) = dependencies.get_mut(&dependent) {
                    deps.remove(&id);
                    if deps.is_empty() {
                        ready.insert(dependent);
                    }
                }
            }
        }

        if order.len() != entries.len() {
            let cycle: Vec<ModuleId> = dependencies
                .iter()
                .filter(|(_, deps)| !deps.is_empty())
                .map(|(id, _)| *id)
                .collect();
            return Err(ModuleError::DependencyCycle { modules: cycle });
        }

        Ok(order)
    }

    fn build(
        entries: Vec<&Entry>,
        order: Vec<ModuleId>,
        providers: &BTreeMap<Capability, ModuleId>,
    ) -> Resolved {
        let by_id: BTreeMap<ModuleId, &&Entry> =
            entries.iter().map(|entry| (entry.id, entry)).collect();

        let mut records = Vec::with_capacity(order.len());
        let mut absent = Vec::new();

        for id in &order {
            let Some(entry) = by_id.get(id) else {
                continue;
            };

            for capability in entry.consumes_optional {
                if !providers.contains_key(capability) {
                    let behavior = entry
                        .degradations
                        .iter()
                        .find(|degradation| degradation.capability == *capability)
                        .map(|degradation| degradation.behavior)
                        .unwrap_or("undeclared");
                    absent.push(Degradation {
                        capability: *capability,
                        behavior,
                    });
                }
            }

            records.push(ModuleRecord {
                id: entry.id,
                version: entry.version,
                provides: entry.provides,
                requires: entry.requires,
                consumes_optional: entry.consumes_optional,
                degradations: entry.degradations,
                system_names: entry.systems.iter().map(|system| system.name).collect(),
                fields: entry.fields.clone(),
            });
        }

        let hash = Self::schedule_hash(&records);
        Resolved::new(records, absent, hash)
    }

    /// A digest over the resolved schedule.
    ///
    /// Covers module identity and version, and every system's name and phase, in
    /// resolved order. It deliberately does *not* cover registration order —
    /// that is the property the shuffled test asserts.
    fn schedule_hash(records: &[ModuleRecord]) -> u64 {
        let mut hash = mix64(records.len() as u64);
        for record in records {
            for byte in record.id.name().as_bytes() {
                hash = combine(hash, *byte as u64);
            }
            hash = combine(hash, record.version.major as u64);
            hash = combine(hash, record.version.minor as u64);
            for name in &record.system_names {
                for byte in name.as_bytes() {
                    hash = combine(hash, *byte as u64);
                }
            }
            for field in &record.fields {
                for byte in field.name.as_bytes() {
                    hash = combine(hash, *byte as u64);
                }
                hash = combine(hash, field.bytes_per_cell as u64);
            }
        }
        hash
    }

    /// Resolves and installs every system into a schedule.
    pub fn build_schedule(
        self,
        schedule: &mut cx_ecs::SimSchedule,
    ) -> Result<Resolved, ModuleError> {
        let resolved = {
            let entries = self.validated_entries()?;
            let providers = Self::provider_map(&entries)?;
            self.check_requirements(&entries, &providers)?;
            Self::check_degradations_declared(&entries)?;
            let order = Self::topological_order(&entries, &providers)?;
            Self::build(entries, order, &providers)
        };

        // Install in resolved order, so within a phase the systems of an earlier
        // module are added first. Ordering within a phase must not affect
        // results, but a stable installation order keeps the schedule's own
        // internal identity reproducible.
        let order: Vec<ModuleId> = resolved.modules().map(|record| record.id).collect();
        let mut by_id: BTreeMap<ModuleId, Vec<SystemDecl>> = BTreeMap::new();
        for entry in self.entries {
            by_id.insert(entry.id, entry.systems);
        }
        for id in order {
            if let Some(systems) = by_id.remove(&id) {
                for system in systems {
                    system.install(schedule);
                }
            }
        }

        Ok(resolved)
    }
}
