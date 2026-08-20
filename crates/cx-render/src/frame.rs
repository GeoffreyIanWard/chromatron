//! The two renderers a frame needs, as one thing.
//!
//! A frame is the scene plus whatever is drawn on top of it to explain the
//! scene. Those are two pipelines, and every caller wants both — so they are
//! handed out together rather than as two objects an app has to remember to keep
//! in step.
//!
//! # This is also what keeps `wgpu` out of the public API
//!
//! Debug lines and instances draw into targets of the same colour format, and
//! that format has to reach both pipelines. Exposing it would put a
//! `wgpu::TextureFormat` in a signature `cx-app` calls, which is exactly what
//! `ADR-0010` keeps out of this crate's surface. Owning both renderers here
//! means the format never leaves.

use cx_view::DebugVertex;

use crate::camera::Camera;
use crate::cull_pass::CullPass;
use crate::debug::{DebugRenderer, DebugStats};
use crate::device::RenderDevice;
use crate::error::RenderError;
use crate::instanced::{DrawStats, InstancedRenderer, OffscreenTarget};
use crate::mesh::MeshData;
use crate::offscreen::{Readback, Rgba};
use crate::ui::{UiRenderer, UiStats};
use cx_view::ExtractedInstance;

/// What one frame draws.
///
/// A struct because the two lists are filled by different producers and it is
/// otherwise two same-shaped slice arguments in a row, which is the signature
/// that eventually gets called with them the wrong way round.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameContents<'a> {
    /// Extracted instances, the scene itself.
    pub instances: &'a [ExtractedInstance],
    /// Rebased debug-line vertices. Two per segment.
    pub debug: &'a [DebugVertex],
}

/// Everything needed to draw a frame.
#[derive(Debug)]
pub struct FrameRenderer {
    instanced: InstancedRenderer,
    debug: DebugRenderer,
    ui: UiRenderer,
    cull: CullPass,
}

impl FrameRenderer {
    /// Builds every pipeline for `mesh`.
    ///
    /// `instance_capacity` sizes the culling pass's output buffer. It is a
    /// startup decision because growing it mid-frame would mean rebuilding a
    /// bind group during encoding; a scene larger than it is clamped, and the
    /// clamp is tested.
    pub fn new(
        device: &RenderDevice,
        mesh: &MeshData,
        instance_capacity: u32,
    ) -> Result<Self, RenderError> {
        Ok(Self {
            instanced: InstancedRenderer::new(device, mesh)?,
            debug: DebugRenderer::new(device),
            ui: UiRenderer::new(device),
            cull: CullPass::new(device, instance_capacity),
        })
    }

    /// The culling pass.
    pub(crate) const fn cull(&self) -> &CullPass {
        &self.cull
    }

    /// The culling pass, for encoding — it caches a bind group.
    pub(crate) const fn cull_mut(&mut self) -> &mut CullPass {
        &mut self.cull
    }

    /// Draws a frame to an offscreen target and reads the pixels back.
    ///
    /// Takes the overlay too, so the windowed path's *only* untested part is
    /// the swapchain itself: everything from tessellation to pixels is
    /// exercisable on a machine with no display.
    pub fn render_offscreen(
        &mut self,
        device: &RenderDevice,
        size: [u32; 2],
        camera: &Camera,
        contents: FrameContents<'_>,
        overlay: Option<cx_ui::UiOutput>,
        clear: Rgba,
    ) -> Result<(Readback, DrawStats, DebugStats, UiStats), RenderError> {
        let [width, height] = size;

        self.instanced.render_to_format(
            device,
            OffscreenTarget {
                width,
                height,
                format: crate::offscreen::TARGET_FORMAT,
            },
            camera,
            contents,
            clear,
            crate::instanced::OffscreenExtras {
                debug: Some(&mut self.debug),
                overlay: overlay.map(|overlay| (&mut self.ui, overlay)),
            },
        )
    }

    /// The scene renderer.
    pub(crate) const fn instanced(&self) -> &InstancedRenderer {
        &self.instanced
    }

    /// The debug-line renderer.
    pub(crate) const fn debug(&self) -> &DebugRenderer {
        &self.debug
    }

    /// The scene renderer, for the upload half of a frame.
    pub(crate) const fn instanced_mut(&mut self) -> &mut InstancedRenderer {
        &mut self.instanced
    }

    /// The debug-line renderer, for the upload half of a frame.
    pub(crate) const fn debug_mut(&mut self) -> &mut DebugRenderer {
        &mut self.debug
    }

    /// How many GPU resources the frame path has created since this renderer was
    /// built.
    ///
    /// The pooling gate: after a warm-up frame, drawing the same scene again
    /// must not move this. It counts creations rather than measuring time, so it
    /// means the same thing on a laptop GPU and on a software rasterizer — which
    /// is what M1 asked for when it recorded the per-frame allocations as
    /// outstanding.
    pub fn creations(&self) -> u32 {
        self.instanced.creations() + self.debug.creations() + self.cull.creations()
    }

    /// Replaces the overlay renderer with one built for `format`.
    ///
    /// `egui-wgpu` bakes the colour format into the whole renderer rather than
    /// into a swappable pipeline, so a surface whose format differs from the
    /// offscreen one needs the renderer rebuilt rather than a second pipeline.
    pub(crate) fn rebuild_ui_for(&mut self, device: &RenderDevice, format: wgpu::TextureFormat) {
        self.ui = UiRenderer::for_format(device, format);
    }

    /// The overlay renderer.
    pub(crate) const fn ui_mut(&mut self) -> &mut UiRenderer {
        &mut self.ui
    }
}
