# ADR-0013 — Heightfield terrain with Valheim-style editing; verticality comes from structures

**Status:** accepted · **Date:** 2026-08-16 · **Closes:** the S12/S19 open question on terrain representation

## Context

S12 and S19 both flagged the same unresolved question as the highest-leverage item in the plan: whether terrain is a heightfield or needs a voxel or layered representation. It mattered because tile layout is fixed at M1 and a later change would touch S06, S07, S12, and S19 simultaneously.

The question got sharper once digging became a core mechanic (`ADR-0011`), since a heightfield cannot represent an overhang.

## Decision

**Terrain is a 2.5D heightfield.** One elevation value per cell, no overhangs, no caves, no tunnels in terrain.

Editing follows the **Valheim model**: a radius brush with falloff, applied around a point, offering raise, lower, level-to-reference-height, and smooth. Digging produces ditches, pits, and moats; raising produces mounds, ramps, and platforms. Walls can be steep but are always a surface, never a ceiling.

**Depth and height are clamped relative to generated elevation.** `MAX_DIG_DEPTH` and `MAX_RAISE_HEIGHT` are measured against the base terrain from generation, which is always available for free because it is regenerable from the seed. This bounds delta storage, prevents digging to the world floor, and gives a natural, explainable limit rather than an arbitrary one.

**Verticality comes from structures, not terrain.** Mines, tunnels, cellars, caves, and dungeons are entities with their own meshes and colliders, placed into or onto the terrain. This is how Valheim handles dungeons, and it is the right split: terrain is a continuous field, enclosed space is an object.

## Rationale

Voxel terrain would cost a different storage model (S06), a different generation pipeline (S07), a different mesher — marching cubes or dual contouring instead of a trivial heightfield remesh (S12) — a different collider representation (S11), and roughly an order of magnitude more memory for the same extent. It would buy overhangs.

The Valheim model demonstrates that raise, lower, flatten, and ditch cover the great majority of what players actually want from terrain manipulation, and that the absence of caves in terrain is not felt when enclosed spaces exist as placed structures.

Clamping to generated elevation is the detail that makes editing cheap rather than merely possible: the base is free, so the delta is genuinely sparse, and the bound is enforced without extra state.

## Consequences

- **The open question is closed.** Tile layout can be fixed at M1 without risk.
- Tile patch meshing is a plain heightfield remesh — no isosurface extraction, no seam-stitching between differing voxel LODs. S12's patch path stays sub-millisecond.
- Rapier heightfield colliders remain correct; no collider representation change is ever needed for terrain.
- Water interaction stays simple: an infinite body's depth is `surface_level - elevation`, which is only well-defined for a heightfield (`ADR-0009`).
- Players will still attempt to dig caves. The engine should fail this *legibly* — the brush simply cannot produce a ceiling — rather than producing broken geometry. Worth a UI affordance rather than silent clamping.
- If caves are wanted as content, they are authored or procedurally placed structures, and that is a content problem rather than an engine one.
