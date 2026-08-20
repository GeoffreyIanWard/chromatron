//! The S21 architecture graph export.
//!
//! Serializes what the engine *resolved*, not what the documentation claims.
//! `ADR-0012` already requires every subsystem to declare its capabilities in one
//! place and `cx-module` resolves that into a schedule at startup, so this is a
//! projection of the same data the schedule is built from. It therefore cannot
//! drift from the engine: if the diagram is wrong, the engine is wrong.
//!
//! # Determinism is a feature, not a nicety
//!
//! Two exports of the same build and profile are byte-identical: nodes and edges
//! are emitted in sorted order, and nothing time-, path-, or environment-derived
//! reaches the payload. That is what makes `--baseline` diffing possible, and it
//! is the same discipline `ADR-0004` applies to the simulation itself.
//!
//! Lives in `cx-module` rather than `cx-diag` because it serializes this
//! crate's own resolved state — the graph is a projection of the registry, so
//! keeping them together means the two cannot drift apart across a crate
//! boundary. The CLI subcommand and the viewer are separate (S21).
//!
//! # Capabilities are nodes
//!
//! Modules never name each other (`ADR-0012`), so drawing module→module edges
//! would depict a coupling the architecture forbids. The capability sits between
//! them as its own node, which is what makes an undeclared reliance visible: the
//! edge has nowhere to attach.
//!
//! **Absent optional capabilities are emitted too**, marked, carrying their
//! declared degraded behaviour. That degradation is exactly what is invisible in
//! review and expensive at 2am.

use std::fmt::Write as _;

use crate::module::{Access, Source};
use crate::resolved::Resolved;

/// Schema version of the exported payload.
///
/// A viewer refuses a payload whose major version it does not know, rather than
/// rendering a partial diagram and letting someone reason from a picture with
/// pieces missing. A **minor** bump is additive and older viewers keep working,
/// which is what lets a field be added without rebuilding every consumer in
/// lockstep.
///
/// `1.1` added `source` to systems and field access.
pub const SCHEMA_VERSION: &str = "1.1";

/// Builds the S21 graph payload for a resolved module set.
///
/// Hand-written JSON rather than serde: the output must be byte-stable, and
/// writing it explicitly makes the ordering guarantees visible at the point they
/// are made instead of depending on a derive's field order.
pub fn export(resolved: &Resolved) -> String {
    let mut out = String::with_capacity(4_096);

    out.push_str("{\n");
    writeln!(out, "  \"schema\": \"{SCHEMA_VERSION}\",").ok();
    writeln!(
        out,
        "  \"schedule_hash\": \"{:016x}\",",
        resolved.schedule_hash()
    )
    .ok();

    write_modules(&mut out, resolved);
    write_capabilities(&mut out, resolved);
    write_systems(&mut out, resolved);
    write_field_access(&mut out, resolved);

    out.push_str("}\n");
    out
}

fn write_modules(out: &mut String, resolved: &Resolved) {
    out.push_str("  \"modules\": [\n");

    let mut first = true;
    for record in resolved.modules() {
        if !first {
            out.push_str(",\n");
        }
        first = false;

        write!(out, "    {{ \"id\": \"{}\", ", escape(record.id.name())).ok();
        write!(out, "\"version\": \"{}\", ", record.version).ok();
        write!(out, "\"provides\": {}, ", capability_list(record.provides)).ok();
        write!(out, "\"requires\": {}, ", capability_list(record.requires)).ok();
        write!(
            out,
            "\"optional\": {} }}",
            capability_list(record.consumes_optional)
        )
        .ok();
    }

    out.push_str("\n  ],\n");
}

fn write_capabilities(out: &mut String, resolved: &Resolved) {
    // Every capability mentioned by anyone, whether or not it has a provider.
    let mut names: Vec<(&'static str, Option<&'static str>, bool)> = Vec::new();

    for record in resolved.modules() {
        for capability in record.provides {
            names.push((capability.name(), Some(record.id.name()), true));
        }
    }

    for degradation in resolved.absent_capabilities() {
        names.push((degradation.capability.name(), None, false));
    }

    names.sort_unstable();
    names.dedup();

    out.push_str("  \"capabilities\": [\n");
    let mut first = true;
    for (name, provider, present) in &names {
        if !first {
            out.push_str(",\n");
        }
        first = false;

        write!(out, "    {{ \"name\": \"{}\", ", escape(name)).ok();
        write!(out, "\"present\": {present}, ").ok();
        match provider {
            Some(module) => write!(out, "\"provider\": \"{}\"", escape(module)).ok(),
            // An absent capability carries what its consumers do without it.
            None => {
                let behavior = resolved
                    .absent_capabilities()
                    .iter()
                    .find(|degradation| degradation.capability.name() == *name)
                    .map(|degradation| degradation.behavior)
                    .unwrap_or("undeclared");
                write!(out, "\"degraded_behavior\": \"{}\"", escape(behavior)).ok()
            }
        };
        out.push_str(" }");
    }
    out.push_str("\n  ],\n");
}

fn write_systems(out: &mut String, resolved: &Resolved) {
    out.push_str("  \"systems\": [\n");

    let mut rows: Vec<(&'static str, usize, &'static str, &'static str, Source)> = Vec::new();
    for record in resolved.modules() {
        for system in &record.systems {
            rows.push((
                system.name,
                system.phase.index(),
                system.phase.name(),
                record.id.name(),
                system.source,
            ));
        }
    }
    rows.sort_unstable();

    let mut first = true;
    for (name, phase_index, phase_name, module, source) in rows {
        if !first {
            out.push_str(",\n");
        }
        first = false;

        write!(out, "    {{ \"name\": \"{}\", ", escape(name)).ok();
        write!(out, "\"phase\": \"{}\", ", escape(phase_name)).ok();
        write!(out, "\"phase_index\": {phase_index}, ").ok();
        write!(out, "\"module\": \"{}\", ", escape(module)).ok();
        // Where the registration is, so a reader can open it. The path is
        // relative to the workspace root, which keeps the payload identical
        // between machines building the same commit.
        write!(out, "\"source\": \"{}\" }}", escape(&source.to_string())).ok();
    }

    out.push_str("\n  ],\n");
}

fn write_field_access(out: &mut String, resolved: &Resolved) {
    out.push_str("  \"field_access\": [\n");

    let mut rows: Vec<(
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        Source,
    )> = Vec::new();
    for record in resolved.modules() {
        for access in &record.accesses {
            rows.push((
                access.field,
                access.system,
                access.access.name(),
                record.id.name(),
                access.source,
            ));
        }
    }
    rows.sort_unstable();

    let mut first = true;
    for (field, system, access, module, source) in rows {
        if !first {
            out.push_str(",\n");
        }
        first = false;

        write!(out, "    {{ \"field\": \"{}\", ", escape(field)).ok();
        write!(out, "\"system\": \"{}\", ", escape(system)).ok();
        write!(out, "\"access\": \"{}\", ", escape(access)).ok();
        write!(out, "\"module\": \"{}\", ", escape(module)).ok();
        // The `registrar.access(...)` line, which is where a disputed claim
        // about who writes a field gets settled.
        write!(out, "\"source\": \"{}\" }}", escape(&source.to_string())).ok();
    }

    out.push_str("\n  ]\n");
}

/// How many systems write a given field.
///
/// `ADR-0011` permits exactly two writers for `ELEVATION` — generation and edit
/// application — and S21 makes that the one graph assertion that hard-fails CI
/// rather than merely annotating.
pub fn writers_of(resolved: &Resolved, field: &str) -> Vec<&'static str> {
    let mut writers: Vec<&'static str> = resolved
        .modules()
        .flat_map(|record| record.accesses.iter())
        .filter(|access| access.field == field && access.access.is_write())
        .map(|access| access.system)
        .collect();
    writers.sort_unstable();
    writers.dedup();
    writers
}

/// Whether a write is a direct one or goes through the deposit buffer.
pub fn is_direct_write(access: Access) -> bool {
    matches!(access, Access::Write)
}

fn capability_list(capabilities: &[crate::capability::Capability]) -> String {
    let mut names: Vec<&str> = capabilities
        .iter()
        .map(|capability| capability.name())
        .collect();
    names.sort_unstable();

    let mut out = String::from("[");
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "\"{}\"", escape(name));
    }
    out.push(']');
    out
}

/// Escapes the characters JSON requires.
///
/// Capability and module names are identifiers today, but degraded-behaviour
/// text is prose written by whoever declared it, and prose contains quotes.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            control if control.is_control() => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out
}
