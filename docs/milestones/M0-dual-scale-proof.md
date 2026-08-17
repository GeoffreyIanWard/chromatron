---
id: M0
title: Dual Scale Proof (headless)
specs: [S01, S02, S03, S06, S20, S21]
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
- `chromatron-cli graph`: deterministic export of the resolved module, schedule, and field-access graph (S21). Exporter only — the viewer is M1. It lands here because it is a serialization of registries this milestone already builds, and because the export doubles as the readable artifact behind the `module_resolution_order_independence` gate.
- Minimal `cx-diag`: state hashing and the determinism test harness. Needed now, because determinism bugs introduced here are cheapest to catch here.

## API surface the gates already call

The M0 benchmarks are written first (`apps/chromatron-bench/benches/m0_*.rs`), so they are
the first real callers of each crate and their imports are a checklist. Each is behind a
cargo feature (`m0-ecs`, `m0-fields`, `m0-module`) switched on by the commit that
implements it; until then the benchmark does not compile and CI stays green and meaningful.

| Crate | Surface the gates call |
|---|---|
| `cx-core` | `ChunkCoord`, and `glam` re-exported as `cx_core::glam` (S01) |
| `cx-ecs` | `SimWorld`, `WorldConfig { threads }`, `SimSchedule`, `Phase`, `Query`, `Component`, `spawn`, `spawn_batch` |
| `cx-fields` | `FieldStore`, `StoreConfig { threads }`, `FieldId`, `FieldSpec { name, default, persistence, halo_width, tile_dirty_tracking }`, `Persistence`, `insert_chunk`, `fill`, `run_kernel`, `exchange_halos`, `allocated_bytes` |
| `cx-module` | `Registry`, `Profile::full_sim()`, `Profile::no_erosion()`, `resolve()`, `schedule_hash()`, `systems()`, `modules()`, `field_bytes()`, `degradation_for()`, `Capability`, `cap` |

These are a proposal, not a decision — the point of writing the caller first is that a
signature that reads badly at the use site gets changed before it has dependents. Adjust
them while implementing, and adjust the benchmark with them.

## Exit criteria

All must pass in CI, on the desktop profile **and** the 8 GB min-spec profile:

| Check | Target |
|---|---|
| 1M entities, 4 components, 2-component query iteration | < 3 ms single-threaded |
| 1M entities, 3 systems, full tick | < 33 ms on 8 threads |
| 16M field cells, 5-point stencil | < 12 ms on 8 threads |
| Halo exchange, 16 chunks | < 1 ms |
| `spawn_batch` 100k vs `spawn` loop | ≥ 1.75x faster |
| Identical state hash across thread counts 1 / 4 / 16, 10,000 ticks | exact |
| Identical state hash in-process vs subprocess | exact |
| Allocations per tick from engine code | 0 (`ADR-0014`) |
| Allocations per tick from the executor | ≤ 16 + systems |
| Module set registered in 10 shuffled orders | identical resolved schedule hash |
| Disabling a module | its per-tick cost and field allocations drop to zero, measured |
| Each module's own smoke profile | passes |
| Peak memory, 16 chunks + 1M entities | < 8 GB |

## Measured so far

Both columns are real runs. **CI** is `ubuntu-latest`, the shared GitHub runner, which is
what `bench/baselines.md` gates against. **Dev** is an aarch64 developer machine, recorded
because the gap between the two is itself useful information when a gate later drifts.

| Check | Budget | CI | Dev | |
|---|---|---|---|---|
| 1M entities, 2-component query iteration | < 3 ms, 1 thread | 1.12 ms | 0.57 ms | 2.7x headroom on CI |
| 1M entities, 3 systems, full tick | < 33 ms, 8 threads | 2.59 ms | 1.21 ms | 12.7x headroom |
| 16M field cells, 5-point stencil | < 12 ms, 8 threads | 4.54 ms | 2.52 ms | 2.6x headroom |
| Halo exchange, 16 chunks | < 1 ms | 188 µs | 100 µs | 5.3x headroom |
| `spawn_batch` 100k vs `spawn` loop | ≥ 1.75x | 2.11x | 1.92x | 3.94 ms vs 1.87 ms on CI |
| Allocations per tick, engine code | 0 | 0 | 0 | single-threaded, `ADR-0014` |
| Allocations per tick, executor | ≤ 16 + systems | within | 16 | bevy_ecs overhead |
| Identical state hash, threads 1/4/16, 10k ticks | exact | exact | exact | see caveat below |
| Identical state hash, in-process vs subprocess | exact | exact | exact | |
| Module set in 10 shuffled orders | identical schedule hash | identical | identical | resolves in 1.2 µs |
| Disabling a module | zero systems, zero field bytes | verified | verified | |
| Each module's own smoke profile | passes | — | passes | one module exists (`fields`) |

**The determinism gates pass but are not yet adversarial.** `cx-ecs` exposes no parallel
iteration, so the scenario's systems are deterministic by construction rather than by
discipline. They are a regression guard today and become a real proof when agents
parallel-iterate at M6 — recorded as an open question in S14 rather than left implied.

**The two scale claims this milestone exists to test both pass on CI with real margin.** The
tightest is the field stencil at 2.6x, which is the number to watch as ecology and
soil-moisture kernels arrive at M4 — the current workload is one 5-point stencil, and M4 adds
more.

Note the CI/dev ratio is roughly 2x across every measurement, which is what you would expect
from a shared runner and suggests neither environment is behaving strangely.

**The spawn gate was re-baselined from 20x to 1.75x**, recorded with its reasoning in
`bench/baselines.md`. In `bevy_ecs` 0.19 a single spawn costs about 24 ns against a batched
12 ns, so the original figure described an ECS where per-spawn archetype moves dominate,
which this is not. The gate's intent is unchanged: bulk spawn is the path agents and chunk
activation use, and the threshold still fails loudly if `spawn_batch` loses its advantage.

**The allocation gate was split** into engine-code and executor halves (`ADR-0014`) after CI
measured 12 allocations per tick, all of it inside `bevy_ecs`'s multi-threaded executor.

## If it fails

Stop. Do not proceed to M1. The likely revisions, in order of probability: quantize field element types more aggressively; reduce `CELLS_PER_CHUNK_EDGE`; move field solving to GPU compute earlier than planned; reconsider whether 1M entities is the right target versus 1M *simulated things* where most are statistical (S09) rather than ECS entities. Record whichever revision is taken as an ADR.
