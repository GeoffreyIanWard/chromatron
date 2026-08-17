# Scope

## What this is

A Rust engine for running large 3D simulations, with the ability to build shippable games on top of those simulations. Low-poly visual target. Procedural, effectively infinite worlds that persist changes made by the simulation.

## Simulation domains in scope

| Domain | Data regime | Primary specs |
|---|---|---|
| Agent-based (crowds, creatures, AI) | Sparse entities | S02, S05, S09, S10 |
| Tile/grid colony or city | Mixed | S06, S07, S10 |
| Abstract network / economic | Sparse entities + graphs | S02, S09 |
| Physics / particle heavy | Sparse entities | S11 |
| World generation + erosion | Dense fields, generation-time | S07 |
| Ecological simulation | Dense fields | S08 |
| Water and weather | Flow network + derived fields | S08 |

This is a wide set. The engine is designed so all seven share one core, but **the milestone order deliberately drives the dense-field domains first** (M0, M2, M4) because they are the ones whose data model is *not* ECS-shaped. Agent work lands at M6 on top of a world that already exists. Reversing that order is the most likely way to end up rewriting the world module.

## Non-goals

- **Runtime erosion or fluid simulation.** Erosion happens once at generation (`ADR-0008`); water is a flow network with classified bodies, never a fluid solve (`ADR-0009`).
- **Multiplayer / netcode.** Out of scope. The determinism policy in `ADR-0004` deliberately keeps lockstep netcode cheap to add later, but no networking code is written.
- **Photorealistic rendering.** No PBR authoring pipeline, no ray tracing, no global illumination beyond baked/simple ambient. Low-poly with flat and toon shading.
- **A general-purpose engine for other people's projects.** This serves your simulations. No stable public API, no plugin ABI, no backwards-compatibility promises across versions except for save files (S13).
- **Console, mobile, web.** Desktop only — Windows, macOS, Linux (`ADR-0010`). See S18.
- **Visual editor.** Tooling is inspector-and-console shaped (S14), not Unity-shaped. Content is authored in text files (S04).

## Reference points

Shorthand like "Valheim-style with water simulation" is close enough to be dangerous — it maps the editing model onto generation, and it implies a fluid solver that `ADR-0009` exists to prevent. Per-subsystem analogues, to calibrate:

| Subsystem | Closest analogue | Explicitly **not** |
|---|---|---|
| Terrain editing (S19) | Valheim — radius brush, raise/lower/level, no overhangs | Minecraft or Astroneer voxel digging |
| Terrain representation (`ADR-0013`) | Valheim heightfield; verticality via placed structures | Voxels, layered heightfields, marching cubes |
| Worldgen + erosion (S07) | Offline terrain tools — World Machine, Gaea; hydraulic + thermal erosion baked once | Valheim's worldgen, which has no erosion or drainage |
| Drainage + rivers (S08) | GIS flow routing — priority-flood, D8, hydraulic geometry | Any fluid solver: pipe model, shallow water, SPH |
| Water bodies (`ADR-0009`) | Surface levels and spill elevations, geometry only | Volume integration, mass conservation |
| Simulation LOD (S09) | RimWorld / Dwarf Fortress region abstraction and fast-forward | Simulating everything everywhere |
| Entity scale (S02) | Songs of Syx — six-figure agents at low fidelity | Per-agent fidelity at that count |
| Rendering (S12) | Low-poly flat/toon shading, palette atlas, GPU-driven instancing | PBR authoring, deferred, GI |

The one-line version: **Valheim-style terrain interaction on erosion-driven procedural generation, with derived hydrology.** Not simulated hydrology — derived.

## Modularity

Every subsystem is a module that can be enabled, disabled, or developed in isolation (S20, `ADR-0012`). Erosion, hydrology, ecology, physics, agents, and presentation are all independently toggleable, and disabling one frees its memory and tick cost rather than merely branching over it. Curated **profiles** (`minimal`, `terrain`, `hydro`, `full-sim`, `no-erosion`, `game`) are the CI-gated configurations.

This is what makes the wide domain list above tractable: a given simulation enables the modules it needs, and a given engineer works against the smallest profile that covers their subsystem.

## Design targets

| Target | Value | Verified by |
|---|---|---|
| Sparse entities, full simulation | 1,000,000 | M0 benchmark |
| Field cells stepped per tick | 16,000,000 | M0 benchmark |
| Rendered instanced meshes | 100,000 @ 60fps | M1 benchmark |
| Sim tick rate (default) | 30 Hz | S03 |
| Time acceleration | up to 10,000x | S03, S09 |
| Peak memory, desktop | 12 GB | `bench/memory-budget.md` |
| Peak memory, min-spec profile | 8 GB | `bench/memory-budget.md` |
| Block generation (16,384², full pipeline) | < 20 s background | M2 benchmark |

If M0 cannot hit the first two numbers, the architecture is wrong and we stop and revise rather than proceeding to M1. That is the entire purpose of M0.
