# ADR-0004 — Determinism target: reproducible on the same build

**Status:** accepted · **Date:** 2026-08-16

## Context

Determinism ranges from "none" through "reproducible on the same binary and machine" to "bit-exact across architectures". Each step up costs more: strict cross-platform determinism generally requires fixed-point arithmetic throughout and forbids most floating-point library calls.

Determinism buys replay-based bug reports, save-as-seed-plus-inputs, regression golden tests, and cheap lockstep netcode later.

## Decision

Target **bit-exact reproducibility for the same build on any machine** — same binary, any thread count, any CPU of the same architecture. Do not target cross-architecture bit-exactness, but keep the architecture capable of it: the constraints that would make it possible are adopted now, and only the arithmetic representation is left open.

Constraints adopted immediately (see `03-conventions.md`): no hash-map iteration in sim code, no unordered parallel float reduction, per-system RNG streams, positional worldgen, order-independent systems, no wall-clock or pointer-derived values in sim logic.

Constraint deferred: fixed-point arithmetic. Floating point is used, with the understanding that moving to fixed-point later is a contained change if the ordering constraints above hold.

## Rationale

The ordering constraints are nearly free if adopted from the start and prohibitively expensive to retrofit. Fixed-point arithmetic is the opposite: costly now, and only needed if cross-platform lockstep netcode is ever pursued — which `01-scope.md` lists as a non-goal.

## Consequences

- Replays are valid only against the build that produced them. Saves record the build hash; a mismatch warns.
- The pinned `rapier3d` version is part of the determinism contract (S11); changing it invalidates replays and requires a save migration.
- If S11's cross-architecture investigation fails, physics results are excluded from the state hash and physics-dependent outcomes become non-authoritative, recorded as a superseding ADR.
- State hashing and the divergence bisector (S14) exist from M0, not M7, because determinism bugs are cheapest to catch at introduction.
