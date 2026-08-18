---
id: M1
title: Loop & Pixels
specs: [S03, S12, S21]
gate: bench/baselines.md#m1
---

# M1 — Loop & Pixels

First frame on screen, and — more importantly — the first proof that the sim/view split and interpolation actually produce smooth motion from a 30 Hz simulation. That is the real test; drawing cubes is the easy part.

## Deliverables

- `cx-render` using wgpu directly, with the crate boundary enforced by CI (S12, `ADR-0010`).
- `cx-view`: view world, extract phase, `Transform`/`PreviousTransform` interpolation.
- `WindowedDriver` with frame pacing, spiral-of-death guard, pause/step/speed controls (S03).
- Instanced indirect draw path with a hand-authored palette atlas (the full asset pipeline is M3).
- GPU frustum culling compute pass.
- Debug draw: lines, spheres, AABBs, arrows, world-space text.
- egui overlay with tick counter, frame graph, and time controls.
- Free-fly camera.
- `tools/graph-viewer`: the isometric architecture viewer over the M0 graph export, with stable layout and `--baseline` diff (S21). Independent of the renderer above — it is a static page, not engine code — but it lands here because M1 is the first milestone with enough modules and systems registered for the diagram to be worth looking at.
- **Tile layout fixed** (`TILE_CELLS = 64`, 256 tiles per chunk) and dirty-tracking bitsets stubbed in. Mesh layout depends on this and M4B cannot retrofit it (`ADR-0011`). Safe to fix now that `ADR-0013` settled terrain representation as a heightfield.

## Exit criteria

| Check | Target |
|---|---|
| 100,000 instanced meshes at 60 fps | < 20 draw calls |
| 30 Hz sim, 144 Hz render, moving instances | 99th-pct frame time < 8 ms, no visible stutter |
| Extract 100,000 visible instances | < 2 ms |
| Debug draw 10,000 lines | < 1 ms |
| Headless vs windowed state hash, 10,000 ticks | identical |
| wgpu types outside `cx-render` | zero, CI enforced |
| Pause → step 5 → resume vs run 5 continuously | identical state |

## Measured so far

| Check | Budget | Dev | |
|---|---|---|---|
| `extract_100k_instances` | < 2 ms | 626 µs | 3.2x headroom |
| `render_100k_instances_fps` — draw-call clause | < 20 | **1** | instancing; runs anywhere |
| `render_100k_instances_fps` — fps clause | ≥ 60 fps | not measured | needs hardware, see below |

## Open question: three M1 gates need a GPU that CI does not have

`render_100k_instances_fps`, `frame_time_p99_30hz_sim_144hz_render`, and
`debug_draw_10k_lines` all need a working graphics device. GitHub's standard runners have
none, and the milestone rule says a milestone is not complete until its benchmarks pass in
CI. As written, M1 cannot complete.

Three ways out, and they are not exclusive:

1. **Software rasterizer in CI** (lavapipe for Vulkan on Linux). wgpu runs against it, so the
   pipeline builds, draws, and reports draw-call counts — but frame rates from a software
   rasterizer are meaningless, so only the *correctness* half of these gates would move.
2. **A self-hosted runner with a GPU.** Real numbers, real cost and maintenance.
3. **Split the gates**: correctness in CI (draw-call count, zero validation errors, no
   pipeline rebuilds per frame), and frame rate measured on a named developer machine and
   recorded in `bench/baselines.md` the way the CI/dev columns already are for M0.

Leaning 1 plus 3: run everything that can be checked without a real GPU in CI, and treat
frame rate as a recorded measurement against reference hardware rather than a gate a shared
runner could ever honestly enforce. That keeps the gate rule meaningful instead of quietly
exempting the rendering work from it.

**Option 1 is now demonstrably viable.** `cx-render` acquires a device, draws, and reads
pixels back with no window; the draw-call clause of `render_100k_instances_fps` is asserted
as an ordinary test (`crates/cx-render/tests/draw_calls.rs`) and passes in 0.24 s. So the
split is not hypothetical: the half that needs no hardware already runs everywhere, and the
outstanding decision is narrowed to how the *frame rate* half gets measured.

**This needs deciding before the renderer lands**, because it determines whether S12 is
written to be testable headlessly or not.

## Notes

The stutter criterion is the one to take seriously. A 30 Hz sim rendered at 144 Hz without correct interpolation looks obviously wrong, and the fix is architectural rather than a tuning pass. If motion is not smooth here, the extract contract is broken.
