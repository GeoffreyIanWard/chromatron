---
id: S11
title: Physics
status: not started
depends_on: [S02, S05, S06]
provides: [rigid-bodies, colliders, queries, heightfield-collision]
crates_touched: [cx-physics]
milestone: M8
---

# S11 — Physics

Adopt, do not write. `rapier3d` is the recommendation: mature, Rust-native, has a documented determinism mode, and supports heightfield colliders which is exactly what a field-based terrain needs.

## Requirements

- Rapier integration behind an `cx-physics` facade so the dependency is replaceable and so physics types never leak into gameplay specs.
- **Enable rapier's determinism features** and pin the version. Cross-version determinism is not guaranteed; the pinned version is recorded in `ADR-0004` and changing it invalidates existing replays, which must be handled as a save migration (S13).
- Terrain collision uses rapier heightfield colliders generated from the `ELEVATION` field, **one per chunk**, built once at activation and cached.
- Terrain edits (S19) update colliders by **in-place partial height writes** to the existing heightfield, never a rebuild. Chunk granularity is retained deliberately: thousands of small per-tile colliders would cost more in broad-phase than they would save on updates.
- Physics steps in its own phase after `AgentAct`, using the same fixed timestep. Never a variable timestep.
- Only entities tagged `HasPhysics` participate. The overwhelming majority of a million-entity simulation must never touch the physics world — physics is for the hundreds of things that need it, not the substrate.
- Physics bodies are created and destroyed through the deferred command path, in sync with chunk activation.
- Particles that need collision go through physics; purely visual particles live in the view world (S15) and never touch the sim.

## Non-goals

No soft bodies, no cloth, no fluid physics (water is a field, S08). No physics-driven gameplay for aggregate populations.

## Acceptance criteria

- 5,000 active rigid bodies stepping under 8 ms.
- Identical results across thread counts and across two runs on the same build, over 10,000 ticks.
- Heightfield collider build for one chunk under 5 ms, performed once and cached thereafter.
- In-place height update for an edited region under 0.5 ms, with no collider rebuild and no broad-phase churn.
- Bodies entering a chunk that demotes to `Dormant` are frozen and restored without positional drift.
- Physics disabled entirely (headless field-only scenario) has zero measurable tick cost.

## Open questions

- Whether rapier's determinism holds across x86-64 and aarch64. Test explicitly at M8; if it does not, physics results are excluded from the state hash and physics-dependent outcomes become non-authoritative.
