# ADR-0002 — Separate sim world from view world

**Status:** accepted · **Date:** 2026-08-16

## Context

The same engine must serve headless batch runs at 10,000x and a windowed game at 144 fps. Presentation state (animation timers, particles, camera shake, audio) is inherently frame-rate-driven and non-deterministic. If it lives in the same World as the simulation, determinism erodes silently and headless mode drags along dependencies it cannot use.

## Decision

Two worlds. The **sim world** is authoritative, fixed-timestep, deterministic, and has no rendering dependencies. The **view world** is derived and disposable. An **extract** phase copies interpolated visual state from sim to view once per rendered frame. Headless mode never constructs the view world.

CI enforces the boundary: no crate under `sim/` may depend on `wgpu`, `winit`, `kira`, or `egui`.

## Rationale

The alternative — one world with presentation components — works until the first time someone reads frame delta inside a sim system, and then produces a determinism bug that takes days to find. A structural boundary checked by CI is cheaper than the discipline it replaces.

## Consequences

- Every visual property needs an explicit extract path; there is a small ongoing tax per feature.
- `Transform` and `PreviousTransform` must both exist in the sim world for interpolation.
- Headless mode is free rather than a separate code path.
- Sim events consumed by presentation must be drained during extract, and throttled at high time multipliers (S15).
