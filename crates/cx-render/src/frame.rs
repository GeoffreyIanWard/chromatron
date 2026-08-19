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
use crate::debug::{DebugRenderer, DebugStats};
use crate::device::RenderDevice;
use crate::error::RenderError;
use crate::instanced::{DrawStats, InstancedRenderer, OffscreenTarget};
use crate::mesh::MeshData;
use crate::offscreen::{Readback, Rgba};
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
}

impl FrameRenderer {
    /// Builds both pipelines for `mesh`.
    pub fn new(device: &RenderDevice, mesh: &MeshData) -> Result<Self, RenderError> {
        Ok(Self {
            instanced: InstancedRenderer::new(device, mesh)?,
            debug: DebugRenderer::new(device),
        })
    }

    /// Draws a frame to an offscreen target and reads the pixels back.
    pub fn render_offscreen(
        &self,
        device: &RenderDevice,
        width: u32,
        height: u32,
        camera: &Camera,
        contents: FrameContents<'_>,
        clear: Rgba,
    ) -> Result<(Readback, DrawStats, DebugStats), RenderError> {
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
            Some(&self.debug),
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
}
