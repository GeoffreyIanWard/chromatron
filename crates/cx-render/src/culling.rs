//! Frustum extraction and the visibility test (S12/M1).
//!
//! The culling *decision* lives here, in Rust, even though the culling *work*
//! happens in a compute shader. The shader implements the same six inequalities;
//! this is where they are stated and tested, and where a mismatch between the
//! two is caught — see the test that runs both over the same instances.
//!
//! # Planes from the matrix, not from the camera
//!
//! The six planes come from the view-projection matrix directly
//! (Gribb–Hartmann), not from reconstructing corners out of the camera's field
//! of view and aspect ratio. Two reasons:
//!
//! - It cannot disagree with what is actually drawn. The matrix that culls is
//!   the matrix that projects, so an off-by-one in the aspect ratio moves both
//!   together instead of culling things that would have been visible.
//! - It does not care which projection convention is in use. This engine uses a
//!   `0..1` depth range (`ADR`-free, but see `Camera::view_projection`), and the
//!   near-plane row differs between that and OpenGL's `-1..1`. Deriving from the
//!   matrix means the difference is already accounted for.

use cx_core::math::{Mat4, Vec3, Vec4};

/// A plane, as `ax + by + cz + d = 0` with the normal pointing *into* the
/// visible half-space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    /// Plane normal, normalized.
    pub normal: Vec3,
    /// Distance term.
    pub distance: f32,
}

impl Plane {
    /// Signed distance from the plane to a point. Positive is inside.
    pub fn distance_to(&self, point: Vec3) -> f32 {
        self.normal.dot(point) + self.distance
    }
}

/// The six planes bounding what a camera can see.
///
/// Ordered left, right, bottom, top, near, far — the order the extraction
/// produces and the order the shader reads. The order is not arbitrary
/// decoration: the shader indexes the same array, so the two have to agree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frustum {
    /// The planes, all normals pointing inward.
    pub planes: [Plane; 6],
}

impl Frustum {
    /// Extracts the frustum from a view-projection matrix.
    pub fn from_view_projection(view_projection: Mat4) -> Self {
        // Rows of the matrix, which is where the plane equations live. `glam` is
        // column-major, so a "row" is one component taken from each column.
        let matrix = view_projection.to_cols_array_2d();
        let row = |index: usize| {
            Vec4::new(
                matrix[0][index],
                matrix[1][index],
                matrix[2][index],
                matrix[3][index],
            )
        };

        let x = row(0);
        let y = row(1);
        let z = row(2);
        let w = row(3);

        // Left is w + x, right is w - x, and so on. The near plane is `z` alone
        // rather than `w + z`, which is the part that differs between a `0..1`
        // depth range and OpenGL's `-1..1` — and getting it wrong clips
        // everything closer than half the far plane, which looks like a draw
        // distance bug rather than a culling one.
        Self {
            planes: [
                plane(w + x),
                plane(w - x),
                plane(w + y),
                plane(w - y),
                plane(z),
                plane(w - z),
            ],
        }
    }

    /// Whether a sphere is at least partly inside.
    ///
    /// Conservative: a sphere straddling a plane counts as visible. Culling
    /// something that should have been drawn is a hole in the world; drawing
    /// something that turns out to be off-screen costs a few wasted fragments.
    pub fn intersects_sphere(&self, centre: Vec3, radius: f32) -> bool {
        self.planes
            .iter()
            .all(|plane| plane.distance_to(centre) >= -radius)
    }

    /// The planes as raw floats, in the layout the compute shader reads.
    ///
    /// `[nx, ny, nz, d]` per plane, in the same order as [`Frustum::planes`].
    pub fn to_raw(self) -> [[f32; 4]; 6] {
        self.planes.map(|plane| {
            [
                plane.normal.x,
                plane.normal.y,
                plane.normal.z,
                plane.distance,
            ]
        })
    }
}

/// Normalizes a plane equation so distances are in world units.
///
/// Without this the "distance" is scaled by the normal's length, which varies
/// per plane — so a radius compared against it would be wrong by a different
/// factor on each side of the frustum. The bug looks like objects popping out
/// at the top of the screen but not the sides.
fn plane(equation: Vec4) -> Plane {
    let normal = Vec3::new(equation.x, equation.y, equation.z);
    let length = normal.length();

    if length > 0.0 {
        Plane {
            normal: normal / length,
            distance: equation.w / length,
        }
    } else {
        // A degenerate matrix. Everything is "inside" rather than nothing:
        // drawing too much is a frame-rate problem, drawing nothing is a black
        // screen with no explanation.
        Plane {
            normal: Vec3::ZERO,
            distance: f32::INFINITY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;

    fn camera_at(position: Vec3, target: Vec3) -> Camera {
        Camera::looking_at(position, target)
    }

    fn frustum_of(camera: &Camera) -> Frustum {
        Frustum::from_view_projection(camera.view_projection(16.0 / 9.0))
    }

    #[test]
    fn a_point_in_front_of_the_camera_is_visible() {
        let camera = camera_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let frustum = frustum_of(&camera);

        assert!(frustum.intersects_sphere(Vec3::ZERO, 0.0));
        assert!(frustum.intersects_sphere(Vec3::new(0.0, 0.0, 5.0), 0.0));
    }

    #[test]
    fn a_point_behind_the_camera_is_culled() {
        // The case that matters most: geometry behind the viewer is half the
        // world, and a near plane pointing the wrong way passes all of it.
        let camera = camera_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let frustum = frustum_of(&camera);

        assert!(!frustum.intersects_sphere(Vec3::new(0.0, 0.0, 20.0), 0.0));
        assert!(!frustum.intersects_sphere(Vec3::new(0.0, 0.0, 100.0), 0.0));
    }

    #[test]
    fn a_point_beyond_the_far_plane_is_culled() {
        let camera = camera_at(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0));
        let frustum = frustum_of(&camera);

        assert!(frustum.intersects_sphere(Vec3::new(0.0, 0.0, -100.0), 0.0));
        assert!(
            !frustum.intersects_sphere(Vec3::new(0.0, 0.0, -5_000.0), 0.0),
            "the default far plane is 2000 m"
        );
    }

    #[test]
    fn points_outside_the_sides_are_culled() {
        let camera = camera_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let frustum = frustum_of(&camera);

        for offset in [
            Vec3::new(500.0, 0.0, 0.0),
            Vec3::new(-500.0, 0.0, 0.0),
            Vec3::new(0.0, 500.0, 0.0),
            Vec3::new(0.0, -500.0, 0.0),
        ] {
            assert!(
                !frustum.intersects_sphere(offset, 1.0),
                "{offset:?} should be outside the frustum"
            );
        }
    }

    #[test]
    fn a_sphere_straddling_a_plane_counts_as_visible() {
        // Conservative on purpose. Culling something that should have been drawn
        // is a hole in the world; drawing something off-screen costs fragments.
        let camera = camera_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO);
        let frustum = frustum_of(&camera);

        // Just behind the camera, but large enough to reach in front of it.
        let behind = Vec3::new(0.0, 0.0, 11.0);
        assert!(!frustum.intersects_sphere(behind, 0.1));
        assert!(frustum.intersects_sphere(behind, 5.0));
    }

    #[test]
    fn the_planes_are_normalized() {
        // If they are not, the "distance" is scaled by the normal's length,
        // which differs per plane — so a radius means something different on
        // each side of the frustum. The symptom is objects popping out at the
        // top of the screen but not the sides.
        let camera = camera_at(Vec3::new(3.0, 4.0, 5.0), Vec3::new(-2.0, 0.0, 1.0));
        let frustum = frustum_of(&camera);

        for (index, plane) in frustum.planes.iter().enumerate() {
            assert!(
                (plane.normal.length() - 1.0).abs() < 1e-4,
                "plane {index} has a normal of length {}",
                plane.normal.length()
            );
        }
    }

    #[test]
    fn distance_is_in_world_units() {
        // The consequence of normalizing: a point exactly `d` in front of the
        // near plane is `d` metres from it, so a bounding radius can be compared
        // against it directly.
        let camera = Camera::looking_at(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0));
        let frustum = frustum_of(&camera);
        let near = frustum.planes[4];

        // The near plane is 0.1 m ahead; a point 10 m ahead is 9.9 m past it.
        let distance = near.distance_to(Vec3::new(0.0, 0.0, -10.0));
        assert!(
            (distance - 9.9).abs() < 0.01,
            "expected about 9.9 m past the near plane, got {distance}"
        );
    }

    #[test]
    fn the_frustum_follows_the_camera() {
        // A frustum built once and reused is a common bug: the world keeps
        // rendering but things vanish as the camera turns away from where it
        // was when the planes were built.
        let point = Vec3::new(0.0, 0.0, -50.0);

        let looking_at_it = frustum_of(&camera_at(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0)));
        let looking_away = frustum_of(&camera_at(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0)));

        assert!(looking_at_it.intersects_sphere(point, 1.0));
        assert!(!looking_away.intersects_sphere(point, 1.0));
    }

    #[test]
    fn a_degenerate_matrix_shows_everything_rather_than_nothing() {
        // A zero matrix can arise from a zero-sized viewport mid-resize.
        // Drawing too much is a frame-rate problem; drawing nothing is a black
        // screen with no explanation.
        let frustum = Frustum::from_view_projection(Mat4::ZERO);
        assert!(frustum.intersects_sphere(Vec3::new(1_000.0, 0.0, 0.0), 1.0));
    }

    #[test]
    fn the_raw_layout_matches_the_planes() {
        // Two declarations of one layout: this array and the shader's struct. A
        // mismatch silently culls against the wrong plane.
        let frustum = frustum_of(&camera_at(Vec3::new(0.0, 5.0, 10.0), Vec3::ZERO));
        let raw = frustum.to_raw();

        for (index, plane) in frustum.planes.iter().enumerate() {
            assert_eq!(raw[index][0], plane.normal.x);
            assert_eq!(raw[index][1], plane.normal.y);
            assert_eq!(raw[index][2], plane.normal.z);
            assert_eq!(raw[index][3], plane.distance);
        }
    }

    #[test]
    fn culling_keeps_what_a_projection_would_actually_draw() {
        // The end-to-end property, checked against the projection itself rather
        // than against intuition: anything the frustum keeps should land inside
        // clip space, and the two must agree about the boundary.
        let camera = camera_at(Vec3::new(0.0, 20.0, 40.0), Vec3::ZERO);
        let view_projection = camera.view_projection(16.0 / 9.0);
        let frustum = Frustum::from_view_projection(view_projection);

        let mut disagreements = 0;
        for index in 0..2_000 {
            let angle = index as f32 * 0.37;
            let point = Vec3::new(
                angle.cos() * (index as f32 % 97.0),
                (index as f32 % 31.0) - 15.0,
                angle.sin() * (index as f32 % 89.0),
            );

            let clip = view_projection * point.extend(1.0);
            // Inside clip space for a 0..1 depth range.
            let projected_visible = clip.w > 0.0
                && clip.x.abs() <= clip.w
                && clip.y.abs() <= clip.w
                && clip.z >= 0.0
                && clip.z <= clip.w;

            if frustum.intersects_sphere(point, 0.0) != projected_visible {
                disagreements += 1;
            }
        }

        assert_eq!(
            disagreements, 0,
            "the frustum and the projection disagreed about {disagreements} points"
        );
    }
}
