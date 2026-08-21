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
| Single block generation (full pipeline; erosion at 5,120² per `ADR-0015`, bake at 16,384²) | < 20 s, 8 background threads |
| Chunk extraction from cached block | < 5 ms |
| Terrain mesh bake, one chunk | < 200 ms offline |
| Flow continuity walk over 100 km of channel | unbroken across chunk *and* block seams |
| Camera traversal at 200 m/s | frontier never outrun, no frame > 20 ms |
| Delete block cache, replay | identical world state |
| 10,000 generated chunks resident as `Dormant` | within memory budget |
| `no-erosion` profile | generates a valid world; differs from `full-sim` only in terrain shape |

## Resolved before starting: the erosion grid

M2's first arithmetic check found that a block's erosion working set at 0.5 m is **6.64 GB
against the 0.8 GB** `bench/memory-budget.md` budgets — 8.3x over, for one in-flight block,
before any frontier concurrency. Generation *time* was not the binding constraint (200
iterations is 10–21 s on 8 threads against a 20 s target); memory was.

`ADR-0015` settles it: **steps 2–5 run on a 2 m grid**, 5,120² with halo and 0.42 GB, and
step 6 resamples to the 0.5 m field grid. The block stays 8,192 m, so seam frequency — the
question this milestone is meant to answer visually — is unchanged. Erosion supplies the
landform and positional noise supplies the sub-2 m surface texture, which is what makes it
affordable to have both.

## Progress

- **The block grid** — `cx_worldgen::block`. Coordinates, halo indexing, the erosion grid, and
  base elevation over a whole block. A full block fills in 680 ms single-threaded at 100 MB.
  A block's halo holds cell-for-cell the same terrain its neighbour computes as core, checked
  by walking the whole seam, and four adjacent blocks were rendered and looked at with no
  visible seam.

- **Step 2, the flow network** — `cx_worldgen::flow`. Priority-flood fill, D8 routing, and
  flow accumulation over a block in 3.3 s single-threaded; the largest channel carries 30.8%
  of the block. Three separate wrong versions preceded it, and the one that assertions could
  not distinguish from the right answer was caught by rendering the network and looking at it.
  See S07's "what is implemented" for what each cost.

- **Step 3, hydraulic erosion** — `cx_worldgen::hydraulic`. Implicit stream power, 12 rounds
  over a block in 89 s single-threaded, 22 m mean lowering, zero sinks. Carries a recorded and
  diagnosed artifact: D8 grid bias printing a herringbone into the eroded surface. See S07.

- **Step 4, thermal erosion** — `cx_worldgen::thermal`. Talus-angle relaxation, mass-conserving,
  read-then-write for order independence.
- **The grid-bias artifact is diagnosed**, and it is not an erosion bug. Three candidate fixes
  were tried and falsified; the cause is filled basins in scale-free noise, and the fix is the
  **world map**. That makes the world map the next thing to build rather than a later
  deliverable. See S07.

- **The world map** — `cx_worldgen::worldmap`. Continental elevation and uplift, positional
  rather than stored. Basin fraction 32.4% to 26.7%; the artifact is much improved but not
  gone, because the gradient is near zero at the continental surface's own ridges. See S07.

- **The grid-bias artifact is resolved.** Four hypotheses were falsified before the cause was
  found — and the fix was not the world map either, though that helped. Flat resolution's
  tie-break was smooth in position, so equal-distance contours (straight diagonals on a grid)
  all drained the same way. It is a coordinate hash now: inside a lake the direction is
  genuinely arbitrary, and an arbitrary choice leaves no pattern to carve.

- **Step 5, channel carving** — `cx_worldgen::carve`. S08's hydraulic geometry cut into the
  eroded surface; 8 s per block, deepest 11.1 m, zero sinks after re-routing.
- **Steps 1–5 now run end to end** at roughly 130 s single-threaded per block, against a 20 s
  target on 8 threads. Nothing is parallelised yet and erosion's re-routing dominates.

- **Step 6, the bake** — `cx_worldgen::bake`. Catmull-Rom resample from 2 m to 0.5 m plus
  drainage-faded detail, closing out `ADR-0015`. Both of the ADR's named correctness
  questions are now tested, and both tests had to be strengthened after falsification
  showed they passed against the exact artifacts they existed to exclude.

## Notes

**The seam question gets answered here, visually.** Fine erosion detail cannot be perfectly continuous across block boundaries with a finite halo. Rivers should stay coherent because region-level drainage constrains them from above — verify that first, since it is the failure that would actually be noticeable. If hillside detail shows a visible seam, the mitigations in order of preference are a wider halo, fewer iterations with stronger per-iteration effect, or a post-pass seam blend.

The other thing to watch is generation latency. 20 s per block is tolerable behind a frontier and intolerable in front of one. If the frontier cannot keep up at realistic travel speeds, reduce block size before reducing erosion quality.
