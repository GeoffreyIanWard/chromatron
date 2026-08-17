---
id: S09
title: Simulation LOD & Fast-Forward
status: not started
depends_on: [S02, S06, S07, S08]
provides: [lod-tiers, aggregation, budgeting, fast-forward, catch-up]
crates_touched: [cx-lod]
milestone: M5
---

# S09 — Simulation LOD & Fast-Forward

The spec that makes an infinite, persistently-simulated world tractable. Without it, cost grows with world size rather than with what the player can see, and 10,000x time acceleration is impossible.

## Requirements

### Tiers

| Tier | Entities | Fields | Cost |
|---|---|---|---|
| `Full` | Individual, every tick | Fine resolution, every tick | O(n) |
| `Coarse` | Individual, strided (1 in N ticks) | 8x downsampled | O(n/N) |
| `Statistical` | Replaced by population aggregates | Region-level only | O(1) per chunk |
| `Dormant` | Frozen | Frozen | 0 |

- Tier is assigned per chunk, and per entity within a chunk (a tracked agent can stay `Full` inside a `Coarse` chunk).
- Tier assignment inputs: distance to nearest interest point, time multiplier from S03, global budget, and explicit pins from script or gameplay.
- **At high time multipliers, tiers demote globally.** Running at 1000x does not mean running 1000 ticks — it means most of the world drops to `Statistical` and the tick advances by a larger simulated interval. This is the mechanism that makes acceleration possible; document it prominently because it is counterintuitive.

### Aggregation and disaggregation

- `Statistical` chunks hold population vectors (counts by prototype, mean age, mean condition, resource totals) rather than entities.
- **Aggregate** on demotion: fold entities into the vector, despawn them, record enough state that promotion is plausible.
- **Disaggregate** on promotion: spawn entities from the vector using a positional RNG stream so the same chunk promoted twice yields the same population.
- Aggregation must be *conservative* for tracked quantities: total food, total population, total water. Entities may lose individual identity; totals may not drift.
- Entities marked `Persistent` (named characters, player property) are never aggregated — they are serialized individually and restored exactly.

### Fast-forward

- On promotion from `Dormant`, a chunk owes N ticks of simulation. It does **not** replay them.
- Each solver and each aggregate system provides `fast_forward(state, n_ticks) -> state`, using closed-form or coarse-stepped approximation. Accuracy target is qualitative plausibility, not exactness — see S08's 5% agreement criterion.
- Fast-forward is materially simpler than originally planned: no continuous process changes terrain (`ADR-0008`) and infinite water has no volume to integrate (`ADR-0009`), so only ecology, soil moisture, finite bodies, and population aggregates need advancing.
- A chunk with terrain edits (S19) must have its delta applied on rehydration **before** fast-forward runs, so the advanced state is computed against the edited terrain rather than the generated terrain.
- Fast-forward is capped: beyond a configured horizon, additional elapsed time produces no further change (a forest reaches climax and stops). This prevents both unbounded computation and absurd results.
- Fast-forward runs on a background pool over multiple frames for large N, with the chunk remaining unrenderable until complete.

### Budgeting

- Every system that can be budgeted declares a per-tick work quota. Over-quota work defers to the next tick via a round-robin cursor, so cost is bounded rather than proportional to entity count.
- Budget overruns report to S14 rather than silently stretching the tick.

## Non-goals

No automatic LOD tier inference — tier policy is explicit and content-configured. Automatic heuristics here produce unpredictable simulation behavior, which is worse than a tunable knob.

## Acceptance criteria

- World with 10,000 generated chunks, 16 `Active`: tick time is within 20% of the same scenario with only 16 chunks existing at all.
- 1,000,000 agents across mixed tiers: tick under 33 ms.
- Chunk aggregated then disaggregated conserves population and resource totals exactly.
- The same chunk promoted twice from identical `Statistical` state produces identical entities.
- A `Persistent` entity survives aggregation round-trip byte-identical.
- 10,000x time acceleration sustains real-time frame pacing.
- Fast-forward of 1,000,000 ticks completes in under 500 ms per chunk.

## Open questions

- Whether `Coarse` is worth its complexity or whether `Full` → `Statistical` is a sufficient two-tier system. Build all four but measure whether `Coarse` earns its keep at M5.
