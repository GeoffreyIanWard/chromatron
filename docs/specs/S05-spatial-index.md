---
id: S05
title: Spatial Index & Queries
status: not started
depends_on: [S01, S02]
provides: [spatial-hash, bvh, neighbor-queries, raycast, picking]
crates_touched: [cx-spatial]
milestone: M6
---

# S05 — Spatial Index & Queries

Answers "what is near here" for sparse entities. Dense field lookups do **not** go through this — they are O(1) array indexing (S06).

## Requirements

- **Uniform spatial hash** as the primary structure, cell size configurable per entity class (a `128 m` cell for buildings, `4 m` for creatures). Rebuilt in the `SpatialRebuild` phase from the previous tick's positions; rebuild is parallel by chunk and allocation-free after warmup.
- **Multiple indices**: agents, static structures, and query volumes are separate indices. One index with mixed scales performs badly for all of them.
- **BVH** for static geometry only, rebuilt on chunk activation rather than per tick.
- Query API: `nearest_k`, `within_radius`, `within_aabb`, `raycast`, `sweep`. All return borrowed slices from preallocated scratch buffers; no allocation per query.
- **Deterministic results**: query results are returned in a defined order (distance, then `Entity` id as tiebreak). Two runs must return identical orderings.
- Queries are read-only and safely parallel. Systems in the `AgentSense` phase may query freely; systems in `AgentAct` may not (the index is stale by then, and that staleness is intentional and documented).
- Coarse-to-fine: a query spanning multiple chunks must not force those chunks to activate. Queries into `Coarse` or `Dormant` chunks return aggregate answers from S09 rather than individual entities.

## Non-goals

No collision resolution (S11). No navigation (S10). No occlusion culling (S12 owns its own GPU-side culling).

## Acceptance criteria

- 1,000,000 agents indexed: full rebuild under 8 ms on 8 threads.
- 100,000 `within_radius` queries at radius 10 m against that index complete under 5 ms.
- Identical query result ordering across thread counts 1, 4, 16 over a 10,000-tick scenario.
- Zero allocations in the steady state, verified by an allocation-counting test harness.
- A `within_radius` spanning a `Dormant` chunk returns an aggregate result and does not trigger chunk activation.

## Open questions

- Whether a loose octree beats the uniform hash for the very sparse case (scattered structures across a large area). Benchmark both at M6 and keep the winner.
