//! Terrain chunk meshes and the pass that draws them (S07/M2).
//!
//! The instanced path draws many copies of one mesh; terrain is the opposite —
//! every chunk is unique geometry, drawn once. So chunks are **retained**: a
//! mesh is uploaded when the chunk lifecycle promotes it, kept across frames,
//! and removed when the chunk demotes. Per frame the only work is rewriting a
//! small offset buffer against the floating origin and issuing one draw per
//! resident chunk.
//!
//! # Where the mesh comes from
//!
//! [`TerrainMeshData::from_heights`] turns a square, cell-centred height grid —
//! an Active chunk's baked elevation, or a Coarse chunk's downsampled grid —
//! into positions, normals, and indices. It is pure data and unit-tested
//! without a GPU; only [`TerrainRenderer`] touches `wgpu`.
//!
//! # Known seams
//!
//! Adjacent chunk meshes meet exactly in x/z, but each samples its own edge
//! cells for height, so a boundary can show a step of roughly one cell of
//! slope — half a metre of horizontal error at Active resolution, four at
//! Coarse. Visible as a hairline on cliffs, invisible on open ground. Skirts
//! or edge-stitching can close it when terrain stops being a demo.

use std::collections::BTreeMap;

use cx_core::math::{CHUNK_SIZE, ChunkCoord};

use crate::camera::Camera;
use crate::device::RenderDevice;
use crate::instanced::{DEPTH_FORMAT, camera_bind_group_layout};
use wgpu::util::DeviceExt as _;

/// One vertex of a terrain mesh.
///
/// `repr(C)` because this is uploaded straight to the GPU. Position is
/// chunk-local in x/z (`0..=CHUNK_SIZE`) with absolute elevation in y, so the
/// mesh never needs re-uploading as the floating origin moves.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TerrainVertex {
    /// Chunk-local position, metres.
    pub position: [f32; 3],
    /// Surface normal.
    pub normal: [f32; 3],
}

impl TerrainVertex {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: size_of::<TerrainVertex>() as wgpu::BufferAddress,
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

/// A chunk's offset from the floating origin, as the GPU receives it.
///
/// Padded to 16 bytes; the shader reads `xyz` and ignores `w`.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChunkOffset {
    offset: [f32; 4],
}

impl ChunkOffset {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: size_of::<ChunkOffset>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[wgpu::VertexAttribute {
            offset: 0,
            shader_location: 2,
            format: wgpu::VertexFormat::Float32x4,
        }],
    };
}

/// Vertices and indices for one chunk of terrain.
///
/// Indices are `u32` where [`crate::mesh::MeshData`] uses `u16`: a chunk meshed
/// at 2 m already has 66,049 vertices, which does not fit.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainMeshData {
    /// Vertices, row-major with +X fastest, matching the source grid.
    pub vertices: Vec<TerrainVertex>,
    /// Triangle indices into `vertices`, counter-clockwise seen from above.
    pub indices: Vec<u32>,
}

impl TerrainMeshData {
    /// How many triangles this mesh draws.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Meshes a square, cell-centred height grid.
    ///
    /// `heights` is `edge * edge` values, row-major with +X fastest, each the
    /// elevation at a cell's centre; `cell_size` is the cell's width in metres.
    /// `stride` takes every `stride`-th cell, so an Active chunk's 1024-cell
    /// grid at stride 4 meshes at 2 m. It must divide `edge`.
    ///
    /// The first and last vertex of each row are pinned to the chunk's exact
    /// edge (`0` and `edge * cell_size`) rather than to their sample cell's
    /// centre, so neighbouring chunk meshes abut with no horizontal gap — the
    /// remaining seam is height-only, described in the module docs.
    ///
    /// Returns `None` when the inputs disagree — a length that is not
    /// `edge * edge`, a stride of zero or one that does not divide `edge` —
    /// because a mesh built from misread heights is a plausible-looking
    /// landscape in the wrong shape, which is worse than no mesh.
    pub fn from_heights(
        heights: &[f32],
        edge: usize,
        cell_size: f32,
        stride: usize,
    ) -> Option<Self> {
        if edge == 0 || stride == 0 || heights.len() != edge * edge || !edge.is_multiple_of(stride)
        {
            return None;
        }

        let quads = edge / stride;
        let verts = quads + 1;
        let extent = edge as f32 * cell_size;

        // Where vertex `i` sits along an axis, and which cell it samples.
        let position_of = |i: usize| -> f32 {
            if i == 0 {
                0.0
            } else if i == quads {
                extent
            } else {
                (i * stride) as f32 * cell_size + 0.5 * cell_size
            }
        };
        let cell_of = |i: usize| -> usize { (i * stride).min(edge - 1) };
        let height_at = |i: usize, j: usize| -> f32 {
            *heights.get(cell_of(j) * edge + cell_of(i)).unwrap_or(&0.0)
        };

        let mut vertices = Vec::with_capacity(verts * verts);
        for j in 0..verts {
            for i in 0..verts {
                // Central differences over the neighbouring vertices' sample
                // positions. At the borders the difference is one-sided, which
                // is exactly what a border has.
                let (left, right) = (i.saturating_sub(1), (i + 1).min(quads));
                let (near, far) = (j.saturating_sub(1), (j + 1).min(quads));
                let dx = position_of(right) - position_of(left);
                let dz = position_of(far) - position_of(near);
                let dhdx = if dx > 0.0 {
                    (height_at(right, j) - height_at(left, j)) / dx
                } else {
                    0.0
                };
                let dhdz = if dz > 0.0 {
                    (height_at(i, far) - height_at(i, near)) / dz
                } else {
                    0.0
                };

                let normal = normalise([-dhdx, 1.0, -dhdz]);
                vertices.push(TerrainVertex {
                    position: [position_of(i), height_at(i, j), position_of(j)],
                    normal,
                });
            }
        }

        let mut indices = Vec::with_capacity(quads * quads * 6);
        for j in 0..quads {
            for i in 0..quads {
                let a = (j * verts + i) as u32;
                let b = a + 1;
                let d = a + verts as u32;
                let c = d + 1;
                // Counter-clockwise seen from above (+Y), matching the
                // back-face culling convention (`03-conventions.md`).
                indices.extend_from_slice(&[a, d, c, a, c, b]);
            }
        }

        Some(Self { vertices, indices })
    }
}

fn normalise(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length > 0.0 {
        [v[0] / length, v[1] / length, v[2] / length]
    } else {
        [0.0, 1.0, 0.0]
    }
}

/// What drawing the terrain cost this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerrainStats {
    /// Chunk meshes resident on the GPU.
    pub chunks: u32,
    /// Draw calls issued — one per chunk, today.
    pub draw_calls: u32,
    /// Triangles across those draws.
    pub triangles: u32,
}

/// One chunk's mesh, resident on the GPU.
struct GpuChunk {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

/// Everything a terrain draw needs beyond the renderer itself.
pub(crate) struct TerrainPass<'a> {
    /// Colour attachment. Loaded, not cleared — terrain draws over the scene
    /// pass's output.
    pub target: &'a wgpu::TextureView,
    /// Depth attachment shared with the scene pass.
    pub depth: &'a wgpu::TextureView,
    /// Target width, for the projection's aspect ratio.
    pub width: u32,
    /// Target height.
    pub height: u32,
    /// The view to draw from.
    pub camera: &'a Camera,
}

/// Draws retained terrain chunk meshes.
pub struct TerrainRenderer {
    chunks: BTreeMap<(i32, i32), GpuChunk>,
    /// Per-chunk offsets from the floating origin, rewritten each frame.
    offsets: crate::pool::GrowableBuffer,
    /// Staging for the offsets, kept so the rewrite does not allocate.
    staging: Vec<ChunkOffset>,
    /// The origin chunk offsets are computed against, set once per frame
    /// before the draw paths run.
    origin: ChunkCoord,
    pipeline: wgpu::RenderPipeline,
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
}

impl std::fmt::Debug for TerrainRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerrainRenderer")
            .field("chunks", &self.chunks.len())
            .finish_non_exhaustive()
    }
}

impl TerrainRenderer {
    /// Builds the terrain pipeline. No meshes exist yet.
    pub fn new(render_device: &RenderDevice) -> Self {
        let device = render_device.wgpu_device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cx-render terrain"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/terrain.wgsl").into()),
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cx-render terrain camera"),
            size: size_of::<crate::instanced::CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_layout = camera_bind_group_layout(device, "cx-render terrain camera layout");
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cx-render terrain camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cx-render terrain pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            ..wgpu::PipelineLayoutDescriptor::default()
        });

        let pipeline = build_pipeline(
            device,
            &shader,
            &pipeline_layout,
            crate::offscreen::TARGET_FORMAT,
        );

        Self {
            chunks: BTreeMap::new(),
            offsets: crate::pool::GrowableBuffer::new(
                "cx-render terrain offsets",
                wgpu::BufferUsages::VERTEX,
            ),
            staging: Vec::new(),
            origin: ChunkCoord::new(0, 0),
            pipeline,
            shader,
            pipeline_layout,
            camera_buffer,
            camera_bind_group,
        }
    }

    /// A pipeline for a target of `format`, for the window surface.
    pub(crate) fn pipeline_for(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_pipeline(device, &self.shader, &self.pipeline_layout, format)
    }

    /// The pipeline for the offscreen format.
    pub(crate) const fn pipeline(&self) -> &wgpu::RenderPipeline {
        &self.pipeline
    }

    /// Uploads (or replaces) one chunk's mesh.
    ///
    /// An empty mesh removes the chunk instead, because a resident
    /// zero-triangle mesh would still cost a draw call every frame.
    pub fn upload(&mut self, device: &RenderDevice, chunk: ChunkCoord, mesh: &TerrainMeshData) {
        if mesh.indices.is_empty() {
            self.remove(chunk);
            return;
        }

        let wgpu_device = device.wgpu_device();
        let vertices = wgpu_device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cx-render terrain chunk vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = wgpu_device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cx-render terrain chunk indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        self.chunks.insert(
            (chunk.x, chunk.z),
            GpuChunk {
                vertices,
                indices,
                index_count: mesh.indices.len() as u32,
            },
        );
    }

    /// Drops one chunk's mesh, if resident.
    pub fn remove(&mut self, chunk: ChunkCoord) {
        self.chunks.remove(&(chunk.x, chunk.z));
    }

    /// How many chunk meshes are resident.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Sets the floating origin the next draw rebases against.
    ///
    /// Must match the camera's space, exactly as
    /// [`cx_view::ViewWorld::origin`] must — a mismatch draws the terrain a
    /// chunk-multiple away from where the camera is looking.
    pub const fn set_origin(&mut self, origin: ChunkCoord) {
        self.origin = origin;
    }

    /// Frame-path GPU resource creations, for the pooling gate.
    ///
    /// Deliberately excludes mesh uploads: those happen when the lifecycle
    /// promotes or demotes a chunk, which is event-driven work, not per-frame
    /// work. Only the offset buffer is on the frame path, and it stops
    /// reallocating once it has grown to the working set.
    pub(crate) const fn creations(&self) -> u32 {
        self.offsets.creations()
    }

    /// Encodes the terrain draw over an already-cleared target.
    pub(crate) fn encode(
        &mut self,
        device: &RenderDevice,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        pass: TerrainPass<'_>,
    ) -> TerrainStats {
        if self.chunks.is_empty() {
            return TerrainStats::default();
        }

        let queue = device.wgpu_queue();

        let uniform = crate::instanced::CameraUniform {
            view_projection: pass
                .camera
                .view_projection(pass.width as f32 / pass.height as f32)
                .to_cols_array_2d(),
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));

        // Rebase every chunk against this frame's origin. Exact while the
        // camera is anywhere near the terrain: the difference is integer
        // chunks, small by construction.
        self.staging.clear();
        for (x, z) in self.chunks.keys() {
            self.staging.push(ChunkOffset {
                offset: [
                    (x - self.origin.x) as f32 * CHUNK_SIZE,
                    0.0,
                    (z - self.origin.z) as f32 * CHUNK_SIZE,
                    0.0,
                ],
            });
        }
        self.offsets.write(
            device.wgpu_device(),
            queue,
            bytemuck::cast_slice(&self.staging),
        );
        let Some(offsets) = self.offsets.get() else {
            return TerrainStats::default();
        };

        let mut stats = TerrainStats {
            chunks: self.chunks.len() as u32,
            draw_calls: 0,
            triangles: 0,
        };

        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cx-render terrain pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: pass.target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The scene pass cleared; terrain draws into the same
                    // picture and shares its depth, so distant terrain sits
                    // correctly behind near entities and vice versa.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: pass.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..wgpu::RenderPassDescriptor::default()
        });

        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, &self.camera_bind_group, &[]);

        for (index, chunk) in self.chunks.values().enumerate() {
            // Each draw reads its own element of the offset buffer by slicing
            // to it, rather than by a non-zero first instance — base-instance
            // support is a hardware feature this has no other reason to need.
            let byte_offset = (index * size_of::<ChunkOffset>()) as wgpu::BufferAddress;
            render_pass.set_vertex_buffer(0, chunk.vertices.slice(..));
            render_pass.set_vertex_buffer(1, offsets.slice(byte_offset..));
            render_pass.set_index_buffer(chunk.indices.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..chunk.index_count, 0, 0..1);

            stats.draw_calls += 1;
            stats.triangles += chunk.index_count / 3;
        }

        stats
    }
}

/// Builds the terrain pipeline for one colour format, for the same reason
/// [`crate::instanced`] does: a pipeline is bound to its target's format.
fn build_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    pipeline_layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cx-render terrain pipeline"),
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(TerrainVertex::LAYOUT), Some(ChunkOffset::LAYOUT)],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::Vec3;

    /// A tilted plane: height rises 1 m per metre of x.
    fn ramp(edge: usize, cell_size: f32) -> Vec<f32> {
        (0..edge * edge)
            .map(|index| ((index % edge) as f32 + 0.5) * cell_size)
            .collect()
    }

    #[test]
    fn a_flat_grid_meshes_with_upward_normals_and_full_coverage() {
        let heights = vec![7.5f32; 16 * 16];
        let mesh = TerrainMeshData::from_heights(&heights, 16, 2.0, 4).expect("valid inputs");

        assert_eq!(mesh.vertices.len(), 5 * 5);
        assert_eq!(mesh.triangle_count(), 4 * 4 * 2);
        for vertex in &mesh.vertices {
            assert_eq!(vertex.position[1], 7.5);
            assert_eq!(vertex.normal, [0.0, 1.0, 0.0]);
        }
    }

    #[test]
    fn the_mesh_spans_the_chunk_exactly() {
        // The corner vertices are pinned to the chunk's edges so neighbouring
        // meshes abut; a mesh that stops at the last cell's centre leaves a
        // visible gap at every boundary.
        let heights = vec![0.0f32; 8 * 8];
        let mesh = TerrainMeshData::from_heights(&heights, 8, 4.0, 2).expect("valid inputs");

        for axis in [0, 2] {
            let min = mesh
                .vertices
                .iter()
                .map(|v| v.position[axis])
                .fold(f32::INFINITY, f32::min);
            let max = mesh
                .vertices
                .iter()
                .map(|v| v.position[axis])
                .fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(min, 0.0, "axis {axis}");
            assert_eq!(max, 32.0, "axis {axis} should reach the chunk edge");
        }
    }

    #[test]
    fn every_triangle_winds_counter_clockwise_seen_from_above() {
        // The same property the cube test checks, for the same reason:
        // back-face culling makes a winding mistake invisible geometry, not an
        // error.
        let mesh = TerrainMeshData::from_heights(&ramp(16, 1.0), 16, 1.0, 4).expect("valid inputs");

        for triangle in mesh.indices.chunks_exact(3) {
            let [i0, i1, i2] = [triangle[0], triangle[1], triangle[2]];
            let a = Vec3::from_array(mesh.vertices[i0 as usize].position);
            let b = Vec3::from_array(mesh.vertices[i1 as usize].position);
            let c = Vec3::from_array(mesh.vertices[i2 as usize].position);

            let geometric = (b - a).cross(c - a);
            assert!(
                geometric.y > 0.0,
                "triangle {triangle:?} winds clockwise seen from above"
            );
        }
    }

    #[test]
    fn a_slope_tilts_the_normals_against_it() {
        let mesh = TerrainMeshData::from_heights(&ramp(16, 1.0), 16, 1.0, 4).expect("valid inputs");

        // Height rises with +x, so normals lean towards -x — and every
        // interior normal of a uniform ramp leans the same amount.
        for vertex in &mesh.vertices {
            assert!(
                vertex.normal[0] < 0.0,
                "a ramp rising with x should tilt normals to -x, got {:?}",
                vertex.normal
            );
            assert!(vertex.normal[1] > 0.0);
        }
    }

    #[test]
    fn normals_are_unit_length() {
        let mesh = TerrainMeshData::from_heights(&ramp(32, 0.5), 32, 0.5, 8).expect("valid inputs");
        for vertex in &mesh.vertices {
            let normal = Vec3::from_array(vertex.normal);
            assert!((normal.length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn indices_stay_within_the_vertex_list() {
        let mesh = TerrainMeshData::from_heights(&ramp(16, 1.0), 16, 1.0, 2).expect("valid inputs");
        let count = mesh.vertices.len() as u32;
        assert!(mesh.indices.iter().all(|index| *index < count));
    }

    #[test]
    fn mismatched_inputs_are_refused() {
        let heights = vec![0.0f32; 16 * 16];
        assert!(TerrainMeshData::from_heights(&heights, 16, 1.0, 0).is_none());
        assert!(TerrainMeshData::from_heights(&heights, 16, 1.0, 3).is_none());
        assert!(TerrainMeshData::from_heights(&heights, 15, 1.0, 1).is_none());
        assert!(TerrainMeshData::from_heights(&[], 0, 1.0, 1).is_none());
    }

    #[test]
    fn stride_one_meshes_every_cell() {
        let mesh = TerrainMeshData::from_heights(&ramp(8, 1.0), 8, 1.0, 1).expect("valid inputs");
        assert_eq!(mesh.vertices.len(), 9 * 9);
        assert_eq!(mesh.triangle_count(), 8 * 8 * 2);
    }
}
