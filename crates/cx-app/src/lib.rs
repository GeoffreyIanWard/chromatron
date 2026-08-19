//! Implements S16/S03 — main loop assembly.
//!
//! Below the firewall. This is where the sim world, the tick clock, the view
//! world, and the renderer are finally composed into something that runs — and
//! where the window lives.
//!
//! # Two layers, split by what CI can check
//!
//! - [`frame`] composes a frame and can target an offscreen texture, so ticks
//!   per frame, interpolation, extract counts, and draw calls are all observable
//!   without a display server.
//! - [`controls`] turns a player action into a [`cx_time::TimeControl`], which is
//!   pure state transition and unit-tested.
//! - [`window`] is `winit` event plumbing and a wall-clock read. It is the only
//!   part that genuinely cannot run in CI, which is why it is the only part left
//!   with nothing else in it.
//!
//! That ordering is deliberate: everything testable landed before the window did
//! (`docs/milestones/M1-loop-and-pixels.md`).
//!
//! # Containment
//!
//! This crate is the only one permitted to depend on `winit`, exactly as
//! `cx-render` is the only one permitted to depend on `wgpu`
//! (`02-architecture.md`, enforced by `tools/ci-checks`). The two boundaries
//! meet through `raw-window-handle`, so neither crate names the other's library.

pub mod controls;
pub mod error;
pub mod frame;
pub mod window;

pub use controls::{Action, Response};
pub use error::AppError;
pub use frame::{FrameLoop, FrameReport};
pub use window::{WindowConfig, run};
