---
id: S15
title: Presentation — Animation, VFX, Audio
status: not started
depends_on: [S03, S12]
provides: [animation, particles, vfx, spatial-audio]
crates_touched: [cx-present, cx-audio, cx-view]
milestone: M9
---

# S15 — Presentation

Everything here lives in the **view world** and is non-authoritative. Nothing in this spec may influence sim state. That constraint is what protects determinism once the engine becomes a game.

## Requirements

- **Animation**: for low-poly, hierarchical rigid-body animation (jointed parts, no vertex skinning) covers most needs at a fraction of the cost and suits the aesthetic. Add **vertex animation textures** for crowds — a baked animation as a texture, sampled in the vertex shader, lets tens of thousands of animated characters draw as instances. Full skeletal skinning only for hero-tier characters, if at all.
- Animation state is driven by sim state read during extract (an agent's `Activity` component selects a clip) but the clip's playback time advances at frame rate in the view world.
- **Particles**: GPU-simulated, spawned by sim events drained during extract. Fully view-side; a particle never affects the sim. Sim-relevant particulates (ash, pollutant) are fields (S06), not particles.
- **VFX**: decals, trails, screen effects, camera shake. Camera shake explicitly lives here and never touches the sim camera used for LOD interest points.
- **Audio** via `kira`: spatial positioning, distance attenuation, occlusion approximation, mixing buses, ducking. Sounds trigger from sim events; a sound never blocks or feeds back into a tick.
- **Event volume control**: an accelerated simulation generates events at 1000x. Presentation must throttle — coalesce identical events within a window, cap concurrent voices and particle systems, and drop rather than queue. Without this, running at high speed floods the audio mixer and the particle budget.

## Non-goals

No IK, no ragdolls, no facial animation, no music system beyond simple layered playback.

## Acceptance criteria

- 50,000 animated instances via vertex animation textures at 60 fps.
- Zero dependency from any `sim/` crate on `cx-present` or `cx-audio`, enforced by CI.
- A scenario run headless and windowed produces identical state hashes, proving presentation is inert.
- At 1000x time acceleration, audio voice count and particle system count stay within budget and no queue grows unbounded.
- Disabling presentation entirely has zero effect on sim results.

## Open questions

- Whether hierarchical rigid animation is expressive enough for the creature simulations, or whether skinning is needed for a subset. Prototype one creature at M9 before committing.
