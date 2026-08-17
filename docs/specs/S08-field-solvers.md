---
id: S08
title: Climate, Hydrology & Ecology
status: not started
depends_on: [S06, S07]
provides: [climate, flow-network, water-bodies, flooding, soil-moisture, biomass]
crates_touched: [cx-solvers]
milestone: M4
---

# S08 — Climate, Hydrology & Ecology

Runtime world processes. Erosion is **not** here — it happens once at generation (`ADR-0008`). Water is **not** simulated as a fluid — it is a flow network plus classified bodies (`ADR-0009`).

## Solver order (fixed)

1. **Climate** — temperature and precipitation advance from the world map's seasonal model plus local perturbation.
2. **Hydrology** — discharge update over the flow network; water body levels; flood extent lookup; infiltration and evaporation into soil moisture.
3. **Ecology** — biomass growth from moisture, temperature, and light; competition; spread; consumption deposits from agents.

## Water model (`ADR-0009`)

Water bodies are classified at generation into two kinds, and they are handled by completely different machinery.

### Infinite bodies — oceans, lakes, main rivers

Anything above a threshold extent or discharge is **infinite**: it has a *surface level*, not a volume. It never drains, never fills, and is never mass-balanced.

- Standing infinite bodies (ocean, large lake): a surface elevation and an extent mask, both computed at generation. `WATER_DEPTH` is derived as `max(0, surface_level - elevation)` — a lookup, not a state.
- Flowing infinite bodies (rivers): edges in the **flow network**, a graph derived from the generation-time drainage routing. Each edge carries a **discharge** (m³/s) computed as upstream catchment area × current effective precipitation, with a routing lag so a storm upstream arrives downstream some ticks later.
- Channel geometry from discharge by standard hydraulic geometry power laws — width ∝ Q^0.5, depth ∝ Q^0.4, velocity ∝ Q^0.1. Constants are content-defined.
- **Flooding** is a lookup, not a solve. Generation precomputes floodplain masks per discharge tier; when discharge crosses a tier boundary, the flood extent for that tier is applied. Tier changes are the only event that dirties the derived `WATER_DEPTH` field.

### Finite bodies — puddles, ponds, cisterns, reservoirs

Small contained water **does** track volume — but there are few of these, so they are **entities**, not fields. This maps cleanly onto the sparse/dense split (`ADR-0003`): infinite water is static dense data, finite water is sparse dynamic data.

- A finite body entity holds volume, a container reference (a natural depression, an excavated pit, or a player-built vessel), inflow sources, and outflow rules.
- It fills from precipitation and inflow, drains by evaporation, infiltration, and overflow into the flow network.
- **Promotion**: a finite body exceeding the infinite threshold is promoted — it gains a surface level and stops being volume-tracked. Demotion does not happen automatically; only an explicit gameplay event (draining a reservoir) demotes a body.
- **Impoundments** (`ADR-0011`): water backed up behind a terrain edit that blocks flow fills to its **spill elevation** — the lowest escape point of the basin, from a bounded flood-fill over terrain — and no further. Overflow continues downstream at the original discharge. The level is a pure function of geometry; no volume is integrated.

### What this removes

Mass conservation as a hard global invariant, the pipe-model stencil, sediment transport at runtime, CFL stability analysis for water, and the f32-drift-over-10^6-ticks concern. Hydrology drops from a 16M-cell per-tick stencil to a graph update over a few thousand edges plus an occasional derived-field refresh.

## Response to terrain edits

The flow network is generated at world generation but is **not frozen**. When an `EditCommand` (S19) changes flow topology — a dam, a canal, an excavation below a water surface — the `TerrainEdit` phase triggers **incremental drainage repair**: re-run depression fill and flow routing over the affected neighborhood only, expanding outward until the result stabilizes against untouched terrain, capped at one block. Never global.

Flow network modifications persist as graph deltas alongside terrain deltas (S13).

## Climate

- Seasonal temperature and precipitation curves from the world map, per region, advanced by tick.
- Local perturbation: lapse rate by elevation, rain shadow by aspect (both precomputed at generation), plus a positional noise term for weather variability.
- Weather events (storm fronts) are region-level entities that modulate precipitation over an area — sparse, not a fluid simulation.

## Ecology

- `BIOMASS` grows as a function of `SOIL_MOISTURE`, `TEMPERATURE`, light, and biome carrying capacity.
- Spread to neighbors is a stencil — this and soil-moisture diffusion are now the primary dense-field workloads.
- Competition between plant types resolves by a deterministic priority rule, never iteration order.
- Consumption from agents arrives via the deposit buffer (S06) and is applied before growth.

## Soil moisture

- Dense field. Diffusion stencil, plus infiltration from precipitation and from adjacent water bodies, minus evaporation and plant uptake.
- Bounded 0–1 and quantized to `u16`; no conservation invariant needed since it is a saturation fraction, not a mass.

## Non-goals

No *continuous* runtime erosion or sedimentation. Event-triggered local erosion (dam break, slope failure) is available via S19 behind a flag. No groundwater aquifers. No fluid dynamics of any kind. No glaciers. No atmospheric circulation — climate is driven from the world-map model.

## Acceptance criteria

- Discharge responds to an upstream precipitation event with a plausible routing lag, verified over a 100 km channel.
- Flood extent lookup on a tier change completes in under 1 ms per affected chunk.
- Water surface is continuous across chunk and block boundaries, verified by a sampling test along seams.
- A finite body fills from rain, overflows into the flow network, and drains by evaporation, all within declared tolerances.
- A finite body crossing the infinite threshold promotes correctly and stops being volume-tracked.
- Ecology and soil-moisture stencils over 16M cells complete within the 33 ms tick budget.
- No NaN in any field over 1,000,000 ticks.
- A region simulated 100k ticks `Active` and the same region `Dormant` then fast-forwarded agree on aggregate biomass and soil moisture within 5%.

## Open questions

- The infinite/finite threshold. Too low and every pond is static; too high and volume tracking gets expensive. Needs tuning at M4 against a real world.
- Whether discharge routing lag needs a proper kinematic wave or whether a fixed per-edge delay is enough. Start with fixed delay; upgrade only if flood timing looks wrong.
