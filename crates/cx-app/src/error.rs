//! Failures from running the app.

/// Why the app could not run, or could not keep running.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppError {
    /// The renderer failed.
    #[error(transparent)]
    Render(#[from] cx_render::RenderError),

    /// The windowing system would not give us an event loop.
    #[error(
        "could not create a window event loop: {reason}. On Linux this usually means no display \
         server is reachable — check DISPLAY or WAYLAND_DISPLAY. A headless run does not need \
         one; only the windowed client does."
    )]
    EventLoop {
        /// What the windowing library reported.
        reason: String,
    },

    /// The window itself could not be created.
    #[error("could not create the window: {reason}")]
    WindowCreation {
        /// What the windowing library reported.
        reason: String,
    },

    /// A frame was requested before the window existed.
    ///
    /// Should not be reachable — the event loop delivers redraws only after
    /// `resumed` — but the loop must not silently skip frames if it ever is.
    #[error("a frame was requested before the window was created")]
    NoWindow,
}
