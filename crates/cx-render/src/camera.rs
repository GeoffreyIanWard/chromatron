//! The view and projection the renderer draws through.
//!
//! # Extract space, not world space
//!
//! A camera here is positioned in the same space `cx-view` emits: metres
//! relative to the extract origin chunk (`03-conventions.md` puts floating
//! origin at extract time). That is why the fields are plain `Vec3` rather than
//! `WorldPos` — by the time geometry reaches the renderer it has already been
//! rebased, and re-introducing world coordinates here would undo the precision
//! the rebasing bought.
//!
//! The practical consequence: whoever drives the renderer must extract with the
//! *same* origin chunk it positions the camera in. Mixing them puts the world
//! several hundred metres from where the camera is looking, per chunk of
//! disagreement.

use cx_core::math::glam::camera::rh::{proj, view};
use cx_core::math::{Mat4, Vec3};

/// A perspective camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// Eye position, in extract space.
    pub position: Vec3,
    /// Point being looked at, in extract space.
    pub target: Vec3,
    /// Which way is up. Normally `Vec3::Y` — the engine is Y-up, right-handed,
    /// matching glTF (`03-conventions.md`).
    pub up: Vec3,
    /// Vertical field of view, in radians.
    pub fov_y: f32,
    /// Near clip plane, in metres.
    pub near: f32,
    /// Far clip plane, in metres.
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            position: Vec3::new(0.0, 0.0, 5.0),
            target: Vec3::ZERO,
            up: Vec3::Y,
            fov_y: std::f32::consts::FRAC_PI_3,
            // 0.1 m near plane: closer crushes depth precision, and nothing in a
            // 3D simulation at this scale needs to render a centimetre from the
            // eye.
            near: 0.1,
            far: 2_000.0,
        }
    }
}

impl Camera {
    /// A camera at `position` looking at `target`, with default lens settings.
    pub fn looking_at(position: Vec3, target: Vec3) -> Self {
        Self {
            position,
            target,
            ..Self::default()
        }
    }

    /// The combined view-projection matrix for a target of the given aspect.
    ///
    /// Right-handed with a `0..1` depth range. glam names that convention
    /// `directx`, which is also what wgpu's NDC uses; the `opengl` variant would
    /// produce `-1..1` and quietly halve the usable depth buffer, showing up as
    /// z-fighting rather than as an error.
    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        };
        let projection = proj::directx::perspective(self.fov_y, aspect, self.near, self.far);
        let view_matrix = view::look_at_mat4(self.position, self.target, self.up);
        projection * view_matrix
    }

    /// Direction the camera is facing, normalized.
    pub fn forward(&self) -> Vec3 {
        (self.target - self.position).normalize_or_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::Vec4;

    /// Projects a point and returns its normalized device coordinates.
    fn project(camera: &Camera, point: Vec3, aspect: f32) -> Option<Vec3> {
        let clip = camera.view_projection(aspect) * Vec4::new(point.x, point.y, point.z, 1.0);
        if clip.w <= 0.0 {
            // Behind the eye. Dividing anyway is how geometry behind the camera
            // ends up mirrored onto the screen.
            return None;
        }
        Some(Vec3::new(clip.x / clip.w, clip.y / clip.w, clip.z / clip.w))
    }

    #[test]
    fn the_target_projects_to_the_centre_of_the_screen() {
        let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let ndc = project(&camera, Vec3::ZERO, 1.0).expect("the target is in front of the eye");

        assert!(
            ndc.x.abs() < 1e-5,
            "target should be horizontally centred, got {}",
            ndc.x
        );
        assert!(
            ndc.y.abs() < 1e-5,
            "target should be vertically centred, got {}",
            ndc.y
        );
    }

    #[test]
    fn depth_is_in_the_zero_to_one_range_wgpu_expects() {
        // perspective_rh_gl would give -1..1 here and silently waste half the
        // depth buffer, which shows up as z-fighting rather than as an error.
        let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);

        let near_point = project(&camera, Vec3::new(0.0, 0.0, 9.0), 1.0).expect("in front");
        let far_point = project(&camera, Vec3::new(0.0, 0.0, -100.0), 1.0).expect("in front");

        assert!(
            (0.0..=1.0).contains(&near_point.z),
            "near depth {} out of range",
            near_point.z
        );
        assert!(
            (0.0..=1.0).contains(&far_point.z),
            "far depth {} out of range",
            far_point.z
        );
        assert!(
            near_point.z < far_point.z,
            "nearer geometry must have smaller depth"
        );
    }

    #[test]
    fn geometry_behind_the_camera_is_not_projected_forward() {
        let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        // Ten metres behind the eye.
        assert!(project(&camera, Vec3::new(0.0, 0.0, 15.0), 1.0).is_none());
    }

    #[test]
    fn a_wider_target_compresses_horizontally_not_vertically() {
        // The check that catches an inverted aspect ratio, which otherwise just
        // looks subtly wrong rather than broken.
        let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let offset = Vec3::new(1.0, 1.0, 0.0);

        let square = project(&camera, offset, 1.0).expect("in front");
        let wide = project(&camera, offset, 2.0).expect("in front");

        assert!(
            wide.x.abs() < square.x.abs(),
            "a wider target should compress x"
        );
        assert!(
            (wide.y - square.y).abs() < 1e-5,
            "y should not change with aspect"
        );
    }

    #[test]
    fn a_degenerate_aspect_falls_back_rather_than_producing_nan() {
        // A zero-height window during a resize is a real event, and NaN in a
        // matrix propagates into every vertex before anyone notices.
        let camera = Camera::default();
        let matrix = camera.view_projection(0.0);
        assert!(matrix.to_cols_array().iter().all(|value| value.is_finite()));
    }

    #[test]
    fn forward_points_from_the_eye_towards_the_target() {
        let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let forward = camera.forward();
        assert!((forward - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-5);
    }
}
