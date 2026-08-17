# ADR-0010 — Desktop only; console dropped

**Status:** accepted · **Date:** 2026-08-16 · **Supersedes:** `ADR-0005`

## Context

Console was listed as an eventual target, and imposed a set of constraints across the plan: a backend-agnostic render abstraction (console graphics APIs are NDA-only and wgpu ships no backends), no dynamic native code, no JIT, a 6 GB memory ceiling, gamepad-first UI, and certification requirements around suspend and resume.

The render abstraction was the expensive one — a full trait surface with a single implementation, constraining the renderer to the intersection of wgpu and a hypothetical backend.

## Decision

Target desktop only: Windows, macOS, Linux. Console is out of scope.

**Dropped:** the `cx-render-api` trait surface (`ADR-0005` superseded), the 6 GB profile, gamepad-first UI as a hard gate, and suspend/resume certification requirements.

**Retained, on their own desktop merits:**

| Constraint | Desktop justification |
|---|---|
| Memory ceiling at 8 GB | 16 GB is a realistic minimum spec; Steam Deck is 16 GB shared |
| No runtime shader compilation | Causes visible hitching on first encounter with a material |
| Asset bundling into indexed archives | Many small files are slow to open on every desktop OS |
| Compile-time plugins, non-JIT scripting (`ADR-0007`) | Rust has no stable ABI; dynamic native plugins were never viable regardless |
| `cx-render` crate boundary, no wgpu types outside it | Free, and keeps graphics code from spreading into gameplay |

Gamepad support is still built (S16) — it is simply no longer a release gate.

## Rationale

Dropping the trait surface is the substantive change; the rest of the console constraints were either good practice anyway or cheap to keep. An abstraction with one implementation is a cost with no benefit.

The retained crate boundary is the hedge: it costs nothing now and would make a future port a contained project rather than an excavation. That is a very different proposition from maintaining an unexercised interface.

## Consequences

- `cx-render-api` and `cx-render-wgpu` collapse into a single `cx-render` crate that uses wgpu directly and idiomatically.
- S12 can use wgpu features freely — bindless resources, whatever the backend offers — without asking whether a hypothetical console API supports them.
- Revisiting console later means a real port. Given the crate boundary, that is bounded work, but it is work, and this ADR is the record that the trade was made deliberately.
