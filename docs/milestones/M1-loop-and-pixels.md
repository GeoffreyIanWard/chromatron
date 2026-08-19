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
- ~~`WindowedDriver`~~ **done** — `cx-app`'s windowed driver, with frame pacing, spiral-of-death guard, and pause/step/speed controls (S03).
- Instanced indirect draw path with a hand-authored palette atlas (the full asset pipeline is M3).
- GPU frustum culling compute pass.
- Debug draw: lines, spheres, AABBs, arrows, world-space text.
- egui overlay with tick counter, frame graph, and time controls.
- ~~Free-fly camera~~ **done** — `cx-app::flycam`, with the floating origin following it.
- `tools/graph-viewer`: the isometric architecture viewer over the M0 graph export, with stable layout and `--baseline` diff (S21). Independent of the renderer above — it is a static page, not engine code — but it lands here because M1 is the first milestone with enough modules and systems registered for the diagram to be worth looking at.
- **Tile layout fixed** (`TILE_CELLS = 64`, 256 tiles per chunk) and dirty-tracking bitsets stubbed in. Mesh layout depends on this and M4B cannot retrofit it (`ADR-0011`). Safe to fix now that `ADR-0013` settled terrain representation as a heightfield.

## Exit criteria

| Check | Target |
|---|---|
| 100,000 instanced meshes at 60 fps | < 20 draw calls |
| 30 Hz sim, 144 Hz render, moving instances | 99th-pct frame time < 8 ms, no visible stutter |
| Extract 100,000 visible instances | < 2 ms |
| Debug draw 10,000 lines | < 1 ms |
| Headless vs windowed state hash, 10,000 ticks | identical — **met** |
| wgpu types outside `cx-render` | zero, CI enforced |
| Pause → step 5 → resume vs run 5 continuously | identical state — **met** |

## The window, and what it found

Measured on the developer machine (Apple M4 Pro, Metal), 400 instances at
2560x1440:

| | |
|---|---|
| Render rate | **120 fps**, vsync-locked (`Fifo`) |
| Sim rate | **exactly 30 ticks/second**, held over minutes |
| Draw calls | **1** |

So the milestone's headline criterion — a 30 Hz simulation drawn at the display's
rate — is demonstrated, at 4 render frames per tick.

**Everything testable landed before the window did, and the window still found two
bugs neither offscreen tests nor CI could have.** Both were fatal, and both were
about the difference between a texture you allocate and a surface someone else
hands you:

1. **The device asked for a texture limit smaller than a window.**
   `Limits::downlevel_defaults()` caps textures at 2048; a 1280x720 window on a 2x
   display is a 2560x1440 surface. Configuring it was a validation error. The device
   now takes its *resolution* limits from the adapter (`using_resolution`) while
   keeping downlevel defaults for everything else, so no adapter is excluded.
2. **The pipeline was built for the wrong colour format.** The offscreen target is
   `Rgba8UnormSrgb`; macOS presents `Bgra8UnormSrgb`. A pipeline is bound to one
   format, and the mismatch is a validation error. Surfaces now build their own
   pipeline for whatever format they get.

**wgpu treats validation errors as fatal**, so neither returned an `Err` to handle —
they aborted the process from inside a windowing callback, with a panic that could
not even unwind. That is worth recording as a property of the library: on the
render path, "handle the error" is often not available, and the check has to happen
*before* the call.

Both now have hardware-independent regression tests that run in CI: the size clamp
is a pure function, and the format bug is caught by drawing to a surface's format
*offscreen* — same pipeline, same format, no display server. Each was confirmed to
fail against the original code before being kept.

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

10,000 entities at 640x360, as the tests report it:

| Device | p99 | median | run-to-run |
|---|---|---|---|
| Apple M4 Pro (Metal, integrated GPU) — developer machine | 3.5 ms | 2.4 ms | — |
| `llvmpipe (LLVM 20.1.2, 256 bits)` (Vulkan, software rasterizer) — Ubuntu runner | 34.1 ms | 27.0 ms | 30.0 / 23.0 ms on an earlier run |
| `Apple Paravirtual device` (Metal, integrated GPU) — macOS runner | 11.7 ms | 6.1 ms | 82.0 / 5.8 ms on an earlier run |
| `Microsoft Basic Render Driver` (DirectX 12, software rasterizer) — Windows runner | 26.8 ms | 20.7 ms | 105.4 / 100.2 ms on an earlier run |

`queue.submit` applies backpressure: when the GPU cannot keep up, submit blocks, and the
GPU's cost lands in the caller's wall-clock timing. On fast hardware that is invisible. On a
software rasterizer it dominates — WARP's 100 ms median is it rasterizing 120,000 triangles,
not this project's code.

**Run-to-run variance makes the case even more strongly than the device spread does.** The
Windows runner measured a 100 ms median on one run and 21 ms on the next, from identical
code. A threshold that admits a 5x swing on the same platform is not measuring the code.

Isolating the genuinely hardware-independent part does not rescue a gate either: sim and
extract alone measure p99 2.2 ms on an M4 Pro, and a runner 2–4x slower would land at 4–9 ms
against any 8 ms budget. There is no threshold there that is both meaningful and not flaky.

**A note on classifying devices.** `DeviceInfo::is_software()` reports the macOS runner's
`Apple Paravirtual device` as *not* software, because it presents as an integrated GPU — yet
it is a VM and its numbers are no more representative of real hardware than lavapipe's. "Not
software" therefore does not mean "reference hardware", and any automated check keyed on the
device *kind* alone would be fooled. The full device name is what carries the truth, which is
why the recorded line prints it.

So frame time follows the same rule as the other hardware-dependent numbers: **recorded with
the device that produced it**. What the test asserts instead is hardware-independent — that a
30 Hz simulation rendered at 144 Hz ticks roughly one frame in five (measured 62/300 = 20.7%),
and that a tickless frame still extracts every entity. Both are real bugs when they break,
and both are catchable on any machine.

That assertion held identically — **62 of 300 frames** — on all four devices, across a 10x
spread in frame cost. Which is exactly the property a hardware-independent check should have.

## The two state-equivalence criteria are met

Both are the same claim from two directions: **sim state is a function of its
ticks and nothing else** — not of whether anything is watching, and not of how
the ticks were requested. Checked in `crates/cx-app/tests/state_equivalence.rs`.

| Criterion | How it is checked |
|---|---|
| Headless vs windowed, 10,000 ticks | Both worlds hashed **every tick**, not just at the end. The windowed side runs the full frame loop, extract and draw included, and the test asserts it actually ticked, extracted, and drew each frame — so a run where drawing quietly stopped fails rather than passes. |
| Pause → step 5 → resume | Driven through `PacedDriver`, which needs no adapter, so it runs everywhere. Each step is followed by an idle paused frame, since that is where a leaked tick would hide. |

Both failure modes are silent, which is why they are worth checking: an extract
that wrote back to sim state would make a game play differently in a window than
it replays headless, discovered whenever a replay or save is first compared.

The drawn comparison runs at 32x32 with 64 entities. Resolution is irrelevant to
whether *drawing at all* perturbs state, and a full-size 10,000-frame run takes
minutes on the software rasterizers CI uses — small and actually run beats
realistic and skipped.

A third test is the control: it asserts the hash genuinely distinguishes states,
including a difference confined to `PreviousTransform` alone. Without it, both
criteria above would pass against a hash function that returned a constant.

`Transform`, `PreviousTransform`, `WorldPos`, and `Quat` are now `StateHashable`.
`WorldPos` hashes its chunk and local offset separately rather than an absolute
position — the absolute value is not representable without precision loss far
from the origin, which is exactly where a determinism bug is hardest to find.

## The previous-transform copy is now built into the schedule

`SimSchedule::new()` registers `copy_previous_transforms` at `IntakeCommands`
itself. A schedule that omitted it still ran, still ticked, and still drew — it
just interpolated from each position to itself, which looks exactly like the
stepping interpolation exists to remove. No error, no failing test, and a display
server required to notice it.

The test that covers this runs **two** ticks, not one. Written with a single tick
it passed with the copy removed, because the seeded `PreviousTransform` already
equalled what the copy would have written.

## The camera, and what it brought with it

`cx-app::flycam` holds all the arithmetic; `cx-app::window` holds only the key
table. Everything with a failure mode is on the testable side of that line:

- **Position is a `WorldPos`, not a `Vec3`.** The camera can fly a long way, and
  at 100 km an absolute `f32` has about a centimetre of resolution. Storing it
  chunk-relative also makes the camera the natural source of the **extract
  origin**, which the frame loop now follows — so whatever is near the eye keeps
  a small local offset and therefore its precision. That is the floating origin
  actually doing its job, rather than being a fixed constant.
- **Pitch is clamped just short of vertical.** At exactly 90° the view direction
  is parallel to up, `look_at` cannot pick a roll, and the matrix comes out full
  of `NaN` — a black screen with no error.
- **Yaw wraps rather than accumulating.** After long enough turning one way, an
  unwrapped `f32` yaw can no longer resolve a small turn and the view ratchets.
- **Diagonal movement is normalized**, and "up" is world up rather than camera
  up, so ascending while looking down does not drift you forwards.

## An occluded window was spinning at 14,000 fps

Running the client while it sat behind another window revealed that the frame
loop was skipping every frame — and then immediately asking for another, because
`ControlFlow::Poll` never waits. **33,348 skipped frames in four seconds**, all
drawing nothing, burning a core.

Two things were wrong and both are fixed:

1. **A skipped frame was invisible.** `present` returned `DrawStats { draw_calls: 0 }`,
   which is also exactly what an empty scene produces. It now returns
   `Presented::{Drawn, Skipped(SkipReason)}`, and the per-second report counts
   skips, so `draw_calls=0` is no longer ambiguous.
2. **Nothing backed off.** A persistent skip — an occluded window stays occluded
   until someone moves it — now schedules a ~60 Hz retry instead of spinning.
   Transient reasons (`Lost`, `Outdated`, `Timeout`) deliberately do *not* back
   off: the surface has just been reconfigured and waiting would add 16 ms of
   stutter to every resize.

The simulation keeps ticking at 30 Hz throughout. Being behind another window is
not being paused.

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
