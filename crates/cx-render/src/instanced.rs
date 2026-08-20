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
use crate::debug::{DebugPass, DebugRenderer, DebugStats};
use crate::device::RenderDevice;
use crate::error::RenderError;
use crate::frame::FrameContents;
use crate::mesh::{MeshData, Vertex};
use crate::offscreen::{Readback, Rgba};

/// A model matrix, as the GPU receives it.
///
/// Sixty-four bytes per instance, which is the figure `bench/memory-budget.md`
/// budgets for instance buffers (1M instances at ~64 B, double-buffered).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct InstanceRaw {
    model: [[f32; 4]; 4],
    /// Palette row (S12). The mesh supplies the slot.
    palette: u32,
    /// To 16 bytes. A vertex attribute's offset is free-form, but keeping the
    /// stride a multiple of 16 keeps this struct readable from a storage buffer
    /// by the cull shader without a second layout to reconcile.
    _pad: [u32; 3],
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
            palette: instance.palette,
            _pad: [0; 3],
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
                shader_location: 3,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 4,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 5,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 48,
                shader_location: 6,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 64,
                shader_location: 7,
                format: wgpu::VertexFormat::Uint32,
            },
        ],
    };
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CameraUniform {
    pub view_projection: [[f32; 4]; 4],
}

/// Depth format for offscreen targets.
///
/// A depth buffer is not optional for solid geometry: without one, triangles
/// draw in submission order and a cube shows whichever face happened to be last
/// rather than whichever is nearest.
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

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

/// Everything one draw needs.
///
/// A struct rather than a long argument list, because the offscreen and
/// windowed paths both call this and eleven positional parameters is a
/// swap-two-of-them-and-nobody-notices waiting to happen.
pub(crate) struct DrawCall<'a> {
    /// The pipeline built for `target`'s format. Passing one built for a
    /// different format is a fatal validation error, not a wrong colour.
    pub pipeline: &'a wgpu::RenderPipeline,
    /// Colour attachment to draw into.
    pub target: &'a wgpu::TextureView,
    /// Depth attachment, which must match `target`'s dimensions.
    pub depth: &'a wgpu::TextureView,
    /// Target width, for the projection's aspect ratio.
    pub width: u32,
    /// Target height.
    pub height: u32,
    /// The view to draw from.
    pub camera: &'a Camera,
    /// Per-instance model matrices, already uploaded.
    pub instance_buffer: &'a wgpu::Buffer,
    /// How many instances to draw.
    ///
    /// Ignored when `indirect` is set: the GPU decides the count, which is the
    /// point of culling on it.
    pub instance_count: u32,
    /// Indirect arguments, when the count comes from a cull pass.
    ///
    /// `None` draws `instance_count` directly. That path is still used by the
    /// offscreen tests, where the whole scene is deliberately in view and a
    /// compute dispatch would only add something else to go wrong.
    pub indirect: Option<&'a wgpu::Buffer>,
    /// Colour to clear to first.
    pub clear: Rgba,
}

/// Draws instanced meshes to an offscreen target.
///
/// Holds the pipeline and the mesh buffers, which are built once. Rebuilding a
/// pipeline per frame is a classic way to lose most of a frame budget to shader
/// validation, so the split between "build once" and "draw many" is the point of
/// the type.
pub struct InstancedRenderer {
    palette: crate::palette::PaletteTexture,
    // The offscreen pipeline. A pipeline is bound to one colour format, and a
    // window's format is not knowable until a window exists — macOS hands out
    // `Bgra8UnormSrgb` where the offscreen target is `Rgba8UnormSrgb` — so the
    // shader and layout are kept to build more on demand.
    pipeline: wgpu::RenderPipeline,
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
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

/// The passes that draw *over* the scene, when a caller wants them.
///
/// Grouped because they are both optional and both come last; as loose
/// parameters they were the seventh and eighth argument of a function that had
/// already been split once for the same reason.
pub(crate) struct OffscreenExtras<'a> {
    /// The debug-line renderer, when there are lines to draw.
    pub debug: Option<&'a DebugRenderer>,
    /// The overlay renderer and this frame's output.
    pub overlay: Option<(&'a mut crate::ui::UiRenderer, cx_ui::UiOutput)>,
}

/// An offscreen target to draw into.
pub(crate) struct OffscreenTarget {
    /// Target width in pixels.
    pub width: u32,
    /// Target height in pixels.
    pub height: u32,
    /// Colour format. Normally [`crate::offscreen::TARGET_FORMAT`]; a surface's
    /// format when checking the windowed path without a window.
    pub format: wgpu::TextureFormat,
}

/// The bind group layout every pipeline here uses for its camera uniform.
///
/// Shared with [`crate::debug`] rather than declared twice: the layout has to
/// match the shader's `@group(0) @binding(0)`, and two copies of that agreement
/// is one copy too many.
pub(crate) fn camera_bind_group_layout(
    device: &wgpu::Device,
    label: &str,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
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
    })
}

/// Builds the draw pipeline for one colour format.
///
/// A pipeline is bound to the format of the target it draws into: using an
/// `Rgba8UnormSrgb` pipeline against a `Bgra8UnormSrgb` swapchain is a
/// validation error, and wgpu treats those as fatal. Offscreen targets and
/// window surfaces therefore get their own, from this one description.
fn build_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    pipeline_layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cx-render instanced pipeline"),
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(Vertex::LAYOUT), Some(InstanceRaw::LAYOUT)],
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
    })
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

        let camera_layout = camera_bind_group_layout(device, "cx-render camera layout");
        let palette_layout = crate::palette::PaletteTexture::layout(device);
        let palette = crate::palette::PaletteTexture::new(
            device,
            render_device.wgpu_queue(),
            &palette_layout,
            &crate::palette::Palette::placeholder(),
        );

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
            // Two groups: the camera, and the shared palette. Every instance of
            // every mesh uses the same second group, which is what "one bind
            // group for the vast majority of scene geometry" means (S12).
            bind_group_layouts: &[Some(&camera_layout), Some(&palette_layout)],
            ..wgpu::PipelineLayoutDescriptor::default()
        });

        let pipeline = build_pipeline(
            device,
            &shader,
            &pipeline_layout,
            crate::offscreen::TARGET_FORMAT,
        );

        Ok(Self {
            palette,
            pipeline,
            shader,
            pipeline_layout,
            vertex_buffer,
            index_buffer,
            index_count: mesh.indices.len() as u32,
            camera_buffer,
            camera_bind_group,
        })
    }

    /// Indices in the mesh, for an indirect draw's arguments.
    pub(crate) const fn index_count(&self) -> u32 {
        self.index_count
    }

    /// A pipeline for a target of `format`, for callers that do not draw
    /// offscreen.
    pub(crate) fn pipeline_for(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_pipeline(device, &self.shader, &self.pipeline_layout, format)
    }

    /// Encodes a draw into `encoder`, targeting an existing view.
    ///
    /// The core of every draw path. An offscreen render and a windowed present
    /// differ only in where the view came from — a texture this crate created,
    /// or a swapchain frame the window handed over — so they share this and
    /// nothing else needs duplicating.
    pub(crate) fn encode_draw(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        call: DrawCall<'_>,
    ) -> DrawStats {
        let DrawCall {
            pipeline,
            indirect,
            target,
            depth,
            width,
            height,
            camera,
            instance_buffer,
            instance_count,
            clear,
        } = call;

        let uniform = CameraUniform {
            view_projection: camera
                .view_projection(width as f32 / height as f32)
                .to_cols_array_2d(),
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));

        let mut draw_calls = 0;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cx-render instanced pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        // Stored, not discarded. This was `Discard` while nothing
                        // read depth after the scene — then the debug pass
                        // started to, and loaded undefined contents, and every
                        // line silently failed its depth test. No error, no
                        // warning: just no lines.
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..wgpu::RenderPassDescriptor::default()
            });

            // An indirect draw is issued even when the CPU-side count is zero:
            // the GPU owns the real count, and skipping the call because the
            // *input* was empty would be a different question than the one that
            // matters. A direct draw with nothing to draw is still skipped.
            if indirect.is_some() || instance_count > 0 {
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_bind_group(1, self.palette.bind_group(), &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, instance_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);

                match indirect {
                    Some(args) => pass.draw_indexed_indirect(args, 0),
                    None => pass.draw_indexed(0..self.index_count, 0, 0..instance_count),
                }
                draw_calls = 1;
            }
        }

        DrawStats {
            draw_calls,
            instances: instance_count,
        }
    }

    /// Uploads instances to a fresh buffer.
    ///
    /// An empty buffer is invalid, and drawing zero instances is a legitimate
    /// frame — an off-screen camera, or a world not yet populated. One zeroed
    /// entry keeps the buffer valid; the draw asks for zero instances anyway.
    pub(crate) fn upload_instances(
        &self,
        device: &wgpu::Device,
        instances: &[ExtractedInstance],
    ) -> wgpu::Buffer {
        let raw: Vec<InstanceRaw> = instances.iter().map(InstanceRaw::from_extracted).collect();
        let bytes: &[u8] = if raw.is_empty() {
            &[0u8; size_of::<InstanceRaw>()]
        } else {
            bytemuck::cast_slice(&raw)
        };

        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cx-render instances"),
            contents: bytes,
            // STORAGE as well as VERTEX: the cull pass reads this buffer as
            // storage before the draw reads the compacted result as vertex
            // data. Without it, binding it to the compute shader is a
            // validation error rather than anything subtle.
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::STORAGE,
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
        let (readback, stats, _, _) = self.render_to_format(
            render_device,
            OffscreenTarget {
                width,
                height,
                format: crate::offscreen::TARGET_FORMAT,
            },
            camera,
            FrameContents {
                instances,
                debug: &[],
            },
            clear,
            OffscreenExtras {
                debug: None,
                overlay: None,
            },
        )?;
        Ok((readback, stats))
    }

    /// [`InstancedRenderer::render`], into a target of a chosen colour format.
    ///
    /// Exists so the *windowed* path can be checked without a window. A surface
    /// picks its own format — macOS presents `Bgra8UnormSrgb` where the offscreen
    /// target is `Rgba8UnormSrgb` — and a pipeline built for the wrong one is a
    /// fatal validation error rather than a wrong colour. Drawing to that format
    /// offscreen exercises the same pipeline against the same format, on a
    /// machine with no display server.
    pub(crate) fn render_to_format(
        &self,
        render_device: &RenderDevice,
        target: OffscreenTarget,
        camera: &Camera,
        contents: FrameContents<'_>,
        clear: Rgba,
        extras: OffscreenExtras<'_>,
    ) -> Result<(Readback, DrawStats, DebugStats, crate::ui::UiStats), RenderError> {
        let OffscreenExtras { debug, overlay } = extras;

        let OffscreenTarget {
            width,
            height,
            format,
        } = target;

        if width == 0 || height == 0 {
            return Err(RenderError::InvalidTargetSize { width, height });
        }

        let device = render_device.wgpu_device();
        let queue = render_device.wgpu_queue();

        let instance_buffer = self.upload_instances(device, contents.instances);
        let (colour_texture, colour_view) = create_colour_target(device, width, height, format);

        // Reuse the offscreen pipeline when the format matches, rather than
        // rebuilding one per call: this is also the normal render path.
        let built;
        let pipeline = if format == crate::offscreen::TARGET_FORMAT {
            &self.pipeline
        } else {
            built = self.pipeline_for(device, format);
            &built
        };
        let depth_view = create_depth_target(device, width, height);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("cx-render instanced draw"),
        });

        let stats = self.encode_draw(
            queue,
            &mut encoder,
            DrawCall {
                pipeline,
                target: &colour_view,
                depth: &depth_view,
                width,
                height,
                camera,
                instance_buffer: &instance_buffer,
                instance_count: contents.instances.len() as u32,
                indirect: None,
                clear,
            },
        );

        // Debug lines after the scene, on top of it, sharing its depth.
        let debug_stats = match debug {
            Some(debug) => {
                let vertices = debug.upload(device, contents.debug);
                let pipeline;
                let debug_pipeline = if format == crate::offscreen::TARGET_FORMAT {
                    debug.pipeline()
                } else {
                    pipeline = debug.pipeline_for(device, format);
                    &pipeline
                };

                debug.encode(
                    queue,
                    &mut encoder,
                    debug_pipeline,
                    &vertices,
                    DebugPass {
                        target: &colour_view,
                        depth: &depth_view,
                        width,
                        height,
                        camera,
                        vertices: contents.debug,
                    },
                )
            }
            None => DebugStats::default(),
        };

        // The overlay last, over a finished picture.
        let ui_stats = match overlay {
            Some((renderer, output)) => renderer.encode(
                render_device,
                &mut encoder,
                &colour_view,
                [width, height],
                output,
            ),
            None => crate::ui::UiStats::default(),
        };

        let readback = crate::offscreen::copy_texture_to_readback(
            device,
            queue,
            encoder,
            &colour_texture,
            width,
            height,
        )?;

        Ok((readback, stats, debug_stats, ui_stats))
    }
}

fn create_colour_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
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
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

pub(crate) fn create_depth_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> wgpu::TextureView {
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
        palette: 0,
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
            palette: 0,
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
