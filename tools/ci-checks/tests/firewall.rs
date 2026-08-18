//! The dependency firewall from `02-architecture.md`.
//!
//! > `---- firewall: nothing above may depend on anything below ----`
//!
//! The sim side is authoritative, deterministic, and headless-capable. It does
//! not know that rendering exists. That property is worth exactly as much as it
//! is enforced, and it is easiest to violate accidentally — one convenience
//! `use` in a debug helper is all it takes, and by then the crate that broke it
//! is three refactors back.
//!
//! This runs over the resolved dependency graph rather than over source text, so
//! it catches transitive violations: `cx-agents` depending on some helper crate
//! that itself pulls in `wgpu` fails here, and grepping for `use wgpu` would not
//! have found it.

use std::collections::BTreeSet;

use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Node, Package, PackageId};

/// Crates above the firewall. Order matches `02-architecture.md`.
const SIM_CRATES: &[&str] = &[
    "cx-core",
    "cx-module",
    "cx-ecs",
    "cx-time",
    "cx-fields",
    "cx-worldgen",
    "cx-edit",
    "cx-solvers",
    "cx-spatial",
    "cx-agents",
    "cx-physics",
    "cx-lod",
    "cx-data",
    "cx-persist",
    "cx-diag",
    "cx-sim",
];

/// Crates below the firewall.
const PRESENTATION_CRATES: &[&str] = &[
    "cx-render",
    "cx-view",
    "cx-present",
    "cx-audio",
    "cx-ui",
    "cx-app",
];

/// External crates the sim side may never reach, transitively or otherwise.
///
/// Matched on the crate name and on `name-` prefixes, so `wgpu-core`,
/// `egui-winit`, and friends are covered without listing every member of every
/// family as it changes across versions.
const BANNED_EXTERNAL: &[&str] = &["wgpu", "winit", "kira", "egui", "eframe", "rodio", "cpal"];

/// `wgpu` types must not escape `cx-render` (`ADR-0005`, `ADR-0010`). The
/// windowing and audio crates are similarly contained.
fn is_banned_external(name: &str) -> bool {
    BANNED_EXTERNAL
        .iter()
        .any(|banned| name == *banned || name.starts_with(&format!("{banned}-")))
}

fn load_metadata() -> Metadata {
    MetadataCommand::new()
        .manifest_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
        .exec()
        .expect("cargo metadata should succeed in a checked-out workspace")
}

fn package_by_name<'a>(metadata: &'a Metadata, name: &str) -> &'a Package {
    metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == name)
        .unwrap_or_else(|| {
            panic!(
                "crate `{name}` is named by the firewall check but does not exist in the \
                 workspace. Either it was renamed without updating \
                 tools/ci-checks/tests/firewall.rs, or 02-architecture.md and the workspace \
                 have drifted apart."
            )
        })
}

fn node_by_id<'a>(nodes: &'a [Node], id: &PackageId) -> &'a Node {
    nodes
        .iter()
        .find(|node| &node.id == id)
        .expect("every resolved package should have a graph node")
}

/// Whether `parent` actually builds `child`, as opposed to merely listing it.
///
/// `cargo metadata`'s resolve graph includes **optional** dependencies whether or
/// not the feature gating them is enabled. Walking it naively reports
/// dependencies that are never compiled — the first live run of this check
/// flagged `cx-ecs -> bevy_ecs -> bevy_reflect -> wgpu-types`, which does not
/// exist in any build we produce, because `bevy_reflect`'s `wgpu-types` feature
/// is off.
///
/// A firewall that cries wolf gets switched off, so an optional dependency
/// counts only when some enabled feature of the parent actually turns it on.
fn is_activated(metadata: &Metadata, parent: &Node, child_name: &str) -> bool {
    let parent_package = &metadata[&parent.id];

    let declared_optional = parent_package
        .dependencies
        .iter()
        .filter(|dependency| {
            matches!(
                dependency.kind,
                DependencyKind::Normal | DependencyKind::Build
            )
        })
        .any(|dependency| dependency.name == child_name && dependency.optional);

    if !declared_optional {
        return true;
    }

    // Enabled either by a feature named after the dependency, or by any enabled
    // feature whose expansion mentions `dep:child` or `child/...`.
    parent.features.iter().any(|feature| {
        let feature = feature.as_str();
        feature == child_name
            || parent_package
                .features
                .iter()
                .find(|(name, _)| name.as_str() == feature)
                .map(|(_, expansions)| expansions)
                .is_some_and(|expansions| {
                    expansions.iter().any(|entry| {
                        entry.as_str() == format!("dep:{child_name}")
                            || entry.as_str().starts_with(&format!("{child_name}/"))
                    })
                })
    })
}

/// Every package reachable from `root` through normal and build dependencies
/// that are actually built.
///
/// Dev-dependencies are deliberately excluded: a sim crate's *tests* may use
/// whatever they like, since test code never ships inside the simulation.
fn transitive_deps(metadata: &Metadata, root: &str) -> BTreeSet<String> {
    let resolve = metadata
        .resolve
        .as_ref()
        .expect("cargo metadata should include a resolved dependency graph");

    let root_id = package_by_name(metadata, root).id.clone();
    let mut seen = BTreeSet::new();
    let mut queue = vec![root_id];

    while let Some(id) = queue.pop() {
        let node = node_by_id(&resolve.nodes, &id);
        for dep in &node.deps {
            let ships_in_the_binary = dep
                .dep_kinds
                .iter()
                .any(|kind| matches!(kind.kind, DependencyKind::Normal | DependencyKind::Build));
            if !ships_in_the_binary {
                continue;
            }

            let name = metadata[&dep.pkg].name.to_string();
            if !is_activated(metadata, node, &name) {
                continue;
            }

            if seen.insert(name) {
                queue.push(dep.pkg.clone());
            }
        }
    }

    seen
}

#[test]
fn sim_crates_do_not_depend_on_presentation_crates() {
    let metadata = load_metadata();
    let mut violations = Vec::new();

    for sim in SIM_CRATES {
        for dep in transitive_deps(&metadata, sim) {
            if PRESENTATION_CRATES.contains(&dep.as_str()) {
                violations.push(format!("  {sim} → {dep}"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "the dependency firewall in 02-architecture.md is broken:\n{}\n\n\
         Nothing above the firewall may depend on anything below it. The sim world is \
         authoritative and headless-capable; the view world is derived and disposable. If \
         sim code needs something from the presentation side, the direction is wrong — the \
         data should flow out through the extract phase (ADR-0002) instead.",
        violations.join("\n")
    );
}

#[test]
fn sim_crates_do_not_depend_on_banned_external_crates() {
    let metadata = load_metadata();
    let mut violations = Vec::new();

    for sim in SIM_CRATES {
        for dep in transitive_deps(&metadata, sim) {
            if is_banned_external(&dep) {
                violations.push(format!("  {sim} → {dep}"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "sim crates must not depend on rendering, windowing, or audio crates:\n{}\n\n\
         This includes transitive paths. `wgpu` types must not escape `cx-render` \
         (ADR-0005, ADR-0010), and a headless run must not link a windowing library.",
        violations.join("\n")
    );
}

/// The banned-external check cannot be exercised end to end without actually
/// adding `wgpu` to a sim crate, which would mean vendoring a large dependency
/// tree to prove a negative. The matcher is tested directly instead — it is the
/// only part with any logic in it.
#[test]
fn banned_external_matcher_covers_crate_families() {
    for banned in [
        "wgpu",
        "wgpu-core",
        "wgpu-hal",
        "winit",
        "egui",
        "egui-winit",
        "kira",
    ] {
        assert!(
            is_banned_external(banned),
            "`{banned}` should be banned from sim crates"
        );
    }

    // Prefix matching must not overreach: a crate merely *starting with* the
    // same letters is unrelated. `wgpu_playground` is not `wgpu-`.
    for allowed in [
        "wgpustub", "winitial", "eguild", "kirakira", "glam", "bevy_ecs",
    ] {
        assert!(
            !is_banned_external(allowed),
            "`{allowed}` should not be banned"
        );
    }
}

/// The one crate permitted to name `wgpu` types (`ADR-0005`, `ADR-0010`).
const RENDER_CRATE: &str = "cx-render";

/// `wgpu` may be a direct dependency of `cx-render` and nothing else.
///
/// `ADR-0005` proposed a backend-agnostic render API so a console backend could
/// be added later; `ADR-0010` put console out of scope, which removed that
/// abstraction's only consumer. What survived is the **crate boundary**: no
/// `wgpu` type appears outside `cx-render`, so graphics code cannot spread into
/// gameplay and a future port stays a contained project.
///
/// Checked on *direct* dependencies rather than transitive ones, deliberately.
/// `cx-app` will depend on `cx-render` and therefore link `wgpu` — that is the
/// point of having a renderer. What must not happen is another crate declaring
/// `wgpu` itself, because that is what naming its types requires.
#[test]
fn only_the_render_crate_depends_on_wgpu() {
    let metadata = load_metadata();
    let mut violations = Vec::new();

    for package in metadata.workspace_packages() {
        let name = package.name.to_string();
        if name == RENDER_CRATE {
            continue;
        }

        for dependency in &package.dependencies {
            // Dev-dependencies are excluded for the same reason as elsewhere:
            // test code never ships inside the engine. A test that wants to
            // poke at wgpu directly is not spreading graphics into gameplay.
            if dependency.kind != DependencyKind::Normal && dependency.kind != DependencyKind::Build
            {
                continue;
            }

            if is_banned_external(&dependency.name) {
                violations.push(format!("  {name} → {}", dependency.name));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "only `{RENDER_CRATE}` may depend on graphics, windowing, or audio crates \
         directly:\n{}\n\n\
         ADR-0010 kept the cx-render crate boundary after ADR-0005's abstraction was dropped: \
         no wgpu type may appear outside it. If another crate needs something the renderer \
         has, the renderer should expose it as plain data — that is what cx-view's \
         ExtractedInstance is for.",
        violations.join("\n")
    );
}

/// The firewall lists above are hand-maintained, so they can silently fall out of
/// date as crates are added. This makes that failure loud.
#[test]
fn every_workspace_crate_is_classified() {
    let metadata = load_metadata();

    let unclassified: Vec<String> = metadata
        .workspace_packages()
        .iter()
        .map(|package| package.name.to_string())
        .filter(|name| {
            !SIM_CRATES.contains(&name.as_str())
                && !PRESENTATION_CRATES.contains(&name.as_str())
                && !name.starts_with("chromatron-")
                && name != "ci-checks"
        })
        .collect();

    assert!(
        unclassified.is_empty(),
        "these workspace crates are on neither side of the firewall: {unclassified:?}\n\n\
         Add each one to SIM_CRATES or PRESENTATION_CRATES in this file, and to the crate \
         graph in 02-architecture.md. A crate that is not classified is not checked.",
    );
}
