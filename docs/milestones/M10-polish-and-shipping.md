---
id: M10
title: Render Polish & Shipping
specs: [S12, S18]
gate: bench/baselines.md#m10
---

# M10 — Render Polish & Shipping

## Deliverables

- Cascaded shadow maps, sky and atmospheric fog.
- Outline post pass, color grading LUT, anti-aliasing (TAA vs FXAA decided here).
- GPU occlusion culling (hi-z) if profiling justifies it.
- Imposter LOD tier for vegetation at scale.
- Asset bundling into indexed archives with mmap loading.
- Shader precompilation; zero runtime compilation in release.
- Platform layer for filesystem paths, save location, and display conventions (`%APPDATA%`, `~/Library`, XDG).
- Crash reporting with minidump plus trailing replay log.
- Build profiles: `dev`, `bench`, `release`.
- CI matrix across Windows, macOS, Linux, and the 8 GB min-spec profile.

## Exit criteria

| Check | Target |
|---|---|
| 1,000,000 instances, mixed LOD, mostly imposters | 60 fps |
| Cold asset load from bundle | < 3 s |
| Runtime shader compilation in release | zero, instrumented |
| Direct `std::fs` / `std::path` outside platform layer | zero, CI enforced |
| Crash report replayed | reproduces the crash |
| Suspend at arbitrary tick and resume | exact restore, fault-injection tested |
| 8 GB min-spec profile | passes all prior milestone gates |
