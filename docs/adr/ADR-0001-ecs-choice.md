# ADR-0001 — Use `bevy_ecs` as a standalone crate

**Status:** accepted · **Date:** 2026-08-16

## Context

The engine needs an ECS capable of a million entities with parallel system execution and change detection. Options: full Bevy, `bevy_ecs` standalone, `hecs`, `sparsey`/`shipyard`, or a hand-written ECS.

## Decision

Use `bevy_ecs` as a standalone dependency, wrapped by `cx-ecs` (S02). Do not adopt the rest of Bevy.

## Rationale

`bevy_ecs` is the most heavily exercised archetypal ECS in Rust, with a mature parallel scheduler, change detection, and system ordering — the pieces most expensive and error-prone to write. Bevy's *renderer*, by contrast, is the component we most need control over, given a console target (`ADR-0005`) and an unusual dense-field workload (`ADR-0003`). Taking the ECS and leaving the renderer gets the leverage without the constraint.

Writing our own ECS was rejected: it is months of work to reach parity, and the resulting performance advantage is speculative. The engine's real scale problem is dense fields, not entity iteration.

## Consequences

- Bound to `bevy_ecs`'s archetypal model. Structural change is expensive, hence the deferred-command discipline in S02.
- Bevy's scheduler defaults are not used; `cx-ecs` imposes phase-based ordering for determinism.
- Upgrades to `bevy_ecs` may change change-detection or scheduling semantics; the version is pinned and upgrades are treated as a milestone-gated task with a full determinism re-verification.
