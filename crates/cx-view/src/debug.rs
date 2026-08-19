//! Debug draw (S14/M1): lines, boxes, spheres, arrows.
//!
//! Everything here is line segments. A sphere is three rings, a box is twelve
//! edges, an arrow is a shaft and four barbs — so there is exactly one primitive
//! to render, one pipeline, and one gate to measure (10,000 lines under 1 ms).
//! Solid debug geometry would double all three for something that is harder to
//! read anyway, since wireframes let you see what is behind them.
//!
//! # World space in, extract space out
//!
//! Shapes are authored with [`WorldPos`], like everything else in the
//! simulation, and rebased at the same moment instances are — see
//! [`DebugDraw::rebase`]. Authoring in extract space would mean every caller had
//! to know the current origin, and would silently misplace anything drawn
//! before the camera moved.
//!
//! # Cleared every frame, never retained
//!
//! Debug draw is immediate mode: whoever wants a line draws it again next frame.
//! Retained handles would need a lifetime, an owner, and a way to leak — for
//! shapes whose entire purpose is to answer a question you are asking right now.

use cx_core::math::{ChunkCoord, Quat, Vec3, WorldPos};

/// A colour, as the GPU takes it.
///
/// Bytes rather than floats: four per vertex instead of sixteen, which at the
/// gate's 20,000 vertices is the difference between 320 KB and 80 KB uploaded
/// every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebugColour(pub [u8; 4]);

impl DebugColour {
    /// Opaque white.
    pub const WHITE: Self = Self([255, 255, 255, 255]);
    /// Opaque red. Conventionally +X, and errors.
    pub const RED: Self = Self([255, 64, 64, 255]);
    /// Opaque green. Conventionally +Y.
    pub const GREEN: Self = Self([64, 255, 64, 255]);
    /// Opaque blue. Conventionally +Z.
    pub const BLUE: Self = Self([80, 140, 255, 255]);
    /// Opaque yellow. Conventionally a highlight or a bound.
    pub const YELLOW: Self = Self([255, 220, 64, 255]);

    /// A colour from linear components in `0..=1`.
    pub fn from_linear(red: f32, green: f32, blue: f32, alpha: f32) -> Self {
        Self([to_byte(red), to_byte(green), to_byte(blue), to_byte(alpha)])
    }
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// One end of a line, as the GPU receives it.
///
/// Sixteen bytes: twelve of position, four of colour.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DebugVertex {
    /// Position in extract space, relative to the origin it was rebased against.
    pub position: [f32; 3],
    /// Vertex colour.
    pub colour: [u8; 4],
}

/// One line segment, before rebasing.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    from: WorldPos,
    to: WorldPos,
    colour: DebugColour,
}

/// Segments used to approximate a sphere's outline, per ring.
///
/// Sixteen reads as round at any size a debug sphere is useful at, and keeps a
/// sphere to 48 segments — cheap enough to draw one per agent without thinking
/// about it.
const SPHERE_SEGMENTS: usize = 16;

/// How long an arrow's barbs are, as a fraction of its length.
const ARROW_HEAD: f32 = 0.15;

/// Lines to draw this frame.
#[derive(Debug, Default)]
pub struct DebugDraw {
    segments: Vec<Segment>,
    vertices: Vec<DebugVertex>,
}

impl DebugDraw {
    /// An empty buffer sized for `capacity` segments.
    ///
    /// Sized up front for the same reason the view world is: this is filled
    /// every frame, and growing it during a frame puts an allocation on the
    /// frame path.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            segments: Vec::with_capacity(capacity),
            vertices: Vec::with_capacity(capacity * 2),
        }
    }

    /// How many segments are queued.
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Drops everything, keeping the allocation.
    pub fn clear(&mut self) {
        self.segments.clear();
        self.vertices.clear();
    }

    /// One line.
    pub fn line(&mut self, from: WorldPos, to: WorldPos, colour: DebugColour) {
        self.segments.push(Segment { from, to, colour });
    }

    /// A connected run of lines.
    ///
    /// Nothing is drawn for fewer than two points, rather than a zero-length
    /// segment: a path with one point is a caller bug, and a dot on screen where
    /// a path should be is a confusing way to report it.
    pub fn line_strip(&mut self, points: &[WorldPos], colour: DebugColour) {
        for pair in points.windows(2) {
            let [from, to] = pair else { continue };
            self.line(*from, *to, colour);
        }
    }

    /// A cross marking a point, `size` metres along each axis.
    ///
    /// The primitive for "this is where the thing is". Three axis-aligned
    /// segments, so it stays visible from any angle, unlike a single line.
    pub fn cross(&mut self, at: WorldPos, size: f32, colour: DebugColour) {
        let half = size * 0.5;
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            self.line(at.offset(-axis * half), at.offset(axis * half), colour);
        }
    }

    /// The twelve edges of an axis-aligned box.
    ///
    /// The corners are sorted, so passing them in the other order draws the same
    /// box rather than one with inverted extents — which renders as nothing and
    /// looks exactly like the box not being drawn at all.
    pub fn aabb(&mut self, a: WorldPos, b: WorldPos, colour: DebugColour) {
        let span = b.delta(a);
        let (min_x, max_x) = ordered(0.0, span.x);
        let (min_y, max_y) = ordered(0.0, span.y);
        let (min_z, max_z) = ordered(0.0, span.z);

        let corner = |x: f32, y: f32, z: f32| a.offset(Vec3::new(x, y, z));

        // Four edges along each axis.
        for (y, z) in [
            (min_y, min_z),
            (min_y, max_z),
            (max_y, min_z),
            (max_y, max_z),
        ] {
            self.line(corner(min_x, y, z), corner(max_x, y, z), colour);
        }
        for (x, z) in [
            (min_x, min_z),
            (min_x, max_z),
            (max_x, min_z),
            (max_x, max_z),
        ] {
            self.line(corner(x, min_y, z), corner(x, max_y, z), colour);
        }
        for (x, y) in [
            (min_x, min_y),
            (min_x, max_y),
            (max_x, min_y),
            (max_x, max_y),
        ] {
            self.line(corner(x, y, min_z), corner(x, y, max_z), colour);
        }
    }

    /// Three rings approximating a sphere.
    pub fn sphere(&mut self, centre: WorldPos, radius: f32, colour: DebugColour) {
        if radius <= 0.0 {
            return;
        }

        for (u, v) in [(Vec3::X, Vec3::Y), (Vec3::Y, Vec3::Z), (Vec3::Z, Vec3::X)] {
            self.ring(centre, radius, u, v, colour);
        }
    }

    /// A single ring in the plane spanned by `u` and `v`.
    fn ring(&mut self, centre: WorldPos, radius: f32, u: Vec3, v: Vec3, colour: DebugColour) {
        let step = std::f32::consts::TAU / SPHERE_SEGMENTS as f32;
        let point = |index: usize| {
            let angle = step * index as f32;
            centre.offset((u * angle.cos() + v * angle.sin()) * radius)
        };

        // `..SPHERE_SEGMENTS` with the last segment wrapping to index 0, rather
        // than `..=`: recomputing the first point would give a value one float
        // rounding away from it and leave a hairline gap in the ring.
        let first = point(0);
        let mut previous = first;
        for index in 1..SPHERE_SEGMENTS {
            let next = point(index);
            self.line(previous, next, colour);
            previous = next;
        }
        self.line(previous, first, colour);
    }

    /// A shaft with a four-barbed head at `to`.
    ///
    /// Four barbs rather than two, so the head reads as an arrow from any angle.
    /// A two-barbed head disappears edge-on, which is reliably the angle you are
    /// looking from when it matters.
    pub fn arrow(&mut self, from: WorldPos, to: WorldPos, colour: DebugColour) {
        self.line(from, to, colour);

        let along = to.delta(from);
        let length = along.length();
        if length <= f32::EPSILON {
            return;
        }

        let direction = along / length;
        // Any vector not parallel to the shaft works as a reference. Y unless
        // the arrow points along Y, in which case X — otherwise the cross
        // product is zero and the head collapses to nothing.
        let reference = if direction.y.abs() > 0.99 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let side = direction.cross(reference).normalize_or_zero();
        let other = direction.cross(side).normalize_or_zero();

        let back = -direction * length * ARROW_HEAD;
        let spread = length * ARROW_HEAD * 0.5;

        for offset in [side, -side, other, -other] {
            self.line(to, to.offset(back + offset * spread), colour);
        }
    }

    /// A box with an orientation, drawn as twelve edges.
    ///
    /// Distinct from [`DebugDraw::aabb`] because a rotated bound drawn as an
    /// axis-aligned one is wrong in the exact case you are drawing it to check.
    pub fn obb(
        &mut self,
        centre: WorldPos,
        half_extents: Vec3,
        rotation: Quat,
        colour: DebugColour,
    ) {
        let corner = |sx: f32, sy: f32, sz: f32| {
            centre.offset(rotation * (half_extents * Vec3::new(sx, sy, sz)))
        };

        let corners = [
            corner(-1.0, -1.0, -1.0),
            corner(1.0, -1.0, -1.0),
            corner(1.0, -1.0, 1.0),
            corner(-1.0, -1.0, 1.0),
            corner(-1.0, 1.0, -1.0),
            corner(1.0, 1.0, -1.0),
            corner(1.0, 1.0, 1.0),
            corner(-1.0, 1.0, 1.0),
        ];

        const EDGES: [(usize, usize); 12] = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];

        for (a, b) in EDGES {
            let (Some(from), Some(to)) = (corners.get(a), corners.get(b)) else {
                continue;
            };
            self.line(*from, *to, colour);
        }
    }

    /// The queued segments as vertices in extract space.
    ///
    /// Rebased here rather than at authoring time so every shape uses the origin
    /// the frame is actually drawn with — the same rule the instance extract
    /// follows, and for the same reason.
    pub fn rebase(&mut self, origin: ChunkCoord) -> &[DebugVertex] {
        let anchor = WorldPos::new(origin, Vec3::ZERO);

        self.vertices.clear();
        self.vertices.reserve(self.segments.len() * 2);

        for segment in &self.segments {
            for end in [segment.from, segment.to] {
                let position = end.delta(anchor);
                self.vertices.push(DebugVertex {
                    position: position.to_array(),
                    colour: segment.colour.0,
                });
            }
        }

        &self.vertices
    }

    /// The most recent [`DebugDraw::rebase`] result.
    pub fn vertices(&self) -> &[DebugVertex] {
        &self.vertices
    }
}

fn ordered(a: f32, b: f32) -> (f32, f32) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: ChunkCoord = ChunkCoord { x: 0, z: 0 };

    fn at(x: f32, y: f32, z: f32) -> WorldPos {
        WorldPos::new(ORIGIN, Vec3::new(x, y, z))
    }

    #[test]
    fn a_line_becomes_two_vertices_in_order() {
        let mut draw = DebugDraw::default();
        draw.line(at(0.0, 0.0, 0.0), at(1.0, 2.0, 3.0), DebugColour::RED);

        assert_eq!(draw.len(), 1);
        let vertices = draw.rebase(ORIGIN);
        assert_eq!(vertices.len(), 2);
        assert_eq!(vertices[0].position, [0.0, 0.0, 0.0]);
        assert_eq!(vertices[1].position, [1.0, 2.0, 3.0]);
        assert_eq!(vertices[0].colour, DebugColour::RED.0);
    }

    #[test]
    fn shapes_have_the_segment_counts_they_claim() {
        // The counts are load-bearing: the 10,000-line gate is measured in
        // segments, so a sphere that quietly costs 96 instead of 48 makes the
        // budget mean half what it says.
        /// Name, the call that builds the shape, and the segments it should cost.
        type Case = (&'static str, fn(&mut DebugDraw), usize);

        let cases: [Case; 5] = [
            (
                "cross",
                |d| d.cross(at(0.0, 0.0, 0.0), 1.0, DebugColour::WHITE),
                3,
            ),
            (
                "aabb",
                |d| d.aabb(at(0.0, 0.0, 0.0), at(1.0, 1.0, 1.0), DebugColour::WHITE),
                12,
            ),
            (
                "obb",
                |d| {
                    d.obb(
                        at(0.0, 0.0, 0.0),
                        Vec3::ONE,
                        Quat::IDENTITY,
                        DebugColour::WHITE,
                    )
                },
                12,
            ),
            (
                "sphere",
                |d| d.sphere(at(0.0, 0.0, 0.0), 1.0, DebugColour::WHITE),
                3 * SPHERE_SEGMENTS,
            ),
            (
                "arrow",
                |d| d.arrow(at(0.0, 0.0, 0.0), at(0.0, 0.0, 5.0), DebugColour::WHITE),
                5,
            ),
        ];

        for (name, build, expected) in cases {
            let mut draw = DebugDraw::default();
            build(&mut draw);
            assert_eq!(draw.len(), expected, "{name} should be {expected} segments");
            assert_eq!(draw.rebase(ORIGIN).len(), expected * 2);
        }
    }

    #[test]
    fn a_box_given_its_corners_backwards_is_the_same_box() {
        // Inverted extents render as nothing, which looks exactly like the box
        // never being drawn.
        let mut forwards = DebugDraw::default();
        forwards.aabb(at(-1.0, -2.0, -3.0), at(4.0, 5.0, 6.0), DebugColour::WHITE);
        let forwards_vertices: Vec<DebugVertex> = forwards.rebase(ORIGIN).to_vec();

        let mut backwards = DebugDraw::default();
        backwards.aabb(at(4.0, 5.0, 6.0), at(-1.0, -2.0, -3.0), DebugColour::WHITE);
        let backwards_vertices: Vec<DebugVertex> = backwards.rebase(ORIGIN).to_vec();

        let extent = |vertices: &[DebugVertex], axis: usize| {
            let values: Vec<f32> = vertices
                .iter()
                .filter_map(|vertex| vertex.position.get(axis).copied())
                .collect();
            let min = values.iter().copied().fold(f32::INFINITY, f32::min);
            let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            (min, max)
        };

        for axis in 0..3 {
            assert_eq!(
                extent(&forwards_vertices, axis),
                extent(&backwards_vertices, axis),
                "axis {axis} should cover the same range either way"
            );
        }
    }

    #[test]
    fn a_box_spans_exactly_its_corners() {
        let mut draw = DebugDraw::default();
        draw.aabb(at(1.0, 2.0, 3.0), at(4.0, 6.0, 8.0), DebugColour::WHITE);

        let vertices = draw.rebase(ORIGIN);
        for (axis, (low, high)) in [(0, (1.0, 4.0)), (1, (2.0, 6.0)), (2, (3.0, 8.0))] {
            let values: Vec<f32> = vertices
                .iter()
                .filter_map(|vertex| vertex.position.get(axis).copied())
                .collect();
            assert!(
                values.iter().any(|value| (value - low).abs() < 1e-6),
                "axis {axis} should touch {low}"
            );
            assert!(
                values.iter().any(|value| (value - high).abs() < 1e-6),
                "axis {axis} should touch {high}"
            );
            assert!(
                values
                    .iter()
                    .all(|value| *value >= low - 1e-6 && *value <= high + 1e-6),
                "axis {axis} should stay within its corners"
            );
        }
    }

    #[test]
    fn a_sphere_ring_closes() {
        // An open ring leaves a visible gap; recomputing the first point instead
        // of reusing it leaves a hairline one, which is worse because it looks
        // like a rendering artifact rather than a bug here.
        let mut draw = DebugDraw::default();
        draw.sphere(at(0.0, 0.0, 0.0), 2.0, DebugColour::WHITE);

        let vertices = draw.rebase(ORIGIN).to_vec();
        let ring = &vertices[..SPHERE_SEGMENTS * 2];

        // Consecutive segments share an endpoint, exactly.
        for pair in ring.chunks_exact(2).collect::<Vec<_>>().windows(2) {
            let [current, next] = pair else { continue };
            assert_eq!(
                current[1].position, next[0].position,
                "each segment must start where the last one ended"
            );
        }

        let first = ring[0].position;
        let last = ring[ring.len() - 1].position;
        assert_eq!(first, last, "the ring must close on the exact first point");
    }

    #[test]
    fn every_sphere_point_is_on_the_sphere() {
        let mut draw = DebugDraw::default();
        let radius = 3.5;
        draw.sphere(at(10.0, -4.0, 2.0), radius, DebugColour::WHITE);

        for vertex in draw.rebase(ORIGIN) {
            let offset = Vec3::from_array(vertex.position) - Vec3::new(10.0, -4.0, 2.0);
            assert!(
                (offset.length() - radius).abs() < 1e-4,
                "point {:?} is not on the sphere",
                vertex.position
            );
        }
    }

    #[test]
    fn an_arrow_pointing_straight_up_still_has_a_head() {
        // The degenerate case: a shaft parallel to the reference axis makes the
        // cross product zero, and the head silently collapses to four
        // zero-length segments at the tip.
        let mut draw = DebugDraw::default();
        draw.arrow(at(0.0, 0.0, 0.0), at(0.0, 5.0, 0.0), DebugColour::WHITE);

        let vertices = draw.rebase(ORIGIN);
        let tip = [0.0, 5.0, 0.0];
        let barbs = vertices.chunks_exact(2).skip(1);

        for barb in barbs {
            assert_eq!(barb[0].position, tip, "each barb starts at the tip");
            let length = (Vec3::from_array(barb[1].position) - Vec3::from_array(tip)).length();
            assert!(
                length > 0.1,
                "a barb collapsed to nothing: {:?}",
                barb[1].position
            );
        }
    }

    #[test]
    fn a_zero_length_arrow_draws_a_shaft_and_no_head() {
        let mut draw = DebugDraw::default();
        draw.arrow(at(1.0, 1.0, 1.0), at(1.0, 1.0, 1.0), DebugColour::WHITE);
        assert_eq!(draw.len(), 1, "no direction means no head to orient");
    }

    #[test]
    fn a_zero_radius_sphere_draws_nothing() {
        let mut draw = DebugDraw::default();
        draw.sphere(at(0.0, 0.0, 0.0), 0.0, DebugColour::WHITE);
        assert!(draw.is_empty());
    }

    #[test]
    fn a_strip_needs_two_points_to_draw_anything() {
        let mut draw = DebugDraw::default();
        draw.line_strip(&[], DebugColour::WHITE);
        draw.line_strip(&[at(0.0, 0.0, 0.0)], DebugColour::WHITE);
        assert!(draw.is_empty(), "fewer than two points is not a line");

        draw.line_strip(
            &[at(0.0, 0.0, 0.0), at(1.0, 0.0, 0.0), at(2.0, 0.0, 0.0)],
            DebugColour::WHITE,
        );
        assert_eq!(draw.len(), 2, "three points make two segments");
    }

    #[test]
    fn rebasing_moves_everything_by_the_origin_and_nothing_else() {
        // Shapes are authored in world space, so a camera that has moved to
        // another chunk must not drag the geometry with it.
        let far = WorldPos::new(ChunkCoord::new(2, -3), Vec3::new(5.0, 1.0, 2.0));
        let mut draw = DebugDraw::default();
        draw.line(
            far,
            far.offset(Vec3::new(1.0, 0.0, 0.0)),
            DebugColour::WHITE,
        );

        let near_origin = draw.rebase(ChunkCoord::new(2, -3)).to_vec();
        let far_origin = draw.rebase(ChunkCoord::new(0, 0)).to_vec();

        assert_eq!(near_origin[0].position, [5.0, 1.0, 2.0]);

        let shift =
            Vec3::from_array(far_origin[0].position) - Vec3::from_array(near_origin[0].position);
        for (near, far) in near_origin.iter().zip(far_origin.iter()) {
            let delta = Vec3::from_array(far.position) - Vec3::from_array(near.position);
            assert!(
                (delta - shift).length() < 1e-3,
                "every vertex should shift by the same amount"
            );
        }
    }

    #[test]
    fn clearing_keeps_the_allocation() {
        // Debug draw is refilled every frame; reallocating each time would put
        // an allocation on the frame path for a tool used while profiling.
        let mut draw = DebugDraw::with_capacity(256);

        for index in 0..100 {
            draw.cross(at(index as f32, 0.0, 0.0), 1.0, DebugColour::WHITE);
        }
        draw.rebase(ORIGIN);

        // Captured *after* filling, not before: 100 crosses is 300 segments,
        // which legitimately grows past the 256 reserved. The property is that
        // clearing keeps whatever has been grown, not that growth never happens.
        let segments = draw.segments.capacity();
        let vertices = draw.vertices.capacity();
        assert!(segments >= 300 && vertices >= 600);

        draw.clear();

        assert!(draw.is_empty());
        assert_eq!(draw.segments.capacity(), segments);
        assert_eq!(draw.vertices.capacity(), vertices);
    }

    #[test]
    fn a_vertex_is_sixteen_bytes() {
        // Four floats' worth. Colour as bytes rather than floats is what keeps
        // the gate's 20,000 vertices to 320 KB rather than 1.2 MB.
        assert_eq!(size_of::<DebugVertex>(), 16);
    }
}
