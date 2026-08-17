---
id: S18
title: Platform & Shipping
status: not started
depends_on: [S12, S13, S16]
provides: [packaging, asset-bundling, shader-precompilation, crash-reporting]
crates_touched: [cx-app, cx-render]
milestone: M10
---

# S18 — Platform & Shipping

**Desktop only** (`ADR-0010`). Windows, macOS, Linux. Console is out of scope.

Three constraints that originally came from the console target are retained, because each pays for itself on desktop:

| Retained constraint | Desktop justification | Owned by |
|---|---|---|
| Memory ceiling, 8 GB min-spec profile | 16 GB is a realistic minimum spec; Steam Deck is 16 GB shared | `bench/memory-budget.md` |
| No runtime shader compilation | Causes visible hitching on first encounter with a material | S12 |
| Asset bundling into indexed archives | Many small files are slow to open on every desktop OS | this spec |

Dropped along with console: the backend-agnostic render trait surface (`ADR-0005`, superseded), gamepad-first UI as a hard requirement (still supported, no longer a gate), and the 6 GB memory profile.

Retained for reasons unrelated to console: compile-time plugins and a non-JIT interpreter (`ADR-0007` — Rust has no stable ABI regardless, so dynamic native plugins were never viable).

## Requirements

- **Asset bundling**: content packs compile into a single archive with an index. Loading is mmap plus offset, not per-file open.
- **Shader precompilation**: all pipeline variants compiled at build time into a cache. Zero runtime compilation in release builds.
- **Platform layer**: filesystem paths, save location, and display behind traits. Not for portability to other platform classes, but so that path handling and save-location conventions (`%APPDATA%`, `~/Library`, XDG) live in one place instead of scattered through the codebase.
- **Crash reporting**: minidump plus the trailing N ticks of the replay log, so a crash report reproduces. This is where the replay system (S13) pays for itself a second time.
- **Build profiles**: `dev` (assertions, inspector, hot reload), `bench` (release plus profiling spans), `release` (stripped).
- CI matrix: Windows, macOS, Linux, plus the 8 GB constrained profile.

## Non-goals

Console, mobile, web. No storefront integration. No auto-updater.

## Acceptance criteria

- Full asset load from bundle under 3 seconds cold.
- Zero runtime shader compilation in release, verified by instrumentation.
- No direct `std::fs` or `std::path` use outside the platform layer, enforced by CI.
- A crash produces a report that reproduces the crash when replayed.
- The 8 GB constrained profile passes every milestone gate from M0 onward.
