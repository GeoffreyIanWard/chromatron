//! Presenting to a window.
//!
//! The counterpart to [`crate::offscreen`]: the same draw, aimed at a swapchain
//! instead of a texture. Everything about *what* is drawn was settled and tested
//! without a window; this adds only where the pixels go.
//!
//! # The window arrives as raw handles, not as a window type
//!
//! [`WindowSurface::new`] takes anything implementing `raw-window-handle`'s
//! traits. That is the interop boundary which keeps two containments intact at
//! once: this crate never names `winit`, and `cx-app` never names `wgpu`
//! (`ADR-0010`, enforced by `tools/ci-checks`). The windowing library could be
//! swapped without touching a line of rendering code.
//!
//! # Why this is the part CI cannot check
//!
//! A swapchain needs a real window and a display server. The offscreen path
//! exists precisely so that everything *except* presentation is verified without
//! one — see the M1 milestone. What is left here is thin by design.

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use crate::camera::Camera;
use crate::device::RenderDevice;
use crate::error::RenderError;
use crate::instanced::{DrawCall, DrawStats, InstancedRenderer, create_depth_target};
use crate::offscreen::Rgba;
use cx_view::ExtractedInstance;

/// Anything that can host a surface.
///
/// `Send + Sync + 'static` because the surface outlives the call and may be used
/// from the render path; in practice this is an `Arc<Window>`.
pub trait WindowTarget: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static {}

impl<T> WindowTarget for T where T: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static {}

/// Why a frame was not drawn.
///
/// Every one of these is normal in the life of a window rather than an error,
/// which is exactly why they need naming: dropping one frame is invisible where
/// ending the run is not, so without a reason attached a skipped frame is
/// indistinguishable from a frame that drew nothing because the scene was empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkipReason {
    /// Nothing is visible — the window is behind another, or off-screen.
    Occluded,
    /// The surface became invalid and was reconfigured.
    Lost,
    /// The surface no longer matches the window and was reconfigured.
    Outdated,
    /// The compositor did not hand over a frame in time.
    Timeout,
}

impl SkipReason {
    /// Whether this is expected to persist rather than clear on the next frame.
    ///
    /// An occluded window stays occluded until someone moves it, so a loop that
    /// keeps asking at full speed burns a core to draw nothing. The others are
    /// transient and worth retrying immediately.
    pub const fn is_persistent(self) -> bool {
        matches!(self, SkipReason::Occluded)
    }
}

/// What presenting a frame did.
///
/// An enum rather than `DrawStats { draw_calls: 0 }`, because that is also what
/// an empty scene legitimately produces and the caller has to tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    /// The frame was drawn and handed to the compositor.
    Drawn(DrawStats),
    /// The frame was skipped.
    Skipped(SkipReason),
}

/// A surface size the device will actually accept.
///
/// Returns `None` when there is nothing to configure — a minimised window
/// reports zero, which is normal rather than an error.
///
/// # Why clamping and not failing
///
/// wgpu treats a validation error as **fatal**: configuring an over-size surface
/// does not return `Err`, it aborts the process from inside a windowing
/// callback, with a panic that cannot even unwind. That is what happened the
/// first time a real window opened here. A window stretched from a smaller
/// surface is a visible degradation; an abort is the end of the session.
///
/// In practice this should never fire now that the device takes its resolution
/// limit from the adapter — which is exactly why it is a pure function with
/// tests, rather than a branch nobody can reach to check.
fn usable_size(width: u32, height: u32, max: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }

    let clamped = (width.min(max), height.min(max));
    if clamped != (width, height) {
        tracing::warn!(
            requested_width = width,
            requested_height = height,
            max,
            "the window is larger than the device's maximum texture size; the surface is \
             clamped and the image will be stretched to fit"
        );
    }

    Some(clamped)
}

/// A window's swapchain, configured and ready to present.
pub struct WindowSurface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    depth: wgpu::TextureView,
    // Built for this surface's colour format rather than borrowed from the
    // renderer, whose pipeline is for the offscreen format. The first real
    // window found the difference the hard way: macOS presents
    // `Bgra8UnormSrgb`, and a mismatched pipeline is a fatal validation error.
    pipeline: wgpu::RenderPipeline,
}

impl std::fmt::Debug for WindowSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowSurface")
            .field("size", &(self.config.width, self.config.height))
            .field("format", &self.config.format)
            .finish_non_exhaustive()
    }
}

impl WindowSurface {
    /// Creates and configures a surface for `window`.
    ///
    /// Fails rather than guessing when the adapter cannot present to this
    /// window: that combination genuinely cannot render, and a silent fallback
    /// would produce a black window with no explanation.
    pub fn new(
        device: &RenderDevice,
        renderer: &InstancedRenderer,
        window: impl WindowTarget,
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        let Some((width, height)) = usable_size(width, height, device.max_texture_dimension())
        else {
            return Err(RenderError::InvalidTargetSize { width, height });
        };

        let surface = device
            .wgpu_instance()
            .create_surface(Box::new(window) as Box<dyn WindowTarget>)
            .map_err(|source| RenderError::SurfaceCreationFailed {
                reason: source.to_string(),
            })?;

        // Start from the driver's own defaults rather than assembling a config
        // field by field: wgpu adds fields between releases, and the defaults
        // are already valid for this adapter and surface.
        let mut config = surface
            .get_default_config(device.wgpu_adapter(), width, height)
            .ok_or_else(|| RenderError::SurfaceCreationFailed {
                reason: "the adapter cannot present to this window — it reported no supported \
                         surface formats"
                    .to_owned(),
            })?;

        // Prefer an sRGB format so the swapchain matches the offscreen target.
        // Otherwise a colour verified by a readback test would appear different
        // in the window, which would make the offscreen tests misleading.
        let capabilities = surface.get_capabilities(device.wgpu_adapter());
        if let Some(srgb) = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
        {
            config.format = srgb;
        }

        // Fifo is always supported and is vsync. S03's frame pacing decides when
        // to draw; letting the swapchain also throttle would put two mechanisms
        // in charge of the same decision.
        config.present_mode = wgpu::PresentMode::Fifo;

        surface.configure(device.wgpu_device(), &config);

        let depth = create_depth_target(device.wgpu_device(), width, height);

        tracing::info!(
            width,
            height,
            surface_format = ?config.format,
            present_mode = ?config.present_mode,
            "window surface configured"
        );

        let pipeline = renderer.pipeline_for(device.wgpu_device(), config.format);

        Ok(Self {
            surface,
            config,
            depth,
            pipeline,
        })
    }

    /// Current surface size in physical pixels.
    pub const fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Reconfigures for a new size.
    ///
    /// A zero dimension is ignored rather than treated as an error: minimising a
    /// window legitimately reports one, and refusing would turn a normal user
    /// action into a crash.
    pub fn resize(&mut self, device: &RenderDevice, width: u32, height: u32) {
        let Some((width, height)) = usable_size(width, height, device.max_texture_dimension())
        else {
            tracing::debug!(
                width,
                height,
                "ignoring resize to zero, window likely minimised"
            );
            return;
        };

        if (width, height) == (self.config.width, self.config.height) {
            return;
        }

        self.config.width = width;
        self.config.height = height;
        self.surface.configure(device.wgpu_device(), &self.config);

        // The depth buffer must match the colour target exactly, so it is
        // rebuilt here rather than per frame.
        self.depth = create_depth_target(device.wgpu_device(), width, height);

        tracing::debug!(width, height, "surface resized");
    }

    /// Draws the instances and presents the result.
    pub fn present(
        &mut self,
        device: &RenderDevice,
        renderer: &InstancedRenderer,
        camera: &Camera,
        instances: &[ExtractedInstance],
        clear: Rgba,
    ) -> Result<Presented, RenderError> {
        // Only `Validation` is a real failure. Everything else is a normal event
        // in the life of a window — resized, minimised, display changed — and
        // dropping one frame is invisible where ending the run is not.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,

            // Usable, but the configuration has drifted. Draw this frame anyway,
            // then reconfigure so the next one is correct.
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.surface.configure(device.wgpu_device(), &self.config);
                frame
            }

            // Lost or Outdated: reconfigure and skip. Occluded or Timeout: skip;
            // there is nothing to fix and nobody is looking at the window.
            //
            // Each says *why* rather than returning a bare skip. A frame that
            // silently draws nothing looks identical to a frame that drew
            // nothing because the scene was empty, and telling those apart with
            // no log means guessing.
            wgpu::CurrentSurfaceTexture::Lost => {
                tracing::debug!("surface lost, reconfiguring");
                self.surface.configure(device.wgpu_device(), &self.config);
                return Ok(Presented::Skipped(SkipReason::Lost));
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                tracing::debug!("surface outdated, reconfiguring");
                self.surface.configure(device.wgpu_device(), &self.config);
                return Ok(Presented::Skipped(SkipReason::Outdated));
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(Presented::Skipped(SkipReason::Occluded));
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                tracing::debug!("acquiring the next frame timed out, skipping");
                return Ok(Presented::Skipped(SkipReason::Timeout));
            }

            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(RenderError::SurfaceCreationFailed {
                    reason: "acquiring the next frame failed validation; the surface \
                             configuration and the window no longer agree"
                        .to_owned(),
                });
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let wgpu_device = device.wgpu_device();
        let instance_buffer = renderer.upload_instances(wgpu_device, instances);

        let mut encoder = wgpu_device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("cx-render present"),
        });

        let stats = renderer.encode_draw(
            device.wgpu_queue(),
            &mut encoder,
            DrawCall {
                pipeline: &self.pipeline,
                target: &view,
                depth: &self.depth,
                width: self.config.width,
                height: self.config.height,
                camera,
                instance_buffer: &instance_buffer,
                instance_count: instances.len() as u32,
                clear,
            },
        );

        let queue = device.wgpu_queue();
        queue.submit(std::iter::once(encoder.finish()));
        // In wgpu 30 presentation is a queue operation rather than a method on
        // the frame.
        queue.present(frame);

        Ok(Presented::Drawn(stats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::MeshData;
    use crate::testing::device_or_skip;
    use cx_core::math::{Quat, Vec3};

    /// What a surface actually presents on macOS. The offscreen target is
    /// `Rgba8UnormSrgb`, so this is the format the two paths disagree about.
    const SURFACE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;

    #[test]
    fn a_minimised_window_has_no_usable_size() {
        assert_eq!(usable_size(0, 1_080, 8_192), None);
        assert_eq!(usable_size(1_920, 0, 8_192), None);
        assert_eq!(usable_size(0, 0, 8_192), None);
    }

    #[test]
    fn an_ordinary_window_passes_through_untouched() {
        assert_eq!(usable_size(2_560, 1_440, 16_384), Some((2_560, 1_440)));
    }

    #[test]
    fn an_oversize_window_is_clamped_rather_than_fatal() {
        // The real case: downlevel limits cap textures at 2048, and a 1280x720
        // window on a 2x display asks for 2560x1440.
        assert_eq!(usable_size(2_560, 1_440, 2_048), Some((2_048, 1_440)));
        assert_eq!(usable_size(4_096, 4_096, 2_048), Some((2_048, 2_048)));
    }

    #[test]
    fn each_dimension_is_clamped_on_its_own() {
        // Clamping both to the smaller of the two would change the aspect ratio
        // twice over: once by the clamp, once by the distortion it causes.
        assert_eq!(usable_size(3_000, 100, 2_048), Some((2_048, 100)));
    }

    /// The window path draws with a pipeline built for the *surface's* format,
    /// which is not the offscreen format. Getting that wrong is not a subtle
    /// colour shift: wgpu treats the mismatch as a validation error and aborts
    /// the process, which is exactly what the first real window did.
    ///
    /// A window cannot be opened in CI, but the pipeline can be pointed at the
    /// same format offscreen, which is the part that broke.
    #[test]
    fn drawing_to_a_surface_format_target_works_and_keeps_channel_order() {
        let Some(device) = device_or_skip() else {
            return;
        };

        let renderer = InstancedRenderer::new(&device, &MeshData::unit_cube())
            .expect("the unit cube is a valid mesh");

        let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO);
        let instances = [ExtractedInstance {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }];

        // A clear colour with three distinguishable channels, so a swapped
        // pipeline shows up as a swapped byte rather than as nothing.
        let clear: Rgba = [0.8, 0.2, 0.05, 1.0];

        let (bgra, stats) = renderer
            .render_to_format(
                &device,
                crate::instanced::OffscreenTarget {
                    width: 64,
                    height: 64,
                    format: SURFACE_FORMAT,
                },
                &camera,
                &instances,
                clear,
            )
            .expect("drawing to a surface-format target should work");
        assert_eq!(stats.draw_calls, 1);

        let (rgba, _) = renderer
            .render(&device, 64, 64, &camera, &instances, clear)
            .expect("drawing to the offscreen format should work");

        let bgra_corner = bgra.pixel(1, 1).expect("in bounds");
        let rgba_corner = rgba.pixel(1, 1).expect("in bounds");

        // Same picture, bytes in the order the format names. If the pipeline had
        // been built for the wrong format this would not have got here at all —
        // and if the shader wrote channels in the wrong order, these would match
        // directly instead of being swapped.
        assert_eq!(
            [
                bgra_corner[2],
                bgra_corner[1],
                bgra_corner[0],
                bgra_corner[3]
            ],
            rgba_corner,
            "a BGRA readback should be the RGBA one with red and blue swapped"
        );

        // And the entity is still drawn, not just the clear colour.
        let centre = bgra.pixel(32, 32).expect("in bounds");
        assert!(
            centre[0] > bgra_corner[0] + 40,
            "the lit cube should be brighter than the background:              centre {centre:?}, corner {bgra_corner:?}"
        );
    }
}
