---
id: M7
title: Persistence, Determinism & Tooling
specs: [S13, S14]
gate: bench/baselines.md#m7
---

# M7 — Persistence, Determinism & Tooling

Saving an infinite world, and building the instruments needed to trust any of it.

## Deliverables

- Save format: seed + config + chunk deltas + persistent entities, `postcard` + zstd, versioned header.
- Sparse delta encoding with per-field quantization thresholds.
- Reflection-based entity serialization with transient-component exclusion.
- Migration chain with fixture-based round-trip tests.
- Replay logs from the command stream.
- Incremental background autosave, write-ahead + atomic rename.
- Full `cx-diag`: state hashing, divergence bisector, invariant system, entity inspector, field inspector, query console, metrics with live charts, Tracy spans.
- Headless metric export to CSV/Parquet.

## Exit criteria

| Check | Target |
|---|---|
| 10,000 generated unmodified chunks | save < 100 KB |
| Save → load → save | byte-identical |
| Autosave of a 500 MB world | no frame > 20 ms |
| Replay of 100,000 ticks | final state hash exact |
| Process killed mid-save | previous save intact, fault-injection tested |
| State hash, 1M entities + 16M cells | < 2 ms |
| Divergence detector on injected non-determinism | located < 30 s |
| Inspector with 1M entities | frame < 16 ms |
