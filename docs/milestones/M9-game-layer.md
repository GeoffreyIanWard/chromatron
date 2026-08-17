---
id: M9
title: Game Layer
specs: [S15, S16, S17]
gate: bench/baselines.md#m9
---

# M9 — Game Layer

Everything that turns the simulation into a game, all of it non-authoritative by construction.

## Deliverables

- App state machine, async loading with progress, settings with live apply.
- Input action mapping, rebinding, gamepad navigation, accessibility options.
- Player actions as commands entering `IntakeCommands` at tick boundaries.
- Camera controllers registered as S09 interest points.
- Game UI behind a `UiBackend` abstraction.
- Hierarchical rigid animation plus vertex animation textures for crowds.
- GPU particles and VFX, spawned from extracted sim events.
- Spatial audio via `kira`, with event coalescing and voice caps for accelerated time.
- Compile-time plugin trait; `rhai` scripting with sandbox, injected RNG, and instruction budgets.
- Mod loading as content packs plus scripts; mod set recorded in saves.

## Exit criteria

| Check | Target |
|---|---|
| Headless vs windowed state hash | identical — proves presentation is inert |
| Any `sim/` crate depending on `cx-present` / `cx-audio` | zero, CI enforced |
| 50,000 animated instances via VAT | 60 fps |
| 1000x time acceleration | audio voices and particle systems within budget, no unbounded queues |
| Full UI navigation by gamepad alone | supported (no longer a release gate, ADR-0010) |
| Every player action | appears in replay log |
| Mod adding prototype + reflected component + behavior script | loads and simulates, no engine change |
| Script exceeding instruction budget | aborts with diagnostic, tick unaffected |
| Sandbox rejecting clock / filesystem / network | verified |
