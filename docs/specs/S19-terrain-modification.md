---
id: S19
title: Terrain Modification & Construction
status: not started
depends_on: [S06, S07, S08, S11, S12, S13]
provides: [edit-commands, tile-dirty-tracking, mesh-patching, collider-updates, impoundments, structure-siting]
crates_touched: [cx-fields, cx-worldgen, cx-solvers, cx-render, cx-physics, cx-persist]
milestone: M4B
---

# S19 — Terrain Modification & Construction

Terrain is **mutable by discrete edit** (`ADR-0011`). What was removed at `ADR-0008` was erosion as a *continuous global process*, not the ability to change the ground. This spec is the machinery that makes local change cheap.

## The distinction that governs everything here

| | Diffuse continuous change | Discrete local change |
|---|---|---|
| Example | Erosion every tick, everywhere | Player digs a trench |
| Cells affected per tick | ~16,000,000 | ~100 |
| Mesh consequence | Rebuild every chunk, forever | Patch one tile |
| Collider consequence | Rebuild every chunk, forever | Update a height range in place |
| Persistence consequence | Every cell drifts; deltas grow without bound | Sparse cell list; a few KB |
| Verdict | Moved to generation (`ADR-0008`) | **Fully supported, this spec** |

## Granularity: the tile

Chunks are too coarse a unit of change. A 4 m dig should not remesh 1024² cells.

- **Tile** = 64×64 cells = 32 m square. 256 tiles per chunk.
- **Mesh dirty granularity = tile.**
- **Collider granularity = chunk**, but updates are *in-place partial height writes* to the rapier heightfield, not rebuilds.
- **Nav cost grid dirty granularity = tile.**
- **Field delta persistence granularity = cell** (sparse index list).

Tile constants live in `03-conventions.md` and must be fixed before M1, because mesh and dirty-tracking layout depend on them.

## Edit model

Terrain is a **2.5D heightfield** and editing follows the **Valheim model** (`ADR-0013`): a radius brush with falloff, applied around a point. Ditches, pits, moats, mounds, ramps, and terraces are all expressible. Caves, tunnels, and overhangs are not — enclosed space comes from structures, not terrain.

- Every terrain change is an **`EditCommand`** entering `IntakeCommands` (S16): a brush centre, radius, falloff curve, operation, and actor. Operations: `Raise`, `Lower`, `LevelTo(reference_height)`, `Smooth`.
- `LevelTo` is the workhorse — the player picks a reference height (usually where they stand) and flattens toward it, which produces building platforms and terraces without needing separate raise and lower passes.
- **Depth and height clamp against generated elevation.** `MAX_DIG_DEPTH` and `MAX_RAISE_HEIGHT` are measured relative to the base terrain from generation, which is free because it is regenerable from the seed. This bounds delta storage, prevents digging to the world floor, and gives a limit that is explainable to players rather than arbitrary.
- The brush **cannot produce a ceiling**. This should fail legibly in the UI rather than silently clamping — players will try.
- Because edits are commands, **undo, replay, and determinism come for free** — they are already in the replay log (S13) and already ordered.
- Edits are applied in a dedicated `TerrainEdit` phase, before `FieldSolve`, so downstream systems see a consistent surface for the whole tick.
- **Constraints** are content-defined (S04) and evaluated before application: maximum unsupported slope by material, material hardness gating which tools apply, minimum and maximum elevation, protected regions.
- **Excavation bookkeeping** is an optional module capability: if the `cap::EXCAVATION_VOLUME` provider is enabled, lowering terrain yields material that must be deposited somewhere, making cut-and-fill a real constraint. Absent, terrain edits are free-form. (Valheim's hoe-costs-stone economy is this capability enabled.)

## Two-tier visual response (S12)

The player must see the change immediately; quality can catch up.

1. **Immediate**: the dirty tile gets a runtime-generated patch mesh, sub-millisecond, swapped into the draw list the same frame. Slightly lower quality than the baked mesh.
2. **Background**: the affected chunk is re-baked at full quality on the generation pool and swaps in within a few frames.

The baked base mesh from generation (`ADR-0008`) still does the work for the 99.9% of the world nobody has touched, so offline meshing quality is preserved everywhere it matters.

## Collider response (S11)

Rapier heightfield colliders support in-place height modification. An edit writes the changed height range directly rather than rebuilding the collider. Chunk-level granularity is retained because thousands of small colliders would cost more in broad-phase than they save in updates.

## Navigation response (S10)

- The slope component of the cost grid is recomputed for dirty tiles only.
- Flow fields whose footprint intersects a dirty tile are invalidated and rebuilt lazily on next use, not eagerly.
- Agents mid-path replan when their next waypoint lands in a dirty tile.

## Water interaction (`ADR-0009` amended)

This is where terrain editing gets interesting, and where the plan needed real additions rather than a permission slip.

- **Digging below a local water surface floods.** The edit triggers a check against nearby body surface levels; if the new terrain is below one, the body's extent mask expands into the excavation.
- **Damming.** Raising terrain across a flow network edge blocks it. The upstream impoundment fills to its **spill elevation** — the lowest escape point of the basin, computed from terrain by a bounded flood-fill. Fill to spill, then overflow continues downstream at the original discharge. **No volume integration**: the level is a pure function of terrain geometry, exactly consistent with `ADR-0009`.
- **Canals.** Carving a channel between two points creates a new flow network edge if the excavation connects them below both water surfaces. Discharge follows the existing routing rules.
- **Incremental drainage repair.** After an edit that changes flow topology, re-run depression fill and flow routing over the **affected neighborhood only** — expanding outward until the routing result stabilizes against the untouched region, capped at one block. Never global.
- **Impoundment classification.** A new impoundment starts `Finite` and volume-tracks while filling, then promotes to `Infinite` at its spill elevation once it exceeds the size threshold. It cannot grow past spill, so promotion is bounded and well-defined.

## Structures

- Structures are **entities**, not terrain. They do not write to `ELEVATION`.
- **Structures are where verticality lives** (`ADR-0013`). Mines, tunnels, cellars, and caves are placed structures with their own meshes and colliders, not terrain features. This is the same split Valheim uses for dungeons.
- **Site preparation**: placing a structure may emit terrain `EditCommand`s (flatten a footprint, cut a terrace). These go through the same path and the same constraints as manual edits — no privileged write.
- Structures contribute to the nav cost grid as a separate overlay component, so removing a building does not require touching terrain fields.
- Structures can block flow network edges the same way a dam does, via the impoundment path above.

## Optional module: locally triggered erosion

The generation-time erosion kernel (S07) is a callable local operation, not solely a generation stage. Packaged as its own module providing `cap::LOCAL_EROSION` (S20), so it can be enabled per scenario:

- A player over-steepens a cut → slope failure, thermal relaxation over the affected tiles.
- A dam breaks → a burst of hydraulic erosion along the release path.
- A sustained top-tier flood → channel widening.

This keeps erosion out of the tick loop while making the *effects* of erosion available to gameplay. Same kernel, different trigger. Disabled by default; consumers degrade by simply never receiving the events.

## Persistence (S13 amended)

- `ELEVATION` returns to `DeltaPersisted`, but the delta is a **sparse cell list**, not a dense field diff. An untouched chunk still costs zero bytes.
- A chunk with edits can never be treated as purely regenerable. It is pinned to delta storage and must restore edits on rehydration *before* fast-forward runs.
- Flow network modifications (blocked edges, new canal edges) persist as graph deltas alongside terrain deltas.

## Non-goals

No voxel terrain, caves, tunnels, or overhangs (`ADR-0013`). No destructible structures at terrain granularity. No continuous per-tick erosion.

## Acceptance criteria

- A single-tile edit: patch mesh visible the same frame, under 1 ms; full chunk rebake swaps in within 5 frames.
- A brush edit at `MAX_DIG_DEPTH` clamps correctly and reports the limit to the UI rather than silently stopping.
- `LevelTo` across a slope produces a flat platform at the reference height within one cell of tolerance.
- 1,000 edits in one tick (large-area terraforming) completes within the tick budget with no frame exceeding 20 ms.
- Collider height update for an edited region under 0.5 ms.
- Nav cost grid update for a dirty tile under 0.5 ms; an agent whose path is blocked replans within 1 simulated second.
- Damming a river produces an impoundment that fills to the computed spill elevation and no further, with overflow continuing downstream at the original discharge.
- A canal connecting two water bodies produces a flow network edge with correct discharge direction.
- Incremental drainage repair after an edit never touches more than one block, and produces the same routing as a full regional recompute over the affected area.
- 100,000 scattered edits across a world save in under 5 MB.
- Edit → save → load → compare matches by field hash; edit → undo → compare matches the pre-edit hash exactly.
- A replay containing 10,000 edits reproduces the final terrain hash exactly.

## Open questions

- ~~Whether excavation volume bookkeeping is a mechanic you want.~~ Decided: **build it,
  default off.** Every `EditCommand` carries the volume it moved from the start, because
  threading volume through the edit path later would touch every command variant. Whether
  that volume must be deposited, hauled, or discarded is a game-layer rule behind a flag,
  decided once terrain editing is playable at M4B.
- ~~Whether heightfield is sufficient once digging is a core mechanic.~~ **Closed by `ADR-0013`**: heightfield, Valheim-style editing, verticality via structures. Tile layout can be fixed at M1 without risk.
- ~~Whether mines and tunnels are wanted as content.~~ Decided: **yes**, as placed
  structures with their own meshes and colliders — `ADR-0013` makes terrain a heightfield
  with no overhangs, so there is no other way to express them, and this stays a content
  problem rather than an engine one. **The structure-siting path must be validated against
  a tunnel entrance at M4B**, while siting is still cheap to change: an entrance is the
  awkward case, because it sits at an edited slope face rather than on flat ground.
