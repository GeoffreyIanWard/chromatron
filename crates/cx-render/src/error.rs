//! Renderer failures.
//!
//! Every variant names what could not be done and, where the underlying API told
//! us, why. `03-conventions.md` requires a loader error to be actionable without
//! reading Rust; the same standard applies here, because the person hitting
//! "no adapter" is usually configuring a machine rather than debugging code.

/// Why the renderer could not do something.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    /// No graphics adapter could be found.
    #[error(
        "no graphics adapter available: {reason}. On a headless machine this usually means no \
         software rasterizer is installed — on Linux, mesa-vulkan-drivers provides lavapipe. \
         The simulation itself does not need a GPU; only rendering does."
    )]
    NoAdapter {
        /// What the graphics API reported.
        reason: String,
    },

    /// An adapter was found but would not hand over a device.
    #[error(
        "graphics device request failed: {reason}. The adapter exists but refused the requested \
         features or limits, which usually means a driver older than the downlevel defaults \
         cx-render asks for."
    )]
    DeviceRequestFailed {
        /// What the graphics API reported.
        reason: String,
    },

    /// A render target size of zero was requested.
    #[error("invalid render target size {width}x{height}: both dimensions must be non-zero")]
    InvalidTargetSize {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },

    /// A mesh with no triangles was handed to the renderer.
    #[error(
        "mesh has no indices: there is nothing to draw. An empty mesh is almost always a \
         loading failure rather than an intent, so it is rejected when the pipeline is built \
         rather than silently drawing nothing every frame."
    )]
    EmptyMesh,

    /// Reading pixels back from the GPU failed.
    #[error("reading back from the GPU failed: {reason}")]
    ReadbackFailed {
        /// What the graphics API reported.
        reason: String,
    },

    /// A surface could not be created for a window.
    #[error("could not create a surface for the window: {reason}")]
    SurfaceCreationFailed {
        /// What the graphics API reported.
        reason: String,
    },
}
