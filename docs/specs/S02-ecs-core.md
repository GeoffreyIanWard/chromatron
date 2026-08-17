---
id: S02
title: ECS Core
status: not started
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

## Open questions

- Whether `bevy_ecs` relations (if stabilized by implementation time) should replace the hand-rolled hierarchy. Revisit at M6 when agents need ownership graphs.
