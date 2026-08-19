//! M1's two state-equivalence exit criteria.
//!
//! | Criterion | Target |
//! |---|---|
//! | Headless vs windowed state hash, 10,000 ticks | identical |
//! | Pause → step 5 → resume vs run 5 continuously | identical state |
//!
//! Both are the same claim from two directions: **the simulation's state is a
//! function of its ticks and nothing else.** Not of whether anything is
//! watching, not of how the ticks were requested, not of how many frames were
//! drawn between them. `ADR-0002` puts the sim above the firewall and the view
//! below it precisely so this holds, and `ADR-0004`'s determinism guarantee is
//! built on top of it.
//!
//! # Why these are worth testing rather than assuming
//!
//! Both failure modes are silent. An extract that wrote back to sim state would
//! produce a game that plays differently in a window than it replays headless —
//! the same seed and inputs diverging with no error, discovered whenever a
//! replay or a save is first compared. Stepping is the same bug with a shorter
//! fuse: a debugging tool that changes what it is used to observe.
//!
//! # A note on scale
//!
//! The windowed comparison draws at 32x32 with 64 entities rather than at a
//! realistic size. Resolution and entity count are irrelevant to what is being
//! asked — whether *drawing at all* perturbs sim state — and a full-size run of
//! 10,000 frames takes minutes on the software rasterizers CI uses. Small and
//! actually run beats realistic and skipped.

use cx_app::FrameLoop;
use cx_core::Fixed;
use cx_core::math::{ChunkCoord, Quat, Vec3, WorldPos};
use cx_diag::{StateHash, StateHasher};
use cx_ecs::{Phase, PreviousTransform, Query, SimSchedule, SimWorld, Transform, WorldConfig};
use cx_render::testing::device_or_skip;
use cx_time::{PacedDriver, TickRate, TimeControl};

/// Ticks the headless/windowed comparison runs.
const TICKS: u64 = 10_000;

/// Entities in the scene.
const ENTITIES: usize = 64;

/// One tick of real time at 30 Hz, so one frame produces exactly one tick.
const ONE_TICK: Fixed = Fixed::from_micros(33_334);

/// Moves everything, so state actually changes tick to tick.
///
/// A scene that never moved would make both criteria pass trivially: identical
/// hashes prove nothing when nothing could have differed.
fn drift(mut query: Query<&mut Transform>) {
    for mut transform in query.iter_mut() {
        transform.position = transform.position.offset(Vec3::new(0.25, 0.0, 0.125));
        transform.rotation = Quat::from_rotation_y(0.02) * transform.rotation;
    }
}

fn scene() -> (SimWorld, SimSchedule) {
    let mut world = SimWorld::new(WorldConfig::default());
    world.spawn_batch((0..ENTITIES).map(|index| {
        let at = WorldPos::new(
            ChunkCoord::new(0, 0),
            Vec3::new(index as f32 * 1.5, 0.0, index as f32 * 0.5),
        );
        let transform = Transform::from_position(at);
        (transform, PreviousTransform(transform))
    }));

    let mut schedule = SimSchedule::new();
    schedule.add_system(Phase::AgentAct, drift);

    (world, schedule)
}

/// The hasher both sides of every comparison use.
///
/// `PreviousTransform` is registered alongside `Transform` deliberately: two
/// runs agreeing on position while disagreeing on the previous position would
/// render differently and hash identically.
fn hasher() -> StateHasher {
    let mut hasher = StateHasher::new(0);
    hasher.register_component::<Transform>("Transform");
    hasher.register_component::<PreviousTransform>("PreviousTransform");
    hasher
}

fn hash(hasher: &StateHasher, world: &mut SimWorld) -> StateHash {
    hasher.hash_world(world)
}

/// **Exit criterion: headless vs windowed state hash, 10,000 ticks, identical.**
///
/// The headless run ticks the schedule directly. The windowed run drives the
/// same ticks through the full frame loop — extract and draw included — and the
/// two must agree at every tick, not merely at the end. A divergence at tick
/// 4,000 that happens to resolve by tick 10,000 is still a divergence.
#[test]
fn drawing_the_world_does_not_change_it() {
    let Some(_device) = device_or_skip() else {
        return;
    };

    let mut frame_loop = FrameLoop::offscreen(TickRate::default(), 32, 32, ENTITIES)
        .expect("a device was just acquired");
    let camera = cx_render::Camera::looking_at(Vec3::new(0.0, 20.0, 40.0), Vec3::ZERO);

    let hasher = hasher();
    let (mut headless_world, mut headless_schedule) = scene();
    let (mut drawn_world, mut drawn_schedule) = scene();

    assert_eq!(
        hash(&hasher, &mut headless_world),
        hash(&hasher, &mut drawn_world),
        "the two scenes must start identical, or the comparison means nothing"
    );

    for tick in 0..TICKS {
        headless_schedule.run(&mut headless_world);

        let report = frame_loop
            .frame(&mut drawn_world, &mut drawn_schedule, &camera, ONE_TICK)
            .expect("the frame should render");

        // The windowed side must actually be doing the work. Without this the
        // test would still pass if drawing quietly stopped happening, which is
        // the failure mode that makes a green run meaningless.
        assert_eq!(
            report.ticks, 1,
            "one tick of real time should run exactly one tick, at tick {tick}"
        );
        assert_eq!(
            report.extracted, ENTITIES,
            "every entity should have been extracted at tick {tick}"
        );
        assert_eq!(
            report.draw.map(|stats| stats.draw_calls),
            Some(1),
            "the frame should have drawn at tick {tick}"
        );

        assert_eq!(
            hash(&hasher, &mut headless_world),
            hash(&hasher, &mut drawn_world),
            "headless and windowed state diverged at tick {tick}. Extract reads sim state \
             and must never write it (ADR-0002) — a divergence here means the view world \
             is feeding back into the simulation."
        );
    }
}

/// **Exit criterion: pause → step 5 → resume vs run 5 continuously, identical.**
///
/// Uses [`PacedDriver`] rather than [`FrameLoop`] because the claim is entirely
/// about how ticks are *requested*, and a `FrameLoop` would drag in a graphics
/// device the question does not involve. This runs on any machine, adapter or
/// not.
#[test]
fn stepping_through_five_ticks_lands_where_running_them_does() {
    let hasher = hasher();
    let (mut continuous_world, mut continuous_schedule) = scene();
    let (mut stepped_world, mut stepped_schedule) = scene();

    let mut continuous = PacedDriver::new(TickRate::default());
    let mut stepped = PacedDriver::new(TickRate::default());

    // Five ticks in a row, at 1x.
    for _ in 0..5 {
        continuous.frame(&mut continuous_world, &mut continuous_schedule, ONE_TICK);
    }

    // The same five, one at a time, from a pause. Each step is followed by a
    // frame that advances no simulation — which is what actually happens while
    // someone looks at the paused frame before pressing step again, and is where
    // an extra tick would leak in.
    stepped.set_control(TimeControl::Paused);
    for step in 0..5 {
        stepped.set_control(TimeControl::Stepping { remaining: 1 });
        let report = stepped.frame(&mut stepped_world, &mut stepped_schedule, ONE_TICK);
        assert_eq!(
            report.ticks, 1,
            "step {step} should advance exactly one tick"
        );

        assert_eq!(
            stepped.control(),
            TimeControl::Paused,
            "a finished step run should retire to paused on its own"
        );

        let idle = stepped.frame(&mut stepped_world, &mut stepped_schedule, ONE_TICK);
        assert_eq!(
            idle.ticks, 0,
            "a paused frame after step {step} must not advance the simulation"
        );
    }

    assert_eq!(
        continuous.clock().tick(),
        stepped.clock().tick(),
        "both routes should have run five ticks"
    );
    assert_eq!(
        hash(&hasher, &mut continuous_world),
        hash(&hasher, &mut stepped_world),
        "stepping through five ticks must land in the same state as running five. \
         A debugging tool that changes what it is used to observe is worse than none."
    );

    // And resuming afterwards continues normally rather than replaying or
    // discharging the time banked while paused.
    stepped.set_control(TimeControl::default());
    let resumed = stepped.frame(&mut stepped_world, &mut stepped_schedule, ONE_TICK);
    assert_eq!(
        resumed.ticks, 1,
        "resuming should tick once, not release time banked while paused"
    );

    continuous.frame(&mut continuous_world, &mut continuous_schedule, ONE_TICK);
    assert_eq!(
        hash(&hasher, &mut continuous_world),
        hash(&hasher, &mut stepped_world),
        "the two should stay in agreement after resuming"
    );
}

/// The comparison above is only as good as the hash under it.
///
/// If `hash_world` returned a constant, or ignored the components registered,
/// both criteria would pass against any implementation at all. This is the
/// control: a state that genuinely differs must hash differently.
#[test]
fn the_hash_actually_distinguishes_states() {
    let hasher = hasher();
    let (mut world, mut schedule) = scene();
    let (mut other, _) = scene();

    let before = hash(&hasher, &mut world);
    assert_eq!(
        before,
        hash(&hasher, &mut other),
        "identical scenes should hash identically"
    );

    schedule.run(&mut world);
    let after = hash(&hasher, &mut world);
    assert_ne!(before, after, "one tick of movement must change the hash");

    // And specifically: a difference in PreviousTransform alone is visible.
    // Without this, registering only Transform would look correct here while
    // leaving interpolation state unchecked in the criteria above.
    let (mut only_previous_differs, _) = scene();
    {
        let mut query = only_previous_differs
            .inner_mut()
            .query::<&mut PreviousTransform>();
        let inner = only_previous_differs.inner_mut();
        for mut previous in query.iter_mut(inner) {
            previous.0.position = previous.0.position.offset(Vec3::new(1.0, 0.0, 0.0));
        }
    }
    assert_ne!(
        hash(&hasher, &mut only_previous_differs),
        hash(&hasher, &mut other),
        "a difference confined to PreviousTransform must still change the hash"
    );
}
