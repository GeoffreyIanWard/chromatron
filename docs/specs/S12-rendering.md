---
id: S12
title: Rendering
status: not started
depends_on: [S03, S04, S06]
provides: [render-graph, instancing, gpu-culling, terrain-meshing, water, shadows, debug-draw]
crates_touched: [cx-render, cx-view]
milestone: M1 (core), M10 (polish)
---

# S12 — Rendering

Low-poly makes this the easy half of the engine, provided one thing is done right: **almost everything shares one material**, so almost everything draws in a handful of indirect calls.

## Requirements

### Crate boundary (`ADR-0010`)

- Rendering lives in `cx-render` and uses wgpu **directly and idiomatically** — bindless resources, whatever the backend offers. There is no backend-agnostic trait surface; `ADR-0005` proposed one and is superseded.
- The one rule that survives: no crate outside `cx-render` may name a wgpu type, enforced by CI. That boundary costs nothing and keeps graphics code from spreading into gameplay.

### Core pipeline

- **Palette atlas materials**: meshes carry UVs indexing a small shared palette texture rather than per-object material parameters. Consequence: one material, one pipeline, one bind group for the vast majority of scene geometry.
- **Instanced indirect draws**: per-instance data (transform, palette offset, LOD index, tint) lives in a storage buffer. Draw calls are `multi_draw_indirect` over compute-culled instance lists.
- **GPU frustum culling** in a compute pass writing indirect draw args. CPU never iterates visible objects. Add compute occlusion culling (hi-z) at M10 if profiling justifies it.
- **Mesh LOD**: 4 levels from S04's pipeline, selected on GPU by screen-space size. Furthest level is a camera-facing billboard imposter, which is how vegetation reaches six-figure counts.
- **Terrain meshing**: chunked heightfield meshes from the `ELEVATION` field with LOD by distance and skirt-based seam handling (skirts are cheaper and far simpler than stitching, and invisible in a low-poly style). Meshes are **baked at block generation and cached to disk alongside the block**, so terrain LOD chains use a better offline algorithm than a real-time budget would allow, and the ~99.9% of the world nobody edits never remeshes.
- **Edited terrain uses a two-tier patch path** (`ADR-0011`, S19). A dirty tile (64×64 cells) gets a runtime-generated patch mesh swapped into the draw list the same frame, sub-millisecond; the affected chunk is then re-baked at full quality on the background pool and swaps in within a few frames. The player sees the change immediately and quality catches up.
- **Water rendering**: infinite bodies render as a flat surface at their level with depth-based color ramp, screen-space refraction, and normal scroll along the static flow direction (`ADR-0009`). Because levels are static and flow is precomputed, water surface meshes bake with the terrain. Flood tiers swap between precomputed extent meshes. Finite bodies are small and render as individual entities. Low-poly water reads well with flat shading plus a foam line at shallow depth.
- **Shading**: flat/faceted and toon ramps, cascaded shadow maps (3–4 cascades), sky and atmospheric fog. No PBR authoring pipeline.
- **Post**: outline pass (depth+normal edge detection — the workhorse of the low-poly look), color grading LUT, FXAA or TAA. TAA requires motion vectors, which requires the extract phase to supply previous-frame transforms; confirm before committing.
- **Debug draw**: immediate-mode lines, spheres, AABBs, arrows, world-space text. Essential for simulation development, not an afterthought — build it at M1.
- **Frame capture**: screenshot and image-sequence export driven by tick number rather than frame number, so a recording of an accelerated simulation is reproducible.

### Extract contract

- The extract phase (S03) is the only path from sim to render. It copies interpolated transforms and visual state into the view world. Renderers never read sim data directly.
- Extract is budgeted and parallel; extracting 100,000 visible instances must stay under 2 ms.

## Non-goals

No PBR, no GI, no ray tracing, no skeletal animation at this layer (S15). No deferred rendering — forward with a depth prepass is sufficient for this material model and much simpler.

## Acceptance criteria

- 100,000 instanced meshes at 60 fps in fewer than 20 draw calls (M1 gate).
- 1,000,000 instances (mixed LOD, mostly imposters) at 60 fps.
- No visible stutter at 30 Hz sim / 144 Hz render, 99th-percentile frame time under 8 ms.
- Terrain mesh bake of one chunk under 200 ms offline.
- Tile patch mesh generated and visible in the same frame, under 1 ms; background chunk rebake swaps in within 5 frames.
- Zero wgpu types reachable from any crate other than `cx-render`, enforced by CI.
- Debug draw of 10,000 lines under 1 ms.

## Open questions

- TAA vs FXAA. TAA on low-poly flat-shaded geometry with hard outlines can smear badly; evaluate visually at M10.
- ~~Whether terrain needs a voxel or layered-heightfield representation.~~ **Closed by `ADR-0013`**: 2.5D heightfield. Tile patch meshing is therefore a plain heightfield remesh — no isosurface extraction, no seam-stitching between differing voxel LODs — which is what keeps the patch path sub-millisecond.
