//! The resolved module set.
//!
//! This is what S21 exports as the architecture graph: not a drawing of the
//! architecture, but the structure the engine actually built at startup.

use crate::capability::{Capability, Degradation};
use crate::module::{FieldDecl, ModuleId, Version};

/// One module, as resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleRecord {
    /// Stable identity.
    pub id: ModuleId,
    /// Version, part of world identity.
    pub version: Version,
    /// Capabilities this module provides.
    pub provides: &'static [Capability],
    /// Hard dependencies.
    pub requires: &'static [Capability],
    /// Soft dependencies.
    pub consumes_optional: &'static [Capability],
    /// Declared behaviour when an optional capability is absent.
    pub degradations: &'static [Degradation],
    /// Systems this module registered, in registration order within the module.
    pub system_names: Vec<&'static str>,
    /// Dense fields this module owns.
    pub fields: Vec<FieldDecl>,
}

impl ModuleRecord {
    /// What this module does when `capability` is absent, if declared.
    pub fn degradation_for(&self, capability: Capability) -> Option<&'static str> {
        self.degradations
            .iter()
            .find(|degradation| degradation.capability == capability)
            .map(|degradation| degradation.behavior)
    }
}

/// A validated, ordered module set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    records: Vec<ModuleRecord>,
    absent: Vec<Degradation>,
    schedule_hash: u64,
}

impl Resolved {
    pub(crate) fn new(
        records: Vec<ModuleRecord>,
        absent: Vec<Degradation>,
        schedule_hash: u64,
    ) -> Self {
        Self {
            records,
            absent,
            schedule_hash,
        }
    }

    /// Modules in resolved order.
    pub fn modules(&self) -> impl Iterator<Item = &ModuleRecord> {
        self.records.iter()
    }

    /// How many modules are enabled.
    pub fn module_count(&self) -> usize {
        self.records.len()
    }

    /// Every registered system name, module-qualified, in resolved order.
    pub fn systems(&self) -> impl Iterator<Item = (ModuleId, &'static str)> {
        self.records
            .iter()
            .flat_map(|record| record.system_names.iter().map(|name| (record.id, *name)))
    }

    /// Whether a named system is scheduled.
    ///
    /// The `disabled_module_zero_cost` gate uses this: a disabled module's
    /// systems must be **absent**, not scheduled behind a branch that returns
    /// early (`ADR-0012`).
    pub fn contains_system(&self, name: &str) -> bool {
        self.records
            .iter()
            .any(|record| record.system_names.contains(&name))
    }

    /// Whether a module is enabled.
    pub fn contains_module(&self, id: ModuleId) -> bool {
        self.records.iter().any(|record| record.id == id)
    }

    /// Digest over module identity, versions, systems, and fields, in resolved
    /// order. Identical for the same set registered in any order.
    pub const fn schedule_hash(&self) -> u64 {
        self.schedule_hash
    }

    /// Bytes per cell across every registered field.
    ///
    /// The measure behind "disabling a module frees its memory rather than
    /// merely idling it": a disabled module's fields are never registered, so
    /// they contribute nothing here.
    pub fn field_bytes_per_cell(&self) -> usize {
        self.records
            .iter()
            .flat_map(|record| record.fields.iter())
            .map(|field| field.bytes_per_cell)
            .sum()
    }

    /// Optional capabilities that ended up with no provider, with the declared
    /// behaviour for each.
    ///
    /// S21 renders these as absent nodes: the degradation that is invisible in
    /// code review is the one worth drawing.
    pub fn absent_capabilities(&self) -> &[Degradation] {
        &self.absent
    }

    /// The module set as recorded in a save (`ADR-0012`, S13).
    ///
    /// Sorted and joined, so a mismatch on load can be diffed rather than merely
    /// reported as unequal.
    pub fn world_identity(&self) -> Vec<String> {
        let mut identity: Vec<String> = self
            .records
            .iter()
            .map(|record| format!("{}@{}", record.id, record.version))
            .collect();
        identity.sort();
        identity
    }
}
