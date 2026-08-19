//! Graphics device acquisition.
//!
//! Everything here is about getting a usable GPU device and describing it in
//! plain data. No `wgpu` type appears in a public signature — that boundary is
//! the whole reason `ADR-0005` was superseded rather than deleted: the trait
//! abstraction went away, the *crate* boundary stayed (`ADR-0010`).
//!
//! # Headless first
//!
//! A device can be acquired without a window. That is not a convenience: it is
//! what lets the renderer be constructed, tested, and inspected in CI, where
//! there is no display server and often no hardware GPU at all. Surface creation
//! is a separate step that happens only when a window exists.

use std::fmt;

use crate::error::RenderError;

/// Which graphics API the device is running on.
///
/// A plain enum rather than `wgpu::Backend`, so callers can log and branch on it
/// without the type escaping this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Backend {
    /// Vulkan, including software implementations such as lavapipe.
    Vulkan,
    /// Metal, on Apple platforms.
    Metal,
    /// Direct3D 12, on Windows.
    Dx12,
    /// OpenGL, generally a fallback.
    Gl,
    /// wgpu's built-in CPU implementation.
    BrowserWebGpu,
    /// Anything this crate does not have a name for yet.
    Other,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Backend::Vulkan => "Vulkan",
            Backend::Metal => "Metal",
            Backend::Dx12 => "DirectX 12",
            Backend::Gl => "OpenGL",
            Backend::BrowserWebGpu => "WebGPU",
            Backend::Other => "other",
        };
        f.write_str(name)
    }
}

impl From<wgpu::Backend> for Backend {
    fn from(backend: wgpu::Backend) -> Self {
        match backend {
            wgpu::Backend::Vulkan => Backend::Vulkan,
            wgpu::Backend::Metal => Backend::Metal,
            wgpu::Backend::Dx12 => Backend::Dx12,
            wgpu::Backend::Gl => Backend::Gl,
            wgpu::Backend::BrowserWebGpu => Backend::BrowserWebGpu,
            _ => Backend::Other,
        }
    }
}

/// What kind of hardware the device is.
///
/// Worth reporting because it changes what performance numbers mean: a frame
/// rate measured on `Cpu` says nothing about a discrete GPU, and recording which
/// one produced a benchmark is the difference between a number and a number that
/// can be compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceKind {
    /// A discrete GPU.
    Discrete,
    /// An integrated GPU sharing system memory.
    Integrated,
    /// A software rasterizer such as lavapipe or WARP.
    Cpu,
    /// Unreported or unrecognised.
    Other,
}

impl fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            DeviceKind::Discrete => "discrete GPU",
            DeviceKind::Integrated => "integrated GPU",
            DeviceKind::Cpu => "software rasterizer",
            DeviceKind::Other => "unknown device",
        };
        f.write_str(name)
    }
}

impl From<wgpu::DeviceType> for DeviceKind {
    fn from(kind: wgpu::DeviceType) -> Self {
        match kind {
            wgpu::DeviceType::DiscreteGpu => DeviceKind::Discrete,
            wgpu::DeviceType::IntegratedGpu => DeviceKind::Integrated,
            wgpu::DeviceType::Cpu => DeviceKind::Cpu,
            _ => DeviceKind::Other,
        }
    }
}

/// What was acquired, in plain data.
///
/// Recorded alongside any benchmark this device produced (`bench/baselines.md`
/// gates on hardware, so the hardware has to be identifiable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Adapter name as the driver reports it.
    pub name: String,
    /// Graphics API in use.
    pub backend: Backend,
    /// Hardware class.
    pub kind: DeviceKind,
    /// Driver name and version, when reported.
    pub driver: String,
}

impl DeviceInfo {
    /// Whether this device is a software rasterizer.
    ///
    /// The renderer runs on one happily; a *frame rate* measured on one is not
    /// comparable to hardware and must not be recorded as though it were.
    ///
    /// **This is not the same question as "is this reference hardware".** A
    /// macOS CI runner reports `Apple Paravirtual device` as an integrated GPU,
    /// so this returns `false`, yet it is a VM whose numbers are no more
    /// representative than lavapipe's. Anything deciding whether a measurement
    /// is comparable should look at [`DeviceInfo::name`] too, not this alone.
    pub const fn is_software(&self) -> bool {
        matches!(self.kind, DeviceKind::Cpu)
    }

    /// One line, for logs and benchmark provenance.
    pub fn summary(&self) -> String {
        format!("{} ({}, {})", self.name, self.backend, self.kind)
    }
}

/// An acquired graphics device.
///
/// Holds the `wgpu` handles privately. Nothing here hands one out — see the
/// module docs.
pub struct RenderDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: DeviceInfo,
    // Kept so a surface can be created later, when a window exists. Acquiring
    // the device does not require one — that is what makes headless rendering
    // and therefore CI testing possible — but presenting does.
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
}

impl fmt::Debug for RenderDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderDevice")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl RenderDevice {
    /// Acquires a device with no surface attached.
    ///
    /// Blocks on adapter and device requests. That is deliberate: startup is the
    /// one place a blocking wait is harmless, and making this `async` would push
    /// a runtime requirement onto every caller for a call that happens once.
    pub fn headless() -> Result<Self, RenderError> {
        // `Instance::default()` picks all available backends and a display-less
        // descriptor, which is what a headless acquisition wants.
        let instance = wgpu::Instance::default();

        // Struct-update from `default()` rather than listing every field: wgpu
        // adds options between releases, and spelling them all out turns a
        // version bump into a compile error with nothing to learn from.
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            // No surface to be compatible with, and a software fallback is
            // acceptable: CI has no GPU, and a renderer that cannot start there
            // cannot be tested there either.
            compatible_surface: None,
            force_fallback_adapter: false,
            ..wgpu::RequestAdapterOptions::default()
        }))
        .map_err(|source| RenderError::NoAdapter {
            reason: source.to_string(),
        })?;

        let adapter_info = adapter.get_info();

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cx-render device"),
            required_features: wgpu::Features::empty(),
            // Downlevel defaults rather than the highest available: the M1 draw
            // path is instanced low-poly geometry, and asking for more than that
            // needs would exclude the software adapters CI depends on.
            //
            // *Except* resolution, which is taken from the adapter. Downlevel
            // caps textures at 2048, and a 1280x720 window on a 2x display is a
            // 2560x1440 surface — configuring it aborted the process the first
            // time a real window opened. Resolution is the one downlevel limit a
            // window can exceed just by existing, and raising it excludes no
            // adapter, because it is the adapter's own number.
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
            ..wgpu::DeviceDescriptor::default()
        }))
        .map_err(|source| RenderError::DeviceRequestFailed {
            reason: source.to_string(),
        })?;

        let info = DeviceInfo {
            name: adapter_info.name.clone(),
            backend: adapter_info.backend.into(),
            kind: adapter_info.device_type.into(),
            driver: if adapter_info.driver.is_empty() {
                "unreported".to_owned()
            } else {
                format!("{} {}", adapter_info.driver, adapter_info.driver_info)
                    .trim()
                    .to_owned()
            },
        };

        tracing::info!(device = %info.summary(), "graphics device acquired");

        Ok(Self {
            device,
            queue,
            info,
            instance,
            adapter,
        })
    }

    /// The largest 2D texture this device will accept, per side.
    ///
    /// Public because it bounds what a *caller* may ask for: a surface larger
    /// than this is a validation error, and wgpu treats validation errors as
    /// fatal, so it has to be caught before it is requested rather than handled
    /// after.
    pub fn max_texture_dimension(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    /// What was acquired.
    pub const fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// The `wgpu` device, for use *within this crate only*.
    pub(crate) const fn wgpu_device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The `wgpu` queue, for use *within this crate only*.
    pub(crate) const fn wgpu_queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The `wgpu` instance, for use *within this crate only*.
    pub(crate) const fn wgpu_instance(&self) -> &wgpu::Instance {
        &self.instance
    }

    /// The `wgpu` adapter, for use *within this crate only*.
    pub(crate) const fn wgpu_adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }

    /// Submits any pending work and waits for the GPU to finish.
    ///
    /// For tests and shutdown. Not a per-frame operation — stalling on the GPU
    /// every frame is how a renderer ends up CPU-bound on its own fence.
    pub fn wait_for_idle(&self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Device acquisition is environment-dependent: a machine with no GPU and no
    /// software rasterizer legitimately cannot do it. The test asserts the
    /// *reporting* is coherent either way rather than requiring hardware, so it
    /// is meaningful on a developer machine and honest on a bare CI runner.
    #[test]
    fn device_acquisition_either_works_or_explains_itself() {
        match RenderDevice::headless() {
            Ok(device) => {
                let info = device.info();
                assert!(
                    !info.name.is_empty(),
                    "an acquired device must report a name"
                );
                assert!(!info.summary().is_empty());
                assert!(
                    device.max_texture_dimension() >= 2_048,
                    "downlevel defaults guarantee at least 2048; got {}",
                    device.max_texture_dimension()
                );
                device.wait_for_idle();
                println!("acquired: {}", info.summary());
            }
            Err(error) => {
                // Must say what went wrong. "Failed to create renderer" with no
                // detail is the error message that wastes an afternoon.
                let message = error.to_string();
                assert!(
                    message.len() > 20,
                    "the failure must be actionable, got: {message}"
                );
                println!("no device available: {message}");
            }
        }
    }

    #[test]
    fn software_devices_are_identifiable() {
        // Frame rates from a software rasterizer are not comparable to hardware,
        // so the distinction has to survive into anything that records a number.
        let software = DeviceInfo {
            name: "llvmpipe".to_owned(),
            backend: Backend::Vulkan,
            kind: DeviceKind::Cpu,
            driver: "Mesa".to_owned(),
        };
        let hardware = DeviceInfo {
            name: "Some GPU".to_owned(),
            backend: Backend::Metal,
            kind: DeviceKind::Discrete,
            driver: "unreported".to_owned(),
        };

        assert!(software.is_software());
        assert!(!hardware.is_software());
        assert!(software.summary().contains("software rasterizer"));
    }
}
