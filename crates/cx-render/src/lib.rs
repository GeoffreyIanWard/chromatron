//! Implements S12 — the wgpu renderer.
//!
//! Below the firewall. **This is the only crate in the workspace permitted to
//! depend on `wgpu`** (`ADR-0005`, `ADR-0010`), and `tools/ci-checks` enforces
//! that rather than trusting it.
//!
//! # Why the boundary survived when the abstraction did not
//!
//! `ADR-0005` originally proposed a backend-agnostic render API so a console
//! backend could be added later. `ADR-0010` put console out of scope, which
//! removed the abstraction's only consumer — an interface with one
//! implementation is a cost with no benefit. What survived is the *crate*
//! boundary: no `wgpu` type appears in a public signature here, so graphics code
//! cannot spread into gameplay, and a future port would be a contained project
//! rather than an archaeology exercise.
//!
//! In practice that means [`RenderDevice`] hands out no `wgpu` handles, and
//! device properties are reported as [`Backend`], [`DeviceKind`], and
//! [`DeviceInfo`] — plain data a caller can log or branch on.
//!
//! # Headless first
//!
//! A device can be acquired with no window and no display server, because that
//! is the environment CI runs in. Three M1 gates still need real hardware to be
//! meaningful; see `docs/milestones/M1-loop-and-pixels.md` for how that is
//! being handled.

pub mod camera;
pub mod debug;
pub mod device;
pub mod error;
pub mod frame;
pub mod instanced;
pub mod mesh;
pub mod offscreen;
pub mod surface;
pub mod testing;
pub mod ui;

pub use camera::Camera;
pub use debug::{DebugRenderer, DebugStats};
pub use device::{Backend, DeviceInfo, DeviceKind, RenderDevice};
pub use error::RenderError;
pub use frame::{FrameContents, FrameRenderer};
pub use instanced::{DrawStats, InstancedRenderer};
pub use mesh::{MeshData, Vertex};
pub use offscreen::{Readback, Rgba};
pub use surface::{Presented, SkipReason, WindowSurface, WindowTarget};
pub use ui::{UiRenderer, UiStats};
