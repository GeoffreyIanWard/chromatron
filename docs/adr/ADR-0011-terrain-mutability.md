# ADR-0011 — Terrain is mutable by discrete edit; the dirty unit is the tile

**Status:** accepted · **Date:** 2026-08-16 · **Clarifies:** `ADR-0008` · **Amends:** `ADR-0009`

## Context

`ADR-0008` moved erosion to generation time and described elevation as "immutable for the life of the world", with gameplay terrain edits mentioned only as an audited exception. S12 went further and made "zero runtime remeshing paths exist" an acceptance criterion.

That was an over-correction. Terrain manipulation and construction are intended game mechanics, and a plan that treats them as an exception will produce an engine that fights them.

## The actual distinction

The cost `ADR-0008` avoided was not mutation. It was **diffuse, continuous, global** change:

| | Continuous erosion | Discrete edit |
|---|---|---|
| Cells changed per tick | ~16,000,000 | ~100 |
| Frequency | Every tick, forever | Event-driven |
| Locality | Everywhere | Bounded region |
| Delta persistence | Every cell drifts; unbounded growth | Sparse cell list |

Ten thousand player edits are a rounding error against one tick of global erosion. The two are not the same kind of thing and should not have shared a policy.

## Decision

Terrain is **mutable by discrete edit**. Edits are commands (S19), applied in a dedicated phase, subject to content-defined constraints, and fully replayable and undoable.

To make local change cheap, dirty tracking granularity is the **tile** — 64×64 cells, 32 m, 256 per chunk:

- **Meshes** dirty per tile, with a two-tier response: an immediate sub-millisecond patch mesh, then a background full-quality chunk rebake.
- **Colliders** stay chunk-level rapier heightfields, updated by in-place partial height writes rather than rebuilds.
- **Nav cost grids** dirty per tile; flow fields invalidate lazily.
- **Persistence** is a sparse cell list; `ELEVATION` returns to `DeltaPersisted`.

What `ADR-0008` still holds: there is **no continuous per-tick erosion**, and generation-time erosion still produces the baked base terrain that 99.9% of the world uses untouched. The generation erosion kernel additionally becomes callable on a bounded region for event-triggered effects (slope failure, dam break, channel widening) behind a flag.

## Amendment to `ADR-0009`

Terrain edits can change water. Damming raises terrain across a flow network edge; the upstream impoundment fills to its **spill elevation** — the lowest escape point of the basin, computed from terrain by bounded flood-fill — then overflows downstream at the original discharge.

This is consistent with `ADR-0009` rather than a retreat from it: the impoundment level is a pure function of terrain geometry, not an integrated volume. Nothing is simulated as fluid. Flow topology changes trigger **incremental drainage repair** over the affected neighborhood only, capped at one block.

## Consequences

- Tile granularity must be fixed before M1, because mesh layout and dirty-tracking structures depend on it. This is the main reason this ADR is not deferrable.
- S12's "zero remeshing paths" criterion is withdrawn and replaced with a latency budget for the patch-and-rebake path.
- Chunks with edits are pinned to delta storage and must restore edits before fast-forward (S09).
- The save-size property survives: untouched chunks still cost zero bytes, because edits are sparse and authored rather than diffuse and emergent.
- Digging invites players to attempt caves. Heightfield terrain cannot represent overhangs, and that limitation becomes much more visible once excavation is a mechanic. S19 raises this as the open question that most needs answering before M1.
