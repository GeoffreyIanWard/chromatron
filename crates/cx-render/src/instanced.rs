//! The instanced draw path.
//!
//! One draw call per mesh, however many instances of it exist. That is the
//! shape M1's gate demands — 100,000 instances in under 20 draw calls — and it
//! is also the only shape that makes sense for a world built from repeated
//! low-poly geometry (S12).
//!
//! # Where the data comes from
//!
//! [`cx_view::ExtractedInstance`] values, produced by the extract phase and
//! already rebased against the floating origin. This crate converts each to a
//! model matrix and uploads them as one buffer. Nothing here reaches back into
//! the sim world — extract is the only channel (`ADR-0002`).
//!
//! # Why it renders offscreen
//!
//! Everything here targets a texture rather than a swapchain, so a draw can be
//! asserted pixel by pixel with no window and no display server. Presenting to
//! a real surface is a thin layer on top, added when there is a window to
//! present to.

use cx_core::math::{Mat4, Quat, Vec3};
use cx_view::ExtractedInstance;
use wgpu::util::DeviceExt as _;

use crate::camera::Camera;
use crate::device::RenderDevice;
use crate::error::RenderError;
use crate::mesh::{MeshData, Vertex};
use crate::offscreen::{Readback, Rgba};

/// A model matrix, as the GPU receives it.
///
/// Sixty-four bytes per instance, which is the figure `bench/memory-budget.md`
/// budgets for instance buffers (1M instances at ~64 B, double-buffered).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceRaw {
    model: [[f32; 4]; 4],
}

impl InstanceRaw {
    fn from_extracted(instance: &ExtractedInstance) -> Self {
        Self {
            model: Mat4::from_scale_rotation_translation(
                instance.scale,
                instance.rotation,
                instance.position,
            )
            .to_cols_array_2d(),
        }
    }

    /// A 4x4 matrix as four `vec4` attributes.
    ///
    /// WGSL vertex inputs cannot be matrices, so the columns occupy four
    /// consecutive shader locations and the shader reassembles them.
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: size_of::<InstanceRaw>() as wgpu::BufferAddress,
        // The attribute that makes this instanced rather than per-vertex.
        // Getting it wrong draws one instance and silently discards the rest.
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 3,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 4,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 48,
                shader_location: 5,
                format: wgpu::VertexFormat::Float32x4,
            },
        ],
    };
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_projection: [[f32; 4]; 4],
}

/// Depth format for offscreen targets.
///
/// A depth buffer is not optional for solid geometry: without one, triangles
/// draw in submission order and a cube shows whichever face happened to be last
/// rather than whichever is nearest.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// How many draw calls a render issued.
///
/// Reported because M1 gates on it: 100,000 instances must cost fewer than 20
/// draw calls, and a count is a far more stable thing to assert than a frame
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawStats {
    /// Draw calls issued.
    pub draw_calls: u32,
    /// Instances submitted across those calls.
    pub instances: u32,
}

/// Draws instanced meshes to an offscreen target.
///
/// Holds the pipeline and the mesh buffers, which are built once. Rebuilding a
/// pipeline per frame is a classic way to lose most of a frame budget to shader
/// validation, so the split between "build once" and "draw many" is the point of
/// the type.
pub struct InstancedRenderer {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
}

impl std::fmt::Debug for InstancedRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstancedRenderer")
            .field("index_count", &self.index_count)
            .finish_non_exhaustive()
    }
}

impl InstancedRenderer {
    /// Builds the pipeline and uploads the mesh.
    pub fn new(render_device: &RenderDevice, mesh: &MeshData) -> Result<Self, RenderError> {
        if mesh.indices.is_empty() {
            return Err(RenderError::EmptyMesh);
        }

        let device = render_device.wgpu_device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cx-render instanced"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/instanced.wgsl").into()),
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cx-render vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cx-render indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cx-render camera"),
            size: size_of::<CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cx-render camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cx-render camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cx-render pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            ..wgpu::PipelineLayoutDescriptor::default()
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cx-render instanced pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(Vertex::LAYOUT), Some(InstanceRaw::LAYOUT)],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: crate::offscreen::TARGET_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                // Culling back faces halves the fragment work on closed
                // geometry, and `03-conventions.md` fixes the winding that makes
                // it safe to switch on.
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
        });

        Ok(Self {
            pipeline,
            vertex_buffer,
            index_buffer,
            index_count: mesh.indices.len() as u32,
            camera_buffer,
            camera_bind_group,
        })
    }

    /// Draws every instance to an offscreen target and reads the pixels back.
    ///
    /// One draw call regardless of instance count — see [`DrawStats`].
    pub fn render(
        &self,
        render_device: &RenderDevice,
        width: u32,
        height: u32,
        camera: &Camera,
        instances: &[ExtractedInstance],
        clear: Rgba,
    ) -> Result<(Readback, DrawStats), RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidTargetSize { width, height });
        }

        let device = render_device.wgpu_device();
        let queue = render_device.wgpu_queue();

        let uniform = CameraUniform {
            view_projection: camera
                .view_projection(width as f32 / height as f32)
                .to_cols_array_2d(),
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));

        let raw: Vec<InstanceRaw> = instances.iter().map(InstanceRaw::from_extracted).collect();
        // An empty buffer is invalid, and drawing zero instances is a legitimate
        // frame — an off-screen camera, or a world not yet populated. One dummy
        // entry keeps the buffer valid; the draw asks for zero instances anyway.
        let instance_bytes: &[u8] = if raw.is_empty() {
            &[0u8; size_of::<InstanceRaw>()]
        } else {
            bytemuck::cast_slice(&raw)
        };

        let instance_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cx-render instances"),
            contents: instance_bytes,
            usage: wgpu::BufferUsages::VERTEX,
        });

        let (colour_texture, colour_view) = create_colour_target(device, width, height);
        let depth_view = create_depth_target(device, width, height);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("cx-render instanced draw"),
        });

        let mut draw_calls = 0;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cx-render instanced pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &colour_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(clear[0]),
                            g: f64::from(clear[1]),
                            b: f64::from(clear[2]),
                            a: f64::from(clear[3]),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                ..wgpu::RenderPassDescriptor::default()
            });

            if !instances.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, instance_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..self.index_count, 0, 0..instances.len() as u32);
                draw_calls = 1;
            }
        }

        let readback = crate::offscreen::copy_texture_to_readback(
            device,
            queue,
            encoder,
            &colour_texture,
            width,
            height,
        )?;

        Ok((
            readback,
            DrawStats {
                draw_calls,
                instances: instances.len() as u32,
            },
        ))
    }
}

fn create_colour_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cx-render colour target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: crate::offscreen::TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn create_depth_target(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("cx-render depth target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// An instance at a position, unrotated and unscaled — for tests and examples.
pub fn instance_at(position: Vec3) -> ExtractedInstance {
    ExtractedInstance {
        position,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Option<(RenderDevice, InstancedRenderer)> {
        let device = crate::testing::device_or_skip()?;
        let renderer = InstancedRenderer::new(&device, &MeshData::unit_cube())
            .expect("the cube pipeline should build on any adapter");
        Some((device, renderer))
    }

    const BLACK: Rgba = [0.0, 0.0, 0.0, 1.0];

    #[test]
    fn a_cube_in_front_of_the_camera_covers_the_centre_and_not_the_corners() {
        let Some((device, renderer)) = setup() else {
            return;
        };

        let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO);
        let (readback, stats) = renderer
            .render(&device, 64, 64, &camera, &[instance_at(Vec3::ZERO)], BLACK)
            .expect("rendering one cube should work");

        let centre = readback.pixel(32, 32).expect("in bounds");
        let corner = readback.pixel(1, 1).expect("in bounds");

        assert!(
            centre[0] > 20,
            "the cube should be lit at the centre, got {centre:?}"
        );
        assert_eq!(
            corner,
            [0, 0, 0, 255],
            "the corner should still be the clear colour"
        );
        assert_eq!(stats.draw_calls, 1, "instancing means one draw call");
        assert_eq!(stats.instances, 1);
    }

    #[test]
    fn many_instances_still_cost_one_draw_call() {
        // The property M1's gate actually measures. It holds at any count, so
        // asserting it here catches a regression long before the 100k benchmark
        // does — and without needing a GPU fast enough to make 100k meaningful.
        let Some((device, renderer)) = setup() else {
            return;
        };

        let instances: Vec<ExtractedInstance> = (0..1_000)
            .map(|i| {
                instance_at(Vec3::new(
                    (i % 40) as f32 - 20.0,
                    0.0,
                    (i / 40) as f32 - 12.0,
                ))
            })
            .collect();

        let camera = Camera::looking_at(Vec3::new(0.0, 30.0, 30.0), Vec3::ZERO);
        let (_, stats) = renderer
            .render(&device, 64, 64, &camera, &instances, BLACK)
            .expect("rendering a thousand cubes should work");

        assert_eq!(
            stats.draw_calls, 1,
            "1000 instances must still be one draw call"
        );
        assert_eq!(stats.instances, 1_000);
    }

    #[test]
    fn geometry_behind_the_camera_does_not_appear() {
        let Some((device, renderer)) = setup() else {
            return;
        };

        // Camera at the origin looking towards -Z; the cube is behind it.
        let camera = Camera::looking_at(Vec3::ZERO, Vec3::new(0.0, 0.0, -10.0));
        let (readback, _) = renderer
            .render(
                &device,
                32,
                32,
                &camera,
                &[instance_at(Vec3::new(0.0, 0.0, 10.0))],
                BLACK,
            )
            .expect("rendering works");

        assert_eq!(
            readback.pixel(16, 16),
            Some([0, 0, 0, 255]),
            "a cube behind the camera must not be drawn"
        );
    }

    #[test]
    fn scale_changes_how_much_of_the_screen_an_instance_covers() {
        // Catches a model matrix built in the wrong order — scale-rotate-
        // translate composed the other way scales the *translation*, which
        // moves instances instead of resizing them.
        let Some((device, renderer)) = setup() else {
            return;
        };

        let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 6.0), Vec3::ZERO);
        let lit = |instance: ExtractedInstance| -> usize {
            let (readback, _) = renderer
                .render(&device, 64, 64, &camera, &[instance], BLACK)
                .expect("rendering works");
            (0..64)
                .flat_map(|y| (0..64).map(move |x| (x, y)))
                .filter(|(x, y)| readback.pixel(*x, *y).is_some_and(|pixel| pixel[0] > 20))
                .count()
        };

        let small = lit(instance_at(Vec3::ZERO));
        let large = lit(ExtractedInstance {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::splat(2.0),
        });

        assert!(small > 0, "the unscaled cube should be visible");
        assert!(
            large > small * 2,
            "doubling scale should cover far more pixels: {small} vs {large}"
        );
    }

    #[test]
    fn an_empty_instance_list_renders_the_clear_colour_without_drawing() {
        let Some((device, renderer)) = setup() else {
            return;
        };

        let camera = Camera::default();
        let (readback, stats) = renderer
            .render(&device, 16, 16, &camera, &[], [1.0, 0.0, 0.0, 1.0])
            .expect("an empty frame is legitimate, not an error");

        assert_eq!(stats.draw_calls, 0, "nothing to draw means no draw call");
        let pixel = readback.pixel(8, 8).expect("in bounds");
        assert!(
            pixel[0] > 200 && pixel[1] < 40,
            "should be the red clear colour, got {pixel:?}"
        );
    }

    #[test]
    fn an_empty_mesh_is_rejected_at_build_time() {
        let Some((device, _)) = setup() else {
            return;
        };

        let empty = MeshData {
            vertices: Vec::new(),
            indices: Vec::new(),
        };
        assert!(InstancedRenderer::new(&device, &empty).is_err());
    }
}
