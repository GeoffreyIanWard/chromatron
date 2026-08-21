//! The window, and the only place `winit` appears.
//!
//! `cx-app` owns windowing the way `cx-render` owns `wgpu` (`02-architecture.md`,
//! enforced by `tools/ci-checks`). The two never meet: a window is handed to the
//! renderer as raw handles, so `cx-render` never names `winit` and this module
//! never names `wgpu`.
//!
//! # What is deliberately not here
//!
//! Everything that can be decided without a display server has already been
//! decided elsewhere — tick pacing in `cx-time`, extract in `cx-view`, the draw
//! in `cx-render`, key semantics in [`crate::controls`]. This module is left
//! with event plumbing and a clock read, which is exactly the amount of code
//! that cannot be tested in CI.
//!
//! # Wall-clock time lives here and only here
//!
//! `ADR-0004` forbids reading the clock inside the simulation, because a
//! determinism guarantee cannot survive a system that asks what time it is. The
//! frame loop needs real elapsed time from *somewhere*, and this is that
//! somewhere: it is measured here, at the outermost edge, and enters the sim
//! only as a [`Fixed`] duration the clock converts into a whole number of ticks.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use cx_core::Fixed;
use cx_ecs::{SimSchedule, SimWorld};
use cx_render::WindowSurface;
use cx_time::TickRate;

use crate::error::AppError;
use crate::flycam::{FlyCamera, LookIntent, MoveIntent};
use crate::frame::{FrameLoop, FrameReport};
use cx_ui::{Action, Overlay, OverlayState, Response, UiInput, respond};

/// How the window should open.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// Title bar text.
    pub title: String,
    /// Initial width in logical pixels.
    pub width: u32,
    /// Initial height in logical pixels.
    pub height: u32,
    /// Instances the view world is sized for up front, so extract does not
    /// allocate on the frame path.
    pub instance_capacity: usize,
    /// Simulation rate.
    pub rate: TickRate,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Chromatron".to_owned(),
            width: 1_280,
            height: 720,
            instance_capacity: 16_384,
            rate: TickRate::default(),
        }
    }
}

/// How long to wait before trying again after a frame was skipped for a reason
/// that will not clear on its own.
///
/// Roughly a 60 Hz retry. Without it the loop spins at whatever rate the CPU
/// allows — 14,000 frames a second, measured, all of them drawing nothing —
/// because `ControlFlow::Poll` never waits and an occluded window stays occluded
/// until someone moves it. The simulation still ticks at 30 Hz through this,
/// which is the point: being behind another window is not being paused.
const IDLE_RETRY: Duration = Duration::from_millis(16);

/// How often the loop reports what it is doing.
///
/// S03 requires falling behind to be visible; this is the other half of that —
/// a steady line proving the loop is running, so "nothing is happening" can be
/// told apart from "everything is happening and the picture is wrong". One line
/// a second is cheap enough to leave on.
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// The key bindings, as a table rather than a match arm per key.
///
/// Data because it is also documentation: this is the list to print in a help
/// overlay, and a binding that exists only inside a `match` cannot be printed.
const BINDINGS: &[(KeyCode, Action)] = &[
    (KeyCode::Escape, Action::Quit),
    (KeyCode::Space, Action::TogglePause),
    (KeyCode::Period, Action::Step),
    (KeyCode::BracketRight, Action::Faster),
    (KeyCode::BracketLeft, Action::Slower),
    (KeyCode::Backslash, Action::NormalSpeed),
];

/// How far the camera turns per pixel of mouse movement, in radians.
///
/// Roughly 640 pixels for a half turn, which is a conventional starting point.
/// Not configurable yet; when it becomes so, this is the default it starts from.
const LOOK_SENSITIVITY: f32 = 0.0025;

/// One direction of camera movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Forward,
    Back,
    Left,
    Right,
    Up,
    Down,
    Boost,
}

/// Movement keys. Held rather than pressed, so they are a separate table from
/// [`BINDINGS`], whose actions fire once per press.
const MOVEMENT: &[(KeyCode, Axis)] = &[
    (KeyCode::KeyW, Axis::Forward),
    (KeyCode::KeyS, Axis::Back),
    (KeyCode::KeyA, Axis::Left),
    (KeyCode::KeyD, Axis::Right),
    (KeyCode::KeyE, Axis::Up),
    (KeyCode::KeyQ, Axis::Down),
    (KeyCode::ShiftLeft, Axis::Boost),
];

/// Which movement keys are currently down.
///
/// Tracked here rather than read from the OS because key *events* are what
/// arrive: without this, releasing a key while the window is unfocused would
/// leave the camera flying forever.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Held {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    boost: bool,
}

impl Held {
    fn set(&mut self, axis: Axis, pressed: bool) {
        let slot = match axis {
            Axis::Forward => &mut self.forward,
            Axis::Back => &mut self.back,
            Axis::Left => &mut self.left,
            Axis::Right => &mut self.right,
            Axis::Up => &mut self.up,
            Axis::Down => &mut self.down,
            Axis::Boost => &mut self.boost,
        };
        *slot = pressed;
    }

    /// Opposite keys cancel, so holding both is standing still rather than
    /// whichever was pressed last.
    fn intent(self) -> MoveIntent {
        MoveIntent {
            forward: axis_value(self.forward, self.back),
            right: axis_value(self.right, self.left),
            up: axis_value(self.up, self.down),
            boost: self.boost,
        }
    }
}

const fn axis_value(positive: bool, negative: bool) -> f32 {
    match (positive, negative) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    }
}

/// When the next frame should be drawn.
///
/// `None` means "as soon as possible". A deadline means the loop is backing off
/// because nothing is being shown.
///
/// The one other place a wall-clock read is legitimate, for the same reason as
/// [`WindowedApp::elapsed`]: it schedules a wake-up and never reaches the
/// simulation.
#[allow(
    clippy::disallowed_methods,
    reason = "scheduling the next wake-up needs the current instant; nothing here enters the \
              simulation, and ADR-0004's rule is about sim logic"
)]
fn next_frame_at(report: &FrameReport) -> Option<Instant> {
    match report.skipped {
        Some(reason) if reason.is_persistent() => Some(Instant::now() + IDLE_RETRY),
        _ => None,
    }
}

/// Whether the deadline has arrived.
#[allow(
    clippy::disallowed_methods,
    reason = "same wall-clock exemption as `next_frame_at`; this only decides when to ask for \
              the next frame"
)]
fn deadline_reached(deadline: Option<Instant>) -> bool {
    deadline.is_none_or(|at| Instant::now() >= at)
}

/// How the event loop should wait, given when the next frame is due.
fn pacing_for(deadline: Option<Instant>) -> ControlFlow {
    match deadline {
        Some(at) => ControlFlow::WaitUntil(at),
        None => ControlFlow::Poll,
    }
}

/// The movement axis a physical key means, if any.
fn axis_for(key: KeyCode) -> Option<Axis> {
    MOVEMENT
        .iter()
        .find(|(bound, _)| *bound == key)
        .map(|(_, axis)| *axis)
}

/// The action a physical key means, if any.
fn action_for(key: KeyCode) -> Option<Action> {
    BINDINGS
        .iter()
        .find(|(bound, _)| *bound == key)
        .map(|(_, action)| *action)
}

/// Runs a simulation in a window until it is closed.
///
/// Blocks for the lifetime of the window. Returns once the event loop exits,
/// either because the window was closed or because a frame failed — a failing
/// frame ends the run rather than being logged and retried, since the realistic
/// causes (device lost, surface unrecoverable) repeat every frame and would
/// otherwise fill the log at the frame rate.
pub fn run<F>(
    config: WindowConfig,
    world: SimWorld,
    schedule: SimSchedule,
    camera: FlyCamera,
    before_frame: F,
) -> Result<(), AppError>
where
    F: FnMut(&mut FrameLoop, &mut FlyCamera, f32),
{
    let event_loop = EventLoop::new().map_err(|source| AppError::EventLoop {
        reason: source.to_string(),
    })?;

    // Poll rather than Wait: a simulation redraws continuously, so there is
    // always another frame due. `Wait` is for applications that are idle between
    // user input, which this is not.
    event_loop.set_control_flow(ControlFlow::Poll);

    let frame_loop = FrameLoop::windowed(config.rate, config.instance_capacity)?;
    tracing::info!(device = %frame_loop.device().info().summary(), "opening window");

    let mut app = WindowedApp {
        config,
        before_frame,
        frame_loop,
        world,
        schedule,
        camera,
        active: None,
        last_frame: None,
        failure: None,
        since_report: None,
        frames_since_report: 0,
        last_report: FrameReport::default(),
        quit_requested: false,
        skipped_since_report: 0,
        held: Held::default(),
        overlay: Overlay::new(),
        ui_wants_pointer: false,
        next_frame_at: None,
        pointer: None,
        pointer_down: false,
        looking: false,
    };

    event_loop
        .run_app(&mut app)
        .map_err(|source| AppError::EventLoop {
            reason: source.to_string(),
        })?;

    // A failure inside a winit callback cannot be returned from it, so it is
    // parked and re-raised here. Without this the process would exit zero after
    // a frame error, which is the kind of green run that costs an afternoon.
    match app.failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// A window and the surface presenting to it.
///
/// One struct because they are created together and neither is usable alone:
/// having a window with no surface is a state the loop would have to keep
/// checking for.
struct Active {
    window: Arc<Window>,
    surface: WindowSurface,
}

struct WindowedApp<F> {
    config: WindowConfig,
    /// Called once per frame, before anything is drawn, with the camera and
    /// the real seconds since the previous frame.
    ///
    /// Where a client queues its debug geometry and drives anything that
    /// follows the camera — the chunk lifecycle, above all. The camera is
    /// mutable so a client can reposition it from what streaming terrain
    /// reveals (the demo lifts the spawn above the first ground that
    /// arrives). A hook rather
    /// than a retained list because debug draw is immediate mode — see
    /// [`crate::frame::FrameLoop::debug`].
    before_frame: F,
    frame_loop: FrameLoop,
    world: SimWorld,
    schedule: SimSchedule,
    camera: FlyCamera,
    active: Option<Active>,
    last_frame: Option<Instant>,
    failure: Option<AppError>,
    since_report: Option<Instant>,
    frames_since_report: u32,
    skipped_since_report: u32,
    /// The previous frame's report, which is what the overlay displays.
    last_report: FrameReport,
    /// Set when the overlay's quit button is pressed, since a callback cannot
    /// reach the event loop.
    quit_requested: bool,
    held: Held,
    overlay: Overlay,
    /// Where the pointer is, in logical pixels, when it is over the window.
    pointer: Option<[f32; 2]>,
    /// Whether the primary pointer button is down.
    pointer_down: bool,
    /// When the next frame is due, when the loop is backing off.
    next_frame_at: Option<Instant>,
    /// Whether the overlay is under the pointer, in which case pointer input
    /// belongs to it rather than to the camera.
    ui_wants_pointer: bool,
    /// Whether the look button is down. Mouse movement only turns the camera
    /// while it is, so the cursor stays usable for everything else.
    looking: bool,
}

impl<F: FnMut(&mut FrameLoop, &mut FlyCamera, f32)> WindowedApp<F> {
    /// Creates the window and its surface.
    fn activate(&mut self, event_loop: &ActiveEventLoop) -> Result<Active, AppError> {
        let attributes = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(LogicalSize::new(self.config.width, self.config.height));

        let window = Arc::new(event_loop.create_window(attributes).map_err(|source| {
            AppError::WindowCreation {
                reason: source.to_string(),
            }
        })?);

        // Physical pixels, not the logical size requested above: the surface is
        // a pixel buffer, and on a 2x display the two differ by exactly the
        // factor that makes everything render at quarter size.
        let size = window.inner_size();
        let surface = self.frame_loop.surface_for(
            Arc::clone(&window),
            size.width.max(1),
            size.height.max(1),
        )?;

        tracing::info!(
            width = size.width,
            height = size.height,
            scale = window.scale_factor(),
            "window created"
        );

        Ok(Active { window, surface })
    }

    /// Real time since the previous frame.
    ///
    /// The first frame reports zero rather than "since startup", which would
    /// otherwise hand the clock every millisecond spent compiling shaders and
    /// opening a window, and make the run begin with a catch-up burst.
    #[allow(
        clippy::disallowed_methods,
        reason = "the frame loop's real-time input has to come from the wall clock somewhere, \
                  and ADR-0004's rule is that it happens outside the simulation — which is here, \
                  at the outermost edge, feeding a Fixed duration inward"
    )]
    fn elapsed(&mut self) -> Fixed {
        let now = Instant::now();
        match self.last_frame.replace(now) {
            Some(previous) => Fixed::from_micros(
                u64::try_from(now.duration_since(previous).as_micros()).unwrap_or(u64::MAX),
            ),
            None => Fixed::ZERO,
        }
    }

    /// Logs frame rate and tick progress once per [`REPORT_INTERVAL`].
    #[allow(
        clippy::disallowed_methods,
        reason = "same wall-clock exemption as `elapsed`: this is diagnostics at the outermost \
                  edge, and nothing it measures reaches the simulation"
    )]
    fn report_progress(&mut self, report: &FrameReport) {
        self.frames_since_report += 1;
        if report.skipped.is_some() {
            self.skipped_since_report += 1;
        }

        let now = Instant::now();
        let started = *self.since_report.get_or_insert(now);
        let elapsed = now.duration_since(started);
        if elapsed < REPORT_INTERVAL {
            return;
        }

        tracing::info!(
            fps = (f64::from(self.frames_since_report) / elapsed.as_secs_f64()).round(),
            tick = self.frame_loop.tick().0,
            instances = report.extracted,
            draw_calls = report.draw.map_or(0, |stats| stats.draw_calls),
            terrain_chunks = report.terrain.chunks,
            terrain_triangles = report.terrain.triangles,
            // Skipped frames counted rather than logged individually: an
            // occluded window produces tens of thousands a second, and a line
            // each would be the only thing in the log.
            skipped = self.skipped_since_report,
            debug_lines = report.debug.lines,
            ui_primitives = report.ui.primitives,
            control = ?self.frame_loop.control(),
            // The camera, so flying is verifiable from a log rather than only
            // by looking at the screen.
            camera_chunk = ?self.camera.origin(),
            camera_local = ?self.camera.position.local,
            heading = self.camera.yaw().to_degrees().round(),
            "running"
        );

        self.since_report = Some(now);
        self.frames_since_report = 0;
        self.skipped_since_report = 0;
    }

    /// Runs and presents one frame.
    fn draw(&mut self) -> Result<FrameReport, AppError> {
        let delta = self.elapsed();

        // Before anything else, so the client's debug geometry is queued in time
        // to be rebased and drawn with this frame rather than the next one.
        (self.before_frame)(&mut self.frame_loop, &mut self.camera, delta.as_secs_f32());

        let (overlay, actions) = self.build_overlay(delta);

        // Remembered before the overlay is handed to the renderer: while the
        // pointer is over a panel, dragging must not also turn the camera.
        self.ui_wants_pointer = overlay.wants_pointer;

        // The overlay's buttons and the keyboard produce the same actions, so
        // there is nothing to translate.
        for action in actions {
            if self.handle(action) {
                self.quit_requested = true;
            }
        }

        // The camera moves in real seconds, not ticks: it is view state, below
        // the firewall, and pinning it to 30 Hz would make it stutter on a
        // 120 Hz display for no reason.
        self.camera.advance(self.held.intent(), delta.as_secs_f32());

        // Follow the camera with the extract origin, which is the whole point of
        // having a floating origin: whatever is near the eye keeps a small local
        // offset and therefore its precision.
        self.frame_loop.set_origin(self.camera.origin());

        // Built against the origin the frame is about to extract with, so the
        // camera and the world end up in the same space.
        let camera = self.camera.camera(self.frame_loop.origin());

        let Some(active) = self.active.as_mut() else {
            return Err(AppError::NoWindow);
        };

        let report = self.frame_loop.present(
            &mut self.world,
            &mut self.schedule,
            &mut active.surface,
            &camera,
            Some(overlay),
            delta,
        )?;

        Ok(report)
    }

    /// Builds the overlay for this frame.
    ///
    /// Reads the *previous* frame's report, because this runs before the current
    /// one exists. At 120 Hz that is 8 ms of lag on a number nobody reads that
    /// precisely, and the alternative is running the UI after the draw and
    /// showing it a frame late anyway.
    fn build_overlay(&mut self, delta: Fixed) -> (cx_ui::UiOutput, Vec<Action>) {
        self.overlay.record_frame(delta.as_secs_f32() * 1_000.0);

        let (size, scale) = match self.active.as_ref() {
            Some(active) => {
                let size = active.window.inner_size();
                let scale = active.window.scale_factor() as f32;
                (
                    [size.width as f32 / scale, size.height as f32 / scale],
                    scale,
                )
            }
            None => ([1.0, 1.0], 1.0),
        };

        let state = OverlayState {
            tick: self.frame_loop.tick().0,
            alpha: self.last_report.alpha,
            control: self.frame_loop.control(),
            instances: self.last_report.extracted,
            draw_calls: self.last_report.draw.map_or(0, |stats| stats.draw_calls),
            debug_lines: self.last_report.debug.lines,
            skipped: self.skipped_since_report,
            fell_behind: self.last_report.fell_behind,
            device: self.frame_loop.device().info().summary(),
            camera: format!(
                "camera {:?} {:.1}, {:.1}, {:.1}",
                self.camera.origin(),
                self.camera.position.local.x,
                self.camera.position.local.y,
                self.camera.position.local.z
            ),
            labels: Vec::new(),
        };

        self.overlay.run(
            UiInput {
                size,
                scale,
                pointer: self.pointer,
                pointer_down: self.pointer_down,
                delta_seconds: delta.as_secs_f32(),
            },
            &state,
        )
    }

    /// Applies a key press.
    ///
    /// Returns whether the run should end.
    fn handle(&mut self, action: Action) -> bool {
        match respond(self.frame_loop.control(), action) {
            Response::Quit => {
                tracing::info!("quit requested");
                true
            }
            Response::Time(control) => {
                if control != self.frame_loop.control() {
                    tracing::debug!(?control, "time control changed");
                    self.frame_loop.set_control(control);
                }
                false
            }
        }
    }

    /// Records a failure and asks the loop to stop.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: AppError) {
        tracing::error!(%error, "ending the run");
        self.failure = Some(error);
        event_loop.exit();
    }
}

impl<F: FnMut(&mut FrameLoop, &mut FlyCamera, f32)> ApplicationHandler for WindowedApp<F> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Called again after a suspend on some platforms, so this must not
        // create a second window.
        if self.active.is_some() {
            return;
        }

        match self.activate(event_loop) {
            Ok(active) => {
                active.window.request_redraw();
                self.active = Some(active);
                // Reset here rather than at construction: the gap between
                // building the loop and the window appearing is not simulated
                // time.
                self.last_frame = None;
            }
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("window closed");
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(active) = self.active.as_mut() {
                    self.frame_loop
                        .resize_surface(&mut active.surface, size.width, size.height);
                }
            }

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        repeat: false,
                        ..
                    },
                ..
            } => {
                let pressed = state == ElementState::Pressed;

                if let Some(axis) = axis_for(code) {
                    self.held.set(axis, pressed);
                } else if pressed
                    && let Some(action) = action_for(code)
                    && self.handle(action)
                {
                    event_loop.exit();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                match button {
                    MouseButton::Right => self.looking = pressed,
                    MouseButton::Left => self.pointer_down = pressed,
                    _ => {}
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let scale = self
                    .active
                    .as_ref()
                    .map_or(1.0, |active| active.window.scale_factor());
                self.pointer = Some([(position.x / scale) as f32, (position.y / scale) as f32]);
            }

            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.pointer_down = false;
            }

            // Losing focus releases everything. Key-up is delivered to whoever
            // has focus, so without this, alt-tabbing mid-movement leaves the
            // camera flying with no way to stop it.
            WindowEvent::Focused(false) => {
                self.held = Held::default();
                self.looking = false;
                self.pointer_down = false;
            }

            WindowEvent::RedrawRequested => {
                match self.draw() {
                    Ok(report) => {
                        self.last_report = report;

                        if self.quit_requested {
                            event_loop.exit();
                        }

                        // Back off when nothing is being shown, rather than
                        // spinning. `Poll` is right while frames are landing and
                        // wrong the moment they stop.
                        self.next_frame_at = next_frame_at(&report);
                        event_loop.set_control_flow(pacing_for(self.next_frame_at));

                        self.report_progress(&report);
                    }
                    Err(error) => self.fail(event_loop, error),
                }

                // The next frame is requested from `about_to_wait`, not here.
                // Requesting it here happens *before* the control flow set above
                // takes effect, so the window redraws immediately and the
                // back-off never applies — which is exactly what an occluded
                // window did: 4,230 frames a second against a 16 ms wait.
            }

            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        // Raw motion rather than cursor position, so looking is not bounded by
        // the edges of the screen.
        if let DeviceEvent::MouseMotion { delta } = event
            && self.looking
            && !self.ui_wants_pointer
        {
            let (dx, dy) = delta;
            self.camera.look(LookIntent {
                // Moving the mouse right should turn right, and positive yaw
                // turns left, hence the negation.
                yaw: -dx as f32 * LOOK_SENSITIVITY,
                pitch: -dy as f32 * LOOK_SENSITIVITY,
            });
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // `request_redraw` wakes the loop immediately, so calling it
        // unconditionally defeats `WaitUntil` entirely — an occluded window ran
        // at 4,300 frames a second against a 16 ms wait until this check
        // existed. Ask only once the deadline has actually arrived.
        if !deadline_reached(self.next_frame_at) {
            return;
        }

        if let Some(active) = self.active.as_ref() {
            active.window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Outstanding GPU work holds resources the surface is about to drop.
        self.frame_loop.device().wait_for_idle();
        tracing::info!(tick = self.frame_loop.tick().0, "run ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opening a window needs a display server, so what is checked here is the
    /// binding table — the part that is data, and the part that silently rots
    /// when a key is rebound and its neighbour is left pointing at the same one.
    #[test]
    fn every_binding_is_distinct() {
        for (index, (key, action)) in BINDINGS.iter().enumerate() {
            for (other_key, other_action) in BINDINGS.iter().skip(index + 1) {
                assert_ne!(key, other_key, "{key:?} is bound twice");
                assert_ne!(
                    action, other_action,
                    "{action:?} is reachable from two keys"
                );
            }
        }
    }

    #[test]
    fn every_action_is_bound_to_something() {
        // A control S03 asks for but nothing can reach is a control that does
        // not exist.
        for action in [
            Action::Quit,
            Action::TogglePause,
            Action::Step,
            Action::Faster,
            Action::Slower,
            Action::NormalSpeed,
        ] {
            assert!(
                BINDINGS.iter().any(|(_, bound)| *bound == action),
                "{action:?} has no key"
            );
        }
    }

    #[test]
    fn unbound_keys_do_nothing() {
        assert_eq!(action_for(KeyCode::Escape), Some(Action::Quit));
        assert_eq!(action_for(KeyCode::KeyM), None);
        assert_eq!(axis_for(KeyCode::KeyM), None);
    }

    #[test]
    fn movement_and_action_keys_do_not_overlap() {
        // A key in both tables would move the camera *and* fire an action. `Q`
        // was in both at one point, as descend and as an unused quit binding.
        for (key, axis) in MOVEMENT {
            assert!(
                action_for(*key).is_none(),
                "{key:?} is bound to both {axis:?} and an action"
            );
        }
    }

    #[test]
    fn every_movement_axis_is_bound() {
        for axis in [
            Axis::Forward,
            Axis::Back,
            Axis::Left,
            Axis::Right,
            Axis::Up,
            Axis::Down,
            Axis::Boost,
        ] {
            assert!(
                MOVEMENT.iter().any(|(_, bound)| *bound == axis),
                "{axis:?} has no key"
            );
        }
    }

    #[test]
    fn opposite_keys_cancel() {
        let mut held = Held::default();
        held.set(Axis::Forward, true);
        assert_eq!(held.intent().forward, 1.0);

        held.set(Axis::Back, true);
        assert!(
            held.intent().is_still(),
            "holding both directions should stand still, not pick one"
        );

        held.set(Axis::Forward, false);
        assert_eq!(held.intent().forward, -1.0);
    }

    #[test]
    fn releasing_everything_stops_the_camera() {
        // What `Focused(false)` relies on: a default `Held` must be motionless.
        let mut held = Held::default();
        for axis in [Axis::Forward, Axis::Right, Axis::Up, Axis::Boost] {
            held.set(axis, true);
        }
        assert!(!held.intent().is_still());
        assert!(Held::default().intent().is_still());
    }

    fn report_with(skipped: Option<cx_render::SkipReason>) -> FrameReport {
        FrameReport {
            ticks: 1,
            skipped,
            ..FrameReport::default()
        }
    }

    /// An occluded window stays occluded until someone moves it, so asking again
    /// immediately burns a core to draw nothing — 14,000 frames a second,
    /// measured, before this existed.
    #[test]
    fn an_occluded_window_makes_the_loop_wait() {
        let deadline = next_frame_at(&report_with(Some(cx_render::SkipReason::Occluded)));
        assert!(
            deadline.is_some(),
            "an occluded frame should set a deadline"
        );
        assert!(
            matches!(pacing_for(deadline), ControlFlow::WaitUntil(_)),
            "and the loop should wait for it"
        );
        assert!(
            !deadline_reached(deadline),
            "the deadline is in the future, so no frame should be asked for yet"
        );
    }

    /// The half that was missing: a deadline is worthless if the loop asks for a
    /// redraw anyway, because `request_redraw` wakes the loop immediately.
    #[allow(
        clippy::disallowed_methods,
        reason = "constructing a deadline to test against needs a reference instant"
    )]
    #[test]
    fn no_frame_is_requested_before_the_deadline() {
        assert!(
            deadline_reached(None),
            "with no deadline, every wake-up should ask for a frame"
        );
        assert!(
            !deadline_reached(Some(Instant::now() + Duration::from_secs(60))),
            "a future deadline must suppress the request"
        );
        assert!(
            deadline_reached(Some(Instant::now() - Duration::from_secs(1))),
            "a past deadline must let it through"
        );
    }

    /// The transient reasons must *not* back off: a lost or outdated surface has
    /// just been reconfigured and the next frame is expected to work, so waiting
    /// would add 16 ms of stutter to every window resize.
    #[test]
    fn transient_skips_retry_immediately() {
        for reason in [
            cx_render::SkipReason::Lost,
            cx_render::SkipReason::Outdated,
            cx_render::SkipReason::Timeout,
        ] {
            assert_eq!(
                next_frame_at(&report_with(Some(reason))),
                None,
                "{reason:?} should retry at once"
            );
            assert!(!reason.is_persistent());
        }
        assert!(cx_render::SkipReason::Occluded.is_persistent());
    }

    #[test]
    fn a_drawn_frame_keeps_polling() {
        assert_eq!(next_frame_at(&report_with(None)), None);
        assert_eq!(pacing_for(None), ControlFlow::Poll);
    }
}
