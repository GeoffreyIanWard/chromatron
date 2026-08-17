# Architecture

## Composition: everything is a module

Every subsystem below is a **module** (S20, `ADR-0012`) declaring what capabilities it provides, requires, and optionally consumes. Modules never reference each other directly — navigation consumes `cap::SURFACE_WATER`, not `hydrology`. Absent capabilities resolve to documented degraded behavior at schedule-build time, so a disabled module costs zero ticks and zero bytes.

There are **two composition graphs**: the tick schedule, and the block generation pipeline (S07). Erosion lives in the second one.

The twelve tick phases below are **not** composable — they are the fixed ordering contract that makes parallel execution safe. Modules insert systems into phases; they never add, remove, or reorder them.

## The three-way split

Everything in this engine is organized around three separations. Violating any of them is the failure mode that costs a rewrite.

### 1. Sim world vs. view world (`ADR-0002`)

**Sim world** is authoritative. Fixed timestep, deterministic, headless-capable, no knowledge that rendering exists. **View world** is derived and disposable: interpolated transforms, animation state, particles, audio emitters, camera shake.

Once per rendered frame, an **extract** phase copies the visual subset of sim state into the view world with an interpolation factor. Headless mode is simply "never construct the view world."

CI enforces this: no crate under `sim/` may depend on `wgpu`, `winit`, `kira`, or `egui`.

### 2. Sparse entities vs. dense fields (`ADR-0003`)

**Sparse**: agents, buildings, items, vehicles — things with identity, variable component sets, and individual behavior. Stored in `bevy_ecs`.

**Dense**: terrain height, water depth, flow velocity, soil moisture, temperature, biomass, pollutant concentration — per-cell values covering all space. Stored as chunked SoA arrays (`Vec<f32>` per field per chunk), stepped by stencil kernels over the whole array.

Never model a terrain cell as an entity. A 1024x1024 chunk is 1M cells; at ten chunks loaded that would be 10M entities to do work an array kernel does in microseconds.

The bridge between them is well-defined and narrow: entities read fields by position (`fields.sample(pos)`) and write fields through a queued deposit buffer applied at a fixed point in the tick. No entity holds a reference into field storage.

### 3. Coarse world vs. block vs. chunk

A precomputed, permanently-resident **world map** (region granularity, 1 km cells) holds elevation, drainage networks, climate zones, and biome assignments. **Blocks** (16×16 chunks, 8,192 m) are the unit of generation, generated on demand and conditioned on the world map. **Chunks** are extracted from generated blocks by slicing.

Two stages force the block granularity: drainage routing needs upstream catchment area, and erosion is iterative and non-local (`ADR-0006`, `ADR-0008`). Neither can be expressed per-chunk.

Below the chunk sits the **tile** (64×64 cells, 32 m, 256 per chunk) — not a unit of generation or storage, but the unit of *dirty tracking* when terrain is edited (`ADR-0011`). Generation produces the baked terrain the untouched world uses; edits dirty tiles, not chunks.

## Crate graph

```
chromatron/
  crates/
    cx-core          # ids, handles, arenas, rng, math re-exports, error types
    cx-module        # Module trait, capability registry, resolution, profiles (S20)
    cx-ecs           # bevy_ecs wrapper: registration, schedules, ordering policy
    cx-time          # tick clock, accumulator, speed control, loop driver
    cx-fields        # chunked SoA field storage, kernels, sampling
    cx-worldgen      # seed hashing, world map, block generation, erosion, biomes
    cx-edit          # terrain EditCommands, dirty tiles, impoundments (S19)
    cx-solvers       # climate, hydrology (flow network), ecology (uses cx-fields)
    cx-spatial       # spatial hash, BVH, raycast, neighbor queries
    cx-agents        # navigation, steering, agent LOD, behavior
    cx-physics       # rapier integration
    cx-lod           # sim LOD tiers, budgeting, fast-forward
    cx-data          # definitions, reflection registry, prototypes, hot reload
    cx-persist       # snapshots, deltas, migrations, replay logs
    cx-diag          # metrics, tracing, state hashing, invariants
    cx-sim           # facade: assembles the above into a runnable simulation
    ---- firewall: nothing above may depend on anything below ----
    cx-render        # wgpu renderer; no wgpu type escapes this crate (ADR-0010)
    cx-view          # view world, extract phase, interpolation
    cx-present       # animation, particles, vfx
    cx-audio         # kira integration
    cx-ui            # egui tooling + game ui abstraction
    cx-app           # window, input, app state machine, main loop assembly
  apps/
    chromatron-game      # windowed client
    chromatron-cli       # headless runner, batch sweeps, benchmarks
    chromatron-bench     # criterion benchmarks
```

## Tick lifecycle

Each sim tick runs these phases in this fixed order. System sets map 1:1 onto phases.

```
1. IntakeCommands     apply buffered player/script commands
2. ChunkLifecycle     activate, demote, dormant-ize chunks; fast-forward loads
2b. TerrainEdit       apply EditCommands; mark dirty tiles; incremental drainage repair
3. FieldSolve         climate → hydrology → ecology (fixed order; no erosion, ADR-0008)
4. SpatialRebuild     update spatial index from last tick's positions
5. AgentSense         read fields and neighbors; no writes
6. AgentDecide        behavior; produces intents only
7. AgentAct           apply intents; movement integration
8. Physics            rapier step
9. FieldDeposit       apply queued entity→field writes
10. Events            drain and dispatch double-buffered events
11. StructuralApply   apply command buffers (spawn/despawn/insert/remove)
12. Diagnostics       metrics, invariants, state hash
```

Read-then-write separation (5/6/7 and 9) is what makes parallel execution safe and results order-independent. Do not let a system both read neighbor state and write shared state in the same phase.

## Data flow per frame

```
input events ──► input buffer ──┐
                                 ▼
   [ 0..N sim ticks, fixed dt ] ────► sim world (authoritative)
                                 │
                                 ├──► state hash ──► cx-diag
                                 │
                                 └──► extract(alpha) ──► view world
                                                            │
                                            frame-rate updates (anim, vfx, audio)
                                                            │
                                                            ▼
                                                          cx-render ──► wgpu
```
