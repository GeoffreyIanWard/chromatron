//! The overlay, from tessellation to pixels.
//!
//! `cx-ui` tests what the overlay *says* and `cx-render` tests the texture
//! handling. What is left is whether the two together actually change the
//! picture — which is the question a green run of both halves does not answer,
//! and the one that has now been wrong twice.

use cx_app::FrameLoop;
use cx_core::Fixed;
use cx_ecs::{SimSchedule, SimWorld, WorldConfig};
use cx_render::Camera;
use cx_render::testing::device_or_skip;
use cx_time::TickRate;
use cx_ui::{Overlay, OverlayState, UiInput};

use cx_core::math::Vec3;

const ONE_TICK: Fixed = Fixed::from_micros(33_334);

const SIZE: u32 = 128;

fn empty_world() -> (SimWorld, SimSchedule) {
    (SimWorld::new(WorldConfig::default()), SimSchedule::new())
}

fn ui_input() -> UiInput {
    UiInput {
        size: [SIZE as f32, SIZE as f32],
        scale: 1.0,
        pointer: None,
        pointer_down: false,
        delta_seconds: 1.0 / 60.0,
    }
}

fn state() -> OverlayState {
    OverlayState {
        tick: 1_234,
        alpha: 0.5,
        instances: 400,
        draw_calls: 1,
        device: "test".to_owned(),
        camera: "camera".to_owned(),
        ..OverlayState::default()
    }
}

/// The overlay must change the pixels.
///
/// Not "must produce primitives" — it did that while drawing nothing at all, in
/// two separate ways: once because the texture atlas was never uploaded, and
/// once because a skipped frame threw the atlas away and left `egui` believing
/// it had been allocated.
#[test]
fn the_overlay_changes_what_is_on_screen() {
    let Some(_device) = device_or_skip() else {
        return;
    };

    let mut frame_loop = FrameLoop::offscreen(TickRate::default(), SIZE, SIZE, 16)
        .expect("a device was just acquired");
    let (mut world, mut schedule) = empty_world();
    let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 4.0), Vec3::ZERO);
    let mut overlay = Overlay::new();

    // Frame one builds the font atlas and draws nothing. Feeding it through the
    // renderer is what uploads the atlas, so it cannot be skipped.
    let (atlas_output, _) = overlay.run(ui_input(), &state());
    assert!(
        atlas_output.primitives.is_empty(),
        "the first pass is the atlas, not the panel"
    );
    let (first, bare) = frame_loop
        .frame_with_readback(
            &mut world,
            &mut schedule,
            &camera,
            Some(atlas_output),
            ONE_TICK,
        )
        .expect("the frame should render");
    assert_eq!(first.ui.draw_calls, 0, "nothing to draw on the atlas frame");

    // Frame two has the panel.
    let (panel_output, _) = overlay.run(ui_input(), &state());
    assert!(!panel_output.primitives.is_empty());
    let (second, drawn) = frame_loop
        .frame_with_readback(
            &mut world,
            &mut schedule,
            &camera,
            Some(panel_output),
            ONE_TICK,
        )
        .expect("the frame should render");

    assert_eq!(second.ui.draw_calls, 1, "the panel should have been drawn");
    assert!(second.ui.primitives > 0);

    let mut differing = 0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let before = bare.pixel(x, y).expect("in bounds");
            let after = drawn.pixel(x, y).expect("in bounds");
            if before != after {
                differing += 1;
            }
        }
    }

    assert!(
        differing > 200,
        "the overlay should visibly change the frame; only {differing} pixels differ"
    );
}

/// Many frames in a row keep drawing.
///
/// `egui` sends the font atlas once and everything afterwards as *partial*
/// updates to it, so anything that loses the original allocation shows up as a
/// panic inside `egui-wgpu` on a later frame rather than on the one that caused
/// it. Eight frames is enough for that to surface.
///
/// The skipped-frame version of this hazard lives in `WindowSurface`, which
/// hands a skipped frame's textures to the renderer rather than dropping them —
/// that path needs a swapchain and is verified by running the client.
#[test]
fn the_overlay_keeps_drawing_across_many_frames() {
    let Some(_device) = device_or_skip() else {
        return;
    };

    let mut frame_loop = FrameLoop::offscreen(TickRate::default(), SIZE, SIZE, 16)
        .expect("a device was just acquired");
    let (mut world, mut schedule) = empty_world();
    let camera = Camera::looking_at(Vec3::new(0.0, 0.0, 4.0), Vec3::ZERO);
    let mut overlay = Overlay::new();

    let mut drawn_frames = 0;
    for frame in 0..8 {
        // The state changes every frame, which is what makes egui re-tessellate
        // and issue the partial atlas updates this is checking.
        let (output, _) = overlay.run(
            ui_input(),
            &OverlayState {
                tick: frame * 37,
                ..state()
            },
        );

        let report = frame_loop
            .frame_with_readback(&mut world, &mut schedule, &camera, Some(output), ONE_TICK)
            .expect("the frame should render")
            .0;

        if report.ui.draw_calls > 0 {
            drawn_frames += 1;
        }
    }

    assert_eq!(
        drawn_frames, 7,
        "every frame after the atlas frame should draw the panel"
    );
}
