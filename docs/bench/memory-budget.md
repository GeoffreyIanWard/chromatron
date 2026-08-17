# Memory Budget

Two profiles, both enforced in CI **from M0**. The min-spec profile is the binding constraint: 16 GB is a realistic desktop minimum and Steam Deck is 16 GB shared, so 8 GB for the process is the number that matters.

| Profile | Total | Notes |
|---|---|---|
| Desktop | 12 GB | Reference target |
| Min-spec | 8 GB | The CI gate that matters (`ADR-0010`) |

## Min-spec profile allocation

| Subsystem | Budget | Notes |
|---|---|---|
| Field storage (active chunks) | 2.4 GB | Dominant cost — see below |
| Field storage (coarse chunks) | 0.5 GB | 8x downsampled |
| Entity storage (1M entities) | 0.7 GB | ~700 B/entity across archetypes |
| World map (coarse layer) | 0.3 GB | Permanently resident |
| Statistical chunk aggregates | 0.2 GB | ~20 KB per chunk × 10k chunks |
| Flow network + water bodies | 0.1 GB | Graph edges plus body records; tiny by design (`ADR-0009`) |
| Meshes, textures, palette atlases | 1.4 GB | Includes baked terrain meshes (`ADR-0008`) |
| Render instance buffers | 0.4 GB | 1M instances × ~64 B, double-buffered |
| Spatial indices | 0.2 GB | Multiple indices, preallocated |
| Block generation working set | 0.8 GB | One in-flight block at 16,384² with halo; bounded by frontier concurrency |
| Scratch and staging | 0.5 GB | Preallocated per `03-conventions.md` |
| Audio, UI, misc | 0.2 GB | |
| Headroom | 0.3 GB | |

**Disk, not RAM**: the block cache (S07) holds generated blocks plus baked terrain meshes. It is disposable and not part of the save, but it does grow — budget on the order of 100–200 MB per generated block and cap the cache with LRU eviction.

## Why field quantization is mandatory

One chunk is 1024 × 1024 = 1,048,576 cells. At `f32`, a single field for a single chunk is **4 MB**.

Runtime field count dropped once erosion moved to generation (`ADR-0008`) and water stopped being simulated (`ADR-0009`), but generation-time working sets grew, so the pressure moved rather than vanishing.

| Field | Type | Lifetime | Rationale |
|---|---|---|---|
| `ELEVATION` | `f32` | Immutable after generation | Precision matters during erosion; frozen after |
| `SLOPE`, `ASPECT` | `u8` | Static, derived at generation | Quantized angles |
| `FLOW_DIR` | `u8` | Static | D8 direction index |
| `FLOW_ACCUM` | `f16` | Static | Drives discharge |
| `WATER_DEPTH` | `f16` | Derived, refreshed on level/tier change | Never accumulated (`ADR-0009`) |
| `FLOODPLAIN_TIER` | `u8` | Static | Precomputed extent masks per tier |
| `SOIL_MOISTURE` | `u16` | Dynamic | Normalized 0–1 saturation fraction |
| `TEMPERATURE` | `u16` | Dynamic | Quantized to 0.01 °C over a bounded range |
| `BIOMASS` | `u16` | Dynamic | |
| `BIOME` | `u8` | Static | Enum |
| `TRAVERSABILITY` | `u8` | Mostly static | Slope component baked; water/construction components dynamic |

Only four fields are stepped per tick. The rest are static or derived, which means they can live in a shared read-only mapping across chunks where values repeat, and they never need double buffering.

Every field declares its quantization error at registration (S06), so precision loss is a stated design fact rather than a discovered bug.

## Modules and memory

Budgets above assume the `game` profile. A disabled module's fields are **never allocated** (S20, `ADR-0012`), so leaner profiles have genuinely smaller footprints rather than merely idle ones:

| Profile | Approximate field storage | Freed by |
|---|---|---|
| `game` | 2.9 GB | — |
| `full-sim` | 2.5 GB | no render instance buffers or presentation |
| `hydro` | 1.6 GB | no ecology (`BIOMASS`), no agents |
| `terrain` | 1.1 GB | no climate, hydrology, or ecology fields |
| `minimal` | < 0.1 GB | terrain fields not registered at all |

`cx-diag` reports live memory per module, so a module that quietly grows shows up against its own line rather than a subsystem aggregate.

## How it is measured

Peak RSS from `/proc/self/status`'s `VmHWM` — the process high water mark, which is what a
budget is about. Allocator accounting was rejected: it misses page-level behaviour and
memory-mapped regions, and counts freed-but-unreturned arena space the OS may have reclaimed.

**Linux only, deliberately.** macOS and Windows equivalents need FFI and neither gates: this
file names Linux as the reference and the CI gates job runs on `ubuntu-latest`. On other
platforms the benchmark builds the same world and reports that it did not measure, rather
than passing quietly.

**The M0 measurement is the unquantized worst case.** Quantized element types are not
implemented yet (S06), so `memory_16_chunks_1m_entities` gates on four `f32` fields where the
table above assumes `u8`, `u16`, and `f16`. If that gate ever fails, implementing
quantization is the first lever — ahead of reducing `CELLS_PER_CHUNK_EDGE`.

## Enforcement

- CI runs every milestone benchmark under the min-spec profile with a hard RSS cap; exceeding it fails the build.
- `cx-diag` reports live memory by subsystem against these budgets, so drift is visible during development rather than at the gate.
- The active-chunk cap (S07) and the generation frontier concurrency are both derived from this budget, not chosen independently.
