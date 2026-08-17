---
id: M8
title: Physics
specs: [S11]
gate: bench/baselines.md#m8
---

# M8 — Physics

Rapier integration, scoped narrowly. Physics is for the hundreds of things that need it, not the million things that do not.

## Deliverables

- `cx-physics` facade over a pinned `rapier3d`, determinism features enabled.
- Heightfield colliders from the `ELEVATION` field, one per chunk, built once at activation and cached. Terrain edits update them by in-place partial height writes rather than rebuilds (S19, `ADR-0011`).
- `HasPhysics` opt-in tag; deferred body creation and destruction synced to chunk lifecycle.
- Freeze/restore across `Dormant` transitions.
- Cross-architecture determinism investigation (resolves S11 and S14 open questions).

## Exit criteria

| Check | Target |
|---|---|
| 5,000 active rigid bodies | step < 8 ms |
| Results across thread counts and two runs, 10,000 ticks | identical |
| Heightfield collider build, one chunk (once, cached) | < 5 ms |
| Bodies through `Dormant` round trip | no positional drift |
| Physics disabled, headless field-only scenario | zero measurable tick cost |
| x86-64 vs aarch64 determinism | documented finding; if it fails, physics excluded from state hash via ADR |
