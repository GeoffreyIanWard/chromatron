//! Comparing two exported graphs (S21).
//!
//! # Why this earns the tool a place in CI
//!
//! A pull request that silently adds a second writer to `ELEVATION`, moves a
//! system across a phase boundary, or introduces a new optional dependency shows
//! that fact here, in review, rather than as a determinism bug at tick 50,000.
//!
//! # Annotate, do not block — with one exception
//!
//! S21 settles this: the diff **annotates**. A check that blocks merges on every
//! legitimate architecture change gets switched off within a month, and a check
//! nobody runs is worth less than one that merely reports.
//!
//! The exception is the `ELEVATION` writer count. `ADR-0011` permits exactly two
//! — generation and terrain edits — so a third is a defect rather than a change,
//! and [`Diff::violations`] reports it separately for a caller that wants to
//! fail on it.

use crate::graph::Graph;

/// The field whose writer count is a rule rather than a preference.
const GUARDED_FIELD: &str = "ELEVATION";

/// How many systems `ADR-0011` permits to write it.
const PERMITTED_WRITERS: usize = 2;

/// What changed between two graphs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// Modules present now and not before.
    pub added_modules: Vec<String>,
    /// Modules present before and not now.
    pub removed_modules: Vec<String>,
    /// Capabilities that lost their provider.
    pub newly_absent: Vec<String>,
    /// Capabilities that gained one.
    pub newly_present: Vec<String>,
    /// Systems added.
    pub added_systems: Vec<String>,
    /// Systems removed.
    pub removed_systems: Vec<String>,
    /// Systems that changed phase, as `(system, before, after)`.
    pub moved_systems: Vec<(String, String, String)>,
    /// Field access that appeared, as `(field, system, access)`.
    pub added_access: Vec<(String, String, String)>,
    /// Field access that disappeared.
    pub removed_access: Vec<(String, String, String)>,
    /// Whether the resolved schedule hash changed at all.
    pub schedule_hash_changed: bool,
}

impl Diff {
    /// Compares `before` against `after`.
    pub fn between(before: &Graph, after: &Graph) -> Self {
        let module_ids = |graph: &Graph| -> Vec<String> {
            graph
                .modules
                .iter()
                .map(|module| module.id.clone())
                .collect()
        };
        let system_names = |graph: &Graph| -> Vec<String> {
            graph
                .systems
                .iter()
                .map(|system| system.name.clone())
                .collect()
        };
        let accesses = |graph: &Graph| -> Vec<(String, String, String)> {
            graph
                .field_access
                .iter()
                .map(|access| {
                    (
                        access.field.clone(),
                        access.system.clone(),
                        access.access.clone(),
                    )
                })
                .collect()
        };

        let absent = |graph: &Graph| -> Vec<String> {
            graph
                .capabilities
                .iter()
                .filter(|capability| !capability.present)
                .map(|capability| capability.name.clone())
                .collect()
        };

        let mut moved_systems = Vec::new();
        for system in &after.systems {
            if let Some(previous) = before
                .systems
                .iter()
                .find(|other| other.name == system.name)
                && previous.phase != system.phase
            {
                moved_systems.push((
                    system.name.clone(),
                    previous.phase.clone(),
                    system.phase.clone(),
                ));
            }
        }
        moved_systems.sort();

        Self {
            added_modules: missing_from(&module_ids(after), &module_ids(before)),
            removed_modules: missing_from(&module_ids(before), &module_ids(after)),
            newly_absent: missing_from(&absent(after), &absent(before)),
            newly_present: missing_from(&absent(before), &absent(after)),
            added_systems: missing_from(&system_names(after), &system_names(before)),
            removed_systems: missing_from(&system_names(before), &system_names(after)),
            moved_systems,
            added_access: missing_from(&accesses(after), &accesses(before)),
            removed_access: missing_from(&accesses(before), &accesses(after)),
            schedule_hash_changed: before.schedule_hash != after.schedule_hash,
        }
    }

    /// Whether anything changed.
    pub fn is_empty(&self) -> bool {
        self.added_modules.is_empty()
            && self.removed_modules.is_empty()
            && self.newly_absent.is_empty()
            && self.newly_present.is_empty()
            && self.added_systems.is_empty()
            && self.removed_systems.is_empty()
            && self.moved_systems.is_empty()
            && self.added_access.is_empty()
            && self.removed_access.is_empty()
    }

    /// Rule violations, as opposed to changes.
    ///
    /// The one thing S21 hard-fails on. Everything else in a diff is a
    /// legitimate architecture change that deserves a note, not a blocked merge.
    pub fn violations(after: &Graph) -> Vec<String> {
        let writers = after.writers_of(GUARDED_FIELD);
        if writers.len() > PERMITTED_WRITERS {
            return vec![format!(
                "{GUARDED_FIELD} has {} writers ({}), but ADR-0011 permits exactly \
                 {PERMITTED_WRITERS}: generation (S07) and terrain edits (S19). A third writer \
                 is a defect rather than a change.",
                writers.len(),
                writers.join(", ")
            )];
        }
        Vec::new()
    }

    /// Which visual change class a node belongs to, if any.
    pub fn change_for(&self, node_id: &str) -> Option<&'static str> {
        let name = node_id.split_once(':').map_or(node_id, |(_, name)| name);

        if self.added_modules.iter().any(|id| id == name)
            || self.added_systems.iter().any(|id| id == name)
            || self.newly_present.iter().any(|id| id == name)
        {
            return Some("added");
        }
        if self.newly_absent.iter().any(|id| id == name) {
            return Some("removed");
        }
        None
    }

    /// A one-paragraph summary for the page banner.
    pub fn summary_html(&self) -> String {
        if self.is_empty() {
            return "<b>No architectural change</b> against the baseline.".to_owned();
        }

        let mut parts = Vec::new();
        let mut note = |label: &str, items: &[String]| {
            if !items.is_empty() {
                parts.push(format!("{} {label}: {}", items.len(), items.join(", ")));
            }
        };

        note("added", &self.added_modules);
        note("removed", &self.removed_modules);
        note("newly absent", &self.newly_absent);

        if !self.moved_systems.is_empty() {
            let moves: Vec<String> = self
                .moved_systems
                .iter()
                .map(|(system, before, after)| format!("{system} {before}→{after}"))
                .collect();
            parts.push(format!("phase changes: {}", moves.join(", ")));
        }

        if !self.added_systems.is_empty() {
            parts.push(format!("{} systems added", self.added_systems.len()));
        }
        if !self.removed_systems.is_empty() {
            parts.push(format!("{} systems removed", self.removed_systems.len()));
        }
        if !self.added_access.is_empty() {
            parts.push(format!("{} field accesses added", self.added_access.len()));
        }

        format!("<b>Changed against the baseline.</b> {}", parts.join(" · "))
    }
}

/// Items in `left` that are not in `right`, sorted.
fn missing_from<T: Clone + Ord + PartialEq>(left: &[T], right: &[T]) -> Vec<T> {
    let mut missing: Vec<T> = left
        .iter()
        .filter(|item| !right.contains(item))
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(modules: &str, systems: &str, access: &str, capabilities: &str, hash: &str) -> Graph {
        Graph::parse(&format!(
            r#"{{"schema":"1.0","schedule_hash":"{hash}","modules":[{modules}],
                "capabilities":[{capabilities}],"systems":[{systems}],"field_access":[{access}]}}"#
        ))
        .expect("the fixture should parse")
    }

    fn module(id: &str) -> String {
        format!(r#"{{"id":"{id}","version":"1.0","provides":[],"requires":[],"optional":[]}}"#)
    }

    fn system(name: &str, phase: &str, index: u32) -> String {
        format!(r#"{{"name":"{name}","phase":"{phase}","phase_index":{index},"module":"m"}}"#)
    }

    #[test]
    fn an_unchanged_graph_reports_nothing() {
        // The common case in CI, and the one that has to be quiet: a diff that
        // reports on every run trains everyone to skip it.
        let before = graph(&module("fields"), "", "", "", "abc");
        let after = graph(&module("fields"), "", "", "", "abc");

        let diff = Diff::between(&before, &after);
        assert!(diff.is_empty());
        assert!(!diff.schedule_hash_changed);
        assert!(diff.summary_html().contains("No architectural change"));
    }

    #[test]
    fn an_added_module_is_reported() {
        let before = graph(&module("fields"), "", "", "", "abc");
        let after = graph(
            &format!("{},{}", module("fields"), module("worldgen")),
            "",
            "",
            "",
            "def",
        );

        let diff = Diff::between(&before, &after);
        assert_eq!(diff.added_modules, vec!["worldgen"]);
        assert!(diff.removed_modules.is_empty());
        assert!(diff.schedule_hash_changed);
        assert_eq!(diff.change_for("module:worldgen"), Some("added"));
    }

    #[test]
    fn a_system_that_changes_phase_is_reported_with_both_phases() {
        // S21 names this specifically. A system crossing a phase boundary is a
        // determinism change, and knowing only that "something moved" would not
        // be enough to judge it.
        let before = graph("", &system("erode", "TerrainEdit", 2), "", "", "a");
        let after = graph("", &system("erode", "FieldSolve", 3), "", "", "b");

        let diff = Diff::between(&before, &after);
        assert_eq!(
            diff.moved_systems,
            vec![(
                "erode".to_owned(),
                "TerrainEdit".to_owned(),
                "FieldSolve".to_owned()
            )]
        );
        assert!(
            diff.summary_html().contains("TerrainEdit→FieldSolve"),
            "the summary should name both phases: {}",
            diff.summary_html()
        );
    }

    #[test]
    fn a_capability_losing_its_provider_is_reported_as_newly_absent() {
        // The degradation case: an optional capability going absent changes
        // behaviour without changing any code that mentions it.
        let before = graph(
            "",
            "",
            "",
            r#"{"name":"erosion","present":true,"provider":"w"}"#,
            "a",
        );
        let after = graph(
            "",
            "",
            "",
            r#"{"name":"erosion","present":false,"degraded":"terrain is not eroded"}"#,
            "b",
        );

        let diff = Diff::between(&before, &after);
        assert_eq!(diff.newly_absent, vec!["erosion"]);
        assert_eq!(diff.change_for("capability:erosion"), Some("removed"));
    }

    #[test]
    fn new_field_access_is_reported() {
        // The case S21 opens with: a pull request that silently starts writing a
        // field it did not write before.
        let before = graph("", "", "", "", "a");
        let after = graph(
            "",
            "",
            r#"{"field":"ELEVATION","system":"sculpt","access":"write","module":"m"}"#,
            "",
            "b",
        );

        let diff = Diff::between(&before, &after);
        assert_eq!(
            diff.added_access,
            vec![(
                "ELEVATION".to_owned(),
                "sculpt".to_owned(),
                "write".to_owned()
            )]
        );
    }

    /// **The one thing S21 hard-fails on.**
    #[test]
    fn a_third_elevation_writer_is_a_violation_not_a_change() {
        let two = graph(
            "",
            "",
            r#"{"field":"ELEVATION","system":"generate","access":"write","module":"a"},
               {"field":"ELEVATION","system":"edit","access":"write","module":"b"}"#,
            "",
            "a",
        );
        assert!(
            Diff::violations(&two).is_empty(),
            "two writers is what ADR-0011 permits"
        );

        let three = graph(
            "",
            "",
            r#"{"field":"ELEVATION","system":"generate","access":"write","module":"a"},
               {"field":"ELEVATION","system":"edit","access":"write","module":"b"},
               {"field":"ELEVATION","system":"sculpt","access":"write","module":"c"}"#,
            "",
            "b",
        );

        let violations = Diff::violations(&three);
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("sculpt") && violations[0].contains("ADR-0011"),
            "the violation should name the offender and the rule: {}",
            violations[0]
        );
    }

    #[test]
    fn a_deposit_counts_towards_the_writer_limit() {
        // ADR-0011 counts deposits. A viewer that only counted direct writes
        // would report two and pass a graph with three.
        let three = graph(
            "",
            "",
            r#"{"field":"ELEVATION","system":"generate","access":"write","module":"a"},
               {"field":"ELEVATION","system":"edit","access":"write","module":"b"},
               {"field":"ELEVATION","system":"erode","access":"deposit","module":"c"}"#,
            "",
            "b",
        );

        assert_eq!(Diff::violations(&three).len(), 1);
    }

    #[test]
    fn reads_do_not_count_towards_the_writer_limit() {
        let many_readers = graph(
            "",
            "",
            r#"{"field":"ELEVATION","system":"generate","access":"write","module":"a"},
               {"field":"ELEVATION","system":"land","access":"read","module":"b"},
               {"field":"ELEVATION","system":"mesh","access":"read","module":"c"},
               {"field":"ELEVATION","system":"nav","access":"read","module":"d"}"#,
            "",
            "a",
        );

        assert!(Diff::violations(&many_readers).is_empty());
    }

    #[test]
    fn a_diff_does_not_depend_on_the_order_of_either_side() {
        let forwards = graph(
            &format!("{},{}", module("alpha"), module("beta")),
            "",
            "",
            "",
            "a",
        );
        let backwards = graph(
            &format!("{},{}", module("beta"), module("alpha")),
            "",
            "",
            "",
            "a",
        );
        let empty = graph("", "", "", "", "z");

        assert_eq!(
            Diff::between(&empty, &forwards),
            Diff::between(&empty, &backwards)
        );
    }
}
