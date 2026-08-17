# ADR-0012 — Runtime module composition, with capabilities instead of direct dependencies

**Status:** accepted · **Date:** 2026-08-16

## Context

Subsystems need to be independently toggleable — erosion on or off, hydrology present or not, physics excluded from headless runs — and independently developable, so refining one does not disturb the others.

The naive implementation is a configuration boolean per subsystem, checked at the call site. That does not deliver either property: the coupling is still direct, the disabled code still costs branches and allocations, and a subsystem still breaks when something it silently assumed is absent.

## Decision

Every subsystem is a **module** (S20) declaring identity, provided capabilities, required capabilities, optionally-consumed capabilities, and its registrations.

**Modules depend on capabilities, never on other modules.** Navigation does not depend on `hydrology`; it optionally consumes `cap::SURFACE_WATER` and has a documented behavior when that capability has no provider.

**Degradation resolves at schedule-build time**, not per tick. An absent capability means the consuming system is not scheduled, or is scheduled in a null-provider variant selected once at startup. A disabled module costs zero ticks and zero bytes.

**Composition is runtime, not cargo features.** All modules compile in and are enabled by config or scenario. Cargo features are reserved for heavy optional dependencies — rapier, kira — where the build-time saving is real.

**Two composition points**: the tick schedule and the block generation pipeline (S07). Erosion is a generation stage rather than a tick system, so toggling it requires the second graph.

## Rationale

Capability indirection is what converts "disabled" from a hazard into a supported configuration. With direct module references, disabling hydrology means every consumer either crashes or carries an untested branch; with capabilities, each consumer has declared in advance what it does without water, and CI runs that configuration.

Runtime composition over cargo features because the feature-flag approach produces a combinatorial build matrix, makes A/B comparison require two binaries, and puts `#[cfg]` throughout the codebase. Runtime resolution costs a startup graph solve and nothing else.

## Consequences

- The **twelve tick phases stay fixed and are not composable.** They are the ordering contract that makes parallel execution safe; if modules could reorder phases, determinism would depend on load order. This is where modularity deliberately stops.
- **The module set becomes part of world identity.** Same seed with erosion on and off are different worlds. The set is recorded in saves and replays; a mismatch on load refuses rather than diverging silently.
- **State hashes are comparable only within an identical module set** (S14).
- Cross-module invariants must declare their own preconditions and skip when unmet.
- Content packs referencing components from disabled modules fail validation with a module-level message (S04).
- Testing moves from "test everything together" to **named profiles** plus a per-module smoke profile of itself and its declared dependencies — which is what catches undeclared reliance.
- Module resolution must be order-independent: topological sort with a stable tiebreak by `ModuleId`, verified by shuffled-registration tests.
