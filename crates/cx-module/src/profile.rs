//! Named module sets.
//!
//! Testing every subset of modules is combinatorial and pointless. Instead a
//! handful of curated sets are gated in CI (S20): `minimal`, `terrain`, `hydro`,
//! `full-sim`, `no-erosion`, `game`, plus a per-module smoke profile of a module
//! and its declared dependencies only — which is what catches a module quietly
//! relying on something it never declared.
//!
//! A profile is a *builder*, not a list of names: it holds the registration
//! functions, so "the `hydro` profile" is something that can be constructed and
//! resolved rather than a string that has to be interpreted somewhere else.

use cx_core::RngStream;

use crate::module::Module;
use crate::registry::Registry;

type RegisterFn = fn(&mut Registry);

/// A named, curated module set.
#[derive(Clone)]
pub struct Profile {
    name: &'static str,
    registrations: Vec<RegisterFn>,
}

impl std::fmt::Debug for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Profile")
            .field("name", &self.name)
            .field("modules", &self.registrations.len())
            .finish()
    }
}

impl Profile {
    /// An empty profile.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            registrations: Vec::new(),
        }
    }

    /// Adds a module.
    #[must_use]
    pub fn with<M: Module>(mut self) -> Self {
        self.registrations.push(|registry: &mut Registry| {
            registry.register::<M>();
        });
        self
    }

    /// The profile's name, as it appears in CI and in save metadata.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// How many modules the profile contains.
    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    /// Whether the profile is empty.
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    /// Registers every module into a fresh registry.
    pub fn build(&self) -> Registry {
        let mut registry = Registry::new();
        for register in &self.registrations {
            register(&mut registry);
        }
        registry
    }

    /// Registers every module in a permuted order.
    ///
    /// Exists for the order-independence gate: the resolved schedule hash must be
    /// identical for every permutation.
    ///
    /// A deterministic Fisher-Yates shuffle keyed on `permutation`, not a random
    /// one — a gate that tests a different arrangement on every run cannot be
    /// bisected when it fails. An earlier version walked the list with a stride,
    /// which silently registered a module twice whenever the stride was not
    /// co-prime with the module count; the gate caught it, which is the argument
    /// for gates-first in one sentence.
    pub fn build_permuted(&self, permutation: usize) -> Registry {
        let count = self.registrations.len();
        let mut indices: Vec<usize> = (0..count).collect();

        let mut rng = RngStream::from_hash(permutation as u64);
        for i in (1..count).rev() {
            let j = rng.next_range(i as u32 + 1) as usize;
            indices.swap(i, j);
        }

        let mut registry = Registry::new();
        for index in indices {
            if let Some(register) = self.registrations.get(index) {
                register(&mut registry);
            }
        }
        registry
    }
}
