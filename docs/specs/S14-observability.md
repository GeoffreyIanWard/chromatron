---
id: S14
title: Observability & Tooling
status: not started
depends_on: [S01, S02, S06]
provides: [inspector, query-console, metrics, profiling, state-hash, invariants]
crates_touched: [cx-diag, cx-ui]
milestone: M7
---

# S14 — Observability & Tooling

For a simulation engine this is not developer convenience — it is the primary interface to the thing you built. A simulation you cannot inspect is a simulation you cannot debug, tune, or trust.

## Requirements

- **State hashing**: a 128-bit digest per tick over authoritative sim state (registered components, persisted fields). Hashes are comparable **only within an identical module set** (`ADR-0012`); the hash record carries the module set fingerprint so a cross-configuration comparison fails loudly instead of reporting spurious divergence. Order-independent by construction (commutative combine over per-entity hashes). This is the mechanism behind every determinism claim in this doc set.
- **Divergence detector**: run two instances of a scenario and report the first tick where hashes differ, then bisect by component and field to name the culprit. Without this, a determinism bug takes days; with it, minutes.
- **Invariant system**: invariants are registered by modules and declare their own capability preconditions, so an invariant spanning two modules is skipped (and reported as skipped) when either is absent. Registered predicates checked every tick (or every N ticks) — population non-negativity, no NaN in any field, no entity outside world bounds, elevation unchanged since generation except at audited edit sites. (Water mass conservation is no longer an invariant; `ADR-0009` removed the possibility of drift.) Violations report with tick, location, and magnitude. Cheap invariants run always; expensive ones run under a debug flag.
- **Entity inspector**: egui panel listing entities with filtering, showing all reflected component values, live-editable in dev builds. Edits are commands, so they appear in replay logs and do not silently break determinism.
- **Field inspector**: visualize any `FieldId` as a color-mapped overlay on the terrain, with a value readout under the cursor. Indispensable for solver debugging — a broken hydrology kernel is obvious as a picture and invisible as a number.
- **Query console**: type a query (`Position + Hunger where Hunger > 0.8`) and get live results with count, table, and a "select in world" action.
- **Metrics**: named counters, gauges, and histograms, sampled per tick into ring buffers with live charts. Standard set: tick time by phase, entity counts by tier, chunk counts by state, field solver time, allocation count, memory by subsystem.
- **Profiling**: `tracing` spans throughout, with Tracy integration. Every phase and every solver is a span.
- **Time-series export**: a scenario can declare metrics to record; the headless runner writes CSV or Parquet for offline analysis. This is how parameter sweeps produce results.

## Non-goals

No remote/network telemetry. No always-on production analytics. Inspector edits are dev-only and compiled out of release.

## Acceptance criteria

- State hash computation under 2 ms for 1,000,000 entities plus 16M field cells.
- Hash is invariant across thread counts and entity iteration order, verified by test.
- Divergence detector locates a deliberately injected non-determinism within 30 seconds.
- Inspector remains responsive (under 16 ms frame) with 1,000,000 entities in the world.
- Field overlay renders any registered field without per-field code.
- Headless runner exports declared metrics for a 100,000-tick run without measurable slowdown.

## Open questions

- Whether the state hash should include physics results, pending S11's cross-architecture determinism finding.
