//! Spatial transforms, and the previous-tick copy that makes interpolation
//! possible.
//!
//! These live above the firewall because position is authoritative simulation
//! state, not presentation (`ADR-0002`). The view world *reads* them at extract
//! and interpolates; it never writes them.
//!
//! # Why `PreviousTransform` exists
//!
//! A 30 Hz simulation rendered at 144 Hz shows each simulated position for four
//! or five frames. Without interpolation that reads as visible stepping, and the
//! fix is not a tuning pass — it is having the previous position available to
//! blend from. S03 makes the copy a contract: `PreviousTransform` is written at
//! the start of each tick, before any system moves anything.

use bevy_ecs::component::Component;
use bevy_ecs::system::Query;
use cx_core::math::{Quat, Vec3, WorldPos};

/// Where a thing is, how it is oriented, and how big it is.
///
/// Position is a [`WorldPos`] rather than a bare `Vec3`: at 100 km from the
/// origin an absolute `f32` has about 1 cm of resolution, and the jitter is
/// visible (`03-conventions.md`).
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Chunk-relative position.
    pub position: WorldPos,
    /// Orientation.
    pub rotation: Quat,
    /// Per-axis scale.
    pub scale: Vec3,
}

impl Transform {
    /// A transform at a position, unrotated and unscaled.
    pub fn from_position(position: WorldPos) -> Self {
        Self {
            position,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }

    /// Blends towards `other` by `alpha`, clamped to `[0, 1]`.
    ///
    /// Position interpolates through the chunk-relative difference rather than
    /// through two absolute positions, so it stays exact far from the origin.
    /// Rotation uses `slerp`, because `lerp` on quaternions shortens the arc and
    /// makes fast spins visibly wobble.
    pub fn interpolate(&self, other: &Transform, alpha: f32) -> Transform {
        let alpha = alpha.clamp(0.0, 1.0);
        let delta = other.position.delta(self.position);

        Transform {
            position: self.position.offset(delta * alpha),
            rotation: self.rotation.slerp(other.rotation, alpha),
            scale: self.scale.lerp(other.scale, alpha),
        }
    }
}

/// Where a thing was at the start of this tick.
///
/// Copied by [`copy_previous_transforms`] in `IntakeCommands`, before anything
/// moves. A system that writes this itself has broken interpolation for every
/// entity it touches.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct PreviousTransform(pub Transform);

/// Copies each [`Transform`] into its [`PreviousTransform`].
///
/// Runs at the top of the tick. Registering it anywhere later means the "previous"
/// value is really the current one for every entity that moved before it ran,
/// and interpolation silently becomes a no-op for those entities — which looks
/// like stepping in exactly the cases that move fastest.
pub fn copy_previous_transforms(mut query: Query<(&Transform, &mut PreviousTransform)>) {
    for (transform, mut previous) in query.iter_mut() {
        previous.0 = *transform;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::{CHUNK_SIZE, ChunkCoord};

    fn at(chunk: ChunkCoord, x: f32, z: f32) -> Transform {
        Transform::from_position(WorldPos::new(chunk, Vec3::new(x, 0.0, z)))
    }

    #[test]
    fn alpha_endpoints_are_the_endpoints() {
        let from = at(ChunkCoord::new(0, 0), 0.0, 0.0);
        let to = at(ChunkCoord::new(0, 0), 10.0, 0.0);

        let start = from.interpolate(&to, 0.0);
        let end = from.interpolate(&to, 1.0);

        assert!(
            (start.position.local.x - 0.0).abs() < 1e-4,
            "alpha 0 must be the start"
        );
        assert!(
            (end.position.local.x - 10.0).abs() < 1e-4,
            "alpha 1 must be the end"
        );
    }

    #[test]
    fn the_midpoint_is_halfway() {
        let from = at(ChunkCoord::new(0, 0), 0.0, 0.0);
        let to = at(ChunkCoord::new(0, 0), 10.0, 20.0);

        let middle = from.interpolate(&to, 0.5);

        assert!((middle.position.local.x - 5.0).abs() < 1e-4);
        assert!((middle.position.local.z - 10.0).abs() < 1e-4);
    }

    #[test]
    fn interpolation_crosses_chunk_boundaries_smoothly() {
        // The case a naive lerp of two local offsets gets catastrophically
        // wrong: moving off the end of one chunk into the next, the local x
        // jumps from ~512 back to ~0, and lerping the locals would send the
        // entity flying backwards across the whole chunk.
        let from = at(ChunkCoord::new(0, 0), CHUNK_SIZE - 2.0, 0.0);
        let to = at(ChunkCoord::new(1, 0), 2.0, 0.0);

        let middle = from.interpolate(&to, 0.5);
        let travelled = middle.position.delta(from.position);

        assert!(
            (travelled.x - 2.0).abs() < 1e-3,
            "halfway across a 4 m gap should be 2 m, got {}",
            travelled.x
        );
    }

    #[test]
    fn alpha_is_clamped_rather_than_extrapolating() {
        // A late frame can produce alpha above 1. Extrapolating there overshoots
        // and then snaps back, which reads worse than a held frame.
        let from = at(ChunkCoord::new(0, 0), 0.0, 0.0);
        let to = at(ChunkCoord::new(0, 0), 10.0, 0.0);

        let over = from.interpolate(&to, 1.5);
        assert!((over.position.local.x - 10.0).abs() < 1e-4);

        let under = from.interpolate(&to, -0.5);
        assert!((under.position.local.x - 0.0).abs() < 1e-4);
    }

    #[test]
    fn rotation_interpolates_along_the_short_arc() {
        let mut from = at(ChunkCoord::new(0, 0), 0.0, 0.0);
        let mut to = from;
        from.rotation = Quat::IDENTITY;
        to.rotation = Quat::from_rotation_y(std::f32::consts::PI * 0.5);

        let middle = from.interpolate(&to, 0.5);
        let expected = Quat::from_rotation_y(std::f32::consts::PI * 0.25);

        assert!(
            middle.rotation.dot(expected).abs() > 0.999,
            "halfway through a quarter turn should be an eighth turn"
        );
    }

    #[test]
    fn scale_interpolates_linearly() {
        let mut from = at(ChunkCoord::new(0, 0), 0.0, 0.0);
        let mut to = from;
        from.scale = Vec3::ONE;
        to.scale = Vec3::splat(3.0);

        assert!((from.interpolate(&to, 0.5).scale.x - 2.0).abs() < 1e-5);
    }
}
