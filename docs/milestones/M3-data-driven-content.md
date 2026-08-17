---
id: M3
title: Data-Driven Content
specs: [S04]
gate: bench/baselines.md#m3
---

# M3 — Data-Driven Content

The point at which the engine stops requiring Rust changes to add things to the world.

## Deliverables

- `bevy_reflect` type registry with a single registration macro at component definition sites.
- RON definition files, content packs, manifests, dependency-ordered loading.
- Prototypes with `extends` inheritance and documented merge rules.
- Load-time validation with file/line/column errors and nearest-match suggestions.
- Cross-pack override and conflict detection.
- Hot reload in dev builds.
- Asset server: async loading, ref counting, placeholder assets.
- Mesh pipeline: glTF import, LOD chain generation, palette atlas assignment and UV rewriting.
- Binary content cache keyed by content hash.

## Exit criteria

| Check | Target |
|---|---|
| Component defined only in content, never in engine Rust | spawns and simulates |
| Every error class (unknown component, bad type, range, missing ref, cycle) | file/line/column message, table-driven test |
| 10,000 prototypes across 20 packs from binary cache | < 200 ms |
| Two packs patching one prototype at equal precedence | load error naming both |
| 50,000 objects from 200 meshes after atlas assignment | < 10 materials |
| Binary cache vs source parse | ≥ 10x faster |
