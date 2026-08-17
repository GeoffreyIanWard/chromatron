# ADR-0007 — Compile-time plugins and a non-JIT script interpreter

**Status:** accepted · **Date:** 2026-08-16

## Context

Extensibility options were: dynamic native plugins via a C ABI, WebAssembly via `wasmtime`, or an embedded interpreter. The console target originally constrained this. Console was later dropped (`ADR-0010`), but the decision stands on its own merits — see rationale.

## Decision

Plugins are **compile-time** Rust crates implementing a `Plugin` trait, linked into the binary. Scripting uses **`rhai`**, a pure-Rust bytecode interpreter with no JIT. Mods are content packs plus scripts, and cannot add native code.

## Rationale

Dynamic native plugins are ruled out by Rust itself: there is no stable ABI, which makes the C-ABI approach a known and painful trap. This was true independently of the console constraint, which is why the decision survived `ADR-0010`. `wasmtime` is an otherwise excellent fit — sandboxed, fast, language-agnostic — but its JIT is the problem; its interpreter mode gives up most of the performance advantage that motivated it.

`rhai`'s performance ceiling is acceptable because S17 confines scripts to decisions and reactions, never to inner loops. If behavior scoring at agent scale proves too slow, the fallback is a data-defined expression tree evaluated in Rust, with scripts reserved for exotic cases.

## Consequences

- Mod authors write content and scripts, not Rust. This lowers the ceiling on what mods can do and raises the floor on how safely they can do it.
- Script sandbox constraints (no clock, no filesystem, injected RNG only) must be enforced at the binding layer rather than by documentation, since a script that reads wall-clock silently breaks determinism.
- Adding a first-party plugin requires a rebuild, which is acceptable for a project where the plugin authors are the engine authors.
