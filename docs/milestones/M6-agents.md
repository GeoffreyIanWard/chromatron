---
id: M6
title: Agents & Navigation
specs: [S05, S10]
gate: bench/baselines.md#m6
---

# M6 — Agents & Navigation

Individuals arrive, into a world that already exists and already changes. This ordering is deliberate: agents built on top of a working world are far easier than a world retrofitted under existing agents.

## Deliverables

- Spatial hash indices per entity class, parallel rebuild, allocation-free queries.
- Static BVH rebuilt on chunk activation.
- Query API with deterministic result ordering; coarse answers for non-active chunks.
- Three-tier navigation: local steering, chunk flow fields, world-graph A*.
- Cost grids derived from field data. The slope component is baked at generation and never changes; only water, biome, and construction components invalidate a grid.
- Utility-based behavior over content-defined considerations (or behavior trees — decide here, write the ADR).
- Agent LOD coupled to S09 tiers.
- Deterministic contention tiebreaking.

## Exit criteria

| Check | Target |
|---|---|
| 1M agents, spatial index full rebuild | < 8 ms on 8 threads |
| 100k `within_radius` queries at 10 m | < 5 ms |
| 100k `Full` agents, sense-decide-act | < 15 ms on 8 threads |
| 1M mixed-tier agents | within 33 ms tick budget |
| Flow field rebuild, one chunk | < 3 ms |
| Query ordering across thread counts, 10,000 ticks | identical |
| Two agents contending for one resource | identical resolution across thread counts |
| Path crossing chunk boundaries | no visible hesitation |
| Path blocked by flood tier change or construction | replanned within 1 simulated second |
