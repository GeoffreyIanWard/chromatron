---
id: S16
title: App Shell, Input & Game UI
status: not started
depends_on: [S03, S12]
provides: [app-states, input-mapping, settings, game-ui, camera-controllers]
crates_touched: [cx-app, cx-ui]
milestone: M9
---

# S16 — App Shell, Input & Game UI

The layer that turns a simulation into something a person can sit down in front of.

## Requirements

- **App state machine**: `Boot → MainMenu → Loading → Playing → Paused → Saving → Shutdown`. Transitions are explicit; each state declares which schedules run. Loading is async with progress reporting from S07's chunk generation.
- **Input mapping**: physical inputs bind to named actions; content-defined default bindings; runtime rebinding; per-context binding sets (world, menu, build mode). Gamepad support from the start — Steam Deck and controller users on desktop make it worth designing in rather than retrofitting, though it is no longer a release gate (`ADR-0010`).
- **Commands, not direct mutation**: every player action becomes a command entering `IntakeCommands` at a tick boundary. This is what makes replay (S13), undo, and eventual netcode possible, and it costs almost nothing if done from the start.
- **Camera controllers**: orbit, follow, free-fly, top-down strategy. The active camera registers as an S09 interest point.
- **Settings**: graphics (resolution, vsync, shadow quality, draw distance, LOD bias), audio buses, controls, accessibility (colorblind-safe palettes — important given palette atlas materials, text scaling, reduced motion, remappable everything). Settings persist and apply without restart where possible.
- **Game UI** behind an abstraction (`UiBackend`) so egui can prototype and be replaced. Do not let egui idioms leak into gameplay code. Requirements: layout, styling, controller navigation, text scaling, localization-ready string handling.
- Debug tooling (S14) is a separate UI surface from game UI, toggled by a dev key, compiled out of release.

## Non-goals

No in-game modding UI. No accessibility features beyond those listed. No localization *content* — just the plumbing so strings are never hardcoded.

## Acceptance criteria

- Every player-initiated state change flows through a command and appears in the replay log.
- The entire UI is navigable by gamepad alone.
- Rebinding persists and applies without restart.
- Loading screen reports accurate progress with no frame exceeding 20 ms.
- No `egui` type appears outside `cx-ui`, enforced by CI.
- Settings changes apply without restart, except those documented as requiring one.

## Open questions

- Game UI backend beyond egui. Candidates depend on the ecosystem at M9; write an ADR when chosen.
