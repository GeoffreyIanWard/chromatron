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

/// Which crate owns each contained external dependency.
///
/// `02-architecture.md` assigns each of these to exactly one crate: `wgpu` to
/// the renderer (`ADR-0005`, `ADR-0010`), windowing to the app shell, audio to
/// `cx-audio`, and the debug UI to `cx-ui`. Containment is per-library, not a
/// blanket "only the renderer touches anything graphical" — an earlier version
/// of this check said the latter, which would have blocked `cx-app` from
/// declaring `winit` at all.
///
/// The value is what containment buys: graphics code cannot spread into
/// gameplay, and swapping any one of these stays a contained project rather
/// than an archaeology exercise.
const EXTERNAL_OWNERS: &[(&str, &str)] = &[
    ("wgpu", "cx-render"),
    ("winit", "cx-app"),
    ("kira", "cx-audio"),
    ("rodio", "cx-audio"),
    ("cpal", "cx-audio"),
    ("egui", "cx-ui"),
    ("eframe", "cx-ui"),
];

/// The crate permitted to declare `name`, if it is a contained dependency.
fn owner_of(name: &str) -> Option<&'static str> {
    EXTERNAL_OWNERS
        .iter()
        .find(|(family, _)| name == *family || name.starts_with(&format!("{family}-")))
        .map(|(_, owner)| *owner)
}

/// Each contained external dependency may be declared only by its owning crate.
///
/// Checked on *direct* dependencies rather than transitive ones, deliberately.
/// `cx-app` will link `wgpu` transitively through `cx-render` — that is the
/// point of having a renderer. What must not happen is another crate declaring
/// it, because that is what naming its types requires.
#[test]
fn contained_dependencies_are_declared_only_by_their_owner() {
    let metadata = load_metadata();
    let mut violations = Vec::new();

    for package in metadata.workspace_packages() {
        let name = package.name.to_string();

        for dependency in &package.dependencies {
            // Dev-dependencies are excluded for the same reason as elsewhere:
            // test code never ships inside the engine.
            if dependency.kind != DependencyKind::Normal && dependency.kind != DependencyKind::Build
            {
                continue;
            }

            if let Some(owner) = owner_of(&dependency.name)
                && owner != name
            {
                violations.push(format!(
                    "  {name} → {} (only {owner} may declare it)",
                    dependency.name
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "contained dependencies were declared outside their owning crate:\n{}\n\n\
         02-architecture.md gives each of these exactly one home so that graphics, windowing, \
         and audio cannot spread through the codebase. If another crate needs something the \
         owner has, the owner should expose it as plain data — that is what cx-view's \
         ExtractedInstance and cx-render's DeviceInfo are for.",
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
