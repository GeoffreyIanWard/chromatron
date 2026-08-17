---
id: S17
title: Extensibility & Scripting
status: not started
depends_on: [S02, S04]
provides: [plugin-api, scripting, mod-loading]
crates_touched: [cx-data, cx-sim]
milestone: M9
---

# S17 — Extensibility & Scripting

Constrained hard by the console target: consoles prohibit loading arbitrary native code and typically prohibit JIT compilation (`ADR-0007`). That rules out dynamic native plugins and `wasmtime`'s JIT.

## Requirements

- **Plugins are modules** (S20). There is no separate plugin abstraction: a first-party subsystem and a third-party extension use the same `Module` trait, the same capability declarations, and the same resolution. This is part of why S20 lands at M0 — it is the extensibility mechanism, not just internal tidiness.
- **Compile-time linked, runtime composed** (`ADR-0012`). Modules are Rust crates linked into the binary and enabled by config or scenario. No dynamic ABI — Rust has no stable one, and pursuing it is a well-known trap.
- **Scripting for content behavior** via a bytecode interpreter with no JIT. `rhai` is the recommendation: pure Rust, sandboxed, no JIT, easy to bind, and its performance ceiling is acceptable because scripts drive decisions, not inner loops.
- **Scripts never run in hot paths.** Bound to: behavior consideration scoring (S10), event reactions, scenario setup, and content validation hooks. A script must not be callable per-cell or per-tick-per-entity at scale.
- **Determinism constraints on scripts**: no wall-clock access, no filesystem, no ambient RNG (scripts draw from an injected `RngStream`), no unordered collection iteration. Enforced by the binding layer, not by documentation.
- **Script execution budget**: per-script instruction cap; exceeding it aborts that script with a diagnostic rather than stalling the tick.
- **Mod loading**: mods are content packs (S04) plus scripts. Load order from manifest dependencies; conflicts are errors. Mods cannot add native code.
- A mod-enabled world records its mod set in the save; loading with a different set warns and lists the differences.

## Non-goals

No native mods. No `wasmtime`. No hot-reloading of Rust plugins. No mod marketplace or distribution.

## Acceptance criteria

- A mod adding a new prototype, a new component (via reflection), and a behavior script loads and simulates without engine changes.
- Script sandbox rejects wall-clock, filesystem, and network access, verified by test.
- A script exceeding its instruction budget aborts with a diagnostic and does not stall the tick.
- Two runs with identical mods and seed produce identical state hashes.
- No JIT and no dynamic library loading anywhere in the shipping build, verified by binary inspection in CI.

## Open questions

- Whether rhai is fast enough for behavior scoring at agent scale, or whether scoring must be a data-defined expression tree evaluated in Rust with scripts only for exotic cases. Benchmark at M9; the expression-tree fallback is the likely outcome.
