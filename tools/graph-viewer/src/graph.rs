//! The exported graph, as this tool reads it (S21).
//!
//! A separate set of types from `cx-module`'s, on purpose. The viewer consumes a
//! *file*, possibly one written by an older build, and the whole reason the
//! payload carries a schema version is that the two are allowed to differ.
//! Sharing the engine's types would make that impossible to represent — the
//! viewer would deserialize into whatever today's engine believes and could not
//! notice the disagreement.

use std::fmt;

/// Schema major version this viewer understands.
///
/// A payload from a different major version is refused rather than rendered
/// partially: a diagram missing a layer looks like an architecture missing a
/// layer, which is worse than no diagram.
pub const SUPPORTED_MAJOR: u32 = 1;

/// Why a graph could not be read.
#[derive(Debug)]
pub enum GraphError {
    /// The file was not valid JSON, or not the shape expected.
    Malformed(String),
    /// The schema is from a major version this viewer does not know.
    UnsupportedSchema {
        /// What the payload declared.
        found: String,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::Malformed(reason) => write!(
                f,
                "the graph could not be read: {reason}. It should be the output of \
                 `chromatron-cli graph`"
            ),
            GraphError::UnsupportedSchema { found } => write!(
                f,
                "this viewer understands schema {SUPPORTED_MAJOR}.x but the graph declares \
                 {found}. Rendering it partially would draw an architecture with pieces \
                 missing, which is worse than drawing nothing — rebuild the viewer, or \
                 re-export the graph with a matching build."
            ),
        }
    }
}

impl std::error::Error for GraphError {}

/// A module in the resolved set.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Module {
    /// Stable identity.
    pub id: String,
    /// Declared version.
    pub version: String,
    /// Capabilities it offers.
    pub provides: Vec<String>,
    /// Capabilities it cannot start without.
    pub requires: Vec<String>,
    /// Capabilities it uses when present.
    #[serde(default)]
    pub optional: Vec<String>,
}

/// A capability, drawn as a node in its own right.
///
/// `ADR-0012` forbids a module from naming another module, so a diagram with
/// module-to-module edges would depict a coupling the architecture does not
/// allow. Drawing the capability *between* them is what makes an undeclared
/// reliance visually obvious: the edge has nowhere to attach.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Capability {
    /// Its name.
    pub name: String,
    /// Whether anything provides it.
    pub present: bool,
    /// The providing module, when there is one.
    #[serde(default)]
    pub provider: Option<String>,
    /// What happens without it, for an absent optional capability.
    #[serde(default)]
    pub degraded: Option<String>,
}

/// A system in the resolved schedule.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct System {
    /// Unique within its module.
    pub name: String,
    /// The phase it runs in.
    pub phase: String,
    /// Position of that phase in the tick.
    pub phase_index: u32,
    /// The module that registered it.
    pub module: String,
    /// Where it was registered, as `path:line`.
    ///
    /// Optional because schema 1.0 did not carry it, and a viewer that refused
    /// an older payload over an additive field would defeat the point of having
    /// a minor version at all.
    #[serde(default)]
    pub source: Option<String>,
}

/// One system's access to one field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct FieldAccess {
    /// The field.
    pub field: String,
    /// The system touching it.
    pub system: String,
    /// `read`, `write`, or `deposit`.
    pub access: String,
    /// The module the system belongs to.
    pub module: String,
    /// Where the access was declared, as `path:line`.
    #[serde(default)]
    pub source: Option<String>,
}

impl FieldAccess {
    /// Whether this access modifies the field.
    ///
    /// A deposit is a write that goes through the buffer, and `ADR-0011` counts
    /// it: two systems depositing into `ELEVATION` are two writers.
    pub fn is_write(&self) -> bool {
        matches!(self.access.as_str(), "write" | "deposit")
    }
}

/// A resolved architecture graph.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Graph {
    /// Schema version, `major.minor`.
    pub schema: String,
    /// The resolved schedule hash, which ties this graph to a save or replay.
    pub schedule_hash: String,
    /// Modules, sorted by id.
    pub modules: Vec<Module>,
    /// Capabilities, sorted by name.
    pub capabilities: Vec<Capability>,
    /// Systems, sorted by name.
    pub systems: Vec<System>,
    /// Field access edges.
    pub field_access: Vec<FieldAccess>,
}

impl Graph {
    /// Parses an exported graph, refusing an unknown major version.
    pub fn parse(json: &str) -> Result<Self, GraphError> {
        let graph: Graph =
            serde_json::from_str(json).map_err(|error| GraphError::Malformed(error.to_string()))?;

        let major = graph
            .schema
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
            .ok_or_else(|| {
                GraphError::Malformed(format!("unreadable schema `{}`", graph.schema))
            })?;

        if major != SUPPORTED_MAJOR {
            return Err(GraphError::UnsupportedSchema {
                found: graph.schema.clone(),
            });
        }

        Ok(graph)
    }

    /// Systems belonging to a module, in export order.
    pub fn systems_of<'a>(&'a self, module: &'a str) -> impl Iterator<Item = &'a System> + 'a {
        self.systems
            .iter()
            .filter(move |system| system.module == module)
    }

    /// Every system that writes `field`.
    ///
    /// The one thing S21 hard-fails on: `ADR-0011` permits exactly two writers
    /// of `ELEVATION`, so a third is a defect rather than a change.
    pub fn writers_of<'a>(&'a self, field: &'a str) -> Vec<&'a str> {
        let mut writers: Vec<&str> = self
            .field_access
            .iter()
            .filter(|access| access.field == field && access.is_write())
            .map(|access| access.system.as_str())
            .collect();
        writers.sort_unstable();
        writers.dedup();
        writers
    }

    /// Every distinct field, sorted.
    pub fn fields(&self) -> Vec<&str> {
        let mut fields: Vec<&str> = self
            .field_access
            .iter()
            .map(|access| access.field.as_str())
            .collect();
        fields.sort_unstable();
        fields.dedup();
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
        "schema": "1.0",
        "schedule_hash": "abc",
        "modules": [],
        "capabilities": [],
        "systems": [],
        "field_access": []
    }"#;

    #[test]
    fn a_valid_graph_parses() {
        let graph = Graph::parse(MINIMAL).expect("the minimal graph should parse");
        assert_eq!(graph.schema, "1.0");
        assert_eq!(graph.schedule_hash, "abc");
    }

    #[test]
    fn a_future_schema_is_refused_rather_than_half_rendered() {
        // A diagram missing a layer looks like an architecture missing a layer.
        let future = MINIMAL.replace("\"1.0\"", "\"2.0\"");
        let error = Graph::parse(&future).expect_err("schema 2 should be refused");

        assert!(matches!(error, GraphError::UnsupportedSchema { .. }));
        let message = error.to_string();
        assert!(
            message.contains("2.0") && message.contains("rebuild"),
            "the error should name the version and say what to do: {message}"
        );
    }

    #[test]
    fn a_newer_minor_version_is_accepted() {
        // Minor versions add fields. Refusing them would mean the viewer had to
        // be rebuilt in lockstep with every additive change, which is how a
        // tool stops being used.
        let newer = MINIMAL.replace("\"1.0\"", "\"1.7\"");
        assert!(Graph::parse(&newer).is_ok());
    }

    #[test]
    fn malformed_input_says_so() {
        let error = Graph::parse("not json at all").expect_err("this is not a graph");
        assert!(matches!(error, GraphError::Malformed(_)));
        assert!(
            error.to_string().contains("chromatron-cli graph"),
            "the error should say where a graph comes from"
        );
    }

    #[test]
    fn a_deposit_counts_as_a_write() {
        // ADR-0011 counts deposits: two systems depositing into ELEVATION are
        // two writers, and a viewer that only counted direct writes would show
        // one.
        let graph: Graph = serde_json::from_str(
            r#"{
                "schema": "1.0", "schedule_hash": "x",
                "modules": [], "capabilities": [], "systems": [],
                "field_access": [
                    {"field":"ELEVATION","system":"generate","access":"write","module":"a"},
                    {"field":"ELEVATION","system":"erode","access":"deposit","module":"b"},
                    {"field":"ELEVATION","system":"land","access":"read","module":"c"}
                ]
            }"#,
        )
        .expect("valid");

        assert_eq!(graph.writers_of("ELEVATION"), vec!["erode", "generate"]);
    }

    #[test]
    fn fields_are_listed_once_and_sorted() {
        let graph: Graph = serde_json::from_str(
            r#"{
                "schema": "1.0", "schedule_hash": "x",
                "modules": [], "capabilities": [], "systems": [],
                "field_access": [
                    {"field":"MOISTURE","system":"a","access":"read","module":"m"},
                    {"field":"ELEVATION","system":"b","access":"read","module":"m"},
                    {"field":"ELEVATION","system":"c","access":"write","module":"m"}
                ]
            }"#,
        )
        .expect("valid");

        assert_eq!(graph.fields(), vec!["ELEVATION", "MOISTURE"]);
    }
}
