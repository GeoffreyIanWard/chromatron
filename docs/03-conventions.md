# Conventions

**Read this before writing any code.** These are the facts that, if guessed inconsistently across files, produce bugs that are expensive to find.

## Coordinates and units

- **Y-up, right-handed, counter-clockwise winding.** Matches glTF; no import-time conversion anywhere.
- **Metres** for length, **kilograms** for mass, **seconds** for time, **degrees Celsius** for temperature, **cubic metres** for water volume.
- **World position** = `ChunkCoord { x: i32, z: i32 }` + `Vec3` local offset in `[0, CHUNK_SIZE)`. Never store an absolute `f32` world position; at 100 km out, f32 has ~1 cm resolution and jitter becomes visible.
- **Floating origin** is applied at extract time only. The sim never rebases.
- `CHUNK_SIZE = 512.0` metres. `CELL_SIZE = 0.5` metres. `CELLS_PER_CHUNK_EDGE = 1024`.
- `BLOCK_CHUNKS = 16`, so `BLOCK_SIZE = 8192.0` metres. Generation halo is `2` chunks per side.
- `TILE_CELLS = 64`, so `TILE_SIZE = 32.0` metres and `TILES_PER_CHUNK = 256`. The tile is the dirty-tracking unit for meshes and nav cost grids (`ADR-0011`). Safe to fix at M1 now that `ADR-0013` closed the terrain-representation question.
- Terrain is a **2.5D heightfield** — one elevation per cell, no overhangs (`ADR-0013`). `MAX_DIG_DEPTH` and `MAX_RAISE_HEIGHT` clamp edits relative to *generated* elevation, not absolute height.
- Region cells on the world map are `1024.0` metres.
- `ELEVATION` is written by exactly two paths: the worldgen stage (S07) and `EditCommand` application in the `TerrainEdit` phase (S19). Any other write is a bug; a debug assertion catches it. There is no continuous process that modifies it (`ADR-0008`, `ADR-0011`).

## Time

- Tick duration is stored as `u64` microseconds. **Never a float.** Default `TICK_US = 33_333` (30 Hz).
- `Tick(u64)` is the canonical simulation clock. Wall-clock time never enters sim logic.
- Systems receive `dt` as a `Fixed` type, not `f32`, to make it obvious when someone uses frame time inside the sim by mistake.
- Render-rate code uses `f32` seconds and lives only in `cx-view` and below.

## Determinism rules (`ADR-0004`)

- **No `HashMap`/`HashSet` iteration** in sim crates. Use `IndexMap`, `BTreeMap`, or sorted `Vec`. A lint enforces this.
- **No unordered parallel float reduction.** Parallel sums must use fixed-size partition + deterministic combine order.
- **RNG is never global.** `RngStream::new(seed, StreamId::Erosion, tick)` — each system draws from its own reproducible stream. No system may consume a variable number of draws based on data that another system could reorder.
- **Generation is positional, never sequential** (`ADR-0006`): every generated value derives from `hash(world_seed, block_coord, ...)`. Generation granularity is the **block** (16×16 chunks), because drainage and erosion are non-local; chunks are pure extraction. Generating block B before block A must produce identical output to the reverse. Droplet-style algorithms that consume a sequential RNG are banned.
- Entity iteration order in a system must not affect results. If it does, the system is wrong — restructure it read-then-write.
- Sim code must not read `SystemTime`, thread IDs, or pointer addresses.

## Error handling

- `thiserror` for library errors, `anyhow` only in `apps/`.
- Loaders return errors carrying file path, line, and column. A malformed definition file must produce a message a content author can act on without reading Rust.
- **Sim crates do not panic in release.** No `unwrap`, `expect`, or unchecked indexing in `sim/`; a clippy lint enforces this. Invariant violations report through `cx-diag` and degrade, they do not abort.
- Panics are acceptable in tools, tests, and benchmarks.

## Performance rules

- Hot data uses `u32` generational handles, never `Box<dyn Trait>` or `Rc`.
- No allocation inside per-tick systems. Preallocate scratch buffers in resources; reuse them.
- Structural ECS changes (spawn/despawn/insert/remove) are always deferred to the `StructuralApply` phase via command buffers. Never mutate structure mid-iteration — archetype moves are the dominant cost in an archetypal ECS.
- Bulk spawn uses `spawn_batch`; never loop over `spawn`.
- Every field kernel is written to be SIMD-friendly: flat `&[f32]`, no branches in the inner loop, boundaries handled by a halo ring rather than bounds checks.

## Naming

- Components are nouns: `Position`, `WaterDepth`, `Hunger`.
- Systems are verb phrases: `apply_erosion`, `rebuild_spatial_index`.
- Marker components use the `Is`/`Has` prefix: `IsDormant`, `HasNavTarget`.
- Field IDs are `SCREAMING_SNAKE` consts in `cx-fields::fields`.
- Spec IDs (`S06`) appear in a `//! Implements S06` module doc comment at the top of each crate's `lib.rs`.

## Modules

- Every subsystem is a module (S20). A module declares its capabilities; it never names another module. If you find yourself writing `use cx_hydrology::` from outside hydrology, you want a capability instead.
- Consuming a capability optionally means declaring the behavior when it is absent, in the spec, before writing the code. "It'll just be zero" is a design decision and gets written down.
- Never branch on capability presence inside a system. Resolve it once at schedule-build time by scheduling a different system or not scheduling at all.
- Module registration order must never affect behavior. Resolution is a topological sort with a stable `ModuleId` tiebreak.
- A new module ships with its own CI smoke profile: itself plus its declared dependencies, nothing else. This is what catches undeclared reliance.

## Parallelism

- `bevy_tasks` only. Do not add `rayon` — two thread pools contending is worse than one.
- Field kernels parallelize by chunk, then by row band within a chunk. Never by cell.

## Testing

- Every solver has a **golden test**: fixed seed, N ticks, compare a state hash against a checked-in value.
- Every spec's acceptance criteria map to at least one test named `s06_acceptance_*`.
- Benchmarks live in `apps/chromatron-bench` and gate CI against `bench/baselines.md`.
- Determinism test: run the same scenario twice in-process and once in a subprocess; all three state hash sequences must match.
