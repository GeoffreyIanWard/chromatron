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
- ~~GPU frustum culling compute pass~~ **done** — see below.
- **Debug draw** — lines, spheres, AABBs, OBBs, arrows, crosses: **done**. World-space text landed with the egui overlay, as planned.
- ~~egui overlay~~ **done** — tick counter, frame graph, time controls, and world-space labels.
- ~~Free-fly camera~~ **done** — `cx-app::flycam`, with the floating origin following it.
- `tools/graph-viewer`: the isometric architecture viewer over the M0 graph export, with stable layout and `--baseline` diff (S21). Independent of the renderer above — it is a static page, not engine code — but it lands here because M1 is the first milestone with enough modules and systems registered for the diagram to be worth looking at.
- ~~**Tile layout fixed**~~ **done** — `TILE_CELLS = 64`, 256 tiles per chunk, with `cx_core::TileDirty` as the dirty-tracking bitset. Mesh layout depends on this and M4B cannot retrofit it (`ADR-0011`). Safe to fix now that `ADR-0013` settled terrain representation as a heightfield.

## Exit criteria

| Check | Target |
|---|---|
| 100,000 instanced meshes at 60 fps | < 20 draw calls |
| 30 Hz sim, 144 Hz render, moving instances | 99th-pct frame time < 8 ms, no visible stutter |
| Extract 100,000 visible instances | < 2 ms |
| Debug draw 10,000 lines | < 1 ms — **measured 1.02 ms**, see below |
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

## Tile granularity is fixed, and the bitset exists

`ADR-0011` calls this the one part of the ADR that is not deferrable: mesh layout
depends on tile size and M4B cannot retrofit a different one. The constants were
already in `cx-core`; what was missing was the structure `TileCoord::index()`
pointed at.

`cx_core::TileDirty` is 256 bits — four `u64`s, 32 bytes, no allocation. A
`HashSet<TileCoord>` was rejected for the usual reason plus a specific one:
**rebuild order is observable**. Float accumulation over a region depends on the
order the pieces are summed, so unspecified iteration order here would be a
determinism bug that appears on some machines and not others (`ADR-0004`).

Three of its tests are about mistakes that produce *silence* rather than errors:

- A region handed its corners backwards marks the same tiles, rather than none.
  Marking none looks exactly like the edit failing to apply.
- The last cell of a tile belongs to that tile. The off-by-one leaves a one-cell
  seam unrebuilt along every tile boundary.
- Every tile gets its own bit — checked for all 256. An index collision shows up
  as a tile that never rebuilds, one chunk-edge away from the edit that caused
  it.

## The graph viewer's premise — now partly true

**Update.** `cx-worldgen` is a module, so the graph is no longer a single box.
`--profile terrain` now exports two modules, a `requires` edge between them, two
systems in different phases, and a field with an owner and a declared writer:

```
modules 2 · capabilities 2 · systems 2 · field_access 1
```

Still small, but it has every *kind* of element the viewer draws, which is what a
layout algorithm needs to be developed against. `cx-spatial`, `cx-agents`, and
`cx-physics` are next.

## The original finding, for the record

M1 lists `tools/graph-viewer` and says it belongs here because M1 is "the first
milestone with enough modules and systems registered for the diagram to be worth
looking at". **That is not true as things stand.** Every profile currently
exports the same graph:

```
modules 1 · capabilities 1 · systems 1 · field_access 0
```

Only `cx-fields` is a `Module`. S21 already recorded this — "the profiles are
currently empty" — but M1's ordering was written as though it would have resolved
by now, and it has not, because nothing since has required another crate to become
a module.

A viewer built against a one-node graph would be honest and useless, and worse,
its layout and diff decisions would be made against a shape that tells us nothing
about how they behave at fifty nodes. The dependency is real: the viewer should
follow the modules, not lead them.

## Debug draw, and the budget it lands right on top of

Everything is line segments: a sphere is three rings, a box twelve edges, an
arrow a shaft and four barbs. One primitive, one pipeline, one number to
measure. Shapes are authored in `WorldPos` and rebased with the frame, like
instances, so nothing has to know the current origin.

**Marginal cost of 10,000 lines**, at 640x360:

| Device | Marginal | With lines | Without |
|---|---|---|---|
| Apple M4 Pro (Metal) — developer machine | **1.02 ms** | 1.48 ms | 0.46 ms |
| `llvmpipe` (Vulkan, software) — Ubuntu runner | **23.38 ms** | 23.95 ms | 0.57 ms |

The budget is 1 ms. On hardware that is *on* the line, not under it.

The 23x spread is the case for recording rather than gating, made in one row: a
threshold that lavapipe could pass would be meaningless on hardware, and one
hardware can pass fails every Linux run. Note also that the *baseline* frames
agree closely — 0.46 ms against 0.57 ms — so it is the drawing that diverges,
not the loop around it.

Measured as a difference rather than as a whole frame deliberately: reporting the
frame would have credited debug draw with the simulation, the extract, and the
scene pass, and produced a comfortable-looking number that answered a different
question.

The obvious cost is already known and already recorded below — the vertex buffer
is created and uploaded every frame. Pooling it is the same work that the
instance buffer needs, and this is now a second reason to do it.

Not gated in CI, for the reason set out above: submit backpressure puts the GPU's
cost into any wall-clock frame measurement, and the runners' software
rasterizers vary 5x run to run. What *is* asserted on every platform is
hardware-independent — 10,000 lines cost exactly **one** draw call, and all
10,000 arrive.

### What drawing it found

The scene pass discarded its depth buffer (`StoreOp::Discard`), which was correct
while nothing read depth afterwards. The debug pass then started to, loaded
undefined contents, and **every line silently failed its depth test**: no error,
no validation warning, one draw call reported, zero pixels changed.

The end-to-end test caught it because it asserts on the *pixels* rather than on
the draw call count. A test that checked `debug.lines == 1` would have passed
against a renderer drawing nothing at all — which is the same failure this
project keeps finding in its own checks.

## The overlay, and where `egui` is allowed to live

Split across two crates on purpose, and the split is the interesting part:

| Library | Crate | Why |
|---|---|---|
| `egui` | `cx-ui` | The UI itself. Contained the way `wgpu` is contained in `cx-render`. |
| `egui-wgpu` | **`cx-render`** | It names devices, queues, and command encoders. A UI crate handling those while declaring no dependency on `wgpu` would be containment on paper only. |

`tools/ci-checks` enforces both halves. Its ownership table now prefers an
*exact* match over a family prefix, because `egui-wgpu` matches the `egui`
family and needs its own answer — and an ownership rule that depends on the
order of a list is one nobody can read off the list.

`cx-app` never names `egui`. It passes a `UiOutput` from `cx-ui` to `cx-render`
without opening it.

The time controls also moved: `controls::Action` now lives in `cx-ui`, so the
overlay's buttons and the keyboard produce the same actions. Two definitions of
what pause means is how a debug UI stops being trusted.

### Three fatal traps in `egui`'s texture handling, all found by running it

1. **Dropping a `TexturesDelta` unapplied panics from a destructor**, which
   aborts rather than unwinds.
2. **Applying every texture and then dropping it is still fatal** — `epaint`
   tracks *handled* separately from *uploaded*, so the delta has to be cleared.
3. **Discarding a delta is worse than either.** `egui` sends the font atlas once
   and everything after that as *partial* updates to it. A frame the window
   skipped threw its delta away, leaving the renderer without an allocation
   `egui` believed it had made — and the *next* frame aborted inside `egui-wgpu`
   with "tried to update a texture that has not been allocated".

So a skipped frame now hands its textures to the renderer and simply does not
draw. The relevant test asserts on **pixels changing**, not on primitives being
produced: the overlay produced primitives while drawing nothing in two of the
three cases above.

**`egui`'s first pass draws nothing.** It lays out text before the font atlas
exists, so the panel appears on frame two. Invisible at 120 Hz, but it meant the
first version of the test asserted something false.

## Correction: the occluded back-off shipped broken

`M1`'s previous entry claimed an occluded window backs off to ~60 Hz. **It did
not.** The decision was unit-tested and correct; the loop then called
`request_redraw()` unconditionally at the end of every frame, and
`request_redraw` wakes the event loop immediately, so `ControlFlow::WaitUntil`
never applied. Measured: **4,300 fps while occluded**, against a 16 ms wait.

It was reported as fixed on the strength of a passing unit test and a live
observation of the *bug*, without ever observing the *fix* — the window would
not stay occluded on demand. It has now been seen working: **54 fps occluded,
120 fps visible**, in the same batch of runs.

The unit test was not wrong; it tested one of the two things that had to be true.
The missing half — that no frame is requested before the deadline — is now tested
too.

## GPU frustum culling

A compute pass compacts the visible instances into a second buffer and fills in
an indirect draw's instance count as it goes. **The count never comes back to the
CPU**: the atomic that decides where an instance lands is the same word the draw
call reads, so nothing has to reconcile them, and there is no synchronisation
point in the middle of the frame.

The windowed path culls; the offscreen path draws directly. That is deliberate —
the offscreen tests put the whole scene in view on purpose, and a compute
dispatch there would add something else to go wrong between a queued instance and
an asserted pixel.

### The six inequalities exist twice, and that is checked

One copy has to run on each processor, so the duplication is unavoidable. What
matters is that a disagreement is caught: a test runs `cx_render::culling` and
`shaders/cull.wgsl` over the same 4,096 instances from **five camera positions**
and compares the counts. One position could agree by luck — a shader ignoring the
top plane still matches a camera whose scene sits below it.

A control test asserts culling actually *removes* something, because the
comparison above would pass just as well if both sides kept everything.

### Planes from the matrix, not from the camera

Gribb–Hartmann extraction from the view-projection matrix, rather than
reconstructing corners from field of view and aspect. The matrix that culls is
then the matrix that projects, so an error in the aspect ratio moves both
together instead of culling things that would have been on screen. It also means
the `0..1` depth convention is already accounted for — the near-plane row differs
from OpenGL's `-1..1`, and getting it wrong clips everything nearer than half the
far plane, which reads as a draw-distance bug.

There is a test that the frustum and the projection agree about 2,000 points.

### Two layout traps, one of which cost a debugging session

- **`vec3<u32>` aligns to 16 in WGSL.** Padding the uniform with one would have
  put the struct at 128 bytes against Rust's 112. wgpu catches it — "bound with
  size 112 where the shader expects 128" — but only once something dispatches.
  The padding is three scalars, and the size is asserted in a unit test.
- **The instance buffer needed `STORAGE` as well as `VERTEX`.** The cull pass
  reads it as storage before the draw reads the compacted output as vertex data.

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
