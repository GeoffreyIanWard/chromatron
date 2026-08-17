# ADR-0003 — Dense fields live outside the ECS

**Status:** accepted · **Date:** 2026-08-16

## Context

The simulation domains include hydrology, erosion, weather, and ecology. These are per-cell continuous quantities over all space. One 512 m chunk at 0.5 m cells is 1,048,576 cells; sixteen loaded chunks is 16.7M. Modeling cells as entities would mean 16M entities to perform work that is a stencil pass over an array, and the ECS's per-entity overhead — archetype lookup, component storage indirection, change tick — is pure waste for data that has no identity and no variable component set.

## Decision

Two storage regimes side by side. Sparse entities in `bevy_ecs` (S02). Dense fields in chunked SoA arrays with halo borders, stepped by branch-free stencil kernels (S06). The bridge is narrow and explicit: entities read fields via `fields.sample(pos)`; entities write fields only via a deposit buffer drained deterministically in a fixed phase.

The CPU implementation is authoritative. A wgpu compute fast path may exist for generation-time erosion (S07) but must validate against CPU output in CI, since a block regenerated on different hardware must produce identical terrain.

## Rationale

This is the largest single performance decision in the engine. The array form is roughly two orders of magnitude cheaper per cell and vectorizes; the entity form does neither.

Keeping CPU authoritative preserves determinism, since GPU floating-point results vary across vendors and drivers — unacceptable for a state hash that gates every determinism claim in the doc set.

## Consequences

- Two mental models for contributors; the glossary and `02-architecture.md` must make the distinction unmissable.
- The entity↔field bridge is a designed interface, not an incidental one, and its determinism (sorted deposit application) is load-bearing.
- Memory budgeting becomes a first-class artifact: field storage dominates, and quantization is mandatory rather than optional.
- Any GPU path carries a permanent CI validation cost, which is the price of keeping it honest.
- `ADR-0009` later exploited this split further: infinite water became static dense data and finite water became sparse entities, so neither needs a solver at all.
