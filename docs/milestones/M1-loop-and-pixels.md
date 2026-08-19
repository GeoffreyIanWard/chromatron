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
| `frame_time_p99_30hz_sim_144hz_render` | < 8 ms | recorded per device, see below | not gated in CI — see below |

## Resolved: how the GPU-dependent gates are measured

`render_100k_instances_fps`, `frame_time_p99_30hz_sim_144hz_render`, and
`debug_draw_10k_lines` need a graphics device, and GitHub's standard runners have no GPU.
Rather than exempt rendering from the gate rule, each gate is **split by what can be
measured honestly where**:

| Clause | Where it runs | Why |
|---|---|---|
| Draw-call counts, pixel correctness, zero validation errors | CI, on lavapipe | Hardware-independent properties. Identical on a laptop, a runner, and a workstation. |
| ~~CPU-side frame cost~~ | ~~CI~~ | **This turned out not to be separable** — see the section below. Submit backpressure puts GPU cost into any wall-clock frame measurement, so frame time is recorded per device instead. |
| Absolute frame rate | Named reference hardware, recorded in `bench/baselines.md` | A number from a software rasterizer is not comparable to a GPU. Recording it with its hardware is the same pattern M0's CI/dev columns already use. |

A self-hosted GPU runner was rejected rather than deferred: this repository is public, so a
self-hosted runner would let anyone opening a pull request execute code on that machine.
That is a security tradeoff, not a cost tradeoff, and it is not worth making for a frame-rate
number that can be recorded by hand.

**Every CI runner turned out to have an adapter.** With `CX_REQUIRE_GPU=1` set — which makes
a missing adapter a failure rather than a skip — Ubuntu, macOS, and Windows all pass. So the
renderer's correctness tests genuinely execute on all three platforms, not just the one where
lavapipe is installed deliberately. That was an open empirical question, and the answer means
the requirement does not need narrowing to a subset.

**Skipping is no longer silent.** Renderer tests skip when no adapter exists, so a bare
container stays usable — but cargo swallows a passing test's output, which meant a green CI
run was indistinguishable from one that rendered nothing. CI now sets `CX_REQUIRE_GPU=1`,
which turns a missing adapter into a failure. Locally the variable is unset and the skip
still applies.

## Frame time is recorded, not gated — and here is why

The first attempt made this a CI gate on the assumption that skipping the pixel readback
made it a CPU-only measurement. **That assumption is false**, and CI proved it by failing on
all three platforms at once:

| Device | p99 | median |
|---|---|---|
| Apple M4 Pro (Metal) | 3.5 ms | 2.4 ms |
| lavapipe (Ubuntu runner, software Vulkan) | 30.0 ms | 23.0 ms |
| macOS runner | 82.0 ms | 5.8 ms |
| WARP (Windows runner, software D3D12) | 105.4 ms | 100.2 ms |

`queue.submit` applies backpressure: when the GPU cannot keep up, submit blocks, and the
GPU's cost lands in the caller's wall-clock timing. On fast hardware that is invisible. On a
software rasterizer it dominates — WARP's 100 ms median is it rasterizing 120,000 triangles,
not this project's code.

Isolating the genuinely hardware-independent part does not rescue a gate either: sim and
extract alone measure p99 2.2 ms on an M4 Pro, and a runner 2–4x slower would land at 4–9 ms
against any 8 ms budget. There is no threshold there that is both meaningful and not flaky.

So frame time follows the same rule as the other hardware-dependent numbers: **recorded with
the device that produced it**. What the test asserts instead is hardware-independent — that a
30 Hz simulation rendered at 144 Hz ticks roughly one frame in five (measured 62/300 = 20.7%),
and that a tickless frame still extracts every entity. Both are real bugs when they break,
and both are catchable on any machine.

## Known inefficiency in the frame path

`InstancedRenderer::render` currently creates the colour target, the depth target, and the
instance buffer **every frame**. That is wasteful, and it is part of what the recorded frame
costs above are paying for.

Pooling those resources is the obvious fix and is deliberately not done yet: the loop needed
to exist before optimising it was meaningful. Worth doing before instance counts approach the
100,000 the render gate names — and the recorded per-device numbers are the before-picture to
compare against.

A per-frame **allocation count** would be a genuinely hardware-independent gate for this,
in the same shape as `alloc_per_tick_sim_code` (`ADR-0014`). It is not added yet because it
would fail immediately against the behaviour described above; it belongs with the pooling
work that makes it passable.

## Notes

The stutter criterion is the one to take seriously. A 30 Hz sim rendered at 144 Hz without correct interpolation looks obviously wrong, and the fix is architectural rather than a tuning pass. If motion is not smooth here, the extract contract is broken.
