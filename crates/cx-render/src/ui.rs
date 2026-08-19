//! Drawing the debug overlay (S16/S14).
//!
//! The `egui`↔`wgpu` bridge lives here rather than in `cx-ui` because it names
//! devices, queues, and command encoders. A UI crate handling those while
//! declaring no dependency on `wgpu` would be containment on paper only, so
//! `02-architecture.md` assigns `egui-wgpu` to this crate and `egui` itself to
//! `cx-ui`. `tools/ci-checks` enforces both halves.
//!
//! # Last pass, no depth
//!
//! The overlay draws after the scene and after debug lines, with no depth
//! attachment at all: it is a 2D layer over a finished picture, and depth
//! testing a screen-space panel against world geometry is how a UI ends up
//! disappearing behind a wall.

use cx_ui::UiOutput;

use crate::device::RenderDevice;

/// What one overlay pass cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiStats {
    /// Draw calls issued. Zero when there was nothing to draw.
    pub draw_calls: u32,
    /// Tessellated primitives submitted.
    pub primitives: u32,
}

/// Draws `egui` output.
pub struct UiRenderer {
    renderer: egui_wgpu::Renderer,
}

impl std::fmt::Debug for UiRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiRenderer").finish_non_exhaustive()
    }
}

impl UiRenderer {
    /// Builds the overlay renderer for the offscreen target format.
    pub fn new(device: &RenderDevice) -> Self {
        Self::for_format(device, crate::offscreen::TARGET_FORMAT)
    }

    /// Builds one for a specific colour format.
    ///
    /// A surface's format is not the offscreen one — macOS presents
    /// `Bgra8UnormSrgb` — and unlike the pipelines this crate builds itself,
    /// `egui-wgpu` bakes the format into the whole renderer rather than into a
    /// pipeline that can be swapped. So a surface gets its own.
    pub(crate) fn for_format(device: &RenderDevice, format: wgpu::TextureFormat) -> Self {
        Self {
            renderer: egui_wgpu::Renderer::new(
                device.wgpu_device(),
                format,
                egui_wgpu::RendererOptions {
                    // No depth: the overlay is a 2D layer over a finished
                    // picture.
                    depth_stencil_format: None,
                    ..egui_wgpu::RendererOptions::default()
                },
            ),
        }
    }

    /// Takes a frame's textures without drawing it.
    ///
    /// For a frame that is not being presented — an occluded window, say.
    ///
    /// **Absorbing is not the same as discarding.** `egui` sends the font atlas
    /// once as a full upload and everything after that as *partial* updates to
    /// it. A caller that throws a delta away loses the allocation while `egui`
    /// goes on believing it was made, and the next partial update panics inside
    /// `egui-wgpu` with "tried to update a texture that has not been allocated".
    /// That is precisely what a skipped frame did the first time this ran.
    pub(crate) fn absorb(&mut self, device: &RenderDevice, mut output: UiOutput) {
        self.upload(device, &output);
        self.finish(&mut output);
    }

    /// Applies this frame's texture uploads.
    fn upload(&mut self, device: &RenderDevice, output: &UiOutput) {
        let wgpu_device = device.wgpu_device();
        let queue = device.wgpu_queue();

        // A texture can carry several deltas in one frame — egui batches
        // partial atlas updates — so this is two loops, not one.
        for (id, deltas) in &output.textures.set {
            for delta in deltas {
                self.renderer.update_texture(wgpu_device, queue, *id, delta);
            }
        }
    }

    /// Encodes the overlay pass, consuming `output`.
    ///
    /// Takes ownership because `egui`'s texture delta must be applied exactly
    /// once — dropping it unapplied aborts the process — and consuming it here
    /// makes that the only thing a caller can do with it.
    pub(crate) fn encode(
        &mut self,
        device: &RenderDevice,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: [u32; 2],
        mut output: UiOutput,
    ) -> UiStats {
        let wgpu_device = device.wgpu_device();
        let queue = device.wgpu_queue();

        // Applied before anything is drawn: the first frame's font atlas arrives
        // as a delta, and drawing text before uploading it renders nothing.
        // A texture can carry several deltas in one frame — egui batches
        // partial atlas updates — so this is two loops, not one.
        for (id, deltas) in &output.textures.set {
            for delta in deltas {
                self.renderer.update_texture(wgpu_device, queue, *id, delta);
            }
        }

        if output.primitives.is_empty() {
            // Frees still have to happen — they are how egui reclaims textures —
            // but there is no pass worth encoding.
            self.finish(&mut output);
            return UiStats::default();
        }

        let descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size,
            pixels_per_point: output.pixels_per_point,
        };

        self.renderer
            .update_buffers(wgpu_device, queue, encoder, &output.primitives, &descriptor);

        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cx-render ui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..wgpu::RenderPassDescriptor::default()
            });

            // `forget_lifetime` because egui-wgpu's `render` wants a
            // `RenderPass<'static>`. The pass does not outlive this block; the
            // lifetime is erased, not extended.
            self.renderer
                .render(&mut pass.forget_lifetime(), &output.primitives, &descriptor);
        }

        let primitives = output.primitives.len() as u32;
        self.finish(&mut output);

        UiStats {
            draw_calls: 1,
            primitives,
        }
    }

    /// Releases textures egui has finished with, and marks the delta handled.
    ///
    /// The `clear` is not tidiness. `epaint` tracks whether a delta was dealt
    /// with *separately* from whether its textures were uploaded, and its
    /// destructor panics if it was not — from a destructor, so it aborts rather
    /// than unwinds. Applying every texture and then dropping the delta still
    /// takes the process down.
    fn finish(&mut self, output: &mut UiOutput) {
        for id in &output.textures.free {
            self.renderer.free_texture(id);
        }
        output.textures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::device_or_skip;
    use cx_ui::{Overlay, OverlayState, UiInput};

    fn input() -> UiInput {
        UiInput {
            size: [320.0, 240.0],
            scale: 1.0,
            pointer: None,
            pointer_down: false,
            delta_seconds: 1.0 / 60.0,
        }
    }

    /// The overlay's first frame carries the font atlas and no primitives.
    /// Encoding it must still apply the texture upload, or the second frame
    /// draws text against an atlas that was never uploaded.
    #[test]
    fn the_first_frame_uploads_the_atlas_without_drawing() {
        let Some(device) = device_or_skip() else {
            return;
        };

        let mut renderer = UiRenderer::new(&device);
        let mut overlay = Overlay::new();
        let mut encoder =
            device
                .wgpu_device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("ui test"),
                });

        let (first, _) = overlay.run(input(), &OverlayState::default());
        assert_eq!(
            first.textures.set.len(),
            1,
            "the atlas arrives on frame one"
        );

        let view = crate::instanced::create_depth_target(device.wgpu_device(), 4, 4);
        let stats = renderer.encode(&device, &mut encoder, &view, [320, 240], first);
        assert_eq!(
            stats.draw_calls, 0,
            "nothing to draw on the atlas-building frame"
        );
    }
}
