---
id: S07
title: Worldgen, Erosion & Chunk Lifecycle
status: partial (base elevation at M2)
depends_on: [S01, S06]
provides: [world-map, block-generation, erosion, flow-network, biomes, chunk-states, streaming]
crates_touched: [cx-worldgen]
milestone: M2
---

# S07 — Worldgen, Erosion & Chunk Lifecycle

All *process-driven* terrain shaping happens here, once, at generation time (`ADR-0008`). After a block is generated, **no continuous process modifies its elevation** — which is what simplifies rendering, physics, navigation, and persistence.

This is not the same as read-only terrain. Discrete, local, player- and gameplay-driven edits are fully supported and are specified in **S19** (`ADR-0011`). The baked output of this pipeline is what the untouched world uses; edits patch it locally.

## Generation granularity: the block

Erosion is iterative and non-local — a cell's final height depends on its neighbors over hundreds of iterations. It cannot be expressed as a pure function of a single cell's coordinate. So generation happens at **block** granularity (`ADR-0006` as amended):

- A **block** is 16×16 chunks = 8,192 m square, 16,384² cells at the 0.5 m field resolution.
- **Erosion runs on a coarser grid**: 2 m, so 4,096² per block and 5,120² with the halo
  (`ADR-0015`). At 0.5 m the erosion working set would be 6.64 GB against a 0.8 GB budget.
  Stream-power erosion describes channels and hillslopes — tens of metres across — so the
  finer grid buys sixteen times the samples and no more information about the process.
- A block is generated with a **halo margin** of 2 chunks on each side, which is eroded along with the block and then discarded.
- Blocks are positionally deterministic: `hash(world_seed, block_coord, ...)`. Generating block (3,1) before (0,0) produces identical output to the reverse.
- **Chunks are pure extraction** from a generated block — slicing, no computation.

## Generation pipeline

Run per block, on a background pool:

1. **Base elevation** — positional noise octaves interpolated against the world map's region elevation and uplift fields.
2. **Depression fill + flow routing** at block resolution (priority-flood, then D8 or D-infinity).
3. **Hydraulic erosion** — grid-based stream-power erosion, N iterations. Grid-based, not droplet-based: droplet erosion needs a sequential RNG and is therefore order-dependent, which `ADR-0006` forbids. Grid-based is deterministic and parallelizes by row band.
4. **Thermal erosion** — talus angle relaxation for scree slopes and cliff bases.
5. **Channel carving** — incise the flow network into the eroded surface, with width and depth from discharge (see S08's hydraulic geometry relations).
6. **Discard halo, bake elevation** — resample the eroded 2 m surface up to the 0.5 m field grid and re-add the high-frequency positional detail erosion does not govern (`ADR-0015`). Erosion supplies the landform; noise supplies the surface.
   `ELEVATION` is final as far as *generation* is concerned; from here it changes only via `EditCommand` (S19).
7. **Derive static fields** — slope, aspect, flow direction, flow accumulation, floodplain masks per discharge tier, traversability, water body extents and surface levels.
8. **Biome assignment** — content-defined lookup over temperature, precipitation, elevation, slope, drainage.
9. **Scatter placement** — vegetation, resources, features. Positional per chunk, cheap.

Steps 1–7 are block-level and expensive. Steps 8–9 are chunk-level and cheap, so biome and scatter parameters can change without regenerating terrain.

## Background generation and the block cache

- A **generation frontier** keeps blocks generated ahead of interest points, sized so the player cannot outrun it at maximum travel speed. Generation runs on a background pool, never on the tick thread.
- Generated blocks are written to a **local block cache** on disk. The cache is *disposable and not part of the save* — deleting it costs regeneration time and nothing else. This preserves the save-size property (`S13`) while avoiding repeated multi-second regeneration.
- Cache entries are keyed by `(world_seed, block_coord, generator_version)`. A generator change invalidates the cache without touching saves.
- If the frontier is outrun (teleport, debug fly), the game shows a generation progress state rather than stalling the tick.

## Chunk lifecycle

```
Ungenerated ──(block gen)──► Generated ──activate──► Active
                                 ▲                     │
                                 │                  demote
                             rehydrate                 ▼
                                 │                   Coarse
                                 │                     │
                                 │                  demote
                                 │                     ▼
                                 └────────────────── Dormant ──► (delta on disk)
```

- Transitions are driven by distance from interest points and a global `Active` cap, amortized to at most N per tick.
- Because no continuous process modifies terrain, promotion requires terrain rework only where edits exist. An unedited chunk promotes with its baked mesh and collider straight from cache; an edited chunk applies its sparse delta first (S19).

## Non-goals

No runtime terrain deformation from *natural processes* — no continuous erosion. Player- and gameplay-driven terrain editing is fully supported and is a first-class subsystem; see **S19** and `ADR-0011`. The generation erosion kernel here is also callable on a bounded region for event-triggered effects (slope failure, dam break), which S19 owns.

Not a terrain editor. No live worldgen parameter tweaking with in-place regeneration; dev-only regeneration on seed change is sufficient.

## Acceptance criteria

- A 4×4 block area generated in two different orders produces identical field hashes.
- Single block generation (16,384², full pipeline) completes in under 20 s on 8 background threads.
- Chunk extraction from a cached block is under 5 ms.
- Rivers are continuous across chunk boundaries and across block boundaries, verified by a flow-continuity walk over 100 km of channel.
- Camera traversal at 200 m/s never outruns the generation frontier, and no frame exceeds 20 ms.
- Deleting the block cache and replaying produces identical world state.
- 10,000 generated chunks resident as `Dormant` stay within the memory budget.

## Open questions

- **Halo width vs. erosion iteration count.** The influence radius of erosion grows with iterations; a 2-chunk halo bounds it only up to some iteration count. Beyond that, fine erosion detail cannot be perfectly continuous across block seams. Rivers stay coherent because the region-level drainage network constrains them from above, but hillside detail may show a faint seam. This needs a visual check at M2 — the mitigations, in order of preference, are a wider halo, fewer iterations with stronger per-iteration effect, or a post-pass seam blend.
- Block size. 8 km is a guess balancing generation latency against seam frequency. Measure at M2.
  `ADR-0015` deliberately kept it at 8 km rather than shrinking it to solve the memory problem,
  because shrinking would have multiplied the seams this milestone exists to evaluate.

## What is implemented

**The block grid.** `cx_worldgen::block` is the surface steps 2–5 operate on: a
5,120² erosion grid over one block and its halo, indexed by an `ErosionCell`
that cannot be built out of range. Halo cells are indexed like core cells with
the origin at the halo's corner, so a stage sweeping the buffer needs no
boundary special case — edge handling is where iterative solvers go wrong.

A full block fills in **680 ms single-threaded at 100 MB** for base elevation,
and a block's halo was verified to hold cell-for-cell the same heights its
neighbour computes as core. Four adjacent blocks were also rendered as a shaded
heightmap and looked at: no seam is visible, which is what the halo arithmetic
being right looks like.

**Step 3: hydraulic erosion.** `cx_worldgen::hydraulic` — implicit stream-power
incision (Braun & Willett 2013), one closed-form pass per round over the drainage
order, with a re-route between rounds. Twelve rounds over a whole block take
**89 s** single-threaded and remove a mean of **22 m**, deepest 69 m, leaving
zero interior sinks.

Implicit rather than explicit deliberately. Explicit stream power is stable only
below a timestep that shrinks as drainage area grows, and area here spans 1 to 8
million cells — so the stable step would be set by the largest river and every
hillside integrated thousands of times more finely than it needs. The implicit
update is a weighted average of a cell and its receiver, so the result is always
between them and the timestep becomes a shaping knob rather than a stability
constraint. A 1,000x timestep flattens the landscape instead of tearing it apart,
and that is asserted rather than argued.

**Known artifact: D8 grid bias.** Erosion incises towards one receiver and D8
offers eight, so grooves snap to multiples of 45 degrees and the incision feedback
compounds the bias over rounds. An eroded block renders with a herringbone texture
and channels in hard diagonal segments. Flow accumulation was changed to split
across all downslope neighbours to address it; that improved the channel network
and left the surface striping unchanged, so the cause is the single-receiver
incision, not the area term. Thermal erosion (step 4) relaxes over-steep slopes
and is the next thing to measure against it; multi-receiver incision is the
fallback. Every assertion in the module passes against the biased surface, which
is why this is recorded from a render rather than from a test.

**Step 2: depression fill and flow routing.** `cx_worldgen::flow` — priority-flood
filling, D8 direction, and flow accumulation, over a whole block in **3.3 s**
single-threaded. The largest channel on a test block carries **30.8% of the block**,
which is a drainage network rather than a scatter of puddles.

Three things this cost, all of which only a picture or a count would have caught:

- **Flats need real resolution, not "+epsilon".** Raising each filled cell a hair
  above whichever cell filled it removes every pit and leaves zero sinks — and
  produces drainage that follows the *fill's own search order*. Rendered, that is
  straight 45-degree fans across every basin. Flats are resolved with the two-sweep
  Garbrecht–Martz scheme instead.
- **The resolution must be a tie-break, not a nudge to elevation.** Adding the flat
  gradient to heights pushed flat cells above genuinely lower ground nearby —
  adjacent noise cells can differ by less than the smallest usable step — inverting
  real slopes. The largest channel fell from 31% of the block to under 2%.
- **The two flat gradients cannot be summed as peers.** Distance-to-outlet is
  monotonic by BFS construction; distance-from-higher is not, and their sum has
  interior local minima. That left 618,140 interior sinks. Outlet distance is the
  primary key and the other only breaks its ties.

Filled basins still drain in parallel combs, because a flat lake surface has no
intrinsic drainage direction. What matters is that flow crosses them and collects
at the outlet, which it does.

**Step 1: base elevation.** `cx_worldgen::ElevationGenerator` is a pure
function of `(world_seed, position)` — value noise from a positional hash, with
no permutation table and no initialisation, so there is nothing to get out of
sync between a generation run and a later regeneration of the same block
(`ADR-0006`).

`WorldgenModule` declares it: provides `terrain`, **requires** `fields`, owns
`ELEVATION`, and declares `generate_elevation` as a writer of it. That makes it
the first module with a dependency, and the first entry in S21's field-access
layer.

Steps 4–9 remain: thermal erosion, channel carving, the bake, static field
derivation, biome assignment, and scatter.

**The terrain therefore looks smooth, and should.** A plausible-looking
placeholder would have hidden exactly the difference erosion makes, which is the
one thing worth being able to see when it lands.

`ELEVATION` is registered `DeltaPersisted` with a one-cell halo and tile dirty
tracking, per `ADR-0011` — an untouched chunk costs zero bytes because it can be
regenerated from the seed.
