# ADR-0008 — Erosion happens once, at generation time

**Status:** accepted · **Date:** 2026-08-16 · **Clarified by:** `ADR-0011`

## Context

Erosion was originally specified as a runtime solver (S08), stepping every tick alongside hydrology and ecology. That imposed continuous terrain change, which in turn required terrain remeshing, heightfield collider rebuilds, navigation cost-grid invalidation, elevation delta persistence, and sediment mass conservation over millions of ticks.

## Decision

Erosion runs **once**, during world generation, as stages 3–5 of the block pipeline (S07). There is **no continuous per-tick erosion solver**.

> **Clarified by `ADR-0011`.** This decision removes erosion as a *global continuous process*. It does **not** make terrain read-only: discrete, local, event-driven edits (digging, terracing, construction, dam-break erosion) are fully supported and are specified in S19. The original phrasing "immutable for the life of the world" over-stated the decision and is withdrawn.

Because erosion is iterative and non-local, it cannot be a pure function of a single cell's coordinate. Generation granularity therefore moves from the chunk to the **block** — 16×16 chunks, generated as a unit with a discarded halo margin. `ADR-0006` is amended accordingly: blocks are positionally deterministic; chunks are pure extraction from a generated block.

## Rationale

Continuous runtime erosion is expensive in a way that compounds: it is not only its own cost, but the cost of everything downstream that must react to terrain changing *everywhere, every tick*. Removing the continuous process eliminates all of it at once — while leaving discrete local edits entirely affordable (`ADR-0011`).

Grid-based stream-power erosion is chosen over droplet-based erosion because droplet methods consume a sequential RNG and are therefore order-dependent, which `ADR-0006` forbids. Grid-based erosion is deterministic and parallelizes by row band.

## Consequences

**Gained:**
- Terrain meshes are baked once at generation and used untouched across the ~99.9% of the world nobody edits (S12).
- Physics colliders, slope-derived nav cost grids, and mesh data are dirtied only by discrete edits, never by a background process (S10, S11, S19).
- `ELEVATION` deltas are sparse and authored rather than diffuse and emergent, so untouched chunks still cost zero bytes (S13, S19).
- Sediment conservation disappears as a runtime invariant (S08).

**Paid:**
- Block generation is expensive — seconds rather than the ~40 ms a chunk took. This requires a background generation pipeline with a frontier ahead of the player, plus a disposable on-disk block cache (S07).
- Fine erosion detail cannot be perfectly continuous across block seams with a finite halo. Rivers stay coherent because region-level drainage constrains them from above, but hillside detail may show a faint seam. Open question in S07, resolved visually at M2.
- Player- or gameplay-driven terrain edits are still supported as chunk deltas, but they are discrete authored changes rather than a process.
