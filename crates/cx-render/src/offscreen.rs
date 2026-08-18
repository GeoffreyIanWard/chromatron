//! Rendering to a texture and reading it back.
//!
//! This is the smallest thing that proves a graphics path actually works: clear
//! a texture to a known colour, copy it to a mappable buffer, and check the
//! pixels. It needs no window, no display server, and no swapchain.
//!
//! That matters more than it sounds. Three M1 gates need real hardware to be
//! *meaningful*, but correctness — does the pipeline build, does a draw land,
//! do the right pixels come out — can be checked anywhere `wgpu` finds an
//! adapter, including a software rasterizer on a CI runner. Everything the
//! renderer grows from here should keep that property, because a rendering bug
//! caught by a readback assertion is worth ten caught by looking at a screenshot.

use crate::device::RenderDevice;
use crate::error::RenderError;

/// A linear RGBA colour, each channel in `[0, 1]`.
pub type Rgba = [f32; 4];

/// An image read back from the GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readback {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tightly packed RGBA8 pixels, row-major, no padding.
    ///
    /// The GPU's own copy is padded to a 256-byte row alignment; that padding is
    /// stripped here so callers never have to know about it.
    pub pixels: Vec<u8>,
}

impl Readback {
    /// The RGBA8 pixel at `(x, y)`, or `None` if out of bounds.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((y * self.width + x) * 4) as usize;
        self.pixels.get(offset..offset + 4)?.try_into().ok()
    }
}

/// The format used for offscreen targets.
///
/// `Rgba8UnormSrgb` matches what a surface will present, so a readback compares
/// against the same colours the window would show. Every adapter supports it,
/// including software ones.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Row alignment `copy_texture_to_buffer` requires.
const COPY_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

impl RenderDevice {
    /// Clears a texture to `colour` and reads the result back.
    ///
    /// The seed of the real draw path: everything a frame needs — a target, an
    /// encoder, a render pass, a submit, a readback — is here except the drawing
    /// itself.
    pub fn render_clear(
        &self,
        width: u32,
        height: u32,
        colour: Rgba,
    ) -> Result<Readback, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidTargetSize { width, height });
        }

        let device = self.wgpu_device();
        let queue = self.wgpu_queue();

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cx-render offscreen target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Rows in the copy destination must be 256-byte aligned, which is almost
        // never the natural row length. The padding is stripped after mapping so
        // it never reaches a caller.
        let unpadded_row_bytes = width * 4;
        let padded_row_bytes = unpadded_row_bytes.div_ceil(COPY_ALIGNMENT) * COPY_ALIGNMENT;

        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cx-render readback"),
            size: u64::from(padded_row_bytes) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("cx-render clear"),
        });

        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cx-render clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(colour[0]),
                            g: f64::from(colour[1]),
                            b: f64::from(colour[2]),
                            a: f64::from(colour[3]),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                ..wgpu::RenderPassDescriptor::default()
            });
        }

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        queue.submit(std::iter::once(encoder.finish()));

        let slice = readback_buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});

        // Blocking here is correct: this is a readback, which is inherently a
        // synchronisation point. The per-frame path never does this.
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|source| RenderError::ReadbackFailed {
                reason: source.to_string(),
            })?;

        let mapped = slice
            .get_mapped_range()
            .map_err(|source| RenderError::ReadbackFailed {
                reason: source.to_string(),
            })?;
        let mut pixels = Vec::with_capacity((unpadded_row_bytes * height) as usize);
        for row in 0..height {
            let start = (row * padded_row_bytes) as usize;
            let end = start + unpadded_row_bytes as usize;
            let Some(row_bytes) = mapped.get(start..end) else {
                return Err(RenderError::ReadbackFailed {
                    reason: format!("mapped buffer was shorter than the {height}-row copy"),
                });
            };
            pixels.extend_from_slice(row_bytes);
        }

        drop(mapped);
        readback_buffer.unmap();

        Ok(Readback {
            width,
            height,
            pixels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Skips rather than fails when no adapter exists.
    ///
    /// A developer machine and a CI runner with lavapipe both run this; a bare
    /// container legitimately cannot, and turning that into a red build would
    /// teach everyone to ignore it.
    fn device_or_skip() -> Option<RenderDevice> {
        match RenderDevice::headless() {
            Ok(device) => Some(device),
            Err(error) => {
                println!("skipping: no graphics adapter ({error})");
                None
            }
        }
    }

    #[test]
    fn a_cleared_target_reads_back_the_clear_colour() {
        let Some(device) = device_or_skip() else {
            return;
        };

        // Mid-grey rather than a primary: a channel swap or a format mismatch
        // would still produce "red" from red, but grey lands differently under
        // sRGB conversion if the format is wrong.
        let readback = device
            .render_clear(64, 32, [0.5, 0.25, 0.75, 1.0])
            .expect("clearing a small target should work on any adapter");

        assert_eq!(readback.width, 64);
        assert_eq!(readback.height, 32);
        assert_eq!(
            readback.pixels.len(),
            64 * 32 * 4,
            "readback must be tightly packed"
        );

        let centre = readback.pixel(32, 16).expect("centre pixel is in bounds");
        assert_eq!(centre[3], 255, "alpha should be opaque");

        // sRGB-encoded, so the stored bytes are not the linear values. Check
        // ordering rather than exact numbers: green darkest, blue brightest.
        assert!(centre[1] < centre[0], "green was requested darker than red");
        assert!(centre[0] < centre[2], "red was requested darker than blue");
    }

    #[test]
    fn every_pixel_is_cleared_not_just_the_first_row() {
        // The row-padding arithmetic is the part that silently corrupts a
        // readback: get it wrong and later rows come out shifted or zeroed.
        let Some(device) = device_or_skip() else {
            return;
        };

        // 17 is deliberately awkward: 17 * 4 = 68 bytes per row, which pads to
        // 256, so almost every row is mostly padding.
        let readback = device
            .render_clear(17, 5, [1.0, 1.0, 1.0, 1.0])
            .expect("clear works");

        for y in 0..5 {
            for x in 0..17 {
                let pixel = readback.pixel(x, y).expect("in bounds");
                assert_eq!(
                    pixel,
                    [255, 255, 255, 255],
                    "pixel ({x}, {y}) was not cleared"
                );
            }
        }
    }

    #[test]
    fn out_of_bounds_pixels_report_none() {
        let readback = Readback {
            width: 2,
            height: 2,
            pixels: vec![0; 16],
        };
        assert!(readback.pixel(1, 1).is_some());
        assert!(readback.pixel(2, 0).is_none());
        assert!(readback.pixel(0, 2).is_none());
    }

    #[test]
    fn a_zero_sized_target_is_rejected_with_a_reason() {
        let Some(device) = device_or_skip() else {
            return;
        };

        let error = device
            .render_clear(0, 16, [0.0; 4])
            .expect_err("zero width is invalid");
        assert!(
            error.to_string().contains('0'),
            "the message should name the bad size"
        );
    }
}
