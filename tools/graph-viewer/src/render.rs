//! Drawing a laid-out graph as a self-contained page (S21).
//!
//! # One file, no requests
//!
//! S21's acceptance criterion is that the viewer opens from `file://` with no
//! build step, no network access, and no external asset requests. So everything
//! — markup, styles, the handful of lines of script that switch layers — is
//! inlined into one HTML file, and a test asserts there is no `http` anywhere in
//! the output.
//!
//! That rules out a web font, a CDN, and an icon set, which is why the diagram
//! is drawn in SVG with system fonts.
//!
//! # The page draws; it does not decide
//!
//! Every position comes from [`crate::layout`], which is Rust and therefore
//! tested. The script here toggles layer visibility and nothing else. A page
//! that computed its own positions would put S21's stability criterion somewhere
//! no test could reach it.

use std::fmt::Write as _;

use crate::diff::Diff;
use crate::graph::Graph;
use crate::layout::{Cell, Edge, Layer, Layout, NodeKind, Placed};

/// Half-width of an isometric tile, in SVG units.
const TILE_W: f32 = 46.0;

/// Half-height of an isometric tile. Half of the width gives the conventional
/// 2:1 isometric projection.
const TILE_H: f32 = 23.0;

/// How tall one unit of block height is drawn.
const HEIGHT_UNIT: f32 = 34.0;

/// Distance between adjacent cells, as a multiple of a block's own size.
///
/// Greater than one on purpose. At exactly one, a block spans the full distance
/// to its neighbour, so adjacent cells touch and their labels collide — which is
/// what the first rendering of the real graph looked like, and the reason this
/// constant exists rather than the two being the same number.
const SPACING: f32 = 1.9;

/// Projects a grid cell to screen coordinates.
///
/// The standard 2:1 isometric transform: x runs down-right, z runs down-left,
/// and height lifts straight up the screen.
fn project(cell: Cell, height: f32) -> (f32, f32) {
    let x = (cell.x - cell.z) as f32 * TILE_W * SPACING;
    let y = (cell.x + cell.z) as f32 * TILE_H * SPACING - height * HEIGHT_UNIT;
    (x, y)
}

/// Renders a graph to a standalone HTML page.
pub fn render(graph: &Graph, layout: &Layout, diff: Option<&Diff>) -> String {
    let mut body = String::new();

    for (index, layer) in layout.layers.iter().enumerate() {
        let _ = write!(
            body,
            r#"<section class="layer" id="layer-{index}"{}>{}</section>"#,
            if index == 0 { "" } else { r#" hidden"# },
            svg_for(layer, diff)
        );
    }

    let tabs: String = layout
        .layers
        .iter()
        .enumerate()
        .map(|(index, layer)| {
            format!(
                r#"<button type="button" data-layer="{index}"{}>{}</button>"#,
                if index == 0 { r#" class="on""# } else { "" },
                escape(layer.name)
            )
        })
        .collect();

    let meanings: String = layout
        .layers
        .iter()
        .enumerate()
        .map(|(index, layer)| {
            format!(
                r#"<p class="meaning" data-layer="{index}"{}>{}</p>"#,
                if index == 0 { "" } else { r#" hidden"# },
                escape(layer.height_meaning)
            )
        })
        .collect();

    format!(
        "{}{}{}{}{}",
        PAGE_HEAD,
        header(graph, diff),
        format_args!(r#"<nav class="tabs">{tabs}</nav>{meanings}"#),
        body,
        PAGE_TAIL
    )
}

/// The banner: which profile this is, and what changed if a baseline was given.
fn header(graph: &Graph, diff: Option<&Diff>) -> String {
    let mut header = format!(
        r#"<header><h1>chromatron architecture</h1>
        <p class="hash">schedule hash <code>{}</code> · {} modules · {} systems</p>"#,
        escape(&graph.schedule_hash),
        graph.modules.len(),
        graph.systems.len()
    );

    if let Some(diff) = diff {
        let _ = write!(header, r#"<div class="diff">{}</div>"#, diff.summary_html());
    }

    header.push_str("</header>");
    header
}

/// One layer, as an SVG scene.
fn svg_for(layer: &Layer, diff: Option<&Diff>) -> String {
    if layer.nodes.is_empty() {
        return r#"<p class="empty">Nothing in this layer. A profile that resolves to
            nothing still exports a valid graph — see the milestone notes.</p>"#
            .to_owned();
    }

    // Painter's order: back to front, so a block nearer the viewer overlaps the
    // one behind it. On an isometric grid that is ascending (x + z), with
    // height breaking ties.
    let mut ordered: Vec<&Placed> = layer.nodes.iter().collect();
    ordered.sort_by(|a, b| {
        (a.cell.x + a.cell.z)
            .cmp(&(b.cell.x + b.cell.z))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut shapes = String::new();
    for edge in &layer.edges {
        shapes.push_str(&edge_svg(edge, layer));
    }
    for node in ordered {
        shapes.push_str(&node_svg(node, diff));
    }

    let bounds = bounds(layer);
    format!(
        r#"<svg viewBox="{} {} {} {}" role="img" aria-label="{} layer">{shapes}</svg>"#,
        bounds.0, bounds.1, bounds.2, bounds.3, layer.name
    )
}

/// The viewBox that contains every node, with a margin.
fn bounds(layer: &Layer) -> (f32, f32, f32, f32) {
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for node in &layer.nodes {
        // Two points per block: the top face, which is what `project` returns,
        // and the base, which sits `height` lower. Measuring only one of them
        // leaves either the labels clipped or a band of empty canvas below
        // everything — the first version did the latter, for about a third of
        // the image.
        let (x, top) = project(node.cell, node.height);
        let (_, base) = project(node.cell, 0.0);

        min_x = min_x.min(x - TILE_W);
        max_x = max_x.max(x + TILE_W);
        // The label sits above the block, hence more room at the top.
        min_y = min_y.min(top - TILE_H - 24.0);
        max_y = max_y.max(base + TILE_H);
    }

    let margin = 40.0;
    (
        min_x - margin,
        min_y - margin,
        (max_x - min_x) + margin * 2.0,
        (max_y - min_y) + margin * 2.0,
    )
}

/// One block: a top face and two sides, plus its label.
fn node_svg(node: &Placed, diff: Option<&Diff>) -> String {
    let (x, y) = project(node.cell, node.height);
    let lift = node.height * HEIGHT_UNIT;

    let class = match node.kind {
        NodeKind::Module => "module",
        NodeKind::Capability => "capability",
        NodeKind::AbsentCapability => "absent",
        NodeKind::System => "system",
        NodeKind::Field => "field",
    };

    // A changed node is outlined rather than recoloured: the layer's own colour
    // already means something, and overwriting it would trade one signal for
    // another.
    let change = diff
        .and_then(|diff| diff.change_for(&node.id))
        .map_or(String::new(), |change| format!(" changed {change}"));

    format!(
        r#"<g class="node {class}{change}" transform="translate({x:.1} {y:.1})">
  <title>{label} — {detail}</title>
  <path class="top" d="M 0 {th} L {tw} 0 L 0 {nth} L {ntw} 0 Z"/>
  <path class="left" d="M {ntw} 0 L 0 {th} L 0 {lift_h} L {ntw} {side_h} Z"/>
  <path class="right" d="M {tw} 0 L 0 {th} L 0 {lift_h} L {tw} {side_h} Z"/>
  <text y="-10">{label}</text>
</g>"#,
        th = TILE_H,
        tw = TILE_W,
        nth = -TILE_H,
        ntw = -TILE_W,
        lift_h = TILE_H + lift,
        side_h = lift,
        label = escape(&node.label),
        detail = escape(&node.detail),
    )
}

/// One edge, drawn between the tops of two blocks.
fn edge_svg(edge: &Edge, layer: &Layer) -> String {
    let find = |id: &str| layer.nodes.iter().find(|node| node.id == id);
    let (Some(from), Some(to)) = (find(&edge.from), find(&edge.to)) else {
        // An edge to a node this layer does not contain. Skipped rather than
        // drawn to nowhere: a line ending in empty space reads as a bug in the
        // architecture rather than in the diagram.
        return String::new();
    };

    let (x1, y1) = project(from.cell, from.height);
    let (x2, y2) = project(to.cell, to.height);

    format!(
        r#"<line class="edge {}" x1="{x1:.1}" y1="{y1:.1}" x2="{x2:.1}" y2="{y2:.1}"><title>{}</title></line>"#,
        escape(&edge.relation),
        escape(&edge.relation)
    )
}

/// Escapes text for HTML.
///
/// Module and field names come from Rust identifiers today, but the payload is
/// a file the viewer is handed — possibly one it did not produce — and a viewer
/// that trusted it would be building markup out of unvalidated input.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Everything before the body content: doctype, styles, and the legend.
const PAGE_HEAD: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>chromatron architecture</title>
<style>
:root { color-scheme: dark light; --bg:#12141a; --ink:#e8eaf0; --dim:#8b93a7;
  --module:#5b8def; --capability:#3fb27f; --absent:#c8553d; --system:#b58df1; --field:#e0a83b; }
body { margin:0; background:var(--bg); color:var(--ink);
  font:14px/1.5 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; }
header { padding:20px 24px 8px; }
h1 { margin:0; font-size:18px; font-weight:600; letter-spacing:.01em; }
.hash { margin:4px 0 0; color:var(--dim); font-size:13px; }
.hash code { font-family:ui-monospace, SFMono-Regular, Menlo, monospace; }
.tabs { display:flex; gap:4px; padding:12px 24px 0; }
.tabs button { background:transparent; color:var(--dim); border:1px solid #2a2f3c;
  border-radius:6px; padding:5px 12px; font:inherit; font-size:13px; cursor:pointer; }
.tabs button.on { color:var(--ink); border-color:#4a5468; background:#1b1f29; }
.meaning { margin:10px 24px 0; color:var(--dim); font-size:12.5px; }
.layer { padding:8px 24px 32px; }
.layer[hidden] { display:none; }
svg { width:100%; height:auto; max-height:74vh; }
.empty { color:var(--dim); max-width:52ch; }
.node text { fill:var(--ink); font-size:13px; text-anchor:middle; paint-order:stroke;
  stroke:var(--bg); stroke-width:3px; }
.node .top { fill:var(--module); }
.node .left { fill:var(--module); filter:brightness(.72); }
.node .right { fill:var(--module); filter:brightness(.86); }
.capability .top, .capability .left, .capability .right { fill:var(--capability); }
.absent .top, .absent .left, .absent .right { fill:var(--absent); }
.absent .top { stroke:var(--absent); stroke-dasharray:4 3; fill-opacity:.25; }
.system .top, .system .left, .system .right { fill:var(--system); }
.field .top, .field .left, .field .right { fill:var(--field); }
.changed .top { stroke:#fff; stroke-width:2.5px; }
.changed.added .top { stroke:#3fb27f; }
.changed.removed .top { stroke:#c8553d; stroke-dasharray:5 4; }
.edge { stroke:#5a6478; stroke-width:1.5px; }
.edge.requires { stroke:#7f8ba3; }
.edge.optional { stroke-dasharray:4 4; }
.edge.write, .edge.deposit { stroke:var(--field); }
.edge.read { stroke:#5a6478; stroke-dasharray:3 3; }
.legend { display:flex; flex-wrap:wrap; gap:14px; padding:0 24px 28px; color:var(--dim);
  font-size:12.5px; }
.legend span::before { content:""; display:inline-block; width:10px; height:10px;
  margin-right:6px; border-radius:2px; vertical-align:baseline; }
.legend .m::before { background:var(--module); }
.legend .c::before { background:var(--capability); }
.legend .a::before { background:var(--absent); }
.legend .s::before { background:var(--system); }
.legend .f::before { background:var(--field); }
.diff { margin-top:8px; font-size:13px; }
.diff b { font-weight:600; }
</style></head><body>"#;

/// Everything after: the legend and the layer switch.
const PAGE_TAIL: &str = r#"<div class="legend">
  <span class="m">module</span><span class="c">capability</span>
  <span class="a">capability with no provider</span>
  <span class="s">system</span><span class="f">field</span>
  <span>block height carries the scalar named above each layer</span>
  <span>hover any block for detail</span>
</div>
<script>
// The only script in the page: switching layers. Every position was computed in
// Rust and is already in the markup, because S21 makes layout stability an
// acceptance criterion and a criterion that cannot be tested is a hope.
document.querySelectorAll('.tabs button').forEach(function (button) {
  button.addEventListener('click', function () {
    var wanted = button.dataset.layer;
    document.querySelectorAll('.tabs button').forEach(function (other) {
      other.classList.toggle('on', other === button);
    });
    document.querySelectorAll('.layer').forEach(function (layer) {
      layer.hidden = layer.id !== 'layer-' + wanted;
    });
    document.querySelectorAll('.meaning').forEach(function (meaning) {
      meaning.hidden = meaning.dataset.layer !== wanted;
    });
  });
});
</script></body></html>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::layout;

    fn sample() -> Graph {
        Graph::parse(
            r#"{"schema":"1.0","schedule_hash":"deadbeef",
                "modules":[
                    {"id":"fields","version":"1.0","provides":["fields"],"requires":[],"optional":[]},
                    {"id":"worldgen","version":"0.1","provides":["terrain"],"requires":["fields"],"optional":[]}
                ],
                "capabilities":[
                    {"name":"fields","present":true,"provider":"fields"},
                    {"name":"terrain","present":true,"provider":"worldgen"}
                ],
                "systems":[
                    {"name":"exchange_halos","phase":"FieldSolve","phase_index":3,"module":"fields"},
                    {"name":"generate_elevation","phase":"ChunkLifecycle","phase_index":1,"module":"worldgen"}
                ],
                "field_access":[
                    {"field":"ELEVATION","system":"generate_elevation","access":"write","module":"worldgen"}
                ]}"#,
        )
        .expect("the fixture should parse")
    }

    fn page() -> String {
        let graph = sample();
        render(&graph, &layout(&graph), None)
    }

    /// **S21: the viewer makes no external asset requests.**
    ///
    /// The criterion this project can actually check from a test: if there is no
    /// absolute URL in the file, there is nothing for it to fetch.
    #[test]
    fn the_page_requests_nothing_from_the_network() {
        let html = page();

        for forbidden in ["http://", "https://", "//cdn", "@import", "<link"] {
            assert!(
                !html.contains(forbidden),
                "the page contains `{forbidden}`, so it is not self-contained"
            );
        }
    }

    #[test]
    fn the_page_is_one_file_with_its_styles_and_script_inline() {
        let html = page();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<style>") && html.contains("<script>"));
        assert!(
            !html.contains("src=\""),
            "nothing should be loaded from elsewhere"
        );
    }

    #[test]
    fn rendering_is_deterministic() {
        // The rendered file is what gets committed to a PR or diffed against
        // yesterday's. Two renders of one graph differing would make every such
        // comparison noise.
        let first = page();
        for _ in 0..5 {
            assert_eq!(page(), first);
        }
    }

    #[test]
    fn every_module_and_system_appears_in_the_output() {
        let html = page();
        for name in [
            "fields",
            "worldgen",
            "exchange_halos",
            "generate_elevation",
            "ELEVATION",
        ] {
            assert!(html.contains(name), "`{name}` is missing from the diagram");
        }
    }

    #[test]
    fn the_schedule_hash_is_on_the_page() {
        // S21: the payload carries the resolved hash so a graph can be matched
        // against the save or replay it describes. A diagram that dropped it
        // could not be.
        assert!(page().contains("deadbeef"));
    }

    #[test]
    fn a_name_that_looks_like_markup_is_escaped() {
        // The payload is a file handed to the viewer, possibly one it did not
        // produce. Building markup out of it unescaped is the ordinary way a
        // local tool becomes a vulnerability.
        let graph = Graph::parse(
            r#"{"schema":"1.0","schedule_hash":"h",
                "modules":[{"id":"<script>alert(1)</script>","version":"1","provides":[],"requires":[],"optional":[]}],
                "capabilities":[],"systems":[],"field_access":[]}"#,
        )
        .expect("valid");

        let html = render(&graph, &layout(&graph), None);
        assert!(
            !html.contains("<script>alert(1)</script>"),
            "a module name was injected into the page as markup"
        );
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn an_empty_graph_renders_a_page_that_says_so() {
        let graph = Graph::parse(
            r#"{"schema":"1.0","schedule_hash":"h","modules":[],"capabilities":[],
                "systems":[],"field_access":[]}"#,
        )
        .expect("valid");

        let html = render(&graph, &layout(&graph), None);
        assert!(html.contains("Nothing in this layer"));
        assert!(html.starts_with("<!doctype html>"));
    }

    #[test]
    fn the_legend_names_every_node_kind_drawn() {
        // S21: the convention is documented once in the legend. A kind that is
        // drawn but not in the legend is a colour the reader has to guess at.
        let html = page();
        for kind in ["module", "capability", "system", "field"] {
            assert!(
                html.contains(&format!(">{kind}<")),
                "the legend does not name `{kind}`"
            );
        }
        assert!(html.contains("capability with no provider"));
    }

    #[test]
    fn blocks_are_drawn_back_to_front() {
        // Painter's order. Drawn in the wrong order, a block behind another
        // paints over it and the diagram reads as though the near one is
        // missing.
        let graph = sample();
        let html = render(&graph, &layout(&graph), None);

        let composition = html
            .split("<section class=\"layer\" id=\"layer-0\"")
            .nth(1)
            .expect("the composition layer is present");

        let laid_out = layout(&graph);
        let mut expected: Vec<&Placed> = laid_out.layers[0].nodes.iter().collect();
        expected.sort_by(|a, b| {
            (a.cell.x + a.cell.z)
                .cmp(&(b.cell.x + b.cell.z))
                .then_with(|| a.id.cmp(&b.id))
        });

        let mut last = 0;
        for node in expected {
            let marker = format!("<title>{} —", escape(&node.label));
            let found = composition[last..]
                .find(&marker)
                .unwrap_or_else(|| panic!("{} is not drawn after the block before it", node.label));
            last += found;
        }
    }
}
