---
id: S21
title: Architecture Graph & Isometric Viewer
status: not started
depends_on: [S02, S06, S14, S20]
provides: [graph-export, graph-schema, isometric-viewer, graph-diff]
crates_touched: [cx-module, cx-ecs, cx-fields, cx-diag, chromatron-cli]
milestone: M0 (export), M1 (viewer), M7 (live feed)
---

# S21 — Architecture Graph & Isometric Viewer

A developer tool that renders the engine's *resolved* structure as a navigable isometric
diagram: modules and the capabilities wiring them together, systems placed in the twelve
tick phases, and which systems read or write which fields.

Two purposes, and they pull in the same direction: reasoning about architectural decisions
before making them, and locating the cause of a bug in a system too large to hold in your
head. Both are served by drawing what the engine *actually resolved at startup*, never by
drawing what the docs claim.

## The central decision: the registry is the source of truth

The graph is exported from the running engine's own registries, not recovered by parsing
Rust source.

This is only possible because `ADR-0012` already requires every subsystem to declare its
identity, capabilities, and registrations in one place. `cx-module` resolves that
declaration into a schedule at startup; S21 serializes the result. Source analysis was
rejected — it would re-derive the crate graph, which is the least interesting layer, while
missing every edge that matters: capability wiring is indirect by design, degradation
choices are made at resolution time, and field access is a property of system parameter
sets rather than of `use` statements.

A consequence worth stating plainly: **the graph cannot drift from the engine**, because
it is a projection of the same data the schedule is built from. If the diagram is wrong,
the engine is wrong.

## Three layers

The export carries three layers over a shared node set. The viewer shows one at a time or
overlays them.

| Layer | Nodes | Edges | Answers |
|---|---|---|---|
| **Composition** | Modules | `provides` / `requires` / `consumes_optional` on capabilities | What is switched on, what depends on what, which degradations are active |
| **Schedule** | Systems, grouped by module, placed in the twelve phases | Ordering constraints, phase boundaries | Why does this run before that; where does a determinism bug live |
| **Field access** | `FieldId`s | System → field `Read` / `Write` / `Deposit` | Who touches `ELEVATION`; where does the sparse/dense bridge get crossed |

Capabilities are drawn as **first-class nodes**, not as edge labels. A module never names
another module (`ADR-0012`), and a diagram that draws module→module edges would be
depicting a coupling the architecture forbids. Rendering the capability as the thing in
between is what makes an undeclared reliance visually obvious: the edge has nowhere to
attach.

**Absent capabilities are drawn, not omitted.** A required capability with no provider is
a startup error (S20), but an *optional* one resolves to a documented degraded behavior —
and that degradation is exactly the thing that is invisible in code review and expensive
in debugging. It gets a node, marked absent, with the degraded behavior as its
description.

## Export

- `chromatron-cli graph --profile <name> --out graph.json` builds the world for a profile,
  resolves modules, and serializes without running a tick. Fast enough to run on every
  commit.
- The export is **deterministic**: nodes and edges sorted by stable id (`ModuleId`,
  `FieldId`, system name), no map iteration, no timestamps or paths in the payload. Two
  runs of the same build and profile produce byte-identical files. This is what makes
  diffing possible and is required by the same rules as everything else in `03-conventions.md`.
- The payload carries the **resolved schedule hash** (S20) and the module set, so a graph
  can be matched against the save or replay it describes.
- Schema is versioned. The viewer refuses a payload whose major version it does not know
  rather than rendering a partial diagram.

## Layout

Layout is computed in the viewer, not the engine, and must be **stable**: the same graph
lays out identically every time, and adding one module moves as little as possible.
Positions derive from graph structure with a deterministic tiebreak, never from a
force-directed simulation with a random seed — an architecture diagram that reshuffles
between runs cannot be compared against yesterday's.

Structures are placed on an isometric grid. Height encodes a scalar chosen per layer
(system count, tick cost, field bytes); footprint encodes grouping. The convention is
documented once in the viewer legend and not re-invented per layer.

## Diff

Two exports can be compared: modules and capabilities added, removed, or newly degraded;
systems that changed phase; field access that appeared or disappeared. `--baseline` renders
the diff directly.

This is the feature that earns the tool a place in CI rather than a bookmark. A pull
request that silently adds a second writer to `ELEVATION`, moves a system across a phase
boundary, or introduces a new optional dependency shows that fact as a graph delta, in
review, before it becomes a determinism bug at tick 50,000.

## Live feed (M7, deferred)

The static export is the M0/M1 deliverable. Once the inspector exists (S14), the same
schema streams over a local socket so nodes carry live values — per-system tick cost,
dirty tile counts, field write volume, and divergence location during a bisect. The
viewer is the same page; only the data source changes. The schema is designed for this
now so it does not need reworking later, but nothing streams before M7.

## Non-goals

- **Not an editor.** Nothing is authored, rewired, or toggled from the diagram. It renders
  state; config and scenarios remain the way modules are switched on and off.
- **Not a source browser.** Nodes link to a file and line; they do not display code.
- **Not shipped in the game.** The viewer is a repo tool under `tools/`. The exporter lives
  behind a `cx-diag` feature that the `game` profile does not enable.
- **No render dependency.** Export runs headless. The viewer is a static page with no
  engine code in it, so the `sim/` firewall is untouched.

## Acceptance criteria

- Exporting the same profile twice on the same build produces byte-identical JSON.
- Exports of 10 shuffled module registration orders are byte-identical — the same property
  the S20 gate asserts, made visible.
- Every module, capability, system, and field present in the resolved schedule appears in
  the export; a test asserts the counts match the registries directly.
- An optionally-consumed capability with no provider renders as an absent node carrying its
  documented degraded behavior.
- A system registered in a phase renders in that phase's lane; a table-driven test covers
  all twelve.
- Field-access layer shows exactly two writers for `ELEVATION` in the `full-sim` profile
  (S07 generation, S19 edits); a third writer fails the test (`ADR-0011`).
- Layout of an unchanged graph is pixel-identical across runs; adding one module perturbs
  no more than its own neighborhood.
- `--baseline` on two adjacent commits reports added, removed, and degraded elements with
  no false positives.
- Viewer opens from `file://` with no build step, no network access, and no external asset
  requests.

## Open questions

- ~~Whether the field-access layer is derived automatically or declared.~~ Decided:
  **declared explicitly** in `Module::register`, with a test cross-checking the declaration
  against `bevy_ecs` access metadata where that metadata can attribute an access. Automatic
  derivation is silently incomplete when a write goes through the deposit buffer rather than
  a system parameter, and a graph that quietly omits an `ELEVATION` writer is worse than no
  graph. The declaration is the claim; the cross-check is what stops it from rotting.
- Whether the generation pipeline (S07) — the *second* composition graph in `ADR-0012` —
  is a fourth layer here or its own view. It is a DAG of stages rather than a phase
  schedule, so the isometric treatment may not transfer. Revisit at M2.
- ~~Whether graph diff should hard-fail CI or only annotate a pull request.~~ Decided:
  **annotate**, with one exception — the `ELEVATION` writer-count assertion hard-fails
  (`ADR-0011` permits exactly two writers, so a third is a defect rather than a change).
  A diff that blocks merges on every legitimate architecture change gets switched off within
  a month, and a check nobody runs is worth less than one that merely reports.
