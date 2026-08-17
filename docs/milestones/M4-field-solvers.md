---
id: M4
title: Climate, Hydrology & Ecology
specs: [S08]
gate: bench/baselines.md#m4
---

# M4 — Climate, Hydrology & Ecology

The world starts behaving. Rain falls, rivers swell and flood, forests grow — none of it via fluid simulation.

## Deliverables

- Climate solver driven by the world-map seasonal model, with lapse rate and rain shadow from precomputed generation fields, plus storm-front region entities.
- **Flow network hydrology** (`ADR-0009`): discharge per edge from upstream catchment × effective precipitation, with routing lag; channel width, depth, and velocity from hydraulic geometry power laws; flood extent by lookup into precomputed per-tier floodplain masks.
- **Water body classification**: infinite bodies with surface levels and extent masks; finite bodies as volume-tracking entities with fill, drain, overflow, and promotion.
- `WATER_DEPTH` as a derived field, refreshed only on level or discharge-tier change.
- Ecology: biomass growth, competition, spread stencil.
- Soil moisture: diffusion stencil, infiltration, evaporation, plant uptake.
- Content-defined parameters for all of the above (S04).
- Water rendering against static levels and precomputed flow directions (S12).

## Exit criteria

| Check | Target |
|---|---|
| Upstream precipitation event → downstream discharge response | plausible routing lag over 100 km |
| Flood extent lookup on tier change | < 1 ms per affected chunk |
| Water surface continuity across chunk and block seams | verified by seam sampling |
| Finite body fill → overflow → drain cycle | within declared tolerances |
| Finite body crossing the infinite threshold | promotes, stops volume tracking |
| Ecology + soil moisture stencils, 16M cells | within 33 ms tick budget |
| No NaN in any field, 1,000,000 ticks | zero |

## Notes

This milestone got substantially cheaper. The original design had a pipe-model fluid solver as a per-tick 16M-cell stencil with mass conservation to 1e-4 over a million ticks as the hardest criterion in the plan. That is gone. Hydrology is now a graph update over a few thousand edges.

The tuning risk moved rather than disappearing: the infinite/finite threshold is now the knob that decides whether the world feels right. Set it too low and every pond is inert scenery; too high and volume tracking gets expensive. Budget real time for tuning it against a populated world, not a test scene.
