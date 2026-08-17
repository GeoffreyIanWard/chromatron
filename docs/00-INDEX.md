# Engine Plan — Document Index

**This is the routing file. Read it first, then read only the rows that match your task.**

Project codename: `CHROMATRON` — a Rust engine for large-scale 3D simulations, with games built on top. Desktop only (`ADR-0010`).

**Five decisions shape almost everything else. Read these ADRs before any spec:** `ADR-0012` (every subsystem is a module depending on capabilities, never on other modules), `ADR-0003` (dense fields live outside the ECS), `ADR-0008` (erosion happens once at generation — no continuous process changes terrain), `ADR-0009` (water is a flow network plus classified bodies, never a fluid simulation), `ADR-0011` (terrain *is* mutable by discrete edit; the dirty unit is the tile), and `ADR-0013` (terrain is a 2.5D heightfield with Valheim-style editing; verticality comes from structures).

`ADR-0012` is the one that governs how you write anything: subsystems declare capabilities and never name each other, so any of them can be switched off. If you are about to `use` another subsystem's crate, you want a capability instead.

`ADR-0008` and `ADR-0011` are easy to misread as contradictory. They are not: `ADR-0008` removes *continuous, global, process-driven* change; `ADR-0011` supports *discrete, local, event-driven* change. Read both before touching terrain.

## How to use this doc set

1. Always read `03-conventions.md` before writing any code. It defines units, handedness, tick semantics, error policy, and naming. Most cross-file inconsistency comes from skipping it.
2. Find your task in the routing table below. Read the listed files and nothing else.
3. Specs are the source of truth for *what*. Milestones are the source of truth for *when and in what order*. ADRs are the source of truth for *why*. Never restate a fact across two of them — link instead.
4. When you complete work, update `status` in the spec front matter and append to `open_questions` if you discovered something the spec did not anticipate. Do not edit ADRs; add a new one that supersedes.

## Routing table

| If you are working on... | Read |
|---|---|
| Anything at all | `03-conventions.md`, `04-glossary.md`, `specs/S20-module-system.md`, `ADR-0012` |
| Repo setup, crate graph, CI | `02-architecture.md`, `specs/S01-foundations.md`, `ADR-0002`, `ADR-0010` |
| ECS internals, queries, scheduling | `specs/S02-ecs-core.md`, `ADR-0001` |
| Game loop, tick rate, interpolation, pause/speed | `specs/S03-time-and-loop.md`, `ADR-0002` |
| Definition files, prototypes, hot reload, assets | `specs/S04-data-and-assets.md` |
| Neighbor queries, raycasts, spatial index | `specs/S05-spatial-index.md` |
| Terrain storage, soil/climate arrays, chunk memory | `specs/S06-field-grids.md`, `ADR-0003`, `bench/memory-budget.md` |
| Worldgen, erosion, block generation, streaming, seeds | `specs/S07-worldgen-and-chunks.md`, `ADR-0006`, `ADR-0008` |
| Rivers, flooding, water bodies, climate, ecology | `specs/S08-field-solvers.md`, `ADR-0009` |
| Digging, terracing, damming, building, canals | `specs/S19-terrain-modification.md`, `ADR-0011`, `ADR-0013` |
| Adding a subsystem, toggling one off, profiles | `specs/S20-module-system.md`, `ADR-0012` |
| Making the sim cheap at distance, fast-forward | `specs/S09-simulation-lod.md` |
| Creatures, crowds, pathfinding, AI | `specs/S10-agents-and-nav.md`, `specs/S05-spatial-index.md` |
| Rigid bodies, collision, particles-as-physics | `specs/S11-physics.md` |
| Anything drawn on screen | `specs/S12-rendering.md`, `ADR-0010` |
| Save/load, replays, migrations | `specs/S13-persistence.md`, `ADR-0004` |
| Inspector, metrics, profiling, determinism checks | `specs/S14-observability.md`, `ADR-0004` |
| Visualizing the architecture, graph export, isometric viewer | `specs/S21-architecture-graph.md`, `specs/S20-module-system.md` |
| Animation, VFX, audio | `specs/S15-presentation.md`, `ADR-0002` |
| Menus, input, settings, game UI | `specs/S16-app-shell.md` |
| Mods, scripting, plugin API | `specs/S17-extensibility.md`, `ADR-0007` |
| Packaging, platform targets | `specs/S18-platform-and-shipping.md`, `ADR-0010` |
| Performance work of any kind | `bench/baselines.md`, `bench/memory-budget.md` |

## Spec status board

| ID | Spec | Status | Milestone |
|---|---|---|---|
| S01 | Foundations | implemented | M0 |
| S02 | ECS Core | partial | M0 |
| S03 | Time & Game Loop | partial | M0/M1 |
| S04 | Data & Assets | not started | M3 |
| S05 | Spatial Index | not started | M6 |
| S06 | Field Grids | partial | M0 |
| S07 | Worldgen, Erosion & Chunks | not started | M2 |
| S08 | Climate, Hydrology & Ecology | not started | M4 |
| S09 | Simulation LOD | not started | M5 |
| S10 | Agents & Navigation | not started | M6 |
| S11 | Physics | not started | M8 |
| S12 | Rendering | not started | M1 |
| S13 | Persistence | not started | M7 |
| S14 | Observability | partial (hashing at M0) | M0/M7 |
| S15 | Presentation | not started | M9 |
| S16 | App Shell | not started | M9 |
| S17 | Extensibility | not started | M9 |
| S18 | Platform & Shipping (desktop only) | not started | M10 |
| S19 | Terrain Modification & Construction | not started | M4B (tile layout at M1) |
| S20 | Module System & Composition | implemented | M0 |
| S21 | Architecture Graph & Isometric Viewer | partial (export done) | M0 (export) / M1 (viewer) |

## Milestone order

`M0` Dual scale proof → `M1` Loop + pixels → `M2` Worldgen + erosion → `M3` Data-driven content → `M4` Climate/hydrology/ecology → `M4B` Terrain modification → `M5` Sim LOD + fast-forward → `M6` Agents → `M7` Persistence + determinism → `M8` Physics → `M9` Game layer → `M10` Polish + shipping

Gate rule: a milestone is not complete until its benchmark in `bench/baselines.md` passes in CI.

**Profile rule:** every milestone gate runs against its named profile (S20). `minimal` for M0, `terrain` for M2, `full-sim` and `no-erosion` from M4 onward, `game` from M9. A milestone that only passes with everything enabled has a hidden coupling.
