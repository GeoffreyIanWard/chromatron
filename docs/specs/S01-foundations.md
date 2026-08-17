---
id: S01
title: Foundations
status: implemented
depends_on: []
provides: [ids, handles, arenas, rng, error-types, math, config, logging]
crates_touched: [cx-core]
milestone: M0
---

# S01 — Foundations

Primitives every other crate builds on. Small, boring, and load-bearing; get it wrong and every spec inherits the mistake.

## Toolchain pins

Pinned exactly rather than by range, because `ADR-0004` makes the build part of the
determinism contract. Each bump is a deliberate task ending in a determinism re-verify
(threads 1/4/16 plus subprocess), not a routine dependency update.

| Pin | Value | Notes |
|---|---|---|
| Rust | `1.97.1` | `rust-toolchain.toml` at the repo root; exact channel, never `stable`. Edition 2024. |
| `bevy_ecs` | `=0.19.1` | Exact requirement, per `ADR-0001`. Requires Rust ≥ 1.95. |

No MSRV window is published — this is not a library for other people's projects
(`01-scope.md`), so the pinned version *is* the supported version.

## Requirements

- `Handle<T>` — generational index (`u32` index + `u32` generation), `Copy`, 8 bytes, no pointer. Backed by a slot map with free-list reuse.
- `Arena<T>` — dense storage addressed by `Handle<T>`, with stable iteration order.
- Interned strings: `Id(u32)` with a global intern table populated at load. Content files use strings; runtime uses `Id`. Interning is order-independent — the table is sorted and rebuilt deterministically at load end.
- `RngStream` — PCG64 or SplitMix64, constructed as `RngStream::new(world_seed, StreamId, Tick)`. `StreamId` is an enum, one variant per system that draws randomness. No global RNG exists anywhere in the codebase.
- `hash_position(seed, chunk, field, index) -> u64` — the positional hash used by all worldgen (`ADR-0006`). Must be fast (a few ns) and avalanche well; wyhash or xxh3 finalization over a packed key.
- `Fixed` — the sim-side time delta type. Wraps microseconds; converts to `f32` explicitly so accidental frame-time use is visible in review.
- Error types: `CoreError` via `thiserror`. A `Located<E>` wrapper carrying file/line/column for loader errors.
- Config: layered load (defaults → file → env → CLI), typed structs, validated at startup reporting all errors at once rather than the first.
- Logging: `tracing` with structured fields. A `tick` span wraps each tick; every sim log carries the tick number.
- Math: re-export `glam` and add `ChunkCoord`, `CellCoord`, `WorldPos` newtypes with the conversions defined in `03-conventions.md`.

## Non-goals

No allocator work, no custom collections beyond the arena, no serialization (that is S13).

## Acceptance criteria

- `Handle<T>` is 8 bytes and `Copy`; a freed-then-reused slot does not validate against the stale handle.
- Interning the same set of strings in two different orders produces identical `Id` assignments.
- `hash_position` produces identical output on x86-64 and aarch64 for a fixed test vector.
- Two `RngStream`s with identical construction args yield identical sequences; different `StreamId`s yield uncorrelated sequences (chi-squared over 10^6 draws).
- Config validation reports every error in a malformed file, not just the first.

## Open questions

- ~~Whether `Id` interning must survive across saves or can be rebuilt at load.~~ Resolved
  at implementation: rebuild-at-load, and the type system now enforces it. `Interner`
  *stages* strings in any order and `freeze()` assigns ids by sorted position, so no `Id`
  exists before the full set is known and an order-dependent id cannot be observed. Saves
  must therefore store strings, not `Id`s — **confirm against S13 before persistence work
  begins.** `SymbolTable::content_hash` exists for a load to verify it interned the same set.
- ~~`hash_position`'s cross-architecture criterion is only half-testable locally.~~ Not
  actually open: `macos-latest` is aarch64 (macos-14 and later), so the existing CI matrix
  already runs the pinned vector on x86-64 (Linux, Windows) **and** aarch64 (macOS). The
  same constants passing on both *is* the cross-architecture proof the criterion asks for.
  No extra runner needed. Confirm on the first green CI run rather than assuming.
- ~~Whether `Config`'s flat dotted-key model is sufficient.~~ Decided: keep it flat. A TOML
  array becomes a comma-joined string, which covers what S20 and S04 need — lists of module
  ids and content paths. An array of tables would need a different representation; revisit
  then, not before.
