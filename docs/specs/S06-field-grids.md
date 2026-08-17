---
id: S06
title: Field Grids & Chunk Storage
status: partial
depends_on: [S01]
provides: [field-storage, kernels, halos, sampling, deposit-buffer]
crates_touched: [cx-fields]
milestone: M0
---

# S06 — Field Grids & Chunk Storage

The dense half of the architecture (`ADR-0003`). This is the spec most likely to be underestimated: it is where the engine's scale claim is actually won or lost, because field cells outnumber entities by two orders of magnitude.

## Requirements

- `FieldId(u16)` names each dense array. **Registered by a module** (S20) at startup with element type, default value, and persistence policy (`Regenerable`, `DeltaPersisted`, `Transient`). A field belonging to a disabled module is never allocated — disabling ecology genuinely frees `BIOMASS`, it does not merely stop stepping it.
- Consumers of fields they do not own use `optional_field(id)`, resolved once at schedule-build time: present, or a constant-valued read-only view with a documented default. Never a per-tick branch.
- **Derived fields**: a field may declare itself derived from other state, with a refresh function and a dirty flag. `WATER_DEPTH` is derived from water body level minus elevation (`ADR-0009`) and refreshes only on level or discharge-tier change — it is never stepped per tick.
- **`ELEVATION` has exactly two writers** (`ADR-0011`): the worldgen stage (S07) and `EditCommand` application (S19). No continuous process modifies it. It is registered `DeltaPersisted`, but the delta is a sparse cell list, so an unedited chunk still costs zero bytes.
- **Tile dirty tracking**: fields declare whether they participate in tile-granularity dirty tracking (64×64 cells, 256 per chunk). A bitset per chunk marks dirty tiles for consumers — mesh patching (S12), nav cost grids (S10), collider updates (S11). Cleared at end of tick after consumers have read it.
- Storage is **SoA per chunk**: one contiguous `Vec<f32>` (or `Vec<u8>`/`Vec<u16>` for quantized fields) per field per chunk. 1024×1024 cells = 1,048,576 elements. A single `f32` field for one chunk is 4 MB — quantization is not optional, see `bench/memory-budget.md`.
- **Field set** per chunk is dynamic: an unmodified chunk stores only the fields that differ from their generated value. Allocation is lazy on first write.
- **Halos**: each chunk's field array is allocated with a border ring (width configurable per kernel, default 1) copied from neighbors before the `FieldSolve` phase. Kernels then run with no bounds checks and no neighbor lookups across chunk boundaries. Halo exchange is its own sub-phase and is parallel.
- **Kernel API**: `fn kernel(input: &[f32], output: &mut [f32], stride: usize, range: Range<usize>)`. Flat slices, no branches in the inner loop, row-band parallelism. Double-buffered — kernels never read and write the same array.
- **Sampling** for entities: `fields.sample(FieldId, WorldPos) -> f32` with bilinear interpolation, and `sample_nearest` for discrete fields. Read-only, safe to call in parallel from `AgentSense`.
- **Deposit buffer**: entities write to fields only by pushing `(FieldId, CellCoord, f32, DepositOp)` into a per-thread buffer, drained deterministically in the `FieldDeposit` phase. Ops are `Add`, `Set`, `Max`. Deterministic combine order: sort by `(FieldId, cell_index, op)` before applying, so parallel producers cannot reorder the result.
- CPU reference implementation is authoritative. A GPU compute path may exist for generation-time work (S07 erosion) but must validate against CPU output in CI, because a block regenerated on different hardware must produce identical terrain.

## Non-goals

No physics of the fields themselves — the kernels that model water and erosion are S08. This spec is storage, layout, halos, and the execution harness.

## Acceptance criteria

- 16,000,000 cells stepped through a 5-point stencil in under 12 ms on 8 threads (M0 gate).
- Halo exchange for 16 loaded chunks under 1 ms.
- A field that has never been written allocates zero bytes.
- Deposit buffer produces bit-identical results across thread counts 1, 4, 16 over 10,000 ticks.
- Quantized `u8` field round-trips within its declared precision; the quantization error is reported at registration, not discovered later.
- Zero allocations per tick in the steady state.

## What is implemented

`f32` chunked SoA storage with lazy allocation, halo rings and exchange, the double-buffered
kernel harness, bilinear and nearest sampling, the deterministic deposit buffer, and
tile dirty tracking.

**Not yet**, and all needed before M4 rather than M0: quantized element types (`u8`/`u16`/`f16`)
with declared quantization error — the memory budget in `bench/memory-budget.md` assumes these
and is not reachable without them; derived fields with refresh-on-dirty (`WATER_DEPTH`);
`optional_field(id)` resolved at schedule-build time; and row-band parallelism within a chunk.

Measured at M0: 16M-cell 5-point stencil 2.52 ms against a 12 ms budget, halo exchange
for 16 chunks 100 µs against 1 ms.

## Open questions

- ~~f32 vs fixed-point for water depth.~~ Resolved by `ADR-0009`: water depth is derived, not accumulated, so drift is not possible.
- ~~Whether the runtime stencil workload leaves the 16M-cell M0 target over-provisioned.~~ Closed: keep the target. Headroom is not a problem, and M0 exists to find the ceiling rather than to confirm a comfortable floor.
