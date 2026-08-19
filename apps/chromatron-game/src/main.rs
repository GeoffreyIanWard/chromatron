//! Windowed client.
//!
//! Assembles a sim world and hands it to the windowed driver (S03). The scene is
//! placeholder content — a grid of orbiting cubes — chosen for what it proves
//! rather than for what it looks like: a 30 Hz simulation drawn at the display's
//! rate, with motion smooth enough to show that interpolation works. That is
//! M1's real exit criterion; drawing cubes is the easy part.
//!
//! # Controls
//!
//! `Esc` quit · `Space` pause · `.` step one tick · `[` slower · `]` faster ·
//! `\` normal speed.

use cx_app::{WindowConfig, run};
use cx_core::math::{ChunkCoord, Vec3, WorldPos};
use cx_ecs::{Phase, PreviousTransform, Query, SimSchedule, SimWorld, Transform, WorldConfig};
use cx_render::Camera;

/// Cubes per side of the placeholder grid.
const GRID: i32 = 20;

/// Metres between cubes.
const SPACING: f32 = 2.5;

/// Radians each cube turns about its own axis per tick.
///
/// At 30 Hz this is a revolution every four seconds — fast enough that stepping
/// would be obvious if rotation interpolation were broken, which is the point of
/// having it move at all.
const SPIN_PER_TICK: f32 = 0.05;

/// Radians the whole grid orbits the origin per tick.
///
/// Slower than the spin because it applies at a radius: at the edge of the grid
/// this is around 10 m/s, which is brisk enough to make stepping obvious without
/// making the scene unreadable.
const ORBIT_PER_TICK: f32 = 0.01;

/// What the grid orbits.
fn centre() -> WorldPos {
    WorldPos::new(ChunkCoord::new(0, 0), Vec3::ZERO)
}

/// Turns every cube a fixed amount.
///
/// Deliberately not time-based: a tick is a fixed quantum of simulated time
/// (`ADR-0004`), so a per-tick constant *is* a rate, and reading a clock here
/// would break determinism to express the same thing.
fn spin(mut query: Query<&mut Transform>) {
    for mut transform in query.iter_mut() {
        transform.rotation =
            cx_core::math::Quat::from_rotation_y(SPIN_PER_TICK) * transform.rotation;
    }
}

/// Orbits every cube around the origin.
///
/// Translation, not just rotation — position and orientation interpolate by
/// different means (`lerp` through the chunk-relative difference, `slerp`), so a
/// scene that only spun in place would leave half of that untested.
///
/// The offset is taken relative to [`centre`] rather than from the local
/// coordinate directly: local offsets are chunk-relative and wrap, so rotating
/// one of those would send cubes to entirely the wrong place as they cross a
/// chunk boundary.
fn orbit(mut query: Query<&mut Transform>) {
    let turn = cx_core::math::Quat::from_rotation_y(ORBIT_PER_TICK);
    let centre = centre();

    for mut transform in query.iter_mut() {
        let offset = transform.position.delta(centre);
        transform.position = centre.offset(turn * offset);
    }
}

fn build_world() -> (SimWorld, SimSchedule) {
    let mut world = SimWorld::new(WorldConfig::default());

    // Centred on the origin chunk so the camera, the origin, and the scene all
    // agree — extract rebases against the origin, so a mismatch would put the
    // world a chunk-multiple away from where the camera is looking.
    world.spawn_batch((0..GRID * GRID).map(|index| {
        let x = (index % GRID - GRID / 2) as f32 * SPACING;
        let z = (index / GRID - GRID / 2) as f32 * SPACING;
        let transform =
            Transform::from_position(WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(x, 0.0, z)));
        (transform, PreviousTransform(transform))
    }));

    let mut schedule = SimSchedule::new();

    // The previous-transform copy is registered by the schedule itself, at
    // IntakeCommands, ahead of everything below.
    schedule.add_system(Phase::AgentAct, spin);
    schedule.add_system(Phase::Physics, orbit);

    (world, schedule)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let (world, schedule) = build_world();

    // Above and back, looking at the middle of the grid.
    let camera = Camera::looking_at(Vec3::new(0.0, 28.0, 46.0), Vec3::ZERO);

    let config = WindowConfig {
        title: "Chromatron — M1".to_owned(),
        instance_capacity: (GRID * GRID) as usize,
        ..WindowConfig::default()
    };

    tracing::info!(
        entities = GRID * GRID,
        "starting the windowed client; Esc quits, Space pauses, . steps"
    );

    run(config, world, schedule, camera)?;
    Ok(())
}
