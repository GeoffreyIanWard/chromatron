//! Drawing debug lines (S14/M1).
//!
//! A second pass over the same attachments the instanced draw used, with
//! `LoadOp::Load` so it lands on top of the scene rather than replacing it.
//!
//! # Why a separate pass rather than a separate pipeline in the same pass
//!
//! It could share the pass, and that would save a little. It would also mean
//! [`crate::instanced`] knowing about debug geometry in order to sequence it,
//! which puts a debugging tool into the shape of the main draw path. At the
//! gate's budget — 10,000 lines under 1 ms — the cost of a second pass is not
//! what decides whether that is met.
//!
//! # Depth-tested, but this is a wireframe
//!
//! Lines test against the scene's depth, so a debug box around an object is
//! occluded by things genuinely in front of it. They do **not** write depth: a
//! one-pixel line writing depth would punch holes in anything drawn after it,
//! and debug geometry should never change what the scene looks like.

use cx_view::DebugVertex;

use crate::camera::Camera;
use crate::device::RenderDevice;
use crate::instanced::{CameraUniform, DEPTH_FORMAT, camera_bind_group_layout};

/// What one debug pass cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DebugStats {
    /// Draw calls issued. One, or zero when there was nothing to draw.
    pub draw_calls: u32,
    /// Line segments drawn.
    pub lines: u32,
}

/// Everything one debug pass needs.
pub(crate) struct DebugPass<'a> {
    /// Colour attachment, already holding the scene.
    pub target: &'a wgpu::TextureView,
    /// The scene's depth attachment, tested but not written.
    pub depth: &'a wgpu::TextureView,
    /// Target width, for the projection's aspect ratio.
    pub width: u32,
    /// Target height.
    pub height: u32,
    /// The view to draw from.
    pub camera: &'a Camera,
    /// Vertices, already rebased. Two per segment.
    pub vertices: &'a [DebugVertex],
}

/// Draws line lists.
pub struct DebugRenderer {
    pipeline: wgpu::RenderPipeline,
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
}

impl std::fmt::Debug for DebugRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebugRenderer").finish_non_exhaustive()
    }
}

/// The vertex layout, matching `cx_view::DebugVertex`.
const VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<DebugVertex>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            offset: 0,
            shader_location: 0,
            format: wgpu::VertexFormat::Float32x3,
        },
        wgpu::VertexAttribute {
            offset: 12,
            shader_location: 1,
            // Unorm8x4, not Uint8x4: the shader wants 0..1, and doing the
            // divide in the shader instead would be four extra instructions per
            // vertex to arrive at the same place.
            format: wgpu::VertexFormat::Unorm8x4,
        },
    ],
};

impl DebugRenderer {
    /// Builds the line pipeline for the offscreen target format.
    ///
    /// Takes no format, for the same reason nothing else here does: a `wgpu`
    /// type in a public signature is what `ADR-0010` keeps out of this crate's
    /// API. A surface builds its own with [`DebugRenderer::pipeline_for`].
    pub fn new(render_device: &RenderDevice) -> Self {
        let format = crate::offscreen::TARGET_FORMAT;
        let device = render_device.wgpu_device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cx-render debug lines"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/debug_lines.wgsl").into()),
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cx-render debug camera"),
            size: size_of::<CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_layout = camera_bind_group_layout(device, "cx-render debug camera layout");

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cx-render debug camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cx-render debug pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            ..wgpu::PipelineLayoutDescriptor::default()
        });

        let pipeline = build_pipeline(device, &shader, &pipeline_layout, format);

        Self {
            pipeline,
            shader,
            pipeline_layout,
            camera_buffer,
            camera_bind_group,
        }
    }

    /// A pipeline for a target of `format`, for a surface whose format differs
    /// from the offscreen one.
    pub(crate) fn pipeline_for(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        build_pipeline(device, &self.shader, &self.pipeline_layout, format)
    }

    /// Uploads vertices for one frame.
    pub(crate) fn upload(&self, device: &wgpu::Device, vertices: &[DebugVertex]) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;

        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cx-render debug vertices"),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::VERTEX,
        })
    }

    /// Encodes the debug pass.
    ///
    /// Returns without encoding anything when there is nothing to draw — an
    /// empty pass would still cost an attachment load and store, every frame,
    /// for a feature that is off most of the time.
    pub(crate) fn encode(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        vertex_buffer: &wgpu::Buffer,
        pass: DebugPass<'_>,
    ) -> DebugStats {
        let vertex_count = pass.vertices.len() as u32;
        if vertex_count < 2 {
            return DebugStats::default();
        }

        let uniform = CameraUniform {
            view_projection: pass
                .camera
                .view_projection(pass.width as f32 / pass.height as f32)
                .to_cols_array_2d(),
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cx-render debug pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: pass.target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Load, never Clear: the scene is already in here.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: pass.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        // Discard rather than Store: nothing reads this depth
                        // afterwards, and the pipeline does not write it anyway.
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                ..wgpu::RenderPassDescriptor::default()
            });

            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            render_pass.draw(0..vertex_count, 0..1);
        }

        DebugStats {
            draw_calls: 1,
            lines: vertex_count / 2,
        }
    }

    /// The pipeline built for the offscreen format.
    pub(crate) const fn pipeline(&self) -> &wgpu::RenderPipeline {
        &self.pipeline
    }
}

fn build_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    pipeline_layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cx-render debug pipeline"),
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(VERTEX_LAYOUT)],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Alpha blending, so a translucent debug colour reads as one.
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            // Lines have no facing to cull. Leaving Back here culls nothing on
            // most drivers and everything on some.
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            // Tested but not written: see the module docs.
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
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

    #[test]
    fn the_vertex_layout_matches_the_view_type() {
        // These are two separate declarations of one memory layout, in two
        // crates, and a mismatch is not a compile error — it is geometry drawn
        // in the wrong place, or a driver crash.
        assert_eq!(
            VERTEX_LAYOUT.array_stride as usize,
            size_of::<DebugVertex>()
        );

        let colour_offset = VERTEX_LAYOUT
            .attributes
            .get(1)
            .expect("the layout has a colour attribute")
            .offset;
        assert_eq!(
            colour_offset as usize,
            size_of::<[f32; 3]>(),
            "colour must follow three floats of position"
        );
    }

    #[test]
    fn nothing_to_draw_costs_nothing() {
        let empty = DebugStats::default();
        assert_eq!(empty.draw_calls, 0);
        assert_eq!(empty.lines, 0);
    }
}
