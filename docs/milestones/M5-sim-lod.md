---
id: M5
title: Simulation LOD & Fast-Forward
specs: [S09]
gate: bench/baselines.md#m5
---

# M5 — Simulation LOD & Fast-Forward

The milestone that decouples cost from world size. Until this lands, every chunk you generate makes the tick slower forever.

## Deliverables

- Four-tier system: `Full`, `Coarse`, `Statistical`, `Dormant`, assigned per chunk and per entity.
- Tier policy driven by interest points, time multiplier, global budget, and explicit pins.
- Aggregation and disaggregation with conservative totals and positional-RNG repopulation.
- `Persistent` entity exemption from aggregation.
- `fast_forward(state, n_ticks)` for every solver and aggregate system, with a capped horizon. Simpler than originally scoped: no continuous process changes terrain and infinite water has no volume, so only ecology, soil moisture, finite bodies, and population aggregates advance. Chunks with terrain edits (S19) apply their delta before fast-forward runs.
- Background fast-forward for large N, spread across frames.
- Per-system work budgets with round-robin deferral and overrun diagnostics.
- Time acceleration wired to tier demotion (S03 coupling).

## Exit criteria

| Check | Target |
|---|---|
| 10,000 generated chunks, 16 active, vs 16 chunks total | tick time within 20% |
| Aggregate → disaggregate round trip | population and resource totals exact |
| Same `Statistical` state promoted twice | identical entities |
| `Persistent` entity through aggregation round trip | byte-identical |
| 10,000x time acceleration | sustains real-time frame pacing |
| Fast-forward 1,000,000 ticks, one chunk | < 500 ms |
| Active 100k ticks vs dormant-then-fast-forwarded | biomass + soil moisture within 5% |

## Notes

The 5% agreement criterion is the one that determines whether the world feels coherent. If a region you left for a simulated decade looks nothing like a region you watched for a decade, players will notice, and the fix is in the coarse solver variants from M4 rather than here. The absence of continuous terrain change helps a great deal — the landscape is guaranteed identical either way, so only vegetation and moisture can diverge.
