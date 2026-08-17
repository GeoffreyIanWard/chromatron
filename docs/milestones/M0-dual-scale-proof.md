---
id: M0
title: Dual Scale Proof (headless)
specs: [S01, S02, S03, S06]
gate: bench/baselines.md#m0
---

# M0 — Dual Scale Proof

**The purpose of this milestone is to try to break the architecture before building on it.** Both scale claims — a million sparse entities and sixteen million dense cells — are proven headless, with no window, no renderer, and no content pipeline. If the numbers do not land, we revise the architecture here, where it costs days rather than months.

## Deliverables

- Cargo workspace with the crate graph from `02-architecture.md` and the CI dependency firewall in place from day one.
- Note: the runtime dense-field workload shrank (`ADR-0008`, `ADR-0009` removed erosion and fluid hydrology from the tick). The 16M-cell target is retained anyway — ecology spread and soil-moisture diffusion still need it, and headroom is not a problem.
- `cx-core`: handles, arenas, interning, `RngStream`, `hash_position`, error types, config, tracing (S01).
- `cx-module`: `Module` trait, capability registry, order-independent resolution, startup validation, named profiles (S20). **This lands at M0 deliberately** — modularity retrofitted is modularity that does not work, and it makes the rest of M0 easier by letting the benchmarks run against the `minimal` profile.
- `cx-ecs`: phase-based `SimSchedule`, deferred `SimCommands`, bulk spawn, deterministic iteration helper (S02).
- `cx-time`: `TickClock`, `TimeControl`, `HeadlessDriver` (S03). No windowed driver yet.
- `cx-fields`: chunked SoA storage, halo exchange, kernel harness, sampling, deposit buffer (S06).
- `chromatron-cli` with a `bench` subcommand.
- `chromatron-bench` with criterion benchmarks wired into CI gates.
- Minimal `cx-diag`: state hashing and the determinism test harness. Needed now, because determinism bugs introduced here are cheapest to catch here.

## Exit criteria

All must pass in CI, on the desktop profile **and** the 8 GB min-spec profile:

| Check | Target |
|---|---|
| 1M entities, 4 components, 2-component query iteration | < 3 ms single-threaded |
| 1M entities, 3 systems, full tick | < 33 ms on 8 threads |
| 16M field cells, 5-point stencil | < 12 ms on 8 threads |
| Halo exchange, 16 chunks | < 1 ms |
| `spawn_batch` 100k vs `spawn` loop | ≥ 20x faster |
| Identical state hash across thread counts 1 / 4 / 16, 10,000 ticks | exact |
| Identical state hash in-process vs subprocess | exact |
| Allocations per tick, steady state | 0 |
| Module set registered in 10 shuffled orders | identical resolved schedule hash |
| Disabling a module | its per-tick cost and field allocations drop to zero, measured |
| Each module's own smoke profile | passes |
| Peak memory, 16 chunks + 1M entities | < 8 GB |

## If it fails

Stop. Do not proceed to M1. The likely revisions, in order of probability: quantize field element types more aggressively; reduce `CELLS_PER_CHUNK_EDGE`; move field solving to GPU compute earlier than planned; reconsider whether 1M entities is the right target versus 1M *simulated things* where most are statistical (S09) rather than ECS entities. Record whichever revision is taken as an ADR.
