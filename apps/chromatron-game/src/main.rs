//! Windowed client.
//!
//! The M2 milemarker: fly over terrain that streams in around the camera. The
//! chunk lifecycle (S07) decides what should be resident at what detail; this
//! binary's only jobs are to feed it the camera every frame and to turn its
//! decisions into meshes on the GPU. Everything interesting — the frontier,
//! the background pool, the disk cache, promotion and demotion budgets — is
//! engine code with its own tests. What the window adds is the part no test
//! can: whether it *feels* right. Does ground arrive before you reach it,
//! does detail pop distractingly, does a fast pass stutter.
//!
//! The first minute over unvisited terrain is honest about what generation
//! costs: a block takes tens of seconds to make, and until the first one
//! lands there is nothing under the camera but the clear colour. The disk
//! cache makes the second run start in seconds.
//!
//! # Controls
//!
//! Camera: `W`/`A`/`S`/`D` to fly, `E`/`Q` up and down, hold `Shift` to boost,
//! hold the right mouse button and move the mouse to look.
//!
//! Time: `Esc` quit · `Space` pause · `.` step one tick · `[` slower ·
//! `]` faster · `\` normal speed. (Pause stops the — currently empty —
//! simulation; the camera and the terrain streaming are view-side and keep
//! going, which is exactly the firewall boundary drawn where it should be.)

use std::collections::BTreeMap;

use cx_app::{FlyCamera, FrameLoop, TerrainMeshData, WindowConfig, run};
use cx_core::math::{CELL_SIZE, CELLS_PER_CHUNK_EDGE, CHUNK_SIZE, ChunkCoord, Vec3, WorldPos};
use cx_ecs::{SimSchedule, SimWorld, WorldConfig};
use cx_view::DebugColour;
use cx_worldgen::{
    BlockCache, ChunkLifecycle, LifecycleSettings, Residency, WorldSettings, lifecycle::COARSE_EDGE,
};

/// The demo world. An arbitrary constant, and deliberately so: the same seed
/// on every run is what makes the disk cache worth having and regressions
/// visible by eye.
const SEED: u64 = 20_260_821;

/// Every `stride`-th cell of an Active chunk's 0.5 m grid — a 2 m mesh,
/// 66,049 vertices a chunk. Detail terrain.
const ACTIVE_STRIDE: usize = 4;

/// Every other cell of a Coarse chunk's 4 m grid — an 8 m mesh, 4,225
/// vertices a chunk. Horizon terrain.
const COARSE_STRIDE: usize = 2;

/// Chunk meshes built per frame, at most.
///
/// Meshing an Active chunk is around a millisecond; the budget exists so that
/// arriving in a fresh region rebuilds the neighbourhood over a few frames
/// instead of spending one long frame on all of it — the same amortization
/// the lifecycle itself applies to promotions.
const MESHES_PER_FRAME: usize = 2;

/// Turns lifecycle decisions into resident GPU meshes.
///
/// Owns the lifecycle and remembers, per chunk, which level of detail is
/// currently uploaded. Each frame it diffs that memory against what the
/// lifecycle now holds and closes the gap within [`MESHES_PER_FRAME`].
struct TerrainDriver {
    lifecycle: ChunkLifecycle,
    settings: LifecycleSettings,
    /// What is on the GPU right now: the residency each mesh was built from.
    meshed: BTreeMap<(i32, i32), Residency>,
    /// Chunk offsets within the coarse radius, nearest first — the scan
    /// order, precomputed because it never changes.
    scan: Vec<(i32, i32)>,
    /// Where the camera was last frame, for its velocity.
    previous: Option<WorldPos>,
    /// How many chunks the lifecycle knew last frame, to log block arrivals.
    known: usize,
    /// Whether the camera has been dropped onto arrived terrain yet.
    grounded: bool,
}

impl TerrainDriver {
    fn start(cache_root: std::path::PathBuf) -> Self {
        let settings = LifecycleSettings::DEFAULT;

        let mut scan: Vec<(i32, i32)> = (-settings.coarse_radius..=settings.coarse_radius)
            .flat_map(|dz| {
                (-settings.coarse_radius..=settings.coarse_radius).map(move |dx| (dx, dz))
            })
            .collect();
        scan.sort_by_key(|(dx, dz)| (dx.abs().max(dz.abs()), *dx, *dz));

        Self {
            lifecycle: ChunkLifecycle::start(
                SEED,
                WorldSettings::default(),
                settings,
                Some(BlockCache::new(cache_root)),
            ),
            settings,
            meshed: BTreeMap::new(),
            scan,
            previous: None,
            known: 0,
            grounded: false,
        }
    }

    /// One frame of terrain work: aim the lifecycle, then close the gap
    /// between what it holds and what the GPU shows.
    fn sync(&mut self, frame_loop: &mut FrameLoop, camera: &mut FlyCamera, dt: f32) {
        let position = camera.position;
        let interest = (
            position.chunk.x as f32 * CHUNK_SIZE + position.local.x,
            position.chunk.z as f32 * CHUNK_SIZE + position.local.z,
        );
        let velocity = match self.previous.replace(position) {
            Some(previous) if dt > 1e-6 => {
                let delta = position.delta(previous);
                (delta.x / dt, delta.z / dt)
            }
            _ => (0.0, 0.0),
        };

        let report = self.lifecycle.update(interest, velocity);

        // A block arriving is the demo's biggest event — up to 256 chunks of
        // terrain becoming buildable at once — and on a cold cache it is tens
        // of seconds apart, so a log line each is signal, not noise.
        let known = self.lifecycle.known_chunks();
        if known != self.known {
            tracing::info!(
                chunks_known = known,
                pending_blocks = report.pending_blocks,
                "a block arrived"
            );
            self.known = known;
        }

        let centre = position.chunk;

        // Once, when ground first exists under the camera: make sure the
        // spawn is above it. Absolute elevation varies by hundreds of metres
        // with the continental map, so no fixed start height is safe — the
        // first run of this demo started 200 m inside a mountain.
        if !self.grounded
            && let Some(summary) = self.lifecycle.summary(centre)
        {
            let clearance = summary.max_height + 120.0;
            if camera.position.local.y < clearance {
                tracing::info!(
                    terrain_max = summary.max_height,
                    lifted_to = clearance,
                    "lifting the camera above the arrived terrain"
                );
                camera.position.local.y = clearance;
            }
            self.grounded = true;
        }

        // Drop meshes for chunks that fell out of range or lost their data.
        // Removal is nearly free, so it takes no budget: keeping stale ground
        // visible is worse than a frame with slightly more bookkeeping.
        let stale: Vec<(i32, i32)> = self
            .meshed
            .keys()
            .copied()
            .filter(|(x, z)| {
                let chunk = ChunkCoord::new(*x, *z);
                let out_of_range =
                    (x - centre.x).abs().max((z - centre.z).abs()) > self.settings.coarse_radius;
                let lost = !matches!(
                    self.lifecycle.residency(chunk),
                    Some(Residency::Coarse | Residency::Active)
                );
                out_of_range || lost
            })
            .collect();
        for key in stale {
            frame_loop.remove_terrain_chunk(ChunkCoord::new(key.0, key.1));
            self.meshed.remove(&key);
        }

        // Build or rebuild meshes nearest-first, within budget. A chunk whose
        // uploaded level matches its residency costs nothing here.
        let mut budget = MESHES_PER_FRAME;
        for (dx, dz) in &self.scan {
            if budget == 0 {
                break;
            }
            let chunk = ChunkCoord::new(centre.x + dx, centre.z + dz);
            let residency = match self.lifecycle.residency(chunk) {
                Some(residency @ (Residency::Coarse | Residency::Active)) => residency,
                _ => continue,
            };
            if self.meshed.get(&(chunk.x, chunk.z)) == Some(&residency) {
                continue;
            }

            let mesh = match residency {
                Residency::Active => self.lifecycle.active(chunk).and_then(|active| {
                    TerrainMeshData::from_heights(
                        active.elevation.as_slice(),
                        CELLS_PER_CHUNK_EDGE as usize,
                        CELL_SIZE,
                        ACTIVE_STRIDE,
                    )
                }),
                Residency::Coarse => self.lifecycle.coarse(chunk).and_then(|heights| {
                    TerrainMeshData::from_heights(
                        heights,
                        COARSE_EDGE as usize,
                        CHUNK_SIZE / COARSE_EDGE as f32,
                        COARSE_STRIDE,
                    )
                }),
                Residency::Dormant => None,
            };

            if let Some(mesh) = mesh {
                frame_loop.upload_terrain_chunk(chunk, &mesh);
                self.meshed.insert((chunk.x, chunk.z), residency);
                budget -= 1;
            }
        }
    }
}

/// World axes at the origin, so there is something to orient by before the
/// first block lands — and a fixed reference to judge terrain scale against
/// after it does.
fn draw_reference(frame_loop: &mut FrameLoop) {
    let origin = WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(0.0, 60.0, 0.0));
    let debug = frame_loop.debug();
    for (direction, colour) in [
        (Vec3::X, DebugColour::RED),
        (Vec3::Y, DebugColour::GREEN),
        (Vec3::Z, DebugColour::BLUE),
    ] {
        debug.arrow(origin, origin.offset(direction * 100.0), colour);
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // The sim world is deliberately empty: M2 is about the ground, and an
    // empty world proves the terrain path stands on its own. Entities return
    // when there is something for them to stand on.
    let world = SimWorld::new(WorldConfig::default());
    let schedule = SimSchedule::new();

    // High over the origin chunk, looking out and down across where the first
    // block will appear. Terrain around here sits at roughly 40 m plus a
    // hundred-odd metres of relief.
    let mut camera = FlyCamera::looking_at(
        WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(256.0, 380.0, 256.0)),
        WorldPos::new(ChunkCoord::new(2, 2), Vec3::new(256.0, 60.0, 256.0)),
    );
    // Terrain scale, not room scale: a chunk is 512 m.
    camera.speed = 80.0;

    let cache_root = std::env::temp_dir().join("chromatron-block-cache");
    tracing::info!(
        seed = SEED,
        cache = %cache_root.display(),
        "starting the terrain demo; the first block takes a while on a cold cache — \
         watch for 'a block arrived'"
    );

    let mut terrain = TerrainDriver::start(cache_root);

    let config = WindowConfig {
        title: "Chromatron — M2".to_owned(),
        instance_capacity: 1_024,
        ..WindowConfig::default()
    };

    run(
        config,
        world,
        schedule,
        camera,
        move |frame_loop, camera, dt| {
            terrain.sync(frame_loop, camera, dt);
            draw_reference(frame_loop);
        },
    )?;
    Ok(())
}
