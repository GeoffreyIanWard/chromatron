---
id: S13
title: Persistence
status: not started
depends_on: [S01, S02, S04, S06, S07]
provides: [snapshots, chunk-deltas, migrations, replay-logs, autosave]
crates_touched: [cx-persist]
milestone: M7
---

# S13 — Persistence

An infinite world cannot be saved by serializing everything. Saves store the seed plus what diverged from it.

## Requirements

- **Save = seed + config + deltas + persistent entities + replay metadata.** An untouched world is a few kilobytes regardless of how much of it has been generated.
- **Chunk delta format**: per field, either "identical to generated" (zero bytes) or a delta payload, run-length or sparse-index encoded.
- **`ELEVATION` is `DeltaPersisted` as a sparse cell list** (`ADR-0011`). Terrain never changes on its own (`ADR-0008`), so the only elevation deltas are discrete authored edits — a sparse index list, not a diffuse field-wide drift. An unedited chunk costs zero bytes; a heavily terraformed one costs proportionally to what was actually changed.
- **Edited chunks are pinned**: a chunk with edits can never be treated as purely regenerable, and must restore its delta on rehydration *before* fast-forward runs (S09).
- **Water is not persisted as field data** (`ADR-0009`): infinite body levels and the flow network regenerate; only finite-body entities, impoundments, and flow-network graph deltas from terrain edits are saved.
- The **block cache** (S07) is explicitly *not* part of the save. It is disposable and rebuildable, and must never be required to load a save.
- **Entity serialization** via `bevy_reflect` (S04's registry). Components opt in with a persistence attribute; transient components (cached paths, scratch state) are excluded and recomputed on load.
- **The module set is part of world identity** (`ADR-0012`). Saves record module IDs, versions, and the resolved config hash. Loading with a mismatched set reports the differences and refuses, rather than diverging silently — the same seed with erosion on and off is a different world.
- **Versioned schema with migrations**: a save carries an engine version and a content version. Migrations are ordered, tested functions from version N to N+1. A save from an unsupported old version fails with a clear message rather than loading corrupt state.
- **Replay logs**: seed + config + the ordered command stream from `IntakeCommands`. Replaying reproduces a run exactly (subject to `ADR-0004`'s same-build constraint). Replays are the primary bug-reporting artifact — a bug report is a seed and a command log, not a video.
- **Incremental autosave**: only dirty chunks are rewritten. Autosave runs on a background thread against a consistent snapshot; the sim must not stall for the duration of a write.
- **Crash recovery**: write-ahead to a temp file, atomic rename. A crash mid-save never corrupts the previous save.
- Format: `postcard` + zstd behind a versioned header. A debug mode emits RON for inspection.
- **Interned string safety**: saves store strings, never `Id(u32)`. Confirmed against S01 as
  implemented: `Interner` stages strings and `freeze()` assigns ids by sorted position, so
  ids exist only after the full set is known and are rebuilt every run. A save that stored ids
  would break the moment content changed. `SymbolTable::content_hash` lets a load verify it
  interned the same set as the save did.

## Non-goals

No cloud saves. No cross-engine-version save compatibility beyond the migration chain. No save scumming protection.

## Acceptance criteria

- A world with 10,000 generated but unmodified chunks saves in under 100 KB.
- Save → load → save produces byte-identical output.
- A chunk modified, saved, loaded, and compared matches by field hash.
- Autosave of a 500 MB world causes no frame exceeding 20 ms.
- A replay of 100,000 ticks reproduces the final state hash exactly.
- A process killed mid-save leaves the previous save intact and loadable, verified by a fault-injection test.
- Each migration has a round-trip test against a checked-in fixture save.

## Open questions

- Per-field quantization thresholds for the remaining delta-persisted fields (biomass, soil moisture). Much less pressing now that elevation is regenerable, but still worth measuring at M7.
