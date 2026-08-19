//! Implements S12/S03 — view world, extract phase, interpolation.
//!
//! Below the firewall: derived, disposable presentation state. Nothing above the
//! firewall may depend on this crate.
//!
//! # What extract is, and what it is not
//!
//! Once per rendered frame, [`extract`] copies the visual subset of sim state
//! into a view world with an interpolation factor (`ADR-0002`). It is a **copy**,
//! not a borrow, and it is one-directional: the view world holds no reference
//! into sim state and can never write back. Headless mode is simply never
//! calling this.
//!
//! # Floating origin lives here
//!
//! `03-conventions.md` puts floating origin at extract time only — the sim never
//! rebases. Extract takes the origin chunk and produces positions relative to
//! it, so the renderer receives small `f32` values with uniform precision no
//! matter where in the world the camera is.
//!
//! # No wgpu
//!
//! This crate produces plain data. The renderer consumes it. That boundary is
//! what lets extract be benchmarked and tested on a machine with no GPU, which
//! is also the machine CI runs on.

#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod debug;

pub use debug::{DebugColour, DebugDraw, DebugVertex};

use cx_core::math::{CHUNK_SIZE, ChunkCoord, Quat, Vec3};
use cx_ecs::{PreviousTransform, SimWorld, Transform};

/// One thing to draw, in renderer-ready form.
///
/// Position is absolute *relative to the extract origin*, so it is small enough
/// for `f32` to hold precisely.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExtractedInstance {
    /// Position relative to the extract origin, in metres.
    pub position: Vec3,
    /// Orientation.
    pub rotation: Quat,
    /// Per-axis scale.
    pub scale: Vec3,
}

/// The derived, disposable presentation state for one frame.
///
/// Cleared and refilled each frame rather than diffed. The instances are a
/// projection of sim state, so reconstructing them is cheaper than tracking what
/// changed — and a stale view is a class of bug that cannot occur if there is no
/// retained state to go stale.
#[derive(Debug, Default)]
pub struct ViewWorld {
    instances: Vec<ExtractedInstance>,
    origin: ChunkCoord,
    alpha: f32,
}

impl ViewWorld {
    /// An empty view world.
    pub const fn new() -> Self {
        Self {
            instances: Vec::new(),
            origin: ChunkCoord::new(0, 0),
            alpha: 0.0,
        }
    }

    /// A view world with room for `capacity` instances.
    ///
    /// Preferred: extract runs every frame, and growing a vector during it is
    /// the easiest way to put an allocation on the frame path.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            instances: Vec::with_capacity(capacity),
            origin: ChunkCoord::new(0, 0),
            alpha: 0.0,
        }
    }

    /// What to draw.
    pub fn instances(&self) -> &[ExtractedInstance] {
        &self.instances
    }

    /// The chunk positions are relative to.
    pub const fn origin(&self) -> ChunkCoord {
        self.origin
    }

    /// The interpolation factor this frame was extracted with.
    pub const fn alpha(&self) -> f32 {
        self.alpha
    }

    /// How many instances were extracted.
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    /// Whether nothing was extracted.
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// Empties the view world, keeping its capacity for the next frame.
    pub fn clear(&mut self) {
        self.instances.clear();
    }
}

/// Copies the visual subset of sim state into `view`, interpolated by `alpha`.
///
/// `alpha` comes from the tick clock — `accumulator / tick duration` (S03) — and
/// is clamped, because a late frame can produce a value above 1 and
/// extrapolating there overshoots and snaps back.
///
/// `origin` is the floating-origin chunk, normally the camera's. Positions are
/// emitted relative to it.
pub fn extract(world: &mut SimWorld, view: &mut ViewWorld, alpha: f32, origin: ChunkCoord) {
    view.clear();
    view.alpha = alpha.clamp(0.0, 1.0);
    view.origin = origin;

    let mut query = world.query::<(&Transform, &PreviousTransform)>();

    for (transform, previous) in query.iter(world.inner()) {
        let blended = previous.0.interpolate(transform, view.alpha);

        // Rebase here and only here. The chunk difference is integer, so this
        // stays exact however far from the world origin the camera is.
        let chunk_offset = Vec3::new(
            (blended.position.chunk.x - origin.x) as f32 * CHUNK_SIZE,
            0.0,
            (blended.position.chunk.z - origin.z) as f32 * CHUNK_SIZE,
        );

        view.instances.push(ExtractedInstance {
            position: chunk_offset + blended.position.local,
            rotation: blended.rotation,
            scale: blended.scale,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::WorldPos;
    use cx_ecs::{SimWorld, WorldConfig};

    fn world_with(positions: &[(ChunkCoord, Vec3, Vec3)]) -> SimWorld {
        let mut world = SimWorld::new(WorldConfig::default());
        for (chunk, previous, current) in positions {
            let previous = Transform::from_position(WorldPos::new(*chunk, *previous));
            let current = Transform::from_position(WorldPos::new(*chunk, *current));
            world.spawn((current, PreviousTransform(previous)));
        }
        world
    }

    #[test]
    fn extract_interpolates_between_the_two_transforms() {
        let mut world = world_with(&[(
            ChunkCoord::new(0, 0),
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(10.0, 0.0, 0.0),
        )]);
        let mut view = ViewWorld::new();

        extract(&mut world, &mut view, 0.5, ChunkCoord::new(0, 0));

        assert_eq!(view.len(), 1);
        assert!((view.instances()[0].position.x - 5.0).abs() < 1e-4);
        assert!((view.alpha() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn floating_origin_is_applied_at_extract() {
        // The entity is 200 chunks out; with the camera there too, the renderer
        // should receive a small number rather than 102,400.
        let far = ChunkCoord::new(200, 0);
        let mut world = world_with(&[(far, Vec3::new(5.0, 0.0, 0.0), Vec3::new(5.0, 0.0, 0.0))]);
        let mut view = ViewWorld::new();

        extract(&mut world, &mut view, 1.0, far);

        let position = view.instances()[0].position;
        assert!(
            position.x.abs() < 10.0,
            "rebasing should keep the value small, got {}",
            position.x
        );
    }

    #[test]
    fn an_entity_a_chunk_away_is_offset_by_one_chunk() {
        let origin = ChunkCoord::new(0, 0);
        let mut world = world_with(&[(
            ChunkCoord::new(1, 0),
            Vec3::new(3.0, 0.0, 0.0),
            Vec3::new(3.0, 0.0, 0.0),
        )]);
        let mut view = ViewWorld::new();

        extract(&mut world, &mut view, 1.0, origin);

        assert!((view.instances()[0].position.x - (CHUNK_SIZE + 3.0)).abs() < 1e-3);
    }

    #[test]
    fn extract_clears_the_previous_frame() {
        let mut world = world_with(&[(ChunkCoord::new(0, 0), Vec3::ZERO, Vec3::ZERO)]);
        let mut view = ViewWorld::new();

        extract(&mut world, &mut view, 1.0, ChunkCoord::new(0, 0));
        extract(&mut world, &mut view, 1.0, ChunkCoord::new(0, 0));

        assert_eq!(view.len(), 1, "instances must not accumulate across frames");
    }

    #[test]
    fn clearing_keeps_capacity_so_extract_does_not_reallocate() {
        let mut view = ViewWorld::with_capacity(1_000);
        let capacity = view.instances.capacity();

        view.instances.push(ExtractedInstance {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        });
        view.clear();

        assert!(view.is_empty());
        assert_eq!(
            view.instances.capacity(),
            capacity,
            "capacity should survive a clear"
        );
    }

    #[test]
    fn alpha_above_one_is_clamped_rather_than_extrapolated() {
        let mut world =
            world_with(&[(ChunkCoord::new(0, 0), Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0))]);
        let mut view = ViewWorld::new();

        extract(&mut world, &mut view, 3.0, ChunkCoord::new(0, 0));

        assert!((view.instances()[0].position.x - 10.0).abs() < 1e-4);
        assert!((view.alpha() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn entities_without_a_previous_transform_are_not_extracted() {
        // An entity spawned this tick has no previous position to blend from.
        // Extracting it at its current position would pop it into existence
        // mid-interpolation; skipping it costs one frame of latency instead.
        let mut world = SimWorld::new(WorldConfig::default());
        world.spawn(Transform::from_position(WorldPos::new(
            ChunkCoord::new(0, 0),
            Vec3::ZERO,
        )));

        let mut view = ViewWorld::new();
        extract(&mut world, &mut view, 1.0, ChunkCoord::new(0, 0));

        assert!(view.is_empty());
    }
}
