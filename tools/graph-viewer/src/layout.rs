//! Placing the graph on an isometric grid (S21).
//!
//! # Why this is Rust and not JavaScript
//!
//! S21 says layout is computed "in the viewer, not the engine". This *is* the
//! viewer — a repo tool under `tools/`, not engine code, and nothing here ships
//! in the game. Putting it in Rust rather than in the page's script buys the one
//! thing that matters: S21 makes layout stability an **acceptance criterion**,
//! and a criterion that cannot be tested is a hope.
//!
//! The page draws positions it is given. It computes none.
//!
//! # Hashed placement, not indexed
//!
//! The obvious layout is "sort the modules and lay them out in rows". It is
//! stable across runs, and it fails S21's *other* requirement — that adding one
//! module moves as little as possible — because inserting `agents` between
//! `fields` and `spatial` shifts every module after it and the diagram is
//! unrecognisable.
//!
//! So a module's cell comes from a hash of its **id**. Adding a module leaves
//! every other module exactly where it was, unless the newcomer lands on an
//! occupied cell, in which case it probes outward and only its immediate
//! neighbourhood is disturbed. There is no random seed anywhere: the same id
//! always hashes to the same cell, on any machine, forever.

use crate::graph::Graph;

/// A cell on the isometric grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cell {
    /// Grid column.
    pub x: i32,
    /// Grid row.
    pub z: i32,
}

/// A placed node.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    /// What this is.
    pub id: String,
    /// Text drawn on it.
    pub label: String,
    /// Where it sits.
    pub cell: Cell,
    /// Block height in grid units — the layer's scalar. The legend says what it
    /// means for each layer, once, rather than each layer inventing a rule.
    pub height: f32,
    /// Which visual class it belongs to.
    pub kind: NodeKind,
    /// Extra text for the tooltip.
    pub detail: String,
}

/// What a placed node is, which decides how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A module.
    Module,
    /// A capability with a provider.
    Capability,
    /// A capability nothing provides.
    AbsentCapability,
    /// A system.
    System,
    /// A field.
    Field,
}

/// An edge between two placed nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// Source node id.
    pub from: String,
    /// Target node id.
    pub to: String,
    /// What the edge means: `provides`, `requires`, `optional`, `read`,
    /// `write`, `deposit`.
    pub relation: String,
}

/// One drawable layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    /// Layer name, as the viewer's tabs show it.
    pub name: &'static str,
    /// What the block heights mean here.
    pub height_meaning: &'static str,
    /// The nodes.
    pub nodes: Vec<Placed>,
    /// The edges.
    pub edges: Vec<Edge>,
}

/// The whole laid-out diagram.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// Composition, schedule, and field access, in that order.
    pub layers: Vec<Layer>,
}

/// How far below its provider a capability sits, in cells along both axes.
///
/// Both axes, because the isometric projection puts screen-y at `(x + z)`: a
/// band offset on one axis alone moves the whole band sideways as well as down.
const BAND: i32 = 5;

/// How wide the module grid is before it wraps, in cells.
///
/// Bounds the diagram's aspect ratio. A hash spread over an unbounded plane
/// produces a diagram that is mostly empty space.
const GRID_SPAN: i32 = 7;

/// FNV-1a over the bytes of a name.
///
/// Written out rather than taken from `std`: `DefaultHasher` explicitly does not
/// promise the same output across Rust releases, and a layout that reshuffled
/// when the toolchain was upgraded would fail S21's stability criterion in the
/// least obvious way possible — the diagram would change with no change to the
/// engine.
///
/// The constants are pinned by a test against the published FNV-1a values, which
/// caught the prime being written with one hex digit too many on the first
/// attempt. A hash that is merely *consistent with itself* would have passed
/// every other test here and still produced a layout nobody else could
/// reproduce.
fn stable_hash(name: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Places names on a grid by hash, probing outward on a collision.
///
/// The probe order is fixed, so two runs with the same set of names produce the
/// same placement regardless of the order they were offered in.
fn place_by_hash(names: &[String]) -> Vec<(String, Cell)> {
    // Sorted first so that the *probe* sequence is order-independent too. Two
    // names colliding resolve the same way whichever arrived first.
    let mut sorted: Vec<&String> = names.iter().collect();
    sorted.sort_unstable();
    sorted.dedup();

    let mut taken: Vec<Cell> = Vec::with_capacity(sorted.len());
    let mut placed = Vec::with_capacity(sorted.len());

    for name in sorted {
        let hash = stable_hash(name);
        let home = Cell {
            x: (hash % GRID_SPAN as u64) as i32,
            z: ((hash / GRID_SPAN as u64) % GRID_SPAN as u64) as i32,
        };

        // Outward probe in a fixed order: right, then down a row. Deterministic,
        // and it keeps a displaced node next to where it wanted to be, which is
        // what "perturbs only its own neighbourhood" means.
        let mut cell = home;
        let mut step = 0;
        while taken.contains(&cell) {
            step += 1;
            cell = Cell {
                x: home.x + step % GRID_SPAN,
                z: home.z + step / GRID_SPAN,
            };
        }

        taken.push(cell);
        placed.push((name.clone(), cell));
    }

    placed
}

/// Lays out every layer of a graph.
pub fn layout(graph: &Graph) -> Layout {
    Layout {
        layers: vec![composition(graph), schedule(graph), field_access(graph)],
    }
}

/// Modules and capabilities, wired by what they provide and require.
fn composition(graph: &Graph) -> Layer {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    let module_names: Vec<String> = graph
        .modules
        .iter()
        .map(|module| module.id.clone())
        .collect();
    for (name, cell) in place_by_hash(&module_names) {
        let module = graph
            .modules
            .iter()
            .find(|module| module.id == name)
            .expect("the name came from this list");

        // Height is the module's system count: how much of the tick it owns,
        // which is the thing worth seeing at a glance on this layer.
        let systems = graph.systems_of(&name).count();

        nodes.push(Placed {
            id: format!("module:{name}"),
            label: name.clone(),
            cell,
            height: 0.4 + systems as f32 * 0.35,
            kind: NodeKind::Module,
            detail: format!(
                "v{} · {} system{}",
                module.version,
                systems,
                if systems == 1 { "" } else { "s" }
            ),
        });
    }

    // Capabilities sit on a band below the modules, each **directly beneath its
    // provider**. That is what S21 means by positions deriving from graph
    // structure: hashing a capability's own name instead put it anywhere on the
    // band, and the provides-edge — the most common edge on this layer — became
    // a long diagonal across the whole diagram. Placing it under its provider
    // makes that edge short and vertical, and it is still stable, because a
    // capability moves only when its provider does.
    //
    // A capability with no provider has nothing to sit under, so it falls back
    // to its own hash. Those are the ones S21 wants visible as absences, and
    // they are conspicuous precisely because nothing points at them.
    let module_cells: Vec<(String, Cell)> = nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Module)
        .map(|node| (node.label.clone(), node.cell))
        .collect();

    let orphans: Vec<String> = graph
        .capabilities
        .iter()
        .filter(|capability| capability.provider.is_none())
        .map(|capability| capability.name.clone())
        .collect();
    let orphan_cells = place_by_hash(&orphans);

    let mut taken_in_band: Vec<Cell> = Vec::new();
    for capability in &graph.capabilities {
        let name = capability.name.clone();

        let home = capability
            .provider
            .as_ref()
            .and_then(|provider| {
                module_cells
                    .iter()
                    .find(|(id, _)| id == provider)
                    .map(|(_, cell)| *cell)
            })
            .or_else(|| {
                orphan_cells
                    .iter()
                    .find(|(id, _)| *id == name)
                    .map(|(_, cell)| *cell)
            })
            .unwrap_or(Cell { x: 0, z: 0 });

        // Offset along **both** axes, not just z. The isometric projection puts
        // screen-y at `(x + z)` and screen-x at `(x - z)`, so adding to z alone
        // moves a block down *and to the left* — which is what the first attempt
        // did, and it left every capability nowhere near the module that
        // provides it. Adding the same amount to both keeps `x - z` fixed and
        // moves it straight down the screen.
        let mut cell = Cell {
            x: home.x + BAND,
            z: home.z + BAND,
        };
        while taken_in_band.contains(&cell) {
            cell.x += 1;
        }
        taken_in_band.push(cell);

        nodes.push(Placed {
            id: format!("capability:{name}"),
            label: name.clone(),
            cell,
            height: 0.3,
            kind: if capability.present {
                NodeKind::Capability
            } else {
                NodeKind::AbsentCapability
            },
            // An absent capability carries its degraded behaviour, which is the
            // thing that is invisible in code review and expensive in
            // debugging.
            detail: match (&capability.provider, &capability.degraded) {
                (Some(provider), _) => format!("provided by {provider}"),
                (None, Some(degraded)) => format!("absent — {degraded}"),
                (None, None) => "absent".to_owned(),
            },
        });
    }

    for module in &graph.modules {
        for capability in &module.provides {
            edges.push(Edge {
                from: format!("module:{}", module.id),
                to: format!("capability:{capability}"),
                relation: "provides".to_owned(),
            });
        }
        for capability in &module.requires {
            edges.push(Edge {
                from: format!("capability:{capability}"),
                to: format!("module:{}", module.id),
                relation: "requires".to_owned(),
            });
        }
        for capability in &module.optional {
            edges.push(Edge {
                from: format!("capability:{capability}"),
                to: format!("module:{}", module.id),
                relation: "optional".to_owned(),
            });
        }
    }

    Layer {
        name: "composition",
        height_meaning: "block height is the module's system count",
        nodes,
        edges,
    }
}

/// Systems in phase lanes.
///
/// The lane *is* the phase index, so a system's position on the diagram is its
/// position in the tick. Nothing is hashed here: a phase has a fixed place in
/// the tick, and a diagram that moved it would be lying about the one thing this
/// layer exists to show.
fn schedule(graph: &Graph) -> Layer {
    let mut nodes = Vec::new();

    // Grouped by phase, then ordered within a lane by (module, name) — stable,
    // and adding a system shifts only its own lane.
    let mut sorted: Vec<&crate::graph::System> = graph.systems.iter().collect();
    sorted.sort_by(|a, b| {
        a.phase_index
            .cmp(&b.phase_index)
            .then_with(|| a.module.cmp(&b.module))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut lane_fill: Vec<(u32, i32)> = Vec::new();
    for system in sorted {
        let slot = match lane_fill
            .iter_mut()
            .find(|(phase, _)| *phase == system.phase_index)
        {
            Some((_, next)) => {
                let slot = *next;
                *next += 1;
                slot
            }
            None => {
                lane_fill.push((system.phase_index, 1));
                0
            }
        };

        nodes.push(Placed {
            id: format!("system:{}", system.name),
            label: system.name.clone(),
            cell: Cell {
                x: slot,
                z: system.phase_index as i32,
            },
            height: 0.5,
            kind: NodeKind::System,
            detail: format!("{} · {}", system.module, system.phase),
        });
    }

    Layer {
        name: "schedule",
        height_meaning: "one lane per tick phase, in tick order",
        nodes,
        // The ordering constraint is the lane itself; drawing an edge between
        // consecutive phases would add ink without adding information.
        edges: Vec::new(),
    }
}

/// Fields and the systems that touch them.
fn field_access(graph: &Graph) -> Layer {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    let fields: Vec<String> = graph
        .fields()
        .into_iter()
        .map(std::borrow::ToOwned::to_owned)
        .collect();

    for (name, cell) in place_by_hash(&fields) {
        let writers = graph.writers_of(&name).len();
        nodes.push(Placed {
            id: format!("field:{name}"),
            label: name.clone(),
            cell,
            // Height is the writer count, because that is the number with a
            // rule attached: ADR-0011 permits exactly two writers of ELEVATION,
            // and a third should be visible as a taller block before anyone
            // reads the JSON.
            height: 0.3 + writers as f32 * 0.5,
            kind: NodeKind::Field,
            detail: format!("{writers} writer{}", if writers == 1 { "" } else { "s" }),
        });
    }

    let system_names: Vec<String> = graph
        .field_access
        .iter()
        .map(|access| access.system.clone())
        .collect();
    for (name, cell) in place_by_hash(&system_names) {
        let module = graph
            .field_access
            .iter()
            .find(|access| access.system == name)
            .map_or("", |access| access.module.as_str());

        nodes.push(Placed {
            id: format!("system:{name}"),
            label: name.clone(),
            cell: Cell {
                x: cell.x + BAND,
                z: cell.z + BAND,
            },
            height: 0.4,
            kind: NodeKind::System,
            detail: module.to_owned(),
        });
    }

    for access in &graph.field_access {
        edges.push(Edge {
            from: format!("system:{}", access.system),
            to: format!("field:{}", access.field),
            relation: access.access.clone(),
        });
    }

    Layer {
        name: "field access",
        height_meaning: "block height is the field's writer count (ADR-0011 permits two for ELEVATION)",
        nodes,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;

    fn graph_with(modules: &[&str]) -> Graph {
        let entries: Vec<String> = modules
            .iter()
            .map(|id| {
                format!(
                    r#"{{"id":"{id}","version":"1.0","provides":["cap_{id}"],"requires":[],"optional":[]}}"#
                )
            })
            .collect();
        let capabilities: Vec<String> = modules
            .iter()
            .map(|id| format!(r#"{{"name":"cap_{id}","present":true,"provider":"{id}"}}"#))
            .collect();

        Graph::parse(&format!(
            r#"{{"schema":"1.0","schedule_hash":"h",
                "modules":[{}],"capabilities":[{}],"systems":[],"field_access":[]}}"#,
            entries.join(","),
            capabilities.join(",")
        ))
        .expect("the fixture should parse")
    }

    fn module_cells(layout: &Layout) -> Vec<(String, Cell)> {
        layout.layers[0]
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Module)
            .map(|node| (node.label.clone(), node.cell))
            .collect()
    }

    /// **S21: layout of an unchanged graph is identical across runs.**
    #[test]
    fn the_same_graph_lays_out_identically_every_time() {
        let graph = graph_with(&["fields", "worldgen", "spatial", "agents", "physics"]);
        let first = layout(&graph);

        for _ in 0..10 {
            assert_eq!(layout(&graph), first);
        }
    }

    /// The same property across *processes*, which is what the hash choice is
    /// really about: `DefaultHasher` is explicitly not stable across Rust
    /// releases, so a layout built on it would reshuffle when the toolchain was
    /// upgraded — a diagram changing with no change to the engine.
    #[test]
    fn the_hash_is_pinned_to_known_values() {
        assert_eq!(stable_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(stable_hash("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(stable_hash("fields"), stable_hash("fields"));
        assert_ne!(stable_hash("fields"), stable_hash("physics"));
    }

    /// **S21: adding one module perturbs no more than its own neighbourhood.**
    ///
    /// The reason placement is hashed rather than indexed. Sorting and laying
    /// out in rows is equally stable across runs and fails this completely:
    /// inserting `agents` before `fields` shifts everything after it.
    #[test]
    fn adding_a_module_leaves_the_others_where_they_were() {
        let before = module_cells(&layout(&graph_with(&["fields", "worldgen", "spatial"])));
        let after = module_cells(&layout(&graph_with(&[
            "fields", "worldgen", "spatial", "agents",
        ])));

        let moved: Vec<&(String, Cell)> = before
            .iter()
            .filter(|(name, cell)| {
                after
                    .iter()
                    .find(|(other, _)| other == name)
                    .is_none_or(|(_, other)| other != cell)
            })
            .collect();

        assert!(
            moved.is_empty(),
            "adding one module moved existing ones: {moved:?}"
        );
        assert_eq!(after.len(), before.len() + 1);
    }

    #[test]
    fn removing_a_module_leaves_the_others_where_they_were() {
        // The same property in the other direction — a diff between two commits
        // is as often a removal as an addition.
        let before = module_cells(&layout(&graph_with(&["fields", "worldgen", "spatial"])));
        let after = module_cells(&layout(&graph_with(&["fields", "spatial"])));

        for (name, cell) in &after {
            let original = before
                .iter()
                .find(|(other, _)| other == name)
                .map(|(_, cell)| cell);
            assert_eq!(
                original,
                Some(cell),
                "{name} moved when a module was removed"
            );
        }
    }

    #[test]
    fn placement_does_not_depend_on_the_order_names_arrive_in() {
        // Two exports of the same profile are sorted identically, so this
        // should never differ in practice — which is exactly why it is worth
        // asserting rather than assuming.
        let forwards = module_cells(&layout(&graph_with(&["alpha", "beta", "gamma"])));
        let backwards = module_cells(&layout(&graph_with(&["gamma", "beta", "alpha"])));

        let mut forwards = forwards;
        let mut backwards = backwards;
        forwards.sort();
        backwards.sort();
        assert_eq!(forwards, backwards);
    }

    #[test]
    fn no_two_nodes_share_a_cell() {
        // The collision probe. Two blocks in one cell render as one block, so a
        // module would silently vanish from the diagram.
        let graph = graph_with(&[
            "fields", "worldgen", "spatial", "agents", "physics", "solvers", "lod", "persist",
            "diag", "edit", "data", "sim",
        ]);
        let placed = module_cells(&layout(&graph));

        let mut cells: Vec<Cell> = placed.iter().map(|(_, cell)| *cell).collect();
        let total = cells.len();
        cells.sort();
        cells.dedup();

        assert_eq!(cells.len(), total, "two modules landed on the same cell");
    }

    /// **S21: a system registered in a phase renders in that phase's lane.**
    ///
    /// Table-driven over every phase, which is what the spec asks for — a lane
    /// mapping that is right for the phases someone happened to test and wrong
    /// for the rest is the failure this shape of test exists to prevent.
    #[test]
    fn every_phase_gets_its_own_lane() {
        const PHASES: [(&str, u32); 13] = [
            ("IntakeCommands", 0),
            ("ChunkLifecycle", 1),
            ("TerrainEdit", 2),
            ("FieldSolve", 3),
            ("SpatialRebuild", 4),
            ("AgentSense", 5),
            ("AgentDecide", 6),
            ("AgentAct", 7),
            ("Physics", 8),
            ("FieldDeposit", 9),
            ("Events", 10),
            ("StructuralApply", 11),
            ("Diagnostics", 12),
        ];

        let systems: Vec<String> = PHASES
            .iter()
            .map(|(phase, index)| {
                format!(
                    r#"{{"name":"sys_{index}","phase":"{phase}","phase_index":{index},"module":"m"}}"#
                )
            })
            .collect();

        let graph = Graph::parse(&format!(
            r#"{{"schema":"1.0","schedule_hash":"h","modules":[],"capabilities":[],
                "systems":[{}],"field_access":[]}}"#,
            systems.join(",")
        ))
        .expect("the fixture should parse");

        let laid_out = layout(&graph);
        let lanes = &laid_out.layers[1];

        for (phase, index) in PHASES {
            let node = lanes
                .nodes
                .iter()
                .find(|node| node.label == format!("sys_{index}"))
                .unwrap_or_else(|| panic!("{phase} has no node"));

            assert_eq!(
                node.cell.z, index as i32,
                "{phase} should be in lane {index}, found {}",
                node.cell.z
            );
        }
    }

    #[test]
    fn systems_in_one_phase_share_a_lane_without_overlapping() {
        let graph = Graph::parse(
            r#"{"schema":"1.0","schedule_hash":"h","modules":[],"capabilities":[],
                "systems":[
                    {"name":"b","phase":"AgentAct","phase_index":7,"module":"m"},
                    {"name":"a","phase":"AgentAct","phase_index":7,"module":"m"},
                    {"name":"c","phase":"AgentAct","phase_index":7,"module":"m"}
                ],"field_access":[]}"#,
        )
        .expect("valid");

        let laid_out = layout(&graph);
        let mut cells: Vec<Cell> = laid_out.layers[1]
            .nodes
            .iter()
            .map(|node| node.cell)
            .collect();

        assert!(cells.iter().all(|cell| cell.z == 7));
        let total = cells.len();
        cells.sort();
        cells.dedup();
        assert_eq!(cells.len(), total, "systems in a lane overlapped");
    }

    #[test]
    fn an_absent_capability_is_drawn_and_carries_its_degradation() {
        // S21 is explicit: absent capabilities are drawn, not omitted. The
        // degraded behaviour is the thing that is invisible in code review and
        // expensive in debugging.
        let graph = Graph::parse(
            r#"{"schema":"1.0","schedule_hash":"h","modules":[],
                "capabilities":[
                    {"name":"erosion","present":false,"degraded":"terrain is not eroded"}
                ],"systems":[],"field_access":[]}"#,
        )
        .expect("valid");

        let laid_out = layout(&graph);
        let node = laid_out.layers[0]
            .nodes
            .iter()
            .find(|node| node.label == "erosion")
            .expect("an absent capability should still be drawn");

        assert_eq!(node.kind, NodeKind::AbsentCapability);
        assert!(
            node.detail.contains("not eroded"),
            "the degradation should be on the node: {}",
            node.detail
        );
    }

    #[test]
    fn a_field_with_more_writers_is_drawn_taller() {
        // The height convention doing work: ADR-0011 permits two writers of
        // ELEVATION, and a third should be visible before anyone reads JSON.
        let graph = Graph::parse(
            r#"{"schema":"1.0","schedule_hash":"h","modules":[],"capabilities":[],"systems":[],
                "field_access":[
                    {"field":"ELEVATION","system":"a","access":"write","module":"m"},
                    {"field":"ELEVATION","system":"b","access":"write","module":"m"},
                    {"field":"MOISTURE","system":"c","access":"write","module":"m"}
                ]}"#,
        )
        .expect("valid");

        let laid_out = layout(&graph);
        let height_of = |name: &str| {
            laid_out.layers[2]
                .nodes
                .iter()
                .find(|node| node.label == name && node.kind == NodeKind::Field)
                .map(|node| node.height)
                .expect("the field is drawn")
        };

        assert!(
            height_of("ELEVATION") > height_of("MOISTURE"),
            "two writers should stand taller than one"
        );
    }

    #[test]
    fn every_layer_says_what_its_heights_mean() {
        // S21: the convention is documented once in the legend and not
        // re-invented per layer. A layer with no stated meaning is a layer
        // whose heights are decoration.
        let graph = graph_with(&["fields"]);
        for layer in layout(&graph).layers {
            assert!(
                !layer.height_meaning.is_empty(),
                "layer `{}` does not say what its heights mean",
                layer.name
            );
        }
    }

    #[test]
    fn an_empty_graph_lays_out_without_panicking() {
        let graph = Graph::parse(
            r#"{"schema":"1.0","schedule_hash":"h","modules":[],"capabilities":[],
                "systems":[],"field_access":[]}"#,
        )
        .expect("valid");

        let laid_out = layout(&graph);
        assert_eq!(laid_out.layers.len(), 3);
        assert!(laid_out.layers.iter().all(|layer| layer.nodes.is_empty()));
    }

    #[test]
    fn a_capability_sits_directly_below_its_provider() {
        // Two bugs in one test, both found by looking at the rendered diagram
        // rather than by reasoning about it.
        //
        // First, capabilities were placed by hashing their own *name*, which put
        // them anywhere on the band — so the provides-edge, the most common edge
        // on this layer, became a long diagonal across the whole picture.
        //
        // Then, with them under their providers in grid terms, the band was
        // offset along z alone. The isometric projection puts screen-x at
        // `(x - z)`, so that moved the whole band sideways as well as down and
        // they still were not below anything. The offset has to be equal on both
        // axes, which is what keeps `x - z` — and therefore the screen column —
        // unchanged.
        let graph = graph_with(&["fields", "worldgen", "spatial"]);
        let laid_out = layout(&graph);

        for module in ["fields", "worldgen", "spatial"] {
            let module_cell = laid_out.layers[0]
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Module && node.label == module)
                .map(|node| node.cell)
                .expect("the module is placed");

            let capability_cell = laid_out.layers[0]
                .nodes
                .iter()
                .find(|node| node.label == format!("cap_{module}"))
                .map(|node| node.cell)
                .expect("the capability is placed");

            assert_eq!(
                capability_cell.x - capability_cell.z,
                module_cell.x - module_cell.z,
                "cap_{module} is not in the same screen column as {module}: \
                 module {module_cell:?}, capability {capability_cell:?}"
            );
            assert!(
                capability_cell.x + capability_cell.z > module_cell.x + module_cell.z,
                "cap_{module} should be below {module} on screen, not above it"
            );
        }
    }

    #[test]
    fn two_capabilities_from_one_module_do_not_stack() {
        // A module providing two capabilities would put both on the same cell,
        // and one would render exactly behind the other — invisible.
        let graph = Graph::parse(
            r#"{"schema":"1.0","schedule_hash":"h",
                "modules":[{"id":"m","version":"1","provides":["one","two"],"requires":[],"optional":[]}],
                "capabilities":[
                    {"name":"one","present":true,"provider":"m"},
                    {"name":"two","present":true,"provider":"m"}
                ],"systems":[],"field_access":[]}"#,
        )
        .expect("valid");

        let laid_out = layout(&graph);
        let cells: Vec<Cell> = laid_out.layers[0]
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Capability)
            .map(|node| node.cell)
            .collect();

        assert_eq!(cells.len(), 2);
        assert_ne!(cells[0], cells[1], "two capabilities landed on one cell");
    }

    #[test]
    fn an_orphan_capability_is_still_placed() {
        // Nothing provides it, so there is no provider to sit under. It falls
        // back to its own hash rather than piling up at the origin, where every
        // absent capability would overlap every other.
        let graph = Graph::parse(
            r#"{"schema":"1.0","schedule_hash":"h","modules":[],
                "capabilities":[
                    {"name":"erosion","present":false},
                    {"name":"climate","present":false}
                ],"systems":[],"field_access":[]}"#,
        )
        .expect("valid");

        let laid_out = layout(&graph);
        let cells: Vec<Cell> = laid_out.layers[0]
            .nodes
            .iter()
            .map(|node| node.cell)
            .collect();

        assert_eq!(cells.len(), 2);
        assert_ne!(cells[0], cells[1], "two absent capabilities overlapped");
    }
}
