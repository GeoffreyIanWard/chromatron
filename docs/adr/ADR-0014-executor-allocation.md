# ADR-0014 — The zero-allocation tick applies to our code, not to the executor

**Status:** accepted · **Date:** 2026-08-17

## Context

`03-conventions.md` bans allocation inside per-tick systems, and `bench/baselines.md` states
the M0 gate as `alloc_per_tick_steady_state | 0`. The gate was written before the ECS wrapper
existed, and it measured process-wide allocation across a whole tick.

On first run it failed at 12–14 allocations per tick. Attribution was unambiguous:

| Executor | Systems | Allocations per tick |
|---|---|---|
| Multi-threaded | 0 | 13.0 |
| Multi-threaded | 1 | 14.0 |
| Multi-threaded | 3 | 16.0 |
| Single-threaded | 0 | 0.0 |
| Single-threaded | 3 | 0.0 |

Every allocation comes from `bevy_ecs`'s `MultiThreadedExecutor` — scope setup, task futures,
per-run bookkeeping — and the count scales with system count. **No allocation originates in
engine code**: the same schedule, world, systems, and field storage allocate exactly zero
under the single-threaded executor.

Reaching a literal zero would mean writing an allocation-free executor or patching
`bevy_ecs`, which forks the dependency `ADR-0001` deliberately chose not to fork, for a cost
that is a fixed handful of small allocations per tick rather than anything proportional to
entity or cell count.

## Decision

The zero-allocation rule applies to **code this project writes**, and is measured with the
single-threaded executor, where the executor contributes nothing and any allocation is
therefore ours.

Executor overhead is measured separately with its own budget: `16 + system_count`
allocations per tick, which fails if `bevy_ecs`'s per-run cost grows.

`alloc_per_tick_steady_state` becomes two gates rather than one, both in `bench/baselines.md`.

## Rationale

The rule's purpose is to catch a `Vec` built per tick instead of a reused scratch buffer, or
a boxed trait object on a hot path — failures that scale with the simulation and are ours to
prevent. A flat combined threshold would have hidden exactly that: five new allocations from
a sim system would sit unnoticed inside a budget of twenty.

Splitting keeps a strict zero where a zero is meaningful and achievable, and puts a ceiling
on the part we do not control so that a `bevy_ecs` upgrade regressing it is visible.

## Consequences

- Two gates in `bench/baselines.md` where there was one, and a benchmark that runs the tick
  loop twice, once per executor.
- The single-threaded measurement is the authoritative one for the convention in
  `03-conventions.md`. Contributors should read a failure there as "sim code started
  allocating", not as an executor problem.
- The executor budget is hardware- and version-dependent in principle; in practice it is a
  count of allocations, not bytes or time, so it is stable across machines. A `bevy_ecs`
  upgrade that changes it is a deliberate re-baseline, as `ADR-0001` already requires for
  version bumps.
- If a future profile runs thousands of systems, `16 + system_count` grows linearly and may
  need revisiting. That is a signal worth having rather than a threshold to inflate.
- Does not affect determinism: allocation count is not part of the state hash, and the
  threads 1/4/16 gates continue to check that executor choice cannot change results.
