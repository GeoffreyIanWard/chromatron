---
id: M2
title: Worldgen, Erosion & Chunk Lifecycle
specs: [S07]
gate: bench/baselines.md#m2
---

# M2 — Worldgen, Erosion & Chunk Lifecycle

An effectively infinite world, deterministically generated from a seed, eroded once at generation, with blocks streaming in behind a background frontier. After this milestone, terrain never changes again.

## Deliverables

- World map: coarse elevation, uplift, precipitation, temperature, global drainage, biome assignment.
- **Block generation pipeline** (`ADR-0006`, `ADR-0008`): base elevation → depression fill and flow routing → grid-based hydraulic erosion → thermal erosion → channel carving → halo discard → static field derivation → biome → scatter.
- Generation pipeline as a **composition point** (S20): each stage is a module registration, so hydraulic erosion, thermal erosion, channel carving, biome assignment, and scatter can each be toggled independently.
- Background generation pool with a frontier sized to outpace maximum travel speed.
- Disposable on-disk block cache keyed by `(seed, block_coord, generator_version)`.
- Chunk extraction from cached blocks (pure slicing).
- Chunk state machine with amortized transitions and an `Active` cap.
- Terrain meshes and water surface meshes **baked at generation** and cached alongside the block (S12).
- Field inspector overlay (early slice of S14) — visualizing fields is how worldgen gets debugged.

## Exit criteria

| Check | Target |
|---|---|
| 4×4 block area generated in two different orders | identical field hashes — **met**: 48 block generations in 885 s, all 16 identical across orders and all 16 distinct from each other. Gated in CI. |
| Single block generation (full pipeline; erosion at 5,120² per `ADR-0015`, bake at 16,384²) | < 20 s, 8 background threads — **close**: 45 s → 24 s on the 12-core dev machine (layer-parallel solve, weight-free topology passes, re-route every 2nd round). The remaining gap is the drainage rebuilds; see the closeout notes. |
| Chunk extraction from cached block | < 5 ms — **met**: worst measured bake 2.3 ms (`--example genprofile`). |
| Terrain mesh bake, one chunk | < 200 ms offline — **met**: 2.3 ms bake + 1.5 ms derived fields + 2.9 ms water, worst of four chunks. |
| Flow continuity walk over 100 km of channel | unbroken across chunk *and* block seams — **met**, gated in CI: 219 km walked, channels continue across every crossing, and with the regional water model sharing pour levels the seam surface steps by a median of **0.00 m** (worst 24 m at one saddle where coarse and fine erosion disagree most; was median 13 m, worst 94 m). The walk asserts median ≤ 2 m and worst ≤ 40 m as regression guards. |
| Camera traversal at 200 m/s | frontier never outrun, no frame > 20 ms — **split verdict**, measured in-app (`CX_AUTOFLY=200`): over cached terrain, never outrun (zero underfoot misses across 32 km) with steady-state worst frames of ~17–24 ms — a shade over target, the remainder being chunk promotion baking on the frame thread. Over *cold* terrain, not met and not meetable yet: in-app generation is ~45 s a block against 41 s to fly across one, so this criterion is coupled to the sub-20 s block target. Two real fixes fell out of measuring: promotion budgets sized to the frame (one bake a tick), and frontier priorities that build the travel path in driving order rather than the destination first — the old ordering left a hole under the camera every frame of a cold run. |
| Delete block cache, replay | identical world state — **met**, bit-equality test in `cx_worldgen::cache` |
| 10,000 generated chunks resident as `Dormant` | within memory budget — **met**, counted in bytes (~3 MB vs 200) |
| `no-erosion` profile | generates a valid world; differs from `full-sim` only in terrain shape |

## Resolved before starting: the erosion grid

M2's first arithmetic check found that a block's erosion working set at 0.5 m is **6.64 GB
against the 0.8 GB** `bench/memory-budget.md` budgets — 8.3x over, for one in-flight block,
before any frontier concurrency. Generation *time* was not the binding constraint (200
iterations is 10–21 s on 8 threads against a 20 s target); memory was.

`ADR-0015` settles it: **steps 2–5 run on a 2 m grid**, 5,120² with halo and 0.42 GB, and
step 6 resamples to the 0.5 m field grid. The block stays 8,192 m, so seam frequency — the
question this milestone is meant to answer visually — is unchanged. Erosion supplies the
landform and positional noise supplies the sub-2 m surface texture, which is what makes it
affordable to have both.

## Progress

- **The block grid** — `cx_worldgen::block`. Coordinates, halo indexing, the erosion grid, and
  base elevation over a whole block. A full block fills in 680 ms single-threaded at 100 MB.
  A block's halo holds cell-for-cell the same terrain its neighbour computes as core, checked
  by walking the whole seam, and four adjacent blocks were rendered and looked at with no
  visible seam.

- **Step 2, the flow network** — `cx_worldgen::flow`. Priority-flood fill, D8 routing, and
  flow accumulation over a block in 3.3 s single-threaded; the largest channel carries 30.8%
  of the block. Three separate wrong versions preceded it, and the one that assertions could
  not distinguish from the right answer was caught by rendering the network and looking at it.
  See S07's "what is implemented" for what each cost.

- **Step 3, hydraulic erosion** — `cx_worldgen::hydraulic`. Implicit stream power, 12 rounds
  over a block in 89 s single-threaded, 22 m mean lowering, zero sinks. Carries a recorded and
  diagnosed artifact: D8 grid bias printing a herringbone into the eroded surface. See S07.

- **Step 4, thermal erosion** — `cx_worldgen::thermal`. Talus-angle relaxation, mass-conserving,
  read-then-write for order independence.
- **The grid-bias artifact is diagnosed**, and it is not an erosion bug. Three candidate fixes
  were tried and falsified; the cause is filled basins in scale-free noise, and the fix is the
  **world map**. That makes the world map the next thing to build rather than a later
  deliverable. See S07.

- **The world map** — `cx_worldgen::worldmap`. Continental elevation and uplift, positional
  rather than stored. Basin fraction 32.4% to 26.7%; the artifact is much improved but not
  gone, because the gradient is near zero at the continental surface's own ridges. See S07.

- **The grid-bias artifact is resolved.** Four hypotheses were falsified before the cause was
  found — and the fix was not the world map either, though that helped. Flat resolution's
  tie-break was smooth in position, so equal-distance contours (straight diagonals on a grid)
  all drained the same way. It is a coordinate hash now: inside a lake the direction is
  genuinely arbitrary, and an arbitrary choice leaves no pattern to carve.

- **Step 5, channel carving** — `cx_worldgen::carve`. S08's hydraulic geometry cut into the
  eroded surface; 8 s per block, deepest 11.1 m, zero sinks after re-routing.
- **Steps 1–5 now run end to end** at roughly 130 s single-threaded per block, against a 20 s
  target on 8 threads. Nothing is parallelised yet and erosion's re-routing dominates.

- **Step 6, the bake** — `cx_worldgen::bake`. Catmull-Rom resample from 2 m to 0.5 m plus
  drainage-faded detail, closing out `ADR-0015`. Both of the ADR's named correctness
  questions are now tested, and both tests had to be strengthened after falsification
  showed they passed against the exact artifacts they existed to exclude.

- **Step 7, derived fields (partial)** — `cx_worldgen::derive`. Slope and aspect, a byte
  each, saturating rather than wrapping, with an explicit flat-aspect sentinel. Water body
  extents still need the pre-fill surface retained through the pipeline.

- **The pipeline as one call** — `cx_worldgen::generate_block`, stages 1–6. A generated
  block keeps the ground surface as well as the filled one, so water bodies are computable.
- **M2's headline exit criterion is met**: a 4×4 area generated in two orders produces
  identical hashes — 48 block generations in 885 s, all 16 blocks matching and all 16
  distinct. Gated in CI on Linux only — the test compares two runs on the *same* machine,
  so running it per-platform would test the same property three times. Cross-platform
  bit-exactness, which `ADR-0004` does target, needs a **pinned** hash rather than two runs
  compared with each other; worth adding once the pipeline stops changing every PR.
  `#[ignore]`d locally because of the runtime; the fast 2×2 in `pipeline.rs` runs on every
  commit.
- **The generation pool's shape is settled by arithmetic**: peak ~0.71 GB per in-flight
  block against a 0.8 GB budget, so **one block at a time** with threads inside it, not
  eight blocks at once. See S07.

- **Rock hardness** — `cx_worldgen::hardness`. Erodibility varies by place; soft ground
  tears into fine gullies while hard bands stand as smooth ridges. Directly addresses the
  uniformity feedback from the M2 status review.

- **The block cache** — `cx_worldgen::cache`. Generate once (~44 s), reload after (~5 s:
  read 100 MB + re-route). Stores only the ground surface; terrain and drainage are
  recomputed on load, bit-identically. The delete-and-replay exit criterion is a test.

- **The generation pool and frontier** — `cx_worldgen::pool` + `frontier`. One background
  worker (memory-bounded, cores go inside the block), non-blocking want-list/poll interface,
  look-ahead prioritisation. The 200 m/s traversal criterion still needs app integration.

- **The chunk state machine** — `cx_worldgen::lifecycle`. Amortized promotion/demotion under
  an Active cap, four residency levels, two-block memory ceiling. Budgets tested per-tick;
  the Dormant memory criterion is met by counting. The 200 m/s traversal runs headless over
  cached terrain; the in-app measurement still needs render integration.
- **CI**: the 20-minute worldgen gate now skips PRs that touch nothing terrain-shaping.

- **The interactive demo** — the M2 milemarker made visible. `chromatron-game` now flies
  over streaming terrain: the lifecycle aims at the camera each frame, and a small driver
  diffs its decisions against what is on the GPU, meshing at most two chunks a frame.
  Active chunks mesh at 2 m (66k vertices), Coarse at 8 m (4k), through a new retained
  terrain path in `cx-render` — one draw per chunk, chunk-local vertices, per-frame rebase
  against the floating origin, placeholder height/slope colour bands until biomes exist.
  The mesh builder is pure and unit-tested; the pass is exercised offscreen in CI,
  pixels asserted. Known and accepted: hairline height seams at chunk boundaries (one
  source cell of slope; skirts later), no frustum culling of terrain draws (~170 draws,
  cheap), and LOD pop at the Active/Coarse boundary. This also closes "the 200 m/s
  traversal in-app measurement needs render integration" — the measurement is now a run
  of the demo with the log open.

- **Visible water** — `cx_worldgen::water` + a translucent pass in `cx-render`. S07 step 7
  read out at last: lakes are the fact of the two surfaces (`terrain - ground`), rivers a
  threshold on drainage area — the *same* threshold channel carving uses, so water lies
  exactly in the channels that were cut, flooded to the waterline so a river has its
  trench's width rather than a 2 m thread down its middle. Lake spines spread their fill
  level so pond chains read as connected waterways; spine scanning reaches past chunk
  borders so no chunk edge cuts a river. Per-chunk water rides the lifecycle at both
  Active (2 m) and Coarse (4 m, wettest-cell downsampling so narrow rivers survive
  distance), and the demo draws it: alpha-blended, depth-tinted, shoreline-feathered.
  Presented channel *depth* is explicitly presentation, not hydrology — discharge volume
  is unsimulated. Known and accepted: water floats 15 cm above lake beds because the
  baked terrain *is* the fill level; baking beds from the pre-fill ground is future work.

- **Generation speed, second pass** — 45 s → 24 s a block, in three steps with three
  different guarantees. (1) *Bit-identical*: the erosion solve now runs layer-parallel —
  `accumulate` buckets its topological order by dependency depth as a by-product of the
  Kahn pass it already makes, and a layer's cells solve concurrently through relaxed
  atomics with a barrier between layers; every cell does the same arithmetic on the same
  inputs, proven by an unchanged terrain digest. Topology-only passes also stopped
  computing flow shares nobody read. (2) *Exact-substitution*: `powf(x, 2)` became `x·x`
  and `area^0.5` became `sqrt` — bit-identical on this platform's libm, strictly more
  portable everywhere, `GENERATOR_VERSION` bumped as insurance for platforms whose `powf`
  disagrees. (3) *A knob, chosen by eye*: `reroute_every: 2` halves the drainage rebuilds
  that dominate a round's cost; valleys, rivers, and relief are unchanged, hillside gully
  texture smooths (capture happens half as often), and the two stills were compared side
  by side before the default changed. Every-round re-routing remains one setting away.
  `--example genprofile` is the stopwatch these numbers come from.

- **The seam question, answered by walking it** — a gate test walks every channel-scale
  cell of two adjacent full-pipeline blocks downstream, handing over core-to-core at the
  seam (halo cells are never consulted: nobody renders them). Findings, in order of
  discovery: (1) **channels continue** — every crossing lands on a real flowline within a
  few cells, never an uncarved hillside; (2) **discharge does not** — accumulation restarts
  at each block's grid edge, so a river's catchment is under-stated just downstream of
  every seam, under-charging carve width there; (3) **basins are the seam's real defect** —
  a basin spanning the seam is filled to a different pour level by each block (each sees a
  different saddle beyond the other's halo), and erosion sculpts the diverged surfaces into
  steps of tens of metres exactly where channels cross. The walk reports median and worst
  uphill steps on every gate run, so the fix has a before and after. The fix for (2) and
  (3) is the same instrument: **worldmap-supplied boundary conditions** — regional pour
  elevations to pin trans-seam fill levels, and boundary influx to seed accumulation.
  S07-level design work, deliberately not smuggled into a test PR.

## Notes

**The seam question gets answered here, visually.** Fine erosion detail cannot be perfectly continuous across block boundaries with a finite halo. Rivers should stay coherent because region-level drainage constrains them from above — verify that first, since it is the failure that would actually be noticeable. If hillside detail shows a visible seam, the mitigations in order of preference are a wider halo, fewer iterations with stronger per-iteration effect, or a post-pass seam blend.

The other thing to watch is generation latency. 20 s per block is tolerable behind a frontier and intolerable in front of one. If the frontier cannot keep up at realistic travel speeds, reduce block size before reducing erosion quality.
