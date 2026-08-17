# ADR-0005 — Render behind a backend-agnostic abstraction

**Status:** SUPERSEDED by `ADR-0010` · **Date:** 2026-08-16 · **Superseded:** 2026-08-16

## Original decision

Define `cx-render-api` as a backend-agnostic interface (resources, pipelines, passes, draw submission), implemented by `cx-render-wgpu`, so that a console graphics backend could be added without rewriting rendering code.

## Why it was superseded

The entire justification was the console target: console graphics APIs are NDA-only and wgpu ships no backends for them. With console out of scope (`ADR-0010`), the abstraction has no consumer, and an abstraction with exactly one implementation is a cost with no benefit — it constrains the renderer to the intersection of what wgpu and a hypothetical future backend can express, for a backend that will never exist.

## What survives

The **crate boundary** is retained, the trait surface is not. Rendering lives in `cx-render`, which uses wgpu directly and freely. No crate outside `cx-render` may name a wgpu type, and CI still enforces this. That boundary costs nothing, keeps graphics code from spreading into gameplay, and would make a future port a contained project rather than an archaeology exercise — without paying now for an interface no one calls.

See `ADR-0010` and S12.
