---
id: S03
title: Time & Game Loop
status: not started
depends_on: [S01, S02]
provides: [tick-clock, fixed-timestep, interpolation, speed-control, headless-driver]
crates_touched: [cx-time, cx-app]
milestone: M0 (headless), M1 (windowed)
---

# S03 — Time & Game Loop

One loop serves three masters: a windowed game at 60+ fps, a debug session stepping one tick at a time, and a headless batch run at 10,000x. They are the same code path with different drivers.

## Requirements

- `TickClock` holding `Tick(u64)`, `TICK_US`, and an accumulator in integer microseconds.
- **Tick rate is configurable**, not fixed at 30 Hz. `TICK_US` comes from config, validated
  at startup against a permitted range (10–120 Hz); 30 Hz stays the default. This is nearly
  free because `Fixed::divide` is already rate-agnostic integer arithmetic (S01), and the
  cost of deferring it is not: scenarios, saves, and replay logs would bake in an assumed
  rate and need migrating.
- **The tick rate is part of world identity.** It is recorded in saves and replay logs
  alongside the module set (`ADR-0012`, S13), and a replay at a different rate refuses
  rather than diverging silently — the same command stream at 10 Hz and 30 Hz is not the
  same run.
- `TimeControl` resource: `Paused`, `Playing { multiplier }`, `Stepping { remaining }`. Multiplier range 0.1x–10,000x.
- Spiral-of-death guard: frame delta clamped to `MAX_FRAME_DELTA` (250 ms); catch-up ticks capped at `MAX_CATCHUP` per frame (default 8). When the cap is hit, emit a `SimFallingBehind` diagnostic rather than silently slowing time.
- Interpolation: sim entities carry `Transform` and `PreviousTransform`. `PreviousTransform` is copied at the start of each tick, before any movement. Extract blends by `alpha = accumulator / TICK_US`.
- Two drivers over one core:
  - `WindowedDriver` — the loop in `02-architecture.md`, paced to the display.
  - `HeadlessDriver` — runs ticks as fast as possible for a fixed count or until a stop condition; never constructs a view world.
- Time acceleration above ~20x must interact with S09: rather than running 20x the ticks, the LOD system reduces per-tick work. `TimeControl` exposes the current multiplier to S09 so it can shift tiers.
- Deterministic tick count: a scenario declaring 100,000 ticks executes exactly 100,000 regardless of driver or wall-clock.

## Non-goals

Variable-timestep simulation. Frame-rate-dependent logic of any kind. Both are banned by `03-conventions.md`.

## Acceptance criteria

- A 10,000-tick scenario produces identical state hashes under `WindowedDriver` and `HeadlessDriver`.
- With a 30 Hz tick and 144 fps render, no visible stutter in a scene of 100,000 moving instanced meshes (M1 gate; 99th-percentile frame time under 8 ms by capture).
- Pausing, stepping 5 ticks, and resuming yields the same state as running 5 ticks continuously.
- An injected 2-second stall causes no more than `MAX_CATCHUP` ticks in the following frame, and emits the diagnostic.

## Open questions

- ~~Whether sub-30 Hz tick rates are wanted for the largest scenarios.~~ Decided: support a
  configurable rate now (see Requirements). Deciding *which* rate a given large scenario
  should use is still an M4 question, but the mechanism no longer blocks on that, and the
  save/replay identity consequence is settled before anything depends on it.
