//! Implements S16/S03 — main loop assembly.
//!
//! Below the firewall. This is where the sim world, the tick clock, the view
//! world, and the renderer are finally composed into something that runs a
//! frame.
//!
//! # The loop, in order
//!
//! 1. Feed real elapsed time to the clock, which decides how many ticks are due
//!    and runs them (`cx-time`).
//! 2. Ask the clock for `alpha`, the fraction of the way through the current
//!    tick.
//! 3. Extract the visual subset of sim state, interpolated by `alpha`, rebased
//!    against the origin chunk (`cx-view`).
//! 4. Draw the extracted instances (`cx-render`).
//!
//! Steps 3 and 4 are skipped entirely in a headless run — that is all "headless"
//! means (`ADR-0002`). Nothing in steps 1 and 2 knows the others exist.
//!
//! # Why this composes offscreen
//!
//! [`FrameLoop`] targets a texture, not a window. Everything about the loop that
//! can be wrong — ticks per frame, interpolation factor, instances extracted,
//! draw calls issued — is observable without a display server, so it can be
//! tested and measured in CI. A window is a surface to present the same frame
//! to, and adds no logic worth hiding from tests.

use cx_core::Fixed;
use cx_core::math::ChunkCoord;
use cx_ecs::{SimSchedule, SimWorld};
use cx_render::{Camera, DrawStats, InstancedRenderer, MeshData, RenderDevice, RenderError};
use cx_time::{CatchUp, PacedDriver, TickRate, TimeControl};
use cx_view::{ViewWorld, extract};

/// What one frame did.
///
/// Everything here is a number a test can assert on, which is the point: a
/// frame loop whose behaviour can only be judged by looking at it is a frame
/// loop that gets debugged by looking at it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameReport {
    /// Ticks the simulation advanced this frame. Often zero at a high frame rate.
    pub ticks: u64,
    /// Interpolation factor the frame was drawn at, in `[0, 1)`.
    pub alpha: f32,
    /// Instances extracted from the sim world.
    pub extracted: usize,
    /// What the draw cost, when the frame drew.
    pub draw: Option<DrawStats>,
    /// Whether simulated time was dropped by the clock's guards.
    pub fell_behind: bool,
}

/// Sim, clock, view, and renderer composed into a frame.
pub struct FrameLoop {
    device: RenderDevice,
    renderer: InstancedRenderer,
    driver: PacedDriver,
    view: ViewWorld,
    origin: ChunkCoord,
    width: u32,
    height: u32,
}

impl std::fmt::Debug for FrameLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameLoop")
            .field("device", self.device.info())
            .field("target", &(self.width, self.height))
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl FrameLoop {
    /// Builds a loop that renders to an offscreen target of the given size.
    ///
    /// `instance_capacity` sizes the view world up front. Extract runs every
    /// frame, and growing that vector during it would put an allocation on the
    /// frame path.
    pub fn offscreen(
        rate: TickRate,
        width: u32,
        height: u32,
        instance_capacity: usize,
    ) -> Result<Self, RenderError> {
        let device = RenderDevice::headless()?;
        let renderer = InstancedRenderer::new(&device, &MeshData::unit_cube())?;

        Ok(Self {
            device,
            renderer,
            driver: PacedDriver::new(rate),
            view: ViewWorld::with_capacity(instance_capacity),
            origin: ChunkCoord::new(0, 0),
            width,
            height,
        })
    }

    /// The chunk positions are rebased against.
    ///
    /// Must match the camera's space: the camera is positioned in extract
    /// coordinates, so a mismatch puts the world a chunk-multiple away from
    /// where the camera is looking.
    pub const fn origin(&self) -> ChunkCoord {
        self.origin
    }

    /// Sets the origin chunk, normally to follow the camera.
    pub const fn set_origin(&mut self, origin: ChunkCoord) {
        self.origin = origin;
    }

    /// Pause, play, or single-step.
    pub const fn set_control(&mut self, control: TimeControl) {
        self.driver.set_control(control);
    }

    /// The tick the simulation has reached.
    pub const fn tick(&self) -> cx_core::Tick {
        self.driver.clock().tick()
    }

    /// The device this loop draws with, for provenance when recording numbers.
    pub const fn device(&self) -> &RenderDevice {
        &self.device
    }

    /// The most recent frame's extracted instances.
    pub fn view(&self) -> &ViewWorld {
        &self.view
    }

    /// Advances the simulation by whatever `real_delta` is due, then draws.
    ///
    /// Submits the draw without waiting for it. Reading pixels back would make
    /// every frame a GPU synchronisation point, which is both wrong for a real
    /// loop and would make a CPU-side measurement meaningless — see
    /// [`FrameLoop::frame_with_readback`] for the testing path.
    pub fn frame(
        &mut self,
        world: &mut SimWorld,
        schedule: &mut SimSchedule,
        camera: &Camera,
        real_delta: Fixed,
    ) -> Result<FrameReport, RenderError> {
        let produced = self.advance(world, schedule, real_delta);
        let stats = self.draw(camera)?;
        Ok(self.report(produced, Some(stats)))
    }

    /// A frame that also reads the pixels back.
    ///
    /// For tests that need to assert what was drawn. The readback is a
    /// synchronisation point, so this is not the path a real loop takes.
    pub fn frame_with_readback(
        &mut self,
        world: &mut SimWorld,
        schedule: &mut SimSchedule,
        camera: &Camera,
        real_delta: Fixed,
    ) -> Result<(FrameReport, cx_render::Readback), RenderError> {
        let produced = self.advance(world, schedule, real_delta);

        let (readback, stats) = self.renderer.render(
            &self.device,
            self.width,
            self.height,
            camera,
            self.view.instances(),
            CLEAR_COLOUR,
        )?;

        Ok((self.report(produced, Some(stats)), readback))
    }

    /// Runs the simulation and extracts, without drawing.
    ///
    /// What a headless run does (`ADR-0002`): the view world is never
    /// constructed at all in a truly headless build, but for measurement it is
    /// useful to separate "simulate and extract" from "draw".
    pub fn frame_without_drawing(
        &mut self,
        world: &mut SimWorld,
        schedule: &mut SimSchedule,
        real_delta: Fixed,
    ) -> FrameReport {
        let produced = self.advance(world, schedule, real_delta);
        self.report(produced, None)
    }

    fn advance(
        &mut self,
        world: &mut SimWorld,
        schedule: &mut SimSchedule,
        real_delta: Fixed,
    ) -> CatchUp {
        let produced = self.driver.frame(world, schedule, real_delta);

        // Extract after the ticks, with the alpha the clock arrived at. Doing it
        // before would interpolate towards a position the sim has not reached.
        extract(
            world,
            &mut self.view,
            self.driver.clock().alpha(),
            self.origin,
        );

        produced
    }

    fn draw(&self, camera: &Camera) -> Result<DrawStats, RenderError> {
        let (_, stats) = self.renderer.render(
            &self.device,
            self.width,
            self.height,
            camera,
            self.view.instances(),
            CLEAR_COLOUR,
        )?;
        Ok(stats)
    }

    fn report(&self, produced: CatchUp, draw: Option<DrawStats>) -> FrameReport {
        FrameReport {
            ticks: produced.ticks,
            alpha: self.view.alpha(),
            extracted: self.view.len(),
            draw,
            fell_behind: produced.fell_behind,
        }
    }
}

/// The colour an empty frame shows.
///
/// A dark blue-grey rather than black: black is also what a failed draw
/// produces, and being able to tell "nothing was drawn" from "the target was
/// never cleared" is worth one arbitrary constant.
const CLEAR_COLOUR: [f32; 4] = [0.05, 0.06, 0.09, 1.0];

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::{Vec3, WorldPos};
    use cx_ecs::{Phase, PreviousTransform, Query, SimWorld, Transform, WorldConfig};
    use cx_render::testing::device_or_skip;

    fn drift(mut query: Query<(&mut Transform, &mut PreviousTransform)>) {
        for (mut transform, mut previous) in query.iter_mut() {
            previous.0 = *transform;
            transform.position = transform.position.offset(Vec3::new(0.1, 0.0, 0.0));
        }
    }

    fn world_with(count: usize) -> (SimWorld, SimSchedule) {
        let mut world = SimWorld::new(WorldConfig::default());
        world.spawn_batch((0..count).map(|i| {
            let at = WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(i as f32, 0.0, 0.0));
            let transform = Transform::from_position(at);
            (transform, PreviousTransform(transform))
        }));

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::AgentAct, drift);

        (world, schedule)
    }

    /// Skips unless a device is available, honouring `CX_REQUIRE_GPU`.
    fn loop_or_skip(width: u32, height: u32) -> Option<FrameLoop> {
        device_or_skip()?;
        Some(
            FrameLoop::offscreen(TickRate::default(), width, height, 1_024)
                .expect("a device was just acquired, so the loop should build"),
        )
    }

    #[test]
    fn a_frame_runs_the_sim_extracts_and_draws() {
        let Some(mut frame_loop) = loop_or_skip(64, 64) else {
            return;
        };
        let (mut world, mut schedule) = world_with(4);
        let camera = Camera::looking_at(Vec3::new(0.0, 4.0, 12.0), Vec3::new(2.0, 0.0, 0.0));

        // One tick's worth of real time at 30 Hz.
        let report = frame_loop
            .frame(
                &mut world,
                &mut schedule,
                &camera,
                Fixed::from_micros(33_333),
            )
            .expect("the frame should render");

        assert_eq!(report.ticks, 1, "one tick of real time should run one tick");
        assert_eq!(
            report.extracted, 4,
            "every entity should have been extracted"
        );
        assert_eq!(report.draw.map(|stats| stats.draw_calls), Some(1));
        assert_eq!(frame_loop.tick().0, 1);
    }

    #[test]
    fn a_fast_frame_rate_runs_no_ticks_but_still_draws() {
        // The case the whole interpolation design exists for: at 144 Hz most
        // frames advance no simulation at all and must still produce a picture,
        // blended between the last two tick states.
        let Some(mut frame_loop) = loop_or_skip(32, 32) else {
            return;
        };
        let (mut world, mut schedule) = world_with(2);
        let camera = Camera::looking_at(Vec3::new(0.0, 2.0, 8.0), Vec3::ZERO);

        // 144 Hz is 6.94 ms; four of those is still under one 30 Hz tick.
        let mut drew_without_ticking = false;
        for _ in 0..4 {
            let report = frame_loop
                .frame(
                    &mut world,
                    &mut schedule,
                    &camera,
                    Fixed::from_micros(6_944),
                )
                .expect("the frame should render");

            if report.ticks == 0 {
                drew_without_ticking = true;
                assert_eq!(report.extracted, 2, "a tickless frame still extracts");
                assert_eq!(report.draw.map(|stats| stats.draw_calls), Some(1));
            }
        }

        assert!(
            drew_without_ticking,
            "at 144 Hz some frames must run zero ticks"
        );
    }

    #[test]
    fn alpha_advances_between_ticks() {
        // If alpha were always 0 or 1, motion would step rather than interpolate
        // and the whole extract contract would be decorative.
        let Some(mut frame_loop) = loop_or_skip(16, 16) else {
            return;
        };
        let (mut world, mut schedule) = world_with(1);

        let mut seen_partial = false;
        for _ in 0..4 {
            let report = frame_loop.frame_without_drawing(
                &mut world,
                &mut schedule,
                Fixed::from_micros(6_944),
            );
            if report.alpha > 0.01 && report.alpha < 0.99 {
                seen_partial = true;
            }
        }

        assert!(
            seen_partial,
            "alpha should land between ticks, not only at the endpoints"
        );
    }

    #[test]
    fn a_paused_loop_draws_the_same_scene_without_advancing() {
        let Some(mut frame_loop) = loop_or_skip(32, 32) else {
            return;
        };
        let (mut world, mut schedule) = world_with(3);
        let camera = Camera::looking_at(Vec3::new(0.0, 2.0, 8.0), Vec3::ZERO);

        frame_loop.set_control(TimeControl::Paused);
        let report = frame_loop
            .frame(
                &mut world,
                &mut schedule,
                &camera,
                Fixed::from_micros(33_333),
            )
            .expect("a paused frame still renders");

        assert_eq!(report.ticks, 0, "paused means no simulation");
        assert_eq!(report.extracted, 3, "but the world is still drawn");
        assert_eq!(frame_loop.tick().0, 0);
    }

    #[test]
    fn what_the_loop_draws_is_visible_in_the_readback() {
        let Some(mut frame_loop) = loop_or_skip(64, 64) else {
            return;
        };
        let (mut world, mut schedule) = world_with(1);
        let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO);

        let (_, readback) = frame_loop
            .frame_with_readback(
                &mut world,
                &mut schedule,
                &camera,
                Fixed::from_micros(33_333),
            )
            .expect("the frame should render");

        let centre = readback.pixel(32, 32).expect("in bounds");
        let corner = readback.pixel(1, 1).expect("in bounds");

        // Absolute byte values are not obvious here: the target is sRGB, so a
        // linear 0.05 clear reads as 63 rather than 13. Assert the relationships
        // instead, which is what the picture actually has to satisfy.
        assert!(
            centre[0] > corner[0] + 60,
            "the lit entity should be clearly brighter than the background: \
             centre {centre:?}, corner {corner:?}"
        );
        assert!(
            corner[2] > corner[0],
            "the clear colour is blue-tinted, so a background pixel should be \
             blue-dominant, got {corner:?}"
        );
        assert_eq!(
            centre[0], centre[2],
            "the lit surface is neutral grey, got {centre:?}"
        );
    }

    #[test]
    fn an_empty_world_still_produces_a_frame() {
        let Some(mut frame_loop) = loop_or_skip(16, 16) else {
            return;
        };
        let mut world = SimWorld::new(WorldConfig::default());
        let mut schedule = SimSchedule::new();
        let camera = Camera::default();

        let report = frame_loop
            .frame(
                &mut world,
                &mut schedule,
                &camera,
                Fixed::from_micros(33_333),
            )
            .expect("an empty world is a legitimate frame");

        assert_eq!(report.extracted, 0);
        assert_eq!(report.draw.map(|stats| stats.draw_calls), Some(0));
    }
}
