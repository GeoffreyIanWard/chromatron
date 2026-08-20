//! Buffers and targets that survive the frame that made them (S12/M1).
//!
//! Every frame used to create its instance buffer, its debug vertex buffer, its
//! offscreen colour and depth targets, and the cull pass's bind group. All of
//! them had identical contents-shape from one frame to the next; only the bytes
//! differed.
//!
//! # Growth, and why it is counted
//!
//! A pooled buffer grows by doubling and never shrinks, so the number of
//! creations over a run is logarithmic in the largest frame rather than linear
//! in the frame count. That is easy to claim and easy to get subtly wrong — an
//! off-by-one in the capacity check reallocates every frame and nothing looks
//! different.
//!
//! So creations are **counted**, and the count is a gate: after a warm-up, a
//! steady-state frame must create nothing. It is a hardware-independent number,
//! which is what the M1 milestone asked for when it recorded this work as
//! outstanding — unlike a frame time, it means the same thing on a laptop and on
//! a software rasterizer.

/// A GPU buffer that is reused between frames and grows when it has to.
pub(crate) struct GrowableBuffer {
    buffer: Option<wgpu::Buffer>,
    /// Used only when a write is not a multiple of four bytes. Reused, so an
    /// unaligned caller still costs no per-frame allocation.
    scratch: Vec<u8>,
    capacity: u64,
    usage: wgpu::BufferUsages,
    label: &'static str,
    /// How many times a buffer has actually been created here.
    creations: u32,
    /// Bumped on every creation, so a cached bind group can tell that the
    /// buffer it points at is no longer the buffer in use.
    generation: u64,
}

impl std::fmt::Debug for GrowableBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrowableBuffer")
            .field("label", &self.label)
            .field("capacity", &self.capacity)
            .field("creations", &self.creations)
            .finish_non_exhaustive()
    }
}

impl GrowableBuffer {
    /// An empty pool. Nothing is allocated until the first write.
    pub(crate) const fn new(label: &'static str, usage: wgpu::BufferUsages) -> Self {
        Self {
            buffer: None,
            scratch: Vec::new(),
            capacity: 0,
            usage,
            label,
            creations: 0,
            generation: 0,
        }
    }

    /// Writes `bytes`, growing first if they do not fit.
    ///
    /// `COPY_DST` is added to the usage automatically: a pooled buffer is
    /// written with `write_buffer` rather than created with its contents, and
    /// forgetting the flag is a validation error on the first frame that reuses
    /// one.
    ///
    /// A write whose length is not a multiple of four is padded, because
    /// `write_buffer` requires `COPY_BUFFER_ALIGNMENT` and rejects anything else
    /// outright. Every real caller passes whole 16- or 80-byte structs and never
    /// touches that path; it exists so the pool is not a trap for the first
    /// caller that does not.
    pub(crate) fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) {
        const ALIGNMENT: usize = wgpu::COPY_BUFFER_ALIGNMENT as usize;

        let bytes = if bytes.len().is_multiple_of(ALIGNMENT) {
            bytes
        } else {
            self.scratch.clear();
            self.scratch.extend_from_slice(bytes);
            self.scratch
                .resize(bytes.len().next_multiple_of(ALIGNMENT), 0);
            &self.scratch
        };

        // wgpu rejects a zero-sized buffer, and `write_buffer` with an empty
        // slice is a no-op anyway. One alignment unit of slack costs nothing and
        // keeps every caller from special-casing an empty frame.
        let needed = (bytes.len() as u64).max(ALIGNMENT as u64);

        if self.capacity < needed {
            // Doubling, and never shrinking. Growing to exactly what this frame
            // needs would reallocate on any frame that added a single instance,
            // which is the failure this whole module exists to prevent.
            let capacity = needed.next_power_of_two();

            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: capacity,
                usage: self.usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.capacity = capacity;
            self.creations += 1;
            self.generation += 1;

            tracing::debug!(
                label = self.label,
                capacity,
                creations = self.creations,
                "grew a pooled buffer"
            );
        }

        if !bytes.is_empty()
            && let Some(buffer) = &self.buffer
        {
            queue.write_buffer(buffer, 0, bytes);
        }
    }

    /// The buffer, once something has been written.
    pub(crate) const fn get(&self) -> Option<&wgpu::Buffer> {
        self.buffer.as_ref()
    }

    /// How many buffers this pool has created in its lifetime.
    pub(crate) const fn creations(&self) -> u32 {
        self.creations
    }

    /// Which generation the current buffer is.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }
}

/// An offscreen colour-and-depth pair, kept between frames.
///
/// Keyed on size and format, because those are the only things that can change
/// — and when one does, the old pair genuinely cannot be reused.
pub(crate) struct TargetPool {
    colour: Option<wgpu::Texture>,
    colour_view: Option<wgpu::TextureView>,
    depth: Option<wgpu::TextureView>,
    key: Option<(u32, u32, wgpu::TextureFormat)>,
    creations: u32,
}

impl std::fmt::Debug for TargetPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetPool")
            .field("key", &self.key)
            .field("creations", &self.creations)
            .finish_non_exhaustive()
    }
}

impl TargetPool {
    /// An empty pool.
    pub(crate) const fn new() -> Self {
        Self {
            colour: None,
            colour_view: None,
            depth: None,
            key: None,
            creations: 0,
        }
    }

    /// The colour texture, its view, and a depth view for this size and format.
    ///
    /// Recreated only when the key changes. A test that renders the same size
    /// repeatedly should see the creation count stay put.
    ///
    /// Returns **clones**, not references. A `wgpu` handle is `Arc`-backed, so a
    /// clone is a refcount bump rather than a resource creation — and returning
    /// borrows would keep the pool mutably borrowed for as long as the caller
    /// held them, which is exactly the span in which it wants to call other
    /// methods on the renderer that owns it.
    pub(crate) fn get(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
        let key = (width, height, format);

        if self.key != Some(key) || self.colour.is_none() {
            let colour = device.create_texture(&wgpu::TextureDescriptor {
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

            self.colour_view = Some(colour.create_view(&wgpu::TextureViewDescriptor::default()));
            self.colour = Some(colour);
            self.depth = Some(crate::instanced::create_depth_target(device, width, height));
            self.key = Some(key);
            self.creations += 1;

            tracing::debug!(width, height, ?format, "created an offscreen target");
        }

        (
            self.colour.clone().expect("just created"),
            self.colour_view.clone().expect("just created"),
            self.depth.clone().expect("just created"),
        )
    }

    /// How many target pairs this pool has created.
    pub(crate) const fn creations(&self) -> u32 {
        self.creations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::device_or_skip;

    #[test]
    fn a_buffer_is_created_once_and_then_reused() {
        let Some(device) = device_or_skip() else {
            return;
        };

        let mut pool = GrowableBuffer::new("test", wgpu::BufferUsages::VERTEX);
        let bytes = vec![0u8; 256];

        for _ in 0..20 {
            pool.write(device.wgpu_device(), device.wgpu_queue(), &bytes);
        }

        assert_eq!(
            pool.creations(),
            1,
            "twenty writes of the same size should allocate once"
        );
    }

    #[test]
    fn growth_is_logarithmic_rather_than_per_frame() {
        // Doubling. Growing to exactly what each frame needs would allocate on
        // every frame that added one instance, which is the failure this module
        // exists to prevent — and it would look identical from outside.
        let Some(device) = device_or_skip() else {
            return;
        };

        let mut pool = GrowableBuffer::new("test", wgpu::BufferUsages::VERTEX);

        // Sizes that are not multiples of four are included on purpose: they
        // exercise the padding path, which `write_buffer` would otherwise
        // reject outright.
        for size in 1..=2_000 {
            pool.write(device.wgpu_device(), device.wgpu_queue(), &vec![0u8; size]);
        }

        assert!(
            pool.creations() <= 12,
            "growing to 2000 bytes one at a time took {} allocations",
            pool.creations()
        );
    }

    #[test]
    fn a_smaller_frame_does_not_shrink_the_buffer() {
        // Shrinking would make an oscillating scene — a camera turning back and
        // forth past a crowd — reallocate every frame in both directions.
        let Some(device) = device_or_skip() else {
            return;
        };

        let mut pool = GrowableBuffer::new("test", wgpu::BufferUsages::VERTEX);
        pool.write(device.wgpu_device(), device.wgpu_queue(), &vec![0u8; 4_096]);
        let after_large = pool.creations();

        for _ in 0..10 {
            pool.write(device.wgpu_device(), device.wgpu_queue(), &[0u8; 16]);
        }

        assert_eq!(pool.creations(), after_large);
    }

    #[test]
    fn an_empty_write_does_not_allocate_a_zero_sized_buffer() {
        // wgpu rejects a zero-sized buffer, and an empty scene is an ordinary
        // frame rather than an error.
        let Some(device) = device_or_skip() else {
            return;
        };

        let mut pool = GrowableBuffer::new("test", wgpu::BufferUsages::VERTEX);
        pool.write(device.wgpu_device(), device.wgpu_queue(), &[]);

        assert!(pool.get().is_some(), "there should still be a buffer");
        assert_eq!(pool.creations(), 1);
    }

    #[test]
    fn an_unaligned_write_is_padded_rather_than_rejected() {
        // `write_buffer` requires COPY_BUFFER_ALIGNMENT and refuses anything
        // else. Every real caller passes whole structs; this exists so the pool
        // is not a trap for the first one that does not.
        let Some(device) = device_or_skip() else {
            return;
        };

        let mut pool = GrowableBuffer::new("test", wgpu::BufferUsages::VERTEX);
        for size in [1, 2, 3, 5, 7, 13, 4_095] {
            pool.write(device.wgpu_device(), device.wgpu_queue(), &vec![7u8; size]);
        }

        assert!(pool.get().is_some());
    }

    #[test]
    fn the_generation_changes_only_when_the_buffer_does() {
        // A cached bind group points at a specific buffer. It has to be able to
        // tell that the buffer it was built for is gone, and nothing else.
        let Some(device) = device_or_skip() else {
            return;
        };

        let mut pool = GrowableBuffer::new("test", wgpu::BufferUsages::VERTEX);
        pool.write(device.wgpu_device(), device.wgpu_queue(), &[0u8; 64]);
        let generation = pool.generation();

        pool.write(device.wgpu_device(), device.wgpu_queue(), &[0u8; 32]);
        assert_eq!(pool.generation(), generation, "a fitting write reuses");

        pool.write(device.wgpu_device(), device.wgpu_queue(), &[0u8; 8_192]);
        assert_ne!(pool.generation(), generation, "a growth replaces");
    }

    #[test]
    fn a_target_is_reused_until_the_size_changes() {
        let Some(device) = device_or_skip() else {
            return;
        };

        let mut pool = TargetPool::new();
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;

        for _ in 0..10 {
            pool.get(device.wgpu_device(), 64, 64, format);
        }
        assert_eq!(pool.creations(), 1);

        pool.get(device.wgpu_device(), 128, 64, format);
        assert_eq!(pool.creations(), 2, "a resize needs a new target");

        for _ in 0..10 {
            pool.get(device.wgpu_device(), 128, 64, format);
        }
        assert_eq!(pool.creations(), 2, "and then settles again");
    }

    #[test]
    fn a_format_change_also_replaces_the_target() {
        // The windowed and offscreen formats differ on macOS, and a target
        // reused across them is a fatal validation error rather than a wrong
        // colour.
        let Some(device) = device_or_skip() else {
            return;
        };

        let mut pool = TargetPool::new();
        pool.get(
            device.wgpu_device(),
            64,
            64,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        pool.get(
            device.wgpu_device(),
            64,
            64,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        );

        assert_eq!(pool.creations(), 2);
    }
}
