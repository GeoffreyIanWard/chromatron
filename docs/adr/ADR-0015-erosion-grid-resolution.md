# ADR-0015 — Erosion runs on a coarse grid; the bake resamples

**Status:** accepted · **Date:** 2026-08-20 · **Amends:** `ADR-0006`, `ADR-0008` · **Affects:** `S07`, `bench/memory-budget.md`

## Context

`ADR-0008` moved erosion to generation time and `ADR-0006` raised generation granularity to the block to accommodate it. Neither said what *resolution* erosion runs at, and the implicit assumption was the field cell size — 0.5 m, the resolution `ELEVATION` is stored at.

That assumption does not survive arithmetic. A block is 8,192 m square with a 1,024 m halo on each side (`GENERATION_HALO_CHUNKS = 2`), so the eroded area is 10,240 m square:

| | |
|---|---|
| Erosion grid at 0.5 m | 20,480 × 20,480 = **419,430,400 cells** |
| Elevation alone (`f32`) | 1.56 GB |
| Working set — elevation, water, sediment, flow accumulation (`f32`), flow direction (`u8`) | **6.64 GB** |
| Budgeted in `bench/memory-budget.md` | 0.8 GB |

**8.3x over budget, for a single in-flight block**, before any frontier concurrency. Generation *time* is not the binding constraint — 200 iterations over that grid is 10–21 s on 8 threads against a 20 s target, tight but not impossible. Memory is.

## Decision

**Steps 2–5 of S07's pipeline run on a coarse grid of `EROSION_CELL_SIZE = 2 m`.** Depression fill, flow routing, hydraulic erosion, thermal erosion, and channel carving all operate at that resolution. Step 6 — the bake — resamples the eroded surface up to the 0.5 m field grid and re-adds the high-frequency positional detail that erosion does not govern.

| | |
|---|---|
| Erosion grid at 2 m, with halo | 5,120 × 5,120 = **26,214,400 cells** |
| Working set, same five fields | **0.42 GB** |
| Against the 0.8 GB budget | fits, with room for a second block in flight |
| 200 iterations, 8 threads | **~0.7 s** |

The block stays 8,192 m. Nothing about seam frequency changes, which matters because cross-block seams are the open question S07 asks M2 to answer visually, and shrinking the block would have made that question harder rather than resolving it.

## Rationale

**This is what the process is, not a concession.** Stream-power erosion describes how channels incise and hillslopes retreat. Those are phenomena at the scale of tens of metres. A 0.5 m erosion grid does not resolve finer erosion — it resolves the same landforms with sixteen times the samples per unit area and sixteen times the cost, and then hands the result to a bake that stores it at 0.5 m anyway. The extra samples carry no extra information about the physics.

The high-frequency detail that *is* wanted at 0.5 m — the texture of a hillside — comes from positional noise, which is already a pure function of coordinate and costs nothing to evaluate at any resolution. Erosion supplies the landform; noise supplies the surface. Separating them is what makes it affordable to have both.

**Determinism is unaffected.** The coarse grid is derived from `(world_seed, block_coord)` exactly as the fine one would be, and the resample is a pure function of the eroded grid. `ADR-0006`'s guarantee — generating block (3,1) before (0,0) yields identical output — holds unchanged, and the block cache key does not gain a field.

### Alternatives rejected

**Shrink the block to 2 km.** Reaches the same memory figure while keeping 0.5 m erosion, but multiplies the number of blocks by sixteen, and therefore the number of cross-block seams. S07 already records seam continuity as its open risk; sixteen times more of them is the wrong direction to move a question this milestone exists to answer. Catchments would also more often span several blocks, weakening the drainage coherence that `ADR-0006` relies on to keep rivers continuous.

**Stream row bands to disk.** Keeps 8 km blocks at 0.5 m by holding only a band of rows resident. But erosion is iterative and non-local — each of ~200 iterations needs a full sweep — so this converts a compute problem into hundreds of passes over disk. It is the most complex option and by a wide margin the slowest.

**Raise the budget to ~7 GB.** Spends most of a 16 GB machine on one in-flight block, leaving no room for the frontier to generate more than one at a time. The frontier's whole purpose is to stay ahead of the player, and serialising it to one block is not a tradeoff worth making to avoid a resample.

## Consequences

- `EROSION_CELL_SIZE` joins `CELL_SIZE` in `cx_core::math` as a named world constant. Two grids now exist and code has to be explicit about which one it is on; the type system carries this rather than a comment.
- The bake (step 6) becomes a real stage with its own correctness question — the resample must not introduce terracing at coarse-cell boundaries, and the re-added detail must not fill in channels the erosion cut. Both are visible, and both get checked by looking at output as well as by assertion.
- Channel width from discharge (step 5) is now quantised to 2 m at carve time. Channels narrower than that are represented by depth rather than width, which is what the hydraulic geometry relations in S08 would do at this scale anyway.
- `bench/memory-budget.md`'s 0.8 GB line becomes achievable rather than aspirational, and is the constraint that sets frontier concurrency.
- If a future world preset wants finer landforms, `EROSION_CELL_SIZE` is the knob — and the memory cost of turning it is quadratic and now written down.
