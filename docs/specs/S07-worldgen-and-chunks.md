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

**The world map.** `cx_worldgen::worldmap` — continental elevation and uplift as very
long wavelength positional noise (64 km, 1,400 m relief, ~44 m/km typical gradient). Not a
stored grid: the world is effectively infinite, so there is no array to hold and no global
drainage to route across it. What the pipeline actually needs is that a block has somewhere
downhill to send its water, and a regional gradient several times block-scale relief supplies
that everywhere at once.

Built out of order because the grid-bias artifact below made it the priority. It **helps but
does not fully resolve it**: filled basin drops from 32.4% to 26.7% of a block, and the
rendered terrain is dendritic over much of its area with striped patches remaining. The
reason is that a *typical* gradient of 44 m/km is near zero at the continental surface's own
ridges and troughs, and those flat zones pond exactly as before. The lever is the **minimum**
gradient, not the typical one — ridged noise, which has no flat tops, is the obvious next
thing to try.

`WorldMapSettings::typical_gradient` exists so the tuning trade is a number rather than an
impression, and it is gated: the default must clear the 40 m/km that was measured to remove
the artifact.

Adding the world map broke three tests that had assumed terrain sat near zero, and each
needed its claim restated rather than its threshold widened. `TerrainShape::flat` also stopped
meaning flat — it removes a block's *local* relief and leaves the continental surface in
place — so `ElevationGenerator::flat` and `Worldgen::flat` now exist for callers that mean it.
A fixture asking for flat ground and silently getting a continental slope is a trap.

**The grid-bias artifact, resolved.** The herringbone is gone. The fix was neither
of the two candidates recorded below, and not ridged noise either — ridged noise is
`1-|2n-1|`, so by the chain rule its gradient is `±2·dn/dx` and it is still flat
wherever the underlying noise is. It moves the flat spots rather than removing them,
and *any* smooth field has critical points.

Basins are therefore not eliminable, and should not be: real worlds have endorheic
basins. What was fixable is the **geometric drainage inside them**. Flat resolution
ordered cells by `(distance to outlet, distance from higher ground)`, and both terms
are smooth in position — so every cell of an equal-distance contour, which on a
regular grid is a straight diagonal line, was sent the same way. That is where the
parallel combs came from, and erosion carved them.

Inside a filled flat the drainage direction is **genuinely arbitrary**: a lake surface
has no slope to follow. So the second term is now a hash of the cell's own coordinate.
That is not an approximation of something better — it is the honest representation of
a free choice, and unlike a smooth tie-break it leaves no pattern for erosion to find.
Deterministic per `ADR-0006`, since the hash is a pure function of the coordinate.

**The history, kept.** Eroding bare noise produced a herringbone
over every hillside and channels in hard 45-degree runs. Three fixes were tried and
all three failed: splitting the accumulation across downslope neighbours, thermal
erosion planing the grooves off, and multi-receiver incision. The actual cause is
**filled basins** — base elevation is scale-free noise with no regional drainage,
so about a third of a block ponds, and flat resolution gives those basins a
geometric BFS-distance gradient that erosion then carves. With a 40 m/km regional
tilt the same code on the same seed produces dendritic valleys and no herringbone.

So the fix is the **world map** (M2's first deliverable), not a change to the
erosion stages. Recorded because it is the strongest argument yet for building the
world map before tuning anything else, and because every assertion in the erosion
modules passes against the biased surface — it took four renders to find.

**Order independence is verified at the size the criterion states**: a 4x4 area generated
forwards, then backwards with an unrelated block between each pair, produces identical
hashes for all sixteen — 48 block generations in 885 s. The intruder matters: reversing
alone would only show the pipeline does not depend on *direction*. The sixteen are also
checked to be distinct from one another, or the comparison would be vacuous.

**The pipeline, as one call.** `cx_worldgen::generate_block` — stages 1 to 6 composed,
a pure function of `(seed, block_coord, settings)`. Six stages threaded by hand at each
call site would be six chances to thread them differently, and "differently" here means a
world that does not regenerate.

`WorldSettings` carries all five stages' knobs, so S07's `full-sim` and `no-erosion`
profiles are two *values* rather than two code paths — which makes `no-erosion` testable as
the identity rather than as an untaken branch. Every stage still runs under it; a world
without erosion still needs drainage.

A generated block now keeps **both** the filled surface and the ground beneath it. Their
difference is standing water: where the fill raised ground, that is a lake, and how far it
raised it is how deep. Step 7's water body extents come from that and from nothing else,
and the earlier shape — returning only the filled surface — had thrown it away.

**Concurrency is bounded by memory, not by cores.** S07 asks for 20 s per block on 8
background threads, which reads as eight blocks at once. The arithmetic says otherwise:

| | |
|---|---|
| Resident per block — filled surface, accumulation, drainage order, direction, ground | 0.415 GB |
| Worst transient — flat resolution's height copy plus two distance maps | 0.293 GB |
| **Peak per in-flight block** | **~0.71 GB** |
| Budget | 0.8 GB |

**1.13 blocks fit.** So the pool must generate one block at a time with its threads *inside*
the block — which is what `ADR-0008` said originally (*"parallelizes by row band"*). Worth
stating explicitly, because the other reading exceeds the budget by 7x on the first busy
frontier.

**Step 7: derived static fields (partial).** `cx_worldgen::derive` — slope and aspect
from baked `ELEVATION`, quantised to a byte each per `bench/memory-budget.md`. Computed
once and never recomputed: `ADR-0008` removed continuous erosion precisely so these could
be static, and only a discrete edit (S19) dirties them.

Quantisation is **saturating, not wrapping** — a near-vertical face pins at the maximum
rather than rolling over to read as level, which would let navigation route a path up a
cliff. Flat ground gets an explicit `ASPECT_FLAT` sentinel rather than aspect zero, because
zero is north and a world where every plain faces north is the kind of wrong that looks
like a feature. The resolution claims are compile-time assertions on the constants, so
coarsening either step size fails the build rather than quietly degrading every derived
field in the world.

Aspect faces **downhill**, the way water runs, clockwise from north. Tested on both axes,
because a transposed gradient passes a single-axis test — and the sign was falsified by
inverting it, which moves east from 90 degrees to 269.6.

**Known approximation**: slope at a chunk's rim is a one-sided difference, since a central
difference needs a neighbour the chunk does not contain. That is 0.4% of a chunk's cells,
under-reporting slope, and the fix is baking with the one-cell halo `ELEVATION` is already
registered for. Recorded rather than papered over — ignored, it would read as a slightly
cheaper route around the edge of every chunk.

Floodplain masks, traversability, and water body extents remain. Water bodies need the
*pre-fill* surface retained through the pipeline, which the stages currently discard.

**Step 6: the bake.** `cx_worldgen::bake` — `ADR-0015`'s other half, finally delivering.
The eroded 2 m surface is resampled to the 0.5 m `ELEVATION` grid and high-frequency
detail is added back. Chunks are pure extraction plus interpolation, per `ADR-0006`: a
chunk computes nothing of its own.

The ADR named a correctness question for each half, and both are now tested:

- *"The resample must not introduce terracing."* Catmull-Rom rather than bilinear.
  Bilinear is continuous but its derivative is not, so slope jumps at every coarse-cell
  boundary and a hillshade shows a grid of creases every 2 m.
- *"The re-added detail must not fill in channels."* Detail amplitude fades to zero as
  drainage area rises. A channel is 11 m deep and a metre or two wide, so a few metres of
  noise would erase the river five stages went into carving. It is also physically right:
  a channel floor is graded by the water on it and is smoother than the slopes above.

**Both tests were weak on the first attempt and were caught by falsifying them**, not by
review. The crease test used a *planar* fixture — but bilinear reproduces a linear ramp
exactly and has zero second difference along it, so a plane cannot distinguish the two
schemes, and the test passed against the artifact it existed to exclude. It uses a curved
surface now and compares peak curvature against mean, since a C0 interpolant concentrates
all its curvature at cell boundaries. Bilinear scores 6.3x and fails.

The seam test used an absolute threshold loose enough to admit the detail's own
amplitude, and passed against a version that sampled detail in chunk-local coordinates —
a visible seam on every chunk edge in the world. It compares the step across the boundary
against an ordinary within-chunk cell step now, because a seam is a *discontinuity* and
only a relative measure asks that. The broken version gives 0.53 m against 0.08 m.

**Step 5: channel carving.** `cx_worldgen::carve` — the flow network incised into the
eroded surface with S08's hydraulic geometry, width ∝ Q^0.5 and depth ∝ Q^0.4. Erosion
produces *valleys*; a river is a metres-wide trench in the floor of one, and at a 2 m
grid the stream-power term does not resolve that. On a real block: 8 s, 162,192 channel
cells, 587,587 carved including banks, deepest cut 11.1 m.

Both exponents are well under 1, which is what makes a network look like a network — a
hundredfold catchment is ten times as wide and six times as deep, not a hundred times
either, so tributaries stay comparable to their trunk instead of vanishing beside it.

Carving a trench is the obvious way to make water disappear into one, so two things
prevent it and neither is trusted on its own: depth grows with discharge and discharge
grows downstream, so a channel bed cannot rise along its own length; and banks are a
parabolic profile rather than a step, so there is no wall to pond behind. The network is
rebuilt afterwards and interior sinks are counted — zero, on the fixture and on a block.

**Rock hardness.** `cx_worldgen::hardness` — erodibility varied by place instead of one
constant for the whole world. Soft rock opens into wide valleys, hard rock resists and
stands as ridges and cliff bands. Positional noise like everything else, so a hard band
crossing a block seam is the same rock on both sides. One byte per cell (26 MB per block);
`contrast: 1.0` reproduces the old uniform world exactly, which keeps every earlier test's
claim intact. Not yet a full material system — no named rock types — but a later material
model replaces the *source* of this multiplier without changing how erosion consumes it.
Thermal erosion's talus angle and carving's channel shape are natural future consumers.

**Step 4: thermal erosion.** `cx_worldgen::thermal` — talus-angle relaxation, read-then-write
so a cell's result cannot depend on sweep order. Mass is conserved and tested as such:
the failure mode is a stray factor that quietly adds or removes material every round,
which looks like nothing until a landscape has inflated. Progress is measured as *excess
steepness*, not as a count of over-steep cells — the count is not monotonic, because a
spreading debris apron creates new steep front as fast as the peak behind it settles, and
asserting on it failed against a surface that was settling correctly.

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

Steps 8–9 remain, plus the rest of step 7: biome assignment, scatter, floodplain masks,
traversability, and water body extents.

**The chunk state machine.** `cx_worldgen::lifecycle::ChunkLifecycle` — the piece where
frontier, pool, and cache meet. Chunks hold data proportional to how close they are:

| State | Resident | Cost |
|---|---|---|
| Generated / Dormant | a ~64 B summary (min/max height, water fraction) | negligible |
| Coarse | 128x128 downsampled heights | 64 KB |
| Active | full baked elevation + slope/aspect | ~6 MB |

Everything is amortized: a few promotions and demotions per tick, nearest-first up and
farthest-first down, under an Active cap — walking into a new region never bakes 25 chunks
in one frame. The integration tests hold the budgets on *every tick*, not on average, and
the 10,000-Dormant-chunks criterion is counted in bytes (about 3 MB, against 200 budgeted).

Blocks are heavy (~430 MB resident even after shedding erosion-only data), so at most two
stay in memory, least-recently-needed evicted first; the disk cache brings one back in
seconds. A chunk whose block is not resident simply waits a tick.

**Deliberately not decided here**: which tick a chunk activates on depends on disk and CPU
speed, so it is not reproducible across machines — fine for rendering, not for simulation.
Before sim state may depend on chunk contents, activation must either be recorded for
replay or gated deterministically. That is persistence work (S13), recorded now so it is a
decision rather than a surprise.

**The generation pool and frontier.** `cx_worldgen::pool::GenerationPool` and
`cx_worldgen::frontier` — generation off the tick thread, aimed ahead of the camera.

The pool is **one worker, not eight**, because the memory arithmetic already settled it:
one in-flight block peaks at ~0.71 GB against a 0.8 GB budget, so the cores go *inside*
each block (the row-band parallelism) and blocks are made one at a time. The tick thread's
whole interface is non-blocking: hand over a want-list, poll for finished blocks.

The want-list is **replaced, not appended to** — the frontier recomputes priorities every
time the camera moves, so one-off requests would go stale the moment it turned. A block
that drops off the list before the worker reaches it is never made; the one mid-generation
finishes and is delivered anyway. Delivered blocks are remembered so the frontier can
resend its list every frame without duplicating 44 seconds of work — that race is real and
has a test.

The frontier is pure arithmetic, testable without threads: the want-list centres on where
the camera *will be* (position + velocity × a lead time sized to generation cost), always
starts with the ground underfoot, and at speed fills the whole line of travel rather than
just the destination — skipping the middle would generate where the camera is going while
it falls into ungenerated ground on the way.

Still open toward the "200 m/s, frontier never outrun" criterion: wiring pool + frontier +
chunk lifecycle into the running app, which is where that gets measured for real.

**The block cache.** `cx_worldgen::cache::BlockCache` — generate once, reload from disk
after. An entry stores only the carved ground surface (~100 MB, inside the budgeted
100–200 MB per block): the final terrain is that surface with its basins refilled and the
drainage re-routed, both recomputable in seconds by the same code that produced them, so
storing them would be paying disk for what the generator already guarantees.

Keyed by `(GENERATOR_VERSION, seed, block, settings fingerprint)`. Every mismatch — wrong
version, different settings, flipped bit, truncated file — is treated as a miss, never an
error: the generator is the source of truth and the cache is only a shortcut to it. Writes
go through a temp file and rename so a crash mid-write cannot leave a plausible-looking
half-entry. Size-capped with oldest-first eviction. M2's "delete the cache, replay,
identical world" criterion is a test, proven as bit-equality in both directions.

`GENERATOR_VERSION` must be bumped in any PR that changes terrain output — a stale version
key is the one failure the cache cannot detect by itself.

**Generation speed, first pass.** A block went from ~121 s to ~44 s through three
changes, in order of what they bought:

1. *Six erosion rounds at double strength instead of twelve* (~27 s). The implicit solve
   is stable at any step size, so the step is a shaping knob; compared side by side the
   6-round terrain keeps the same valleys and channels.
2. *Mid-erosion rebuilds skip the depression fill* (~20 s). Erosion's update is a weighted
   average of a cell and its receivers, so no cell ever drops below the cell it drains
   into — erosion cannot create a pit, and filling an eroded surface is the identity.
   Proven on real output by a test, not just argued.
3. *Row-band parallelism* (`cx_worldgen::parallel`) for the per-cell stages — elevation and
   hardness sampling, flow directions, thermal erosion, carve stamps. Work splits into a
   **fixed 64 bands** merged in band order, so output is bit-identical at any thread count,
   which `ADR-0004` requires. Plain `std::thread::scope`, no new dependencies.

What remains serial is the genuinely order-dependent core: the erosion solve and flow
accumulation both walk the drainage network in dependency order (~26 s of the 44). Getting
under the 20 s target means parallelising those walks (level scheduling) or cutting the
network-rebuild count further — both real projects, neither started.

**Whole-pipeline cost so far**: about 130 s single-threaded for steps 1–5 over one block
(world map, base elevation, fill and routing, 12 erosion rounds, 4 thermal rounds,
carving). S07's target is 20 s on 8 background threads. Nothing is parallelised yet, and
erosion's re-routing is the bulk of it — that is where the work goes when it is time.

**The terrain therefore looks smooth, and should.** A plausible-looking
placeholder would have hidden exactly the difference erosion makes, which is the
one thing worth being able to see when it lands.

`ELEVATION` is registered `DeltaPersisted` with a one-cell halo and tile dirty
tracking, per `ADR-0011` — an untouched chunk costs zero bytes because it can be
regenerated from the seed.
