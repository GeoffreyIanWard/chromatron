---
id: S20
title: Module System & Composition
status: implemented
depends_on: [S01, S02]
provides: [module-trait, capabilities, composition, profiles, graceful-degradation]
crates_touched: [cx-core, cx-ecs, cx-sim]
milestone: M0
---

# S20 — Module System & Composition

Every subsystem is a **module** that can be enabled, disabled, replaced, or developed in isolation (`ADR-0012`). This lands at M0 because modularity retrofitted is modularity that does not work — by M4 there would be a dozen systems reaching directly into each other's data.

## Module trait

A module declares, in one place:

- **Identity**: stable `ModuleId`, version.
- **Capabilities provided** — what other modules may rely on it for.
- **Capabilities required** — hard dependencies; a missing one is a startup error.
- **Capabilities consumed optionally** — soft dependencies with a documented default when absent.
- **Registrations**: components, fields (S06), resources, content schemas (S04), tick systems with phase and relative ordering, generation pipeline stages (S07), and diagnostics.

```rust
impl Module for HydrologyModule {
    const ID: ModuleId = ModuleId("hydrology");
    fn provides() -> &'static [Capability] { &[cap::SURFACE_WATER, cap::FLOW_NETWORK] }
    fn requires() -> &'static [Capability] { &[cap::TERRAIN, cap::CLIMATE] }
    fn consumes_optional() -> &'static [Capability] { &[cap::TERRAIN_EDIT] }
    fn register(r: &mut Registry) { /* fields, systems, schemas */ }
}
```

## Capabilities, not module references

Modules never name each other. Navigation does not depend on `hydrology`; it optionally consumes `cap::SURFACE_WATER`. This is the mechanism that makes disabling things safe:

| Capability absent | Consumer behavior |
|---|---|
| `SURFACE_WATER` | Nav cost grid omits its water component; traversability computed from slope and construction only |
| `ECOLOGY` | Agents that forage find no resources; foraging behaviors are unschedulable and report so at startup |
| `TERRAIN_EDIT` | Terrain has exactly one writer (generation); dirty-tile tracking is not allocated |
| `CLIMATE` | Hydrology uses a constant precipitation value from config |
| `PHYSICS` | `HasPhysics` entities exist but do not step; colliders are not built |

**Degradation is resolved at schedule-build time, not per tick.** If a capability is absent, the consuming system is either not scheduled at all or is scheduled in a null-provider variant chosen once at startup. There is no `if let Some(water)` in a hot loop, and a disabled module costs exactly zero per-tick time and zero memory.

## Two composition points

Modules compose into **two** graphs, not one:

1. **The tick schedule** — systems inserted into the fixed phases from `02-architecture.md`.
2. **The generation pipeline** — stages inserted into block generation (S07). Erosion is a *generation stage*, not a tick system, so making erosion toggleable requires this second composition point. So does thermal erosion, channel carving, scatter placement, and biome assignment.

Both use the same dependency resolution.

## What is *not* pluggable

The **twelve tick phases are fixed**. Modules insert systems into phases with relative ordering constraints; they cannot add, remove, or reorder phases. This is deliberate: the phase list is the ordering contract that makes parallel execution safe and results order-independent. If phases were composable, determinism would depend on module load order, and the read-then-write discipline in `02-architecture.md` would have no stable meaning.

Likewise fixed: `cx-core` primitives, the ECS wrapper, the tick clock, and the field storage layer. Modules build on these; they do not replace them.

## Resolution and determinism

- The module graph resolves by topological sort with a **stable tiebreak by `ModuleId`**. Registration order must not affect the resulting schedule — verified by a test that registers the same set in shuffled orders and compares the resolved schedule hash.
- **The module set is part of world identity.** Same seed, erosion on vs off, is a different world. The set — module IDs, versions, and the resolved config hash — is recorded in saves (S13) and replay logs. Loading a save with a mismatched set reports the differences and refuses rather than silently producing divergence.
- State hashes (S14) cover registered components and fields only, so hashes are comparable only within an identical module set. This is documented, not implied.

## Startup validation

All of these fail at startup with a message naming the module and the problem, never at tick 50,000:

- A required capability with no provider.
- A dependency cycle.
- Two modules exclusively providing the same capability (e.g. two terrain sources).
- A module registering a field or component name already taken.
- A version constraint between modules unsatisfied.

## Profiles

Testing every subset is combinatorial and pointless. Instead, named **profiles** are curated module sets, defined in content and gated in CI:

| Profile | Contents | Purpose |
|---|---|---|
| `minimal` | core, ecs, time, fields | M0 benchmarks; fastest iteration |
| `terrain` | + worldgen, erosion, render | Worldgen and meshing work |
| `hydro` | + climate, hydrology | Water behavior without agents |
| `full-sim` | all simulation modules, headless | Batch runs, parameter sweeps |
| `game` | everything including presentation | Shipping configuration |
| `no-erosion` | `full-sim` minus the erosion generation stage | Proves the toggle works end to end |

Additionally, each module gets a smoke profile of *itself plus its required dependencies only*, run in CI. That catches the common failure where a module quietly relies on something it never declared.

## Consequences worth accepting knowingly

- **Cross-module invariants weaken.** An invariant spanning two modules (S14) must itself be conditional on both being present, and must declare that.
- **Content must be module-aware.** A pack referencing a component from a disabled module fails validation with a message naming the module, not the component (S04).
- **Runtime composition, not cargo features** (`ADR-0012`). All modules are compiled in and enabled by config or scenario. Cargo features are reserved for genuinely heavy optional dependencies — rapier, kira — where the build-time saving is real.

## Acceptance criteria

- Registering the same module set in 10 shuffled orders produces an identical resolved schedule hash.
- Disabling a module removes its per-tick cost and its field allocations entirely, verified by benchmark and memory report — not merely branched over.
- Every capability-absent degradation in the table above is covered by a test running the consumer without the provider.
- Every startup validation failure produces a message naming the module and the specific problem, table-driven test.
- Each module's own smoke profile passes in CI.
- The `no-erosion` profile produces a valid, playable world differing from `full-sim` only in terrain shape.
- Saves record the module set; loading with a mismatch refuses and lists the differences.
- A module can be developed against `minimal` plus its own dependencies without compiling or running the rest.

## What is implemented

The `Module` trait, capabilities, order-independent resolution, the five startup
validations, profiles, and the degradation declaration — plus a sixth validation the spec
implied but did not list: consuming a capability optionally without declaring the absent
behavior fails to resolve.

**Not yet**: the generation-pipeline composition point (needs S07), per-module smoke
profiles in CI (needs real modules), and version constraints between modules.

## Open questions

- ~~Whether modules should be able to *replace* a capability provider or only add.~~
  Decided: **add-only**, with two modules exclusively providing the same capability being a
  startup error that names both. Replacement is more powerful and more dangerous, and
  nothing in the doc set currently wants a second implementation of anything. Revisit if
  and when one actually exists — a real alternative implementation is a far better guide to
  the right semantics than a hypothetical one.
