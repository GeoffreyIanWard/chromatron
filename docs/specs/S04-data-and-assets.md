---
id: S04
title: Data-Driven Content & Assets
status: not started
depends_on: [S01, S02]
provides: [definitions, prototypes, reflection-registry, hot-reload, asset-server, palette-atlas-pipeline]
crates_touched: [cx-data]
milestone: M3
---

# S04 — Data-Driven Content & Assets

The engine's "data-driven" requirement lives here: an author writes a RON file, and entities with components the Rust code never explicitly names come into existence.

## Requirements

- **Type registry** via `bevy_reflect`. Every component that may appear in content registers a name, a deserializer, and a schema description. Registration is a single macro at the component's definition site.
- **Definition files** in RON. Content is organized into *packs*, each a directory with a manifest declaring id, version, dependencies, and load order.
- **Prototypes**: a named template listing components with values. `spawn_prototype(id, overrides)` produces a configured entity. Prototypes support single inheritance (`extends:`), with a documented merge rule: child scalar fields replace, child list fields replace by default and append with an explicit `+` prefix.
- **Module awareness**: a pack referencing a component from a disabled module fails validation with a message naming the *module* and how to enable it, not just the unknown component (S20).
- **Validation** at load, not at spawn. A pack that references an unknown component, an out-of-range value, or a missing prototype fails to load with a message naming the file, line, column, and the closest valid alternative. Content authors must never see a Rust panic.
- **Override and merge across packs**: later packs may patch earlier ones by id. Conflicts between two packs at the same precedence are an error, not a silent last-wins.
- **Hot reload** in development: file watcher, revalidate, apply. Reloading a prototype affects newly spawned entities only; live entities are not retroactively mutated (that path is a debugging trap and is explicitly excluded).
- **Asset server**: `Handle<Mesh>`, `Handle<Texture>` etc. with async loading on a background pool, reference counting, and placeholder assets so a missing file degrades to a visible magenta marker rather than a crash.
- **Mesh pipeline**: glTF import, vertex deduplication, LOD chain generation (target ratios 1.0 / 0.5 / 0.2 / imposter), and **palette atlas** assignment — mesh UVs are rewritten to index a shared palette texture so nearly all geometry shares one material. This is the single largest lever on draw call count; see S12.
- Content is compiled to a binary cache keyed by content hash. Loading the cache must be at least 10x faster than parsing source.

## Non-goals

No visual editor. No runtime authoring. No backwards compatibility for content across engine versions (save compatibility is S13's problem, not content's).

## Acceptance criteria

- A component defined only in a pack file, never named in engine Rust code, spawns and simulates correctly.
- Every error class (unknown component, bad type, out of range, missing reference, circular `extends`) produces a message with file, line, and column, verified by a table-driven test.
- Loading 10,000 prototypes across 20 packs completes in under 200 ms from binary cache.
- Two packs both patching the same prototype at equal precedence produce a load error naming both files.
- A scene of 50,000 objects drawn from 200 distinct meshes resolves to fewer than 10 materials after palette atlas assignment.

## Open questions

- Whether prototypes need multiple inheritance. Single plus patching probably covers it; revisit if content authoring hits friction at M6.
