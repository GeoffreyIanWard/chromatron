---
id: M4B
title: Terrain Modification & Construction
specs: [S19]
gate: bench/baselines.md#m4b
depends_on_milestones: [M2, M4]
note: Runs after M4, before M5. Tile granularity from this spec must be fixed at M1.
---

# M4B — Terrain Modification & Construction

Digging, terracing, damming, and building. Slotted here because it needs generated terrain (M2), the flow network (M4), and the render path (M1) — but **the tile granularity it depends on must be fixed at M1**, before mesh layout is written. That is the one part of this milestone that cannot wait.

## Deliverables

- Valheim-model radius brush with falloff (`ADR-0013`): `Raise`, `Lower`, `LevelTo(reference_height)`, `Smooth`, entering `IntakeCommands` and applied in the `TerrainEdit` phase.
- `MAX_DIG_DEPTH` / `MAX_RAISE_HEIGHT` clamping against generated elevation, with legible UI feedback at the limit.
- Content-defined edit constraints: max unsupported slope by material, hardness gating, elevation bounds, protected regions (S04).
- Tile dirty-tracking bitsets in `cx-fields` (S06).
- Two-tier mesh response: immediate tile patch mesh, background full-quality chunk rebake (S12).
- In-place partial height updates to rapier heightfield colliders (S11).
- Separable nav cost grid components with per-tile slope recomputation and lazy flow-field invalidation (S10).
- **Impoundments**: spill-elevation flood-fill, promotion from finite to infinite, overflow routing (S08).
- **Incremental drainage repair** on flow-topology change, neighborhood-bounded, capped at one block (S08).
- Canal creation as a new flow network edge.
- Structure siting: placement emits terrain edits through the same constrained path; structures contribute a separate nav overlay component.
- Sparse cell-list elevation deltas; edited-chunk pinning; edit restoration before fast-forward (S13, S09).
- Optional module providing `cap::LOCAL_EROSION`: event-triggered local erosion (slope failure, dam break, flood channel widening) reusing the generation kernel.
- Optional module providing `cap::EXCAVATION_VOLUME`: cut-and-fill bookkeeping.

## Exit criteria

| Check | Target |
|---|---|
| Single-tile edit → patch mesh visible | same frame, < 1 ms |
| Background chunk rebake swap-in | < 5 frames |
| Collider in-place height update | < 0.5 ms, no rebuild, no broad-phase churn |
| Nav cost grid update, one dirty tile | < 0.5 ms |
| Agent path blocked by an edit | replanned < 1 simulated second |
| 1,000 edits in one tick (mass terraforming) | within tick budget, no frame > 20 ms |
| Dam a river | impoundment fills to spill elevation and no further; overflow continues downstream at original discharge |
| Canal between two bodies | flow network edge with correct discharge direction |
| Incremental drainage repair | never exceeds one block; matches full regional recompute over the affected area |
| 100,000 scattered edits | save < 5 MB |
| Edit → save → load | field hash matches |
| Edit → undo | field hash matches pre-edit exactly |
| Replay with 10,000 edits | final terrain hash exact |
| `LevelTo` across a slope | flat platform within one cell of tolerance |
| Edit at `MAX_DIG_DEPTH` | clamps, reports limit to UI |
| Terrain-edit module disabled | dirty-tile tracking unallocated, zero tick cost |

## Notes

**Terrain representation is settled** (`ADR-0013`): 2.5D heightfield, Valheim-style brush, verticality via placed structures. That closes what was the highest-leverage open question in the plan and lets M1 fix the tile layout without risk. It also keeps tile patch meshing trivial — a plain heightfield remesh, no isosurface extraction.

Players will still try to dig caves. Make that fail legibly in the UI rather than silently clamping.

Impoundment-by-spill-elevation is the design worth understanding here. It gives players exactly what they expect from damming a river — water backs up, fills the valley, then overflows — while computing the level as a pure function of terrain geometry. No volume is ever integrated, so `ADR-0009`'s simplification survives contact with the mechanic that would most obviously threaten it.
