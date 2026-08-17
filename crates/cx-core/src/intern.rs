//! String interning.
//!
//! Content files use strings; the runtime uses [`Id`]. The conversion happens
//! once at load.
//!
//! The determinism requirement shapes the whole design: interning must be
//! **order-independent**, so that loading the same content in a different file
//! order produces identical `Id` assignments. If it did not, `Id`s would leak
//! into state hashes and two machines that walked a content directory in
//! different orders would appear to diverge.
//!
//! That is why this is a two-phase type. Strings are *staged* during load in any
//! order, then the table is *frozen*, which sorts them and assigns ids by sorted
//! position. Ids do not exist before freezing, so there is no window in which an
//! order-dependent id could be observed.

use std::collections::{BTreeMap, BTreeSet};

/// An interned string.
///
/// Assigned by sorted position at freeze time, so the same set of strings always
/// yields the same ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Id(pub u32);

impl Id {
    /// The id of the empty string, which every table interns.
    pub const EMPTY: Id = Id(0);
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Id({})", self.0)
    }
}

/// Collects strings during load. Produces a [`SymbolTable`] when frozen.
#[derive(Debug, Default)]
pub struct Interner {
    staged: BTreeSet<String>,
}

impl Interner {
    /// An interner containing only the empty string.
    pub fn new() -> Self {
        let mut staged = BTreeSet::new();
        staged.insert(String::new());
        Self { staged }
    }

    /// Stages a string. Order of calls does not matter.
    pub fn stage(&mut self, value: &str) {
        if !self.staged.contains(value) {
            self.staged.insert(value.to_owned());
        }
    }

    /// Stages many strings.
    pub fn stage_all<'a>(&mut self, values: impl IntoIterator<Item = &'a str>) {
        for value in values {
            self.stage(value);
        }
    }

    /// How many distinct strings are staged.
    pub fn staged_count(&self) -> usize {
        self.staged.len()
    }

    /// Sorts and assigns ids, producing the immutable runtime table.
    ///
    /// `BTreeSet` already holds the strings in sorted order, so this is a walk
    /// rather than a sort — the ordering guarantee comes from the data structure
    /// rather than from remembering to call `sort` at the right moment.
    pub fn freeze(self) -> SymbolTable {
        let strings: Vec<String> = self.staged.into_iter().collect();
        let lookup: BTreeMap<String, Id> = strings
            .iter()
            .enumerate()
            .map(|(index, value)| (value.clone(), Id(index as u32)))
            .collect();

        SymbolTable { strings, lookup }
    }
}

/// The frozen, immutable intern table used at runtime.
#[derive(Debug, Default, Clone)]
pub struct SymbolTable {
    strings: Vec<String>,
    lookup: BTreeMap<String, Id>,
}

impl SymbolTable {
    /// The id of a string, if it was staged before freezing.
    ///
    /// Returns `None` rather than interning on demand: a string that reaches
    /// runtime without having been staged means content loading missed
    /// something, and silently assigning it an id here would make ids depend on
    /// execution order — exactly what the two-phase design prevents.
    pub fn id(&self, value: &str) -> Option<Id> {
        self.lookup.get(value).copied()
    }

    /// The string behind an id.
    pub fn name(&self, id: Id) -> Option<&str> {
        self.strings.get(id.0 as usize).map(String::as_str)
    }

    /// How many strings the table holds.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// Every string, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (Id, &str)> {
        self.strings
            .iter()
            .enumerate()
            .map(|(index, value)| (Id(index as u32), value.as_str()))
    }

    /// A digest over the table's contents, for verifying that two runs interned
    /// the same set (S13 stores strings, not ids, so a load can check this).
    pub fn content_hash(&self) -> u64 {
        let mut hash = crate::hash::mix64(self.strings.len() as u64);
        for value in &self.strings {
            for byte in value.as_bytes() {
                hash = crate::hash::combine(hash, *byte as u64);
            }
            hash = crate::hash::combine(hash, 0xff);
        }
        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s01_acceptance_interning_is_order_independent() {
        let forward = {
            let mut interner = Interner::new();
            interner.stage_all(["wolf", "deer", "oak", "granite", "river"]);
            interner.freeze()
        };

        let reverse = {
            let mut interner = Interner::new();
            interner.stage_all(["river", "granite", "oak", "deer", "wolf"]);
            interner.freeze()
        };

        let interleaved = {
            let mut interner = Interner::new();
            interner.stage_all(["oak", "wolf", "river", "deer", "granite"]);
            // Staging twice must also not matter.
            interner.stage("oak");
            interner.freeze()
        };

        for name in ["wolf", "deer", "oak", "granite", "river"] {
            assert_eq!(
                forward.id(name),
                reverse.id(name),
                "id for {name} depended on order"
            );
            assert_eq!(
                forward.id(name),
                interleaved.id(name),
                "id for {name} depended on order"
            );
        }

        assert_eq!(forward.content_hash(), reverse.content_hash());
        assert_eq!(forward.content_hash(), interleaved.content_hash());
    }

    #[test]
    fn ids_round_trip_to_their_strings() {
        let mut interner = Interner::new();
        interner.stage_all(["alpha", "beta"]);
        let table = interner.freeze();

        let id = table.id("beta").expect("staged");
        assert_eq!(table.name(id), Some("beta"));
        assert_eq!(table.name(Id(9_999)), None);
    }

    #[test]
    fn empty_string_is_always_id_zero() {
        let table = Interner::new().freeze();
        assert_eq!(table.id(""), Some(Id::EMPTY));
        assert_eq!(table.name(Id::EMPTY), Some(""));
    }

    #[test]
    fn unstaged_strings_do_not_get_ids_at_runtime() {
        let mut interner = Interner::new();
        interner.stage("known");
        let table = interner.freeze();
        assert_eq!(
            table.id("unknown"),
            None,
            "interning on demand would be order-dependent"
        );
    }

    #[test]
    fn duplicate_staging_does_not_grow_the_table() {
        let mut interner = Interner::new();
        for _ in 0..100 {
            interner.stage("repeated");
        }
        let table = interner.freeze();
        assert_eq!(table.len(), 2, "the empty string plus one");
    }

    #[test]
    fn iteration_is_in_id_order() {
        let mut interner = Interner::new();
        interner.stage_all(["c", "a", "b"]);
        let table = interner.freeze();

        let ids: Vec<u32> = table.iter().map(|(id, _)| id.0).collect();
        assert_eq!(ids, vec![0, 1, 2, 3]);

        let names: Vec<&str> = table.iter().map(|(_, name)| name).collect();
        assert_eq!(
            names,
            vec!["", "a", "b", "c"],
            "sorted, so ids are reproducible"
        );
    }
}
