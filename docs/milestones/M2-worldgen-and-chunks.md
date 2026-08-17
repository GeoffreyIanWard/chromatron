---
id: M2
title: Worldgen, Erosion & Chunk Lifecycle
specs: [S07]
gate: bench/baselines.md#m2
---

# M2 — Worldgen, Erosion & Chunk Lifecycle

An effectively infinite world, deterministically generated from a seed, eroded once at generation, with blocks streaming in behind a background frontier. After this milestone, terrain never changes again.

## Deliverables

- World map: coarse elevation, uplift, precipitation, temperature, global drainage, biome assignment.
- **Block generation pipeline** (`ADR-0006`, `ADR-0008`): base elevation → depression fill and flow routing → grid-based hydraulic erosion → thermal erosion → channel carving → halo discard → static field derivation → biome → scatter.
- Generation pipeline as a **composition point** (S20): each stage is a module registration, so hydraulic erosion, thermal erosion, channel carving, biome assignment, and scatter can each be toggled independently.
- Background generation pool with a frontier sized to outpace maximum travel speed.
- Disposable on-disk block cache keyed by `(seed, block_coord, generator_version)`.
- Chunk extraction from cached blocks (pure slicing).
- Chunk state machine with amortized transitions and an `Active` cap.
- Terrain meshes and water surface meshes **baked at generation** and cached alongside the block (S12).
- Field inspector overlay (early slice of S14) — visualizing fields is how worldgen gets debugged.

## Exit criteria

| Check | Target |
|---|---|
| 4×4 block area generated in two different orders | identical field hashes |
| Single block generation (16,384², full pipeline) | < 20 s, 8 background threads |
| Chunk extraction from cached block | < 5 ms |
| Terrain mesh bake, one chunk | < 200 ms offline |
| Flow continuity walk over 100 km of channel | unbroken across chunk *and* block seams |
| Camera traversal at 200 m/s | frontier never outrun, no frame > 20 ms |
| Delete block cache, replay | identical world state |
| 10,000 generated chunks resident as `Dormant` | within memory budget |
| `no-erosion` profile | generates a valid world; differs from `full-sim` only in terrain shape |

## Notes

**The seam question gets answered here, visually.** Fine erosion detail cannot be perfectly continuous across block boundaries with a finite halo. Rivers should stay coherent because region-level drainage constrains them from above — verify that first, since it is the failure that would actually be noticeable. If hillside detail shows a visible seam, the mitigations in order of preference are a wider halo, fewer iterations with stronger per-iteration effect, or a post-pass seam blend.

The other thing to watch is generation latency. 20 s per block is tolerable behind a frontier and intolerable in front of one. If the frontier cannot keep up at realistic travel speeds, reduce block size before reducing erosion quality.
