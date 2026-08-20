---
id: S11
title: Physics
status: facade only (rapier at M8)
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

## What is implemented

**The facade, and nothing that claims to be rapier.** S11 says *adopt, do not
write*, and that remains the plan: `rapier3d`, pinned, with determinism features
enabled, at M8. What exists is the boundary it will live behind, plus the one
case that needs no solver — a body falling until it meets the terrain.

The type is `FallingBody`, not `RigidBody`. There are no contacts between
entities, no constraints, and no broad phase. A type that claims more than it
does is how a placeholder survives into a release.

Three things this establishes that rapier will inherit rather than replace:

- **The participation rule.** Only entities with a body are queried, so the
  majority of a million-entity simulation never touches physics. Cheaper to
  establish now than to retrofit, and there is no per-entity check to forget.
- **The fixed timestep.** Never variable. A trajectory that depended on the
  frame rate would diverge between two machines running the same seed.
- **The `ELEVATION` read**, declared as a read. The first reader of that field;
  before this the graph's field-access layer had one edge and the read path was
  drawn by nothing. `ADR-0011` permits exactly two writers and physics is
  neither — a body resting on the ground does not reshape it.

### "There is no ground here" is a structural question

Three distinct cases, and a test for each: the field is not registered, the
chunk is not loaded, or the chunk is loaded but ungenerated. Only the third is
covered by worldgen's sentinel.

The first was found by a failing test. An **unregistered** field samples to
`0.0`, not to the sentinel — and zero is a perfectly plausible ground height, so
a value check alone would have let every body settle at sea level in a world
with no terrain and look entirely correct doing it. The guard asks the store
whether the data exists rather than asking what it says.

### Still M8

Rapier itself, rigid bodies, contacts, constraints, heightfield colliders built
per chunk and cached, in-place partial height updates on terrain edit, and body
freezing across chunk demotion. The acceptance criteria's figures — 5,000 bodies
under 8 ms, a collider build under 5 ms — are not measured, because none of what
they measure exists yet.

The cross-architecture determinism question stays open: it is about rapier,
which is not here.
