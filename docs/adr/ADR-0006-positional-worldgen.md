# ADR-0006 — Generation is positional, never sequential

**Status:** accepted · **Date:** 2026-08-16 · **Amended:** 2026-08-16 (granularity raised to the block, per `ADR-0008`)

## Context

An effectively infinite world generates terrain on demand, in an order determined by where the player goes. If generation consumes a sequential RNG, the same coordinate produces different terrain depending on the route taken to reach it — which breaks reproducibility, replays, and the delta-persistence scheme in S13.

## Decision

Generation output depends only on the world seed and the coordinate, never on generation order.

**Granularity is the block**, not the chunk. A block is 16×16 chunks (8,192 m), generated as a unit from `hash(world_seed, block_coord, ...)`. Chunks are extracted from generated blocks by slicing, with no computation of their own.

The block is the unit rather than the chunk because two generation stages are inherently non-local:

- **Drainage routing** — a river's existence at a point depends on upstream catchment area, which is not locally knowable.
- **Erosion** (`ADR-0008`) — iterative and non-local; a cell's final height depends on its neighbors over hundreds of iterations.

Blocks are generated with a **halo margin** of 2 chunks, eroded and routed along with the block, then discarded. Region-level drainage from the world map constrains the flow network from above, keeping rivers coherent across block boundaries.

## Rationale

Positional determinism is what makes "an unmodified chunk costs zero bytes" true: terrain is a pure function of seed and coordinate, so it need not be stored at all. Without it, every visited chunk must be persisted forever, and an infinite world becomes an unbounded save file.

Raising granularity from chunk to block preserves that property while admitting the two non-local stages. The cost is that generation is now expensive per unit, which S07 handles with a background frontier and a disposable cache.

## Consequences

- Generation algorithms must be expressible as position-indexed functions at block granularity. Techniques relying on sequential state — droplet erosion in particular — are excluded; grid-based equivalents are used instead.
- Blocks are trivially parallel to generate, since no two share state.
- Fine erosion detail cannot be perfectly continuous across block seams with a finite halo. Open question in S07, resolved visually at M2.
- The block cache is disposable: deleting it costs regeneration time and nothing else, because regeneration is guaranteed to reproduce identical output.
