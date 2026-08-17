---
id: S02
title: ECS Core
status: partial
depends_on: [S01]
provides: [world, queries, schedules, commands, change-detection, hierarchy]
crates_touched: [cx-ecs]
milestone: M0
---

# S02 — ECS Core

A thin, opinionated wrapper over `bevy_ecs` (`ADR-0001`). The wrapper exists to enforce policy the raw library does not: deterministic ordering, phase discipline, and deferred structural change.

## Requirements

- Re-export `bevy_ecs` types but **not** its scheduler defaults. Provide `SimSchedule`, built from the twelve phases in `02-architecture.md` as explicit `SystemSet`s with hard ordering edges.
- Systems register with a declared phase, **and always via a module** (S20) — never directly. Registering without a phase is a compile error, not a runtime warning.
- Ordering within a phase is expressed as constraints against *capabilities* (`after(cap::SURFACE_WATER)`), not against named systems in other modules.
- **Deferred structural change**: expose only `SimCommands`, which buffers spawn/despawn/insert/remove and applies in `StructuralApply`. Direct `&mut World` structural access is available solely to exclusive systems, which must be explicitly annotated.
- Bulk operations: `spawn_batch`, `despawn_batch`, `insert_batch` with preallocated archetype reservation. These are the paths agents and chunk activation use; per-entity loops are a performance bug.
- Change detection wrappers (`Changed<T>`, `Added<T>`) with a documented caveat: change ticks are not stable across save/load, so no persisted logic may depend on them.
- Deterministic iteration: provide `query.iter_deterministic()` yielding in `Entity` id order for cases where order genuinely matters. Document that plain `iter()` is unordered and its callers must be order-independent.
- Hierarchy: `Parent`/`Children` with cycle detection and a transform propagation system running in `AgentAct`.
- Double-buffered events: `EventQueue<T>` written during the tick, drained in the `Events` phase, cleared at tick end. No event survives more than one tick.
- Parallel execution via `bevy_tasks` with thread count from config, not `num_cpus` — a machine-varying count complicates reproducibility investigations.

## Non-goals

Not writing an ECS. Not supporting dynamic component registration from scripts at this layer (that is S04 via reflection).

## Acceptance criteria

- Registering a system without a phase fails to compile.
- 1,000,000 entities with 4 components each: full iteration of a 2-component query under 3 ms single-threaded.
- `spawn_batch` of 100,000 entities is at least 20x faster than a `spawn` loop.
- A scenario run with thread count 1, 4, and 16 produces identical state hashes for 10,000 ticks.
- Structural changes issued mid-iteration are visible only after `StructuralApply`, verified by test.

## What is implemented

`Phase`, `SimWorld`, `SimSchedule`, deferred structural change, bulk spawn, and
deterministic iteration. **Not yet**: `EventQueue<T>`, the `Parent`/`Children` hierarchy with
cycle detection, change-detection wrappers beyond re-exporting `Added`/`Changed`, and
ordering constraints expressed against capabilities rather than phases. None block M0's
gates; all are needed before M6.

## Open questions

- ~~Whether `bevy_ecs` relations should replace the hand-rolled hierarchy.~~ Materially
  changed: `bevy_ecs` 0.19 ships `ChildOf`, `Children`, and a relationship system in its
  prelude, so the hand-rolled hierarchy this spec describes may be redundant before it is
  written. Still decided at M6 as planned, but the likely answer is now "use the built-in
  one" — evaluate it rather than writing the alternative first.
- `bevy_ecs` 0.19 stores **resources as entities** (`IsResource`). Anything counting or
  hashing entities must exclude them, and `cx-diag`'s state hash (S14) needs the same care
  `SimWorld::entity_count` now takes.
- Derive macros (`#[derive(Component)]`) expand to absolute `bevy_ecs::` paths, so any crate
  that *defines* components needs `bevy_ecs` as a direct dependency — re-exporting through
  `cx-ecs` is not sufficient. That weakens the "one crate to touch on an ECS change" goal;
  a `cx-ecs` derive re-export shim is possible if this becomes annoying.
