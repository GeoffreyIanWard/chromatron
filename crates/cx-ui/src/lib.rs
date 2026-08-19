//! Implements S16/S14 — the debug overlay.
//!
//! Below the firewall: presentation-side. Nothing above the firewall may depend
//! on this crate (enforced by `tools/ci-checks`).
//!
//! # This crate owns `egui`, and only the half that is not `wgpu`
//!
//! `egui` is contained here the way `wgpu` is contained in `cx-render`. The
//! bridge between them, `egui-wgpu`, lives with the *renderer* rather than here:
//! it names devices, queues, and command encoders, and a UI crate handling those
//! while declaring no dependency on `wgpu` would be containment on paper only.
//!
//! What crosses the boundary is [`UiOutput`] — the tessellated result — which
//! `cx-render` draws and `cx-app` only passes along.
//!
//! # What is testable here
//!
//! The overlay's *content* is a pure function of plain data ([`OverlayState`]),
//! and `egui` runs headlessly, so what the overlay says and which buttons it
//! offers are both checkable in CI. Only the pixels need a window.

pub mod controls;
pub mod frame_graph;
pub mod overlay;

pub use controls::{Action, Response, respond};
pub use frame_graph::{FrameGraph, GraphSummary};
pub use overlay::{Overlay, OverlayState, UiInput, UiOutput};
