---
id: S10
title: Agents & Navigation
status: partial (phase split and steering at M2)
depends_on: [S02, S05, S06, S09]
provides: [pathfinding, flow-fields, steering, behavior, agent-lod]
crates_touched: [cx-agents]
milestone: M6
---

# S10 — Agents & Navigation

Individuals that sense the world, decide, and act. Constrained by the read-then-write phase split in `02-architecture.md`: sensing and deciding produce intents, only `AgentAct` mutates.

## Requirements

- **Navigation is tiered by scale**, because one algorithm does not cover both a crowd crossing a plaza and a caravan crossing a continent:
  - *Local* (< 50 m): steering plus obstacle avoidance against the S05 spatial index.
  - *Chunk* (50 m – 1 km): flow fields over a downsampled cost grid derived from field data (slope, water depth, biome traversability). Flow fields amortize beautifully — one field serves thousands of agents heading to the same place.
  - *World* (> 1 km): A* over the region graph from the world map, refined per chunk on arrival.
- Cost grids derive from S06 fields, composed from separable components: slope (from terrain), water, biome, and construction (a structure overlay). Components invalidate independently, so demolishing a building never touches terrain-derived data.
- **Dirty granularity is the tile** (`ADR-0011`). A terrain edit recomputes the slope component for dirty tiles only, under 0.5 ms per tile. Flow fields whose footprint intersects a dirty tile invalidate lazily and rebuild on next use, not eagerly. Never per tick.
- **Behavior**: utility-based scoring over content-defined considerations (S04), not hardcoded state machines. A behavior evaluates a set of options against sensed values and returns an intent. Behavior trees are an acceptable alternative if utility scoring proves awkward — decide at M6 with a real content set, and record the choice as an ADR.
- **Agent LOD** (coupled to S09): `Full` agents run full behavior every tick; `Coarse` agents run strided behavior with simplified pathing; `Statistical` agents do not exist as entities.
- Sensing reads fields via `fields.sample` and neighbors via S05 queries. Both are read-only and parallel-safe.
- Intents are components written in `AgentDecide` and consumed in `AgentAct`. No agent system both reads a neighbor's state and writes shared state in one phase.
- Deterministic tiebreaking: when two agents contend for the same resource or cell, resolution is by a defined order (`Entity` id), never by iteration order.

## Non-goals

No learning or adaptive AI. No navmesh — the world is heightfield-and-grid shaped, so flow fields over cost grids fit better and rebuild far more cheaply under continuous terrain change.

## Acceptance criteria

- 100,000 `Full` agents with sense-decide-act under 15 ms on 8 threads.
- 1,000,000 mixed-tier agents within the 33 ms tick budget.
- Flow field for one chunk rebuilds under 3 ms.
- Two agents contending for one resource resolve identically across thread counts, over 10,000 ticks.
- Agents path continuously across chunk boundaries without visible hesitation at the seam.
- An agent's path is invalidated and replanned within one simulated second when a terrain edit, flood tier change, or construction blocks its next waypoint.

## Open questions

- Utility scoring vs. behavior trees. Deferred to M6; write an ADR when decided.
- Whether flow fields need to account for other agents as dynamic cost (crowd congestion). Adds a rebuild-per-tick cost; probably worth it only for dense colony scenarios.

## What is implemented

**The read-then-write split**, which is what the rest of S10 is built on:

| Phase | System | Reads | Writes |
|---|---|---|---|
| `AgentDecide` | `decide_steering` | the S05 index, transforms | its own `Intent` |
| `AgentAct` | `resolve_claims` | every `Intent` | `Claimable` holders |
| `AgentAct` | `apply_intents` | its own `Intent` | its own `Transform` |

Nothing reads another agent's intent, and nothing writes shared state before
every agent has decided. That is what makes the tick independent of which agent
the scheduler reached first — within a phase, order is unspecified
(`ADR-0001`), so a decision that depended on it would be a divergence rather
than an oddity.

Plus **local separation steering** — the bottom tier of S10's own navigation
ladder, and the only one needing no infrastructure that does not exist yet — and
**deterministic claim resolution**: a contested claim goes to the lower
`Entity`, never to whichever agent was reached first. Removing that tiebreak
fails a test, checked.

### It changed the S05 query API

`SpatialGrid`'s queries took `&mut self`, because the grid owned the result
buffer. That made a sensing system need `ResMut<SpatialIndex>`, which cannot run
in parallel with anything — the opposite of what S05 and S10 both require of
sensing.

`within_radius_into` now takes `&self` and writes into a caller-supplied buffer;
systems hold theirs as a `Local`, so nothing allocates per query and any number
of them can sense at once. The `&mut self` form remains as a convenience for
single-threaded callers and tests, implemented in terms of the new one.

This was found by the first consumer, one PR after the index landed.

### Still M6

Flow fields, A* over the region graph, cost grids derived from field data,
utility scoring versus behaviour trees, agent LOD, and path invalidation on
terrain edits. The acceptance criteria's figures — 100,000 `Full` agents under
15 ms, a chunk flow field under 3 ms — are not measured; there is no population
that size and no flow field to measure.
