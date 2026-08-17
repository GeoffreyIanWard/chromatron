# ADR-0009 — Water is a flow network plus classified bodies, not a fluid simulation

**Status:** accepted · **Date:** 2026-08-16 · **Amended by:** `ADR-0011` (impoundments and incremental drainage repair)

## Context

The original hydrology design was a pipe-model fluid solver over a dense `WATER_DEPTH` field: a 16M-cell stencil every tick, with mass conservation as a hard invariant checked to 1e-4 relative error over a million ticks. That invariant was identified as the single most important correctness criterion in the plan, because a slow leak becomes an ocean or a desert over simulated decades and is nearly impossible to diagnose after the fact.

The requirement, though, is that water *flow logically* — not that it be volumetrically accurate.

## Decision

Water is classified at generation into two kinds, handled by different machinery.

**Infinite bodies** — anything above a threshold extent or discharge. These have a *surface level*, not a volume. They never drain, never fill, and are never mass-balanced. Standing bodies are a surface elevation plus an extent mask. Flowing bodies are edges in a flow network derived from generation-time drainage, carrying a discharge computed from upstream catchment area times current precipitation, with channel width, depth, and velocity from hydraulic geometry power laws. Flooding is a lookup into floodplain masks precomputed per discharge tier, not a solve.

**Finite bodies** — puddles, ponds, cisterns, reservoirs. These *do* track volume, but there are few of them, so they are entities rather than field data. They fill, drain, overflow, and can be promoted to infinite if they grow past the threshold.

## Rationale

This maps unusually cleanly onto the architecture's existing sparse/dense split (`ADR-0003`): infinite water becomes static dense data, finite water becomes sparse dynamic data. Neither needs a fluid solver.

It also removes the plan's hardest correctness problem. Mass conservation, CFL stability analysis for water, sediment transport, and the f32-drift-over-10^6-ticks concern all disappear. Hydrology drops from a per-tick 16M-cell stencil to a graph update over a few thousand edges with an occasional derived-field refresh.

Hydraulic geometry relations are what preserve the "flows logically" requirement: a river with ten times the catchment is visibly larger and faster, and a storm upstream produces a flood downstream after a routing lag, without any volume ever being integrated.

## Consequences

- `WATER_DEPTH` becomes a derived field, recomputed only when a body's level or a river's discharge tier changes — not per-tick state.
- `FLOW_X`/`FLOW_Z` become static, computed at generation.
- Damming a river edits the flow network graph rather than emerging from fluid behavior. `ADR-0011` specifies how: the upstream impoundment fills to its **spill elevation**, the lowest escape point of the basin, computed from terrain by bounded flood-fill. Fill to spill, then overflow continues downstream at the original discharge. The level is a pure function of terrain geometry — still no volume integration.
- Water cannot be "used up". A settlement drawing from an infinite source has unlimited water by construction; scarcity must come from finite bodies or from access, not from depletion of a river.
- The infinite/finite threshold becomes a significant tuning knob, and gets its own open question in S08.
