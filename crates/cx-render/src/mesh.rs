//! Mesh data, and the one mesh that exists before the asset pipeline.
//!
//! S12 targets low-poly flat-shaded geometry sharing a palette atlas, and M3
//! brings the pipeline that produces it. Until then there is a hand-authored
//! cube, which is enough to answer the questions M1 actually asks: does an
//! instanced draw land, in the right place, at the right size, facing the right
//! way.

use cx_core::math::Vec3;

/// One vertex of a mesh.
///
/// `repr(C)` because this is uploaded straight to the GPU — the Rust field order
/// guarantee is what makes the vertex layout below correct.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    /// Model-space position.
    pub position: [f32; 3],
    /// Model-space normal.
    pub normal: [f32; 3],
}

impl Vertex {
    /// How the GPU should read a vertex buffer of these.
    pub(crate) const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            },
            wgpu::VertexAttribute {
                offset: size_of::<[f32; 3]>() as wgpu::BufferAddress,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x3,
            },
        ],
    };
}

/// Vertices and indices for one mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshData {
    /// Vertices.
    pub vertices: Vec<Vertex>,
    /// Triangle indices into `vertices`.
    pub indices: Vec<u16>,
}

impl MeshData {
    /// How many triangles this mesh draws.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// A unit cube centred on the origin, one metre to a side.
    ///
    /// Twenty-four vertices rather than eight: each face needs its own normals,
    /// and sharing corner vertices would average them into something rounded.
    /// Flat shading is the look S12 asks for, and it starts here.
    ///
    /// Winding is counter-clockwise when viewed from outside, matching
    /// `03-conventions.md` and glTF, so back-face culling removes the inside.
    pub fn unit_cube() -> Self {
        // Each face: outward normal, then its four corners in CCW order seen
        // from outside.
        let faces: [(Vec3, [Vec3; 4]); 6] = [
            // +Z front
            (
                Vec3::Z,
                [
                    Vec3::new(-0.5, -0.5, 0.5),
                    Vec3::new(0.5, -0.5, 0.5),
                    Vec3::new(0.5, 0.5, 0.5),
                    Vec3::new(-0.5, 0.5, 0.5),
                ],
            ),
            // -Z back
            (
                Vec3::NEG_Z,
                [
                    Vec3::new(0.5, -0.5, -0.5),
                    Vec3::new(-0.5, -0.5, -0.5),
                    Vec3::new(-0.5, 0.5, -0.5),
                    Vec3::new(0.5, 0.5, -0.5),
                ],
            ),
            // +X right
            (
                Vec3::X,
                [
                    Vec3::new(0.5, -0.5, 0.5),
                    Vec3::new(0.5, -0.5, -0.5),
                    Vec3::new(0.5, 0.5, -0.5),
                    Vec3::new(0.5, 0.5, 0.5),
                ],
            ),
            // -X left
            (
                Vec3::NEG_X,
                [
                    Vec3::new(-0.5, -0.5, -0.5),
                    Vec3::new(-0.5, -0.5, 0.5),
                    Vec3::new(-0.5, 0.5, 0.5),
                    Vec3::new(-0.5, 0.5, -0.5),
                ],
            ),
            // +Y top
            (
                Vec3::Y,
                [
                    Vec3::new(-0.5, 0.5, 0.5),
                    Vec3::new(0.5, 0.5, 0.5),
                    Vec3::new(0.5, 0.5, -0.5),
                    Vec3::new(-0.5, 0.5, -0.5),
                ],
            ),
            // -Y bottom
            (
                Vec3::NEG_Y,
                [
                    Vec3::new(-0.5, -0.5, -0.5),
                    Vec3::new(0.5, -0.5, -0.5),
                    Vec3::new(0.5, -0.5, 0.5),
                    Vec3::new(-0.5, -0.5, 0.5),
                ],
            ),
        ];

        let mut vertices = Vec::with_capacity(24);
        let mut indices = Vec::with_capacity(36);

        for (normal, corners) in faces {
            let base = vertices.len() as u16;
            for corner in corners {
                vertices.push(Vertex {
                    position: [corner.x, corner.y, corner.z],
                    normal: [normal.x, normal.y, normal.z],
                });
            }
            // Two triangles per quad, preserving the CCW order above.
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }

        Self { vertices, indices }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cube_has_flat_faces_not_shared_corners() {
        let cube = MeshData::unit_cube();

        assert_eq!(
            cube.vertices.len(),
            24,
            "eight would mean averaged corner normals"
        );
        assert_eq!(cube.triangle_count(), 12);
    }

    #[test]
    fn every_face_normal_is_axis_aligned_and_unit_length() {
        for vertex in MeshData::unit_cube().vertices {
            let normal = Vec3::from_array(vertex.normal);
            assert!(
                (normal.length() - 1.0).abs() < 1e-6,
                "normal {normal:?} is not unit length"
            );

            let axis_aligned = normal.abs().to_array().iter().filter(|c| **c > 0.5).count();
            assert_eq!(
                axis_aligned, 1,
                "a cube face normal should point along one axis"
            );
        }
    }

    #[test]
    fn the_cube_is_one_metre_across_and_centred() {
        let cube = MeshData::unit_cube();

        for axis in 0..3 {
            let min = cube
                .vertices
                .iter()
                .map(|v| v.position[axis])
                .fold(f32::INFINITY, f32::min);
            let max = cube
                .vertices
                .iter()
                .map(|v| v.position[axis])
                .fold(f32::NEG_INFINITY, f32::max);

            assert!((min + 0.5).abs() < 1e-6, "axis {axis} min was {min}");
            assert!((max - 0.5).abs() < 1e-6, "axis {axis} max was {max}");
        }
    }

    #[test]
    fn every_triangle_winds_counter_clockwise_seen_from_outside() {
        // Back-face culling removes triangles wound the wrong way, so a mistake
        // here makes faces vanish rather than error. Checking that each
        // triangle's geometric normal agrees with its stored face normal
        // catches it at the source.
        let cube = MeshData::unit_cube();

        for triangle in cube.indices.chunks_exact(3) {
            let [i0, i1, i2] = [triangle[0], triangle[1], triangle[2]];
            let a = Vec3::from_array(cube.vertices[i0 as usize].position);
            let b = Vec3::from_array(cube.vertices[i1 as usize].position);
            let c = Vec3::from_array(cube.vertices[i2 as usize].position);

            let geometric = (b - a).cross(c - a).normalize();
            let stored = Vec3::from_array(cube.vertices[i0 as usize].normal);

            assert!(
                geometric.dot(stored) > 0.9,
                "triangle {triangle:?} winds against its face normal {stored:?}"
            );
        }
    }

    #[test]
    fn indices_stay_within_the_vertex_list() {
        let cube = MeshData::unit_cube();
        let count = cube.vertices.len() as u16;
        assert!(cube.indices.iter().all(|index| *index < count));
    }
}
