//! The debug overlay (S14/M1): tick counter, frame graph, time controls, and
//! world-space labels.
//!
//! # The overlay is a function of plain data
//!
//! [`Overlay::run`] takes an [`OverlayState`] — numbers and enums, nothing
//! borrowed from the engine — and returns the [`Action`]s the user asked for.
//! That makes what the overlay *says* and which buttons it offers testable
//! without a window, because `egui` runs perfectly well headlessly. Only the
//! pixels need a display.
//!
//! # The buttons and the keyboard mean the same thing
//!
//! Both produce [`crate::controls::Action`]. There is no second definition of
//! what pause does, so the overlay cannot drift from the key bindings — which is
//! the failure that makes a debug UI stop being trusted.

use crate::controls::Action;
use crate::frame_graph::FrameGraph;
use cx_time::TimeControl;

/// What the overlay shows.
///
/// Plain data, gathered by the caller. Nothing here is borrowed from the sim or
/// the renderer, so the overlay cannot accidentally read engine state at a
/// moment the engine did not choose.
#[derive(Debug, Clone, Default)]
pub struct OverlayState {
    /// Ticks simulated so far.
    pub tick: u64,
    /// Interpolation factor the frame was drawn at.
    pub alpha: f32,
    /// How time is advancing.
    pub control: TimeControl,
    /// Instances extracted this frame.
    pub instances: usize,
    /// Scene draw calls.
    pub draw_calls: u32,
    /// Debug line segments drawn.
    pub debug_lines: u32,
    /// Frames skipped since the last report, and why they might be.
    pub skipped: u32,
    /// Whether the clock dropped simulated time to catch up.
    pub fell_behind: bool,
    /// Device the frame was drawn with, for provenance.
    pub device: String,
    /// Camera position, formatted by the caller — the overlay does not know
    /// about chunk coordinates.
    pub camera: String,
    /// Labels to draw at world positions, already projected to pixels by the
    /// caller. See [`WorldLabel`].
    pub labels: Vec<WorldLabel>,
}

/// A label anchored to a point in the world.
///
/// Projection happens in the caller, which is the only place that has the camera
/// and the viewport. The overlay is handed pixels and a visibility flag, so it
/// never needs a projection matrix and cannot disagree with the one the scene
/// was drawn with.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldLabel {
    /// Screen position in logical pixels.
    pub screen: [f32; 2],
    /// The text.
    pub text: String,
    /// Distance from the camera, in metres. Used to fade distant labels.
    pub distance: f32,
}

/// Beyond this many metres a world label is not drawn.
///
/// Labels that keep drawing into the far distance turn into an unreadable smear
/// exactly when a scene gets big enough to need them.
const LABEL_MAX_DISTANCE: f32 = 250.0;

/// Whether a label at this distance should be drawn, and how opaque.
///
/// Separated out because it is the one piece of label logic with a decision in
/// it, and the fade is the part that looks wrong rather than fails.
pub fn label_opacity(distance: f32) -> Option<f32> {
    if !distance.is_finite() || !(0.0..=LABEL_MAX_DISTANCE).contains(&distance) {
        return None;
    }

    // Fully opaque for the near half, then fading out. A label that starts
    // fading immediately reads as broken rather than as distant.
    let fade_start = LABEL_MAX_DISTANCE * 0.5;
    if distance <= fade_start {
        return Some(1.0);
    }

    let fraction = (distance - fade_start) / (LABEL_MAX_DISTANCE - fade_start);
    Some((1.0 - fraction).clamp(0.0, 1.0))
}

/// Input the overlay needs, independent of the windowing library.
///
/// A deliberately small subset: the overlay is a handful of buttons and some
/// text, and translating every event a window can produce would be a large
/// amount of code for things it does not react to.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UiInput {
    /// Viewport size in logical pixels.
    pub size: [f32; 2],
    /// Ratio of physical to logical pixels.
    pub scale: f32,
    /// Pointer position in logical pixels, if the pointer is over the window.
    pub pointer: Option<[f32; 2]>,
    /// Whether the primary pointer button is down.
    pub pointer_down: bool,
    /// Seconds since the previous frame.
    pub delta_seconds: f32,
}

/// What the overlay produced.
///
/// Carries `egui`'s tessellated output across to `cx-render`, which is the only
/// other crate permitted to see it. `cx-app` passes this along without opening
/// it.
pub struct UiOutput {
    /// Tessellated shapes to draw.
    pub primitives: Vec<egui::ClippedPrimitive>,
    /// Texture uploads and frees for this frame.
    ///
    /// Must be applied by the renderer or cleared via [`UiOutput::discard`].
    /// Dropping it unapplied aborts the process — see that method.
    pub textures: egui::TexturesDelta,
    /// Physical pixels per logical point.
    pub pixels_per_point: f32,
    /// Whether the pointer is over the overlay.
    ///
    /// The caller must not also treat that input as camera control, or clicking
    /// a button turns the view at the same time.
    pub wants_pointer: bool,
}

impl UiOutput {
    /// Throws the frame away entirely, textures included.
    ///
    /// **Only correct when there is no renderer to tell.** `egui` sends its font
    /// atlas once as a full upload and everything afterwards as partial updates
    /// to it, so throwing a delta away leaves the renderer without an allocation
    /// that `egui` believes it made — and the next partial update panics inside
    /// `egui-wgpu`. A caller holding a renderer must hand the frame to it even
    /// when not drawing.
    ///
    /// Dropping a `UiOutput` without calling this is worse still: `epaint`'s
    /// destructor panics on an unhandled delta, and a panic in a destructor
    /// aborts rather than unwinds.
    pub fn discard(mut self) {
        self.textures.clear();
    }
}

impl std::fmt::Debug for UiOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiOutput")
            .field("primitives", &self.primitives.len())
            .field("wants_pointer", &self.wants_pointer)
            .finish_non_exhaustive()
    }
}

/// The debug overlay.
pub struct Overlay {
    context: egui::Context,
    graph: FrameGraph,
    show_graph: bool,
}

impl Default for Overlay {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Overlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Overlay")
            .field("frames", &self.graph.len())
            .finish_non_exhaustive()
    }
}

impl Overlay {
    /// A new overlay with an empty history.
    pub fn new() -> Self {
        Self {
            context: egui::Context::default(),
            graph: FrameGraph::new(),
            show_graph: true,
        }
    }

    /// The frame-time history, for callers that want the numbers directly.
    pub const fn graph(&self) -> &FrameGraph {
        &self.graph
    }

    /// Records a frame time, in milliseconds.
    pub fn record_frame(&mut self, milliseconds: f32) {
        self.graph.push(milliseconds);
    }

    /// Builds one frame of the overlay.
    ///
    /// Returns what to draw and whatever the user asked for. Actions are
    /// returned rather than applied, so the overlay changes nothing by itself
    /// and a caller can log or ignore them.
    ///
    /// **The first call draws nothing.** `egui` lays out text before its font
    /// atlas exists, so the first pass produces a texture upload and no
    /// primitives, and the panel appears on the second frame. That is normal and
    /// invisible at 120 Hz — but it means "no primitives" on a single frame is
    /// not evidence that anything is wrong.
    pub fn run(&mut self, input: UiInput, state: &OverlayState) -> (UiOutput, Vec<Action>) {
        let mut actions = Vec::new();

        let raw = self.raw_input(input);
        // `run_ui` hands over the root `Ui`, not the context; the context is
        // reachable from it and is an `Arc` inside, so cloning is a refcount
        // bump rather than a copy of the UI state.
        let output = self.context.run_ui(raw, |ui| {
            let context = ui.ctx().clone();
            draw_panel(
                &context,
                state,
                &mut self.graph,
                &mut self.show_graph,
                &mut actions,
            );
            draw_labels(&context, state);
        });

        let pixels_per_point = if input.scale > 0.0 { input.scale } else { 1.0 };

        (
            UiOutput {
                primitives: self.context.tessellate(output.shapes, pixels_per_point),
                textures: output.textures_delta,
                pixels_per_point,
                wants_pointer: self.context.egui_wants_pointer_input(),
            },
            actions,
        )
    }

    /// Translates [`UiInput`] into what `egui` expects.
    fn raw_input(&self, input: UiInput) -> egui::RawInput {
        let mut events = Vec::new();

        if let Some([x, y]) = input.pointer {
            events.push(egui::Event::PointerMoved(egui::pos2(x, y)));
            events.push(egui::Event::PointerButton {
                pos: egui::pos2(x, y),
                button: egui::PointerButton::Primary,
                pressed: input.pointer_down,
                modifiers: egui::Modifiers::default(),
            });
        } else {
            events.push(egui::Event::PointerGone);
        }

        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(input.size[0].max(1.0), input.size[1].max(1.0)),
            )),
            events,
            predicted_dt: input.delta_seconds.max(0.0),
            ..egui::RawInput::default()
        }
    }
}

/// The stats panel and its buttons.
fn draw_panel(
    context: &egui::Context,
    state: &OverlayState,
    graph: &mut FrameGraph,
    show_graph: &mut bool,
    actions: &mut Vec<Action>,
) {
    egui::Window::new("chromatron")
        .default_pos([12.0, 12.0])
        .resizable(false)
        .show(context, |ui| {
            ui.label(format!("tick {}", state.tick));
            ui.label(format!("alpha {:.2}", state.alpha));
            ui.label(control_label(state.control));

            if state.fell_behind {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 160, 60),
                    "falling behind: simulated time is being dropped",
                );
            }

            ui.separator();
            ui.label(format!("{} instances", state.instances));
            ui.label(format!(
                "{} draw calls, {} debug lines",
                state.draw_calls, state.debug_lines
            ));
            if state.skipped > 0 {
                ui.label(format!("{} frames skipped", state.skipped));
            }

            ui.separator();
            if let Some(summary) = graph.summary() {
                ui.label(format!(
                    "{:.1} ms median · {:.1} p99 · {:.1} worst",
                    summary.median, summary.p99, summary.worst
                ));
            } else {
                ui.label("no frames recorded yet");
            }

            ui.checkbox(show_graph, "frame graph");
            if *show_graph && !graph.is_empty() {
                draw_graph(ui, graph);
            }

            ui.separator();
            ui.horizontal(|ui| {
                let paused = !state.control.is_running();
                if ui.button(if paused { "play" } else { "pause" }).clicked() {
                    actions.push(Action::TogglePause);
                }
                if ui.button("step").clicked() {
                    actions.push(Action::Step);
                }
                if ui.button("<<").clicked() {
                    actions.push(Action::Slower);
                }
                if ui.button("1x").clicked() {
                    actions.push(Action::NormalSpeed);
                }
                if ui.button(">>").clicked() {
                    actions.push(Action::Faster);
                }
            });

            ui.separator();
            ui.label(&state.camera);
            ui.weak(&state.device);
        });
}

/// The frame graph itself, drawn as bars.
fn draw_graph(ui: &mut egui::Ui, graph: &FrameGraph) {
    let samples = graph.ordered();
    let height = 48.0;
    let (response, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width().min(240.0), height),
        egui::Sense::hover(),
    );

    let rect = response.rect;
    painter.rect_filled(rect, 2.0, egui::Color32::from_black_alpha(120));

    // Scaled to the worst sample rather than to a fixed budget: a graph pinned
    // to 16 ms clips exactly the spikes it exists to show.
    let worst = samples.iter().copied().fold(1.0_f32, f32::max);
    let width = rect.width() / samples.len().max(1) as f32;

    for (index, sample) in samples.iter().enumerate() {
        let fraction = (sample / worst).clamp(0.0, 1.0);
        let bar_height = fraction * rect.height();
        let x = rect.left() + index as f32 * width;

        // Green under 8 ms, amber under 16, red beyond: the two thresholds that
        // matter at 120 Hz and 60 Hz.
        let colour = if *sample < 8.0 {
            egui::Color32::from_rgb(80, 200, 120)
        } else if *sample < 16.0 {
            egui::Color32::from_rgb(230, 190, 80)
        } else {
            egui::Color32::from_rgb(230, 90, 90)
        };

        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x, rect.bottom() - bar_height),
                egui::pos2(x + width.max(1.0), rect.bottom()),
            ),
            0.0,
            colour,
        );
    }
}

/// World-space labels, drawn at the screen positions the caller projected.
fn draw_labels(context: &egui::Context, state: &OverlayState) {
    if state.labels.is_empty() {
        return;
    }

    let painter = context.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("cx-ui world labels"),
    ));

    for label in &state.labels {
        let Some(opacity) = label_opacity(label.distance) else {
            continue;
        };

        let alpha = (opacity * 255.0) as u8;
        painter.text(
            egui::pos2(label.screen[0], label.screen[1]),
            egui::Align2::CENTER_BOTTOM,
            &label.text,
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
        );
    }
}

/// How the current control state reads.
fn control_label(control: TimeControl) -> String {
    match control {
        TimeControl::Paused => "paused".to_owned(),
        TimeControl::Playing { multiplier } => format!("playing {multiplier}x"),
        TimeControl::Stepping { remaining } => format!("stepping, {remaining} left"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> UiInput {
        UiInput {
            size: [800.0, 600.0],
            scale: 1.0,
            pointer: None,
            pointer_down: false,
            delta_seconds: 1.0 / 60.0,
        }
    }

    /// Runs the overlay and returns the primitive count, discarding the rest.
    ///
    /// Discarding matters: dropping a `UiOutput` with an unapplied texture delta
    /// aborts the process, so a test that ignored the result would take the
    /// whole run down with it.
    fn primitives(overlay: &mut Overlay, state: &OverlayState) -> usize {
        let (output, _) = overlay.run(input(), state);
        let count = output.primitives.len();
        output.discard();
        count
    }

    #[test]
    fn the_overlay_produces_something_to_draw() {
        // egui runs headlessly, so "does the overlay actually build" is a CI
        // question rather than a display-server one.
        let mut overlay = Overlay::new();
        overlay.record_frame(8.0);

        // The first pass measures text before the font atlas exists and draws
        // nothing — asserting on it would have made this test either wrong or
        // vacuous depending on which way round it was written.
        let first = primitives(&mut overlay, &OverlayState::default());
        assert_eq!(first, 0, "the first pass builds the font atlas");

        let second = primitives(&mut overlay, &OverlayState::default());
        assert!(
            second > 0,
            "the panel should tessellate to something by the second frame"
        );
    }

    #[test]
    fn the_font_atlas_arrives_exactly_once() {
        // A delta on every frame would mean re-uploading the atlas 120 times a
        // second, which is invisible except as a mysteriously busy GPU.
        let mut overlay = Overlay::new();

        let (first, _) = overlay.run(input(), &OverlayState::default());
        assert_eq!(first.textures.set.len(), 1, "the atlas should arrive once");
        first.discard();

        for _ in 0..3 {
            let (later, _) = overlay.run(input(), &OverlayState::default());
            assert!(
                later.textures.set.is_empty(),
                "the atlas should not be re-uploaded every frame"
            );
            later.discard();
        }
    }

    #[test]
    fn a_discarded_frame_does_not_take_the_process_with_it() {
        // egui's TexturesDelta panics in its destructor when dropped unapplied,
        // and a panic in a destructor aborts. An occluded window skipping a
        // frame must not be a crash.
        let mut overlay = Overlay::new();
        let (output, _) = overlay.run(input(), &OverlayState::default());
        output.discard();
    }

    #[test]
    fn a_zero_scale_does_not_produce_a_degenerate_frame() {
        // A window can report a scale of zero mid-resize, and dividing by it
        // yields a tessellation full of infinities that the renderer then tries
        // to draw.
        let mut overlay = Overlay::new();
        let (output, _) = overlay.run(
            UiInput {
                scale: 0.0,
                ..input()
            },
            &OverlayState::default(),
        );
        assert!((output.pixels_per_point - 1.0).abs() < f32::EPSILON);
        output.discard();
    }

    #[test]
    fn a_zero_sized_viewport_is_survivable() {
        let mut overlay = Overlay::new();
        let (output, _) = overlay.run(
            UiInput {
                size: [0.0, 0.0],
                ..input()
            },
            &OverlayState::default(),
        );
        assert!(output.pixels_per_point > 0.0);
        output.discard();
    }

    #[test]
    fn every_control_state_has_a_reading() {
        // A control state with no label leaves the overlay showing nothing where
        // the most important line should be.
        for control in [
            TimeControl::Paused,
            TimeControl::Playing { multiplier: 1.0 },
            TimeControl::Playing { multiplier: 8.0 },
            TimeControl::Stepping { remaining: 3 },
        ] {
            let label = control_label(control);
            assert!(!label.is_empty(), "{control:?} has no label");
        }

        assert_eq!(control_label(TimeControl::Paused), "paused");
        assert!(control_label(TimeControl::Playing { multiplier: 4.0 }).contains('4'));
    }

    #[test]
    fn labels_fade_with_distance_and_stop_entirely() {
        assert_eq!(label_opacity(0.0), Some(1.0));
        assert_eq!(label_opacity(100.0), Some(1.0));

        let mid = label_opacity(200.0).expect("still within range");
        assert!(
            mid > 0.0 && mid < 1.0,
            "a distant label should be partly faded, got {mid}"
        );

        assert_eq!(label_opacity(251.0), None, "beyond the limit, nothing");
        assert_eq!(label_opacity(f32::NAN), None);
        assert_eq!(label_opacity(-1.0), None);
    }

    #[test]
    fn the_fade_is_monotonic() {
        // A fade that brightens again partway out reads as flicker.
        let mut previous = 1.1;
        for step in 0..=25 {
            let distance = step as f32 * 10.0;
            let opacity = label_opacity(distance).unwrap_or(0.0);
            assert!(
                opacity <= previous + 1e-6,
                "opacity rose at {distance} m: {previous} then {opacity}"
            );
            previous = opacity;
        }
    }

    #[test]
    fn world_labels_reach_the_tessellated_output() {
        // The check that the label path is wired at all: text produces shapes,
        // so a state with labels must tessellate to more than one without.
        let mut overlay = Overlay::new();
        // Past the font-atlas frame first, or both counts are zero and the
        // comparison passes without meaning anything.
        primitives(&mut overlay, &OverlayState::default());

        let bare = primitives(&mut overlay, &OverlayState::default());
        let labelled = primitives(
            &mut overlay,
            &OverlayState {
                labels: vec![WorldLabel {
                    screen: [400.0, 300.0],
                    text: "entity 42".to_owned(),
                    distance: 10.0,
                }],
                ..OverlayState::default()
            },
        );

        assert!(bare > 0, "the panel should be drawing by now");
        assert!(
            labelled > bare,
            "a label should add a layer to draw: {bare} without, {labelled} with"
        );
    }

    #[test]
    fn a_label_beyond_the_limit_is_not_drawn() {
        let mut overlay = Overlay::new();
        primitives(&mut overlay, &OverlayState::default());

        let bare = primitives(&mut overlay, &OverlayState::default());
        let far = primitives(
            &mut overlay,
            &OverlayState {
                labels: vec![WorldLabel {
                    screen: [400.0, 300.0],
                    text: "far away".to_owned(),
                    distance: 10_000.0,
                }],
                ..OverlayState::default()
            },
        );

        assert_eq!(
            far, bare,
            "a label past the distance limit should add nothing to draw"
        );
    }

    #[test]
    fn frame_times_recorded_through_the_overlay_reach_the_graph() {
        let mut overlay = Overlay::new();
        for value in [4.0, 9.0, 20.0] {
            overlay.record_frame(value);
        }

        let summary = overlay.graph().summary().expect("frames were recorded");
        assert_eq!(summary.frames, 3);
        assert!((summary.worst - 20.0).abs() < f32::EPSILON);
    }
}
