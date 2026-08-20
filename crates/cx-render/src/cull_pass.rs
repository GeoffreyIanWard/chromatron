//! The GPU culling pass (S12/M1).
//!
//! Runs before the scene draw, compacting visible instances into a second buffer
//! and filling in an indirect draw's instance count as it goes. The scene then
//! draws indirect, so the CPU never learns how many instances survived and never
//! waits to find out.
//!
//! # Why the count never comes back
//!
//! Reading the survivor count would mean a GPU synchronisation point in the
//! middle of every frame — the same mistake the offscreen readback path exists
//! to keep *out* of the real loop. The atomic that decides where an instance
//! lands is the same word the draw call reads as its instance count, so nothing
//! has to reconcile them.
//!
//! [`CullPass::debug_readback`] does read it, and is for tests only. Its doc
//! comment says so.

use crate::culling::Frustum;
use crate::device::RenderDevice;

/// Threads per workgroup. Matches `@workgroup_size` in `cull.wgsl`.
const WORKGROUP: u32 = 64;

/// Bytes of one instance, which must match `InstanceRaw`.
const INSTANCE_SIZE: u64 = 64;

/// The uniform the shader reads: six planes, a count, and padding.
///
/// `repr(C)` and explicitly padded, because WGSL's uniform layout rules are not
/// Rust's. The padding is three **scalars**, not a `vec3<u32>`: a `vec3` aligns
/// to 16 in WGSL, which would leave a hole after `instance_count` and make the
/// struct 128 bytes on that side against 112 on this one. wgpu catches the
/// mismatch — "bound with size 112 where the shader expects 128" — but only once
/// something dispatches, which is why the size is also asserted below.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CullUniform {
    planes: [[f32; 4]; 6],
    instance_count: u32,
    _pad: [u32; 3],
}

/// Indirect draw arguments, matching `wgpu::util::DrawIndexedIndirectArgs`.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

/// Compacts visible instances and fills an indirect draw.
pub struct CullPass {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    /// Compacted survivors. Bound as a vertex buffer by the scene draw.
    visible: wgpu::Buffer,
    /// The indirect arguments the scene draw reads.
    draw_args: wgpu::Buffer,
    /// How many instances `visible` can hold.
    capacity: u32,
}

impl std::fmt::Debug for CullPass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CullPass")
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl CullPass {
    /// Builds the pass, sized for `capacity` instances.
    pub fn new(render_device: &RenderDevice, capacity: u32) -> Self {
        let device = render_device.wgpu_device();
        let capacity = capacity.max(1);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cx-render cull"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/cull.wgsl").into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cx-render cull layout"),
            entries: &[
                uniform_entry(0),
                storage_entry(1, true),
                storage_entry(2, false),
                storage_entry(3, false),
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cx-render cull pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            ..wgpu::PipelineLayoutDescriptor::default()
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("cx-render cull pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cull_instances"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cx-render cull uniform"),
            size: size_of::<CullUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let visible = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cx-render visible instances"),
            size: u64::from(capacity) * INSTANCE_SIZE,
            // VERTEX as well as STORAGE: the compute pass writes it and the
            // scene draw reads it as instance data, with no copy in between.
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX,
            mapped_at_creation: false,
        });

        let draw_args = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cx-render draw args"),
            size: size_of::<DrawArgs>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            layout,
            uniform,
            visible,
            draw_args,
            capacity,
        }
    }

    /// How many instances this pass can hold.
    pub const fn capacity(&self) -> u32 {
        self.capacity
    }

    /// The compacted instance buffer, for the scene draw's vertex binding.
    pub(crate) const fn visible(&self) -> &wgpu::Buffer {
        &self.visible
    }

    /// The indirect arguments buffer.
    pub(crate) const fn draw_args(&self) -> &wgpu::Buffer {
        &self.draw_args
    }

    /// Encodes the cull for one frame.
    ///
    /// `instances` is the full, uncompacted set. `index_count` is the mesh's
    /// index count, which goes into the indirect arguments — the shader only
    /// touches the instance count.
    ///
    pub(crate) fn encode(
        &self,
        device: &RenderDevice,
        encoder: &mut wgpu::CommandEncoder,
        instances: &wgpu::Buffer,
        count: u32,
        index_count: u32,
        frustum: Frustum,
    ) -> u32 {
        let queue = device.wgpu_queue();

        // More instances than the buffer can hold would let the shader write
        // past the end. Clamped rather than resized mid-frame: growing a buffer
        // here would mean rebuilding the bind group during encoding, and the
        // capacity is a startup decision.
        let count = count.min(self.capacity);

        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&CullUniform {
                planes: frustum.to_raw(),
                instance_count: count,
                _pad: [0; 3],
            }),
        );

        // The count starts at zero every frame — it is what the shader
        // increments. Leaving last frame's value is the bug that makes the
        // scene draw more and more instances until it reads past the buffer.
        queue.write_buffer(
            &self.draw_args,
            0,
            bytemuck::bytes_of(&DrawArgs {
                index_count,
                instance_count: 0,
                first_index: 0,
                base_vertex: 0,
                first_instance: 0,
            }),
        );

        let bind_group = device
            .wgpu_device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cx-render cull bind group"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: instances.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.visible.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.draw_args.as_entire_binding(),
                    },
                ],
            });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("cx-render cull pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            // Rounded up, which is why the shader bounds-checks: the last group
            // is almost always partly past the end.
            pass.dispatch_workgroups(count.div_ceil(WORKGROUP), 1, 1);
        }

        count
    }

    /// Culls `instances` and reports how many survived.
    ///
    /// **For tests only**, and it exists in this shape for two reasons. In a
    /// real frame nothing reads the count — it stays on the GPU, which is the
    /// entire point of an indirect draw, and reading it per frame would put a
    /// synchronisation point in the middle of the loop.
    ///
    /// And it takes engine types rather than `wgpu` handles so that the tests
    /// can drive the real path without this crate exposing a device or a queue
    /// in a public signature, which `ADR-0010` keeps out of its API.
    pub fn debug_cull_count(
        &self,
        device: &RenderDevice,
        renderer: &crate::instanced::InstancedRenderer,
        camera: &crate::camera::Camera,
        aspect: f32,
        instances: &[cx_view::ExtractedInstance],
    ) -> u32 {
        let buffer = renderer.upload_instances(device.wgpu_device(), instances);

        let mut encoder =
            device
                .wgpu_device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("cx-render cull (test)"),
                });

        let frustum = Frustum::from_view_projection(camera.view_projection(aspect));
        self.encode(
            device,
            &mut encoder,
            &buffer,
            instances.len() as u32,
            renderer.index_count(),
            frustum,
        );
        device
            .wgpu_queue()
            .submit(std::iter::once(encoder.finish()));

        self.readback(device)
    }

    /// Reads the survivor count back from the indirect arguments.
    fn readback(&self, device: &RenderDevice) -> u32 {
        let wgpu_device = device.wgpu_device();

        let staging = wgpu_device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cx-render cull readback"),
            size: size_of::<DrawArgs>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder =
            wgpu_device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(
            &self.draw_args,
            0,
            &staging,
            0,
            size_of::<DrawArgs>() as wgpu::BufferAddress,
        );
        device
            .wgpu_queue()
            .submit(std::iter::once(encoder.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = wgpu_device.poll(wgpu::PollType::wait_indefinitely());

        // `get_mapped_range` returns a Result in wgpu 30; the map above has
        // already completed, so a failure here means the buffer is not what we
        // just wrote, which is worth reporting as zero rather than papering over.
        let count = match slice.get_mapped_range() {
            Ok(data) => {
                let args: DrawArgs = *bytemuck::from_bytes(&data[..size_of::<DrawArgs>()]);
                args.instance_count
            }
            Err(error) => {
                tracing::error!(%error, "could not read back the cull count");
                0
            }
        };
        staging.unmap();

        count
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uniform_layout_matches_what_wgsl_expects() {
        // WGSL pads a `u32` following an array up to the next 16-byte boundary.
        // Rust would not, and the shader would read its instance count out of
        // the middle of a plane.
        // Six planes plus four scalars — what WGSL computes for the same
        // struct, which is the number that has to match.
        assert_eq!(size_of::<CullUniform>(), 6 * 16 + 4 * 4);
        assert_eq!(size_of::<CullUniform>(), 112);
        assert_eq!(std::mem::offset_of!(CullUniform, instance_count), 96);
    }

    #[test]
    fn the_draw_args_layout_matches_wgpus() {
        // Five 32-bit words, in wgpu's order. A mismatch is not a compile error
        // — it is a draw call reading the wrong word and asking for four billion
        // instances.
        assert_eq!(size_of::<DrawArgs>(), 20);
        assert_eq!(std::mem::offset_of!(DrawArgs, instance_count), 4);
        assert_eq!(std::mem::offset_of!(DrawArgs, first_instance), 16);
    }
}
