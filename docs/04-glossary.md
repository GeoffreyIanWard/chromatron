# Glossary

Defined once here; every other document uses these terms exactly.

| Term | Definition |
|---|---|
| **Tick** | One fixed-timestep step of the sim world. Duration `TICK_US`, default 33,333 µs. |
| **Frame** | One rendered image. Zero or more ticks may occur per frame; zero or more frames per tick. |
| **Sim world** | The authoritative, deterministic ECS world plus field storage. No rendering dependencies. |
| **View world** | Derived, disposable presentation state rebuilt each frame from the sim world. |
| **Extract** | The once-per-frame copy of visual state from sim world to view world, with interpolation factor `alpha`. |
| **Sparse data** | Per-entity data with identity and variable component sets. Lives in `bevy_ecs`. |
| **Dense data / field** | Per-cell data covering all space (water depth, temperature). Lives in chunked SoA arrays. |
| **Field** | One named dense array, e.g. `SOIL_MOISTURE`. Identified by `FieldId(u16)`. |
| **Derived field** | A field computed from other state rather than stepped, refreshed only when its inputs change (e.g. `WATER_DEPTH` from body level minus elevation). |
| **Cell** | The smallest field unit, `CELL_SIZE` = 0.5 m square. |
| **Chunk** | A 512 m square containing 1024×1024 cells plus the entities within it. Unit of streaming, persistence, and LOD. Extracted from a generated block. |
| **Block** | 16×16 chunks (8,192 m). The unit of *generation*, because drainage and erosion are non-local. Generated with a discarded halo margin. See `ADR-0006`. |
| **Module** | A subsystem unit that can be enabled, disabled, or developed in isolation. Declares capabilities, never names other modules. See S20. |
| **Capability** | A named interface a module provides or consumes (`cap::SURFACE_WATER`). The indirection that makes disabling safe. |
| **Profile** | A curated named module set (`minimal`, `full-sim`, `game`, `no-erosion`) gated in CI. |
| **Tile** | 64×64 cells (32 m), 256 per chunk. The unit of *dirty tracking* for meshes and nav cost grids when terrain is edited. See `ADR-0011`. |
| **Edit / `EditCommand`** | A discrete, bounded, replayable terrain change (raise, lower, flatten, carve, fill). Distinct from continuous process-driven change, which does not exist. See S19. |
| **Impoundment** | Water backed up behind an edit that blocks flow. Fills to its spill elevation, never beyond. |
| **Spill elevation** | The lowest escape point of a basin, computed from terrain by bounded flood-fill. Bounds an impoundment's level without integrating volume. |
| **Incremental drainage repair** | Re-running depression fill and flow routing over only the neighborhood affected by an edit, capped at one block. |
| **Block cache** | Disposable on-disk store of generated blocks. Not part of the save; deleting it costs regeneration time only. |
| **Infinite body** | A water body above the size or discharge threshold. Has a surface level, not a volume; never fills or drains. See `ADR-0009`. |
| **Finite body** | A small contained water body that tracks actual volume. Stored as an entity, not field data. |
| **Flow network** | The graph of river channels derived from generation-time drainage. Edges carry discharge. |
| **Discharge** | Volumetric flow rate on a flow network edge (m³/s), from upstream catchment area × current precipitation. Drives channel width, depth, and flood tier. |
| **Region** | A 1024 m cell on the coarse world map. Unit of global hydrology and climate. |
| **World map** | The permanently-resident coarse layer: elevation, drainage, climate, biomes. |
| **Chunk state** | One of: `Ungenerated`, `Generated`, `Active`, `Coarse`, `Dormant`. See S07. |
| **Sim LOD tier** | How much simulation an entity or chunk receives: `Full`, `Coarse`, `Statistical`, `Dormant`. See S09. |
| **Fast-forward** | Analytically advancing a `Dormant` chunk by N ticks on reload, rather than replaying those ticks. See S09. |
| **Delta persistence** | Storing only the difference between a chunk's current state and its regenerable-from-seed state. See S13. |
| **Positional determinism** | Generated values depend only on seed and coordinate, never on generation order. See `ADR-0006`. |
| **State hash** | A per-tick 128-bit digest of authoritative sim state, used to detect divergence. See S14. |
| **Deposit buffer** | The queue through which entities write into fields, drained in the `FieldDeposit` phase. |
| **Prototype** | A data-defined entity template that spawns a configured set of components. See S04. |
| **Scenario** | A data file describing a runnable simulation: seed, world params, initial prototypes, duration, metrics to record. |
| **Palette atlas** | The shared small texture that low-poly meshes index by UV, letting nearly all geometry share one material. See S12. |
| **Stencil kernel** | A field update where each cell's new value depends on its neighbors, e.g. flow or diffusion. |
| **Halo** | The one-cell (or wider) border of copied neighbor data around a chunk's field array, so kernels need no bounds checks. |
