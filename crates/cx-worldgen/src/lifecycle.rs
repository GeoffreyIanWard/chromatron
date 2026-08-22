//! The chunk state machine (S07/M2).
//!
//! This is where the pieces meet. The frontier decides which blocks to want,
//! the pool makes them in the background, the cache remembers them — and this
//! module turns arrived blocks into **chunks at the right level of detail for
//! how close they are**, under strict per-tick budgets so no single frame ever
//! does too much work.
//!
//! # The states, and what each one costs
//!
//! | State | What is resident | Cost per chunk |
//! |---|---|---|
//! | `Ungenerated` | nothing — the containing block was never made | 0 |
//! | `Generated` | a record and a summary; terrain lives in the block cache | ~64 B |
//! | `Dormant` | the same — the summary *is* the dormant representation | ~64 B |
//! | `Coarse` | a 128x128 downsampled height grid | 64 KB |
//! | `Active` | full 1024x1024 elevation plus slope and aspect | ~6 MB |
//!
//! Ten thousand Dormant chunks are therefore under a megabyte against the
//! 0.2 GB the memory budget allocates — that exit criterion is a test here,
//! counted in bytes rather than asserted in prose.
//!
//! # Everything is amortized
//!
//! Promotions and demotions happen a few per tick, never all at once. Walking
//! into a new region does not bake 25 chunks in one frame; it bakes one or two
//! per tick until the neighbourhood is Active, nearest first, while the ones
//! left behind demote a step per tick, farthest first. The budgets are
//! settings, and the tests hold the machine to them.
//!
//! # Blocks are heavy; residency is scarce
//!
//! Baking a chunk needs its whole source block in memory, and a resident block
//! is ~430 MB even after shedding what only erosion needed. So at most a
//! couple of blocks stay resident, least-recently-used evicted first — the
//! disk cache brings one back in seconds when it is needed again. A chunk
//! whose block is not resident simply waits; its promotion happens on a later
//! tick, after the pool delivers.
//!
//! # What this deliberately does not decide
//!
//! When a block *arrives* depends on disk and CPU speed, so which tick a chunk
//! promotes on is not reproducible across machines — fine for rendering, not
//! fine for simulation. Before sim state may depend on chunk contents, either
//! activation ticks must be recorded for replay or the sim must gate on
//! "chunk present" deterministically. That is persistence-milestone work
//! (S13); recorded here so it is a decision, not a surprise.

use std::collections::BTreeMap;

use cx_core::math::{BLOCK_CHUNKS, BlockCoord, CELLS_PER_CHUNK_EDGE, CHUNK_SIZE, ChunkCoord};

use crate::bake::{BakeSettings, ChunkElevation, bake_chunk};
use crate::block::ErosionCell;
use crate::cache::BlockCache;
use crate::derive::{DerivedFields, derive_fields};
use crate::frontier::{FrontierSettings, wanted_blocks};
use crate::pipeline::{GeneratedBlock, WorldSettings};
use crate::pool::GenerationPool;
use crate::water::{ChunkWater, WaterSettings, bake_water};

/// Erosion-grid cells along one chunk edge (256 at the current grid ratio).
const EROSION_CELLS_PER_CHUNK: u32 = CELLS_PER_CHUNK_EDGE / cx_core::math::CELLS_PER_EROSION_CELL;

/// Cells along one edge of a Coarse chunk's height grid.
///
/// Public because a renderer meshing a Coarse grid needs to know its shape;
/// the grid itself comes from [`ChunkLifecycle::coarse`].
pub const COARSE_EDGE: u32 = 128;

/// How present a chunk is.
///
/// S07's chart names a fourth state, `Generated`, between "never made" and
/// Dormant. Here the two collapse: the summary a Dormant chunk keeps is twelve
/// bytes, so there is nothing left to shed that would make a distinct cheaper
/// state worth having — and the first version that *did* distinguish them had
/// a real bug because of it. Summary-only records read as Dormant, wanted
/// "Generated", and so were demotion candidates forever; the no-op demotions
/// consumed the whole budget farthest-first and starved every real one, which
/// left 6 MB Active chunks resident permanently. The memory test caught it at
/// 380 KB per "dormant" chunk. A state that exists but cannot be reached is
/// not a state — it is a treadmill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Residency {
    /// Summary only (~12 bytes). The 10,000-resident state the memory budget
    /// names, and the floor: a chunk whose block was ever made never falls
    /// below this, because there is nothing cheaper left to fall to.
    Dormant,
    /// Downsampled heights, enough for far terrain.
    Coarse,
    /// Full elevation and derived fields.
    Active,
}

/// A cheap always-kept digest of a chunk, computed once when its block first
/// arrives. This *is* the Dormant representation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkSummary {
    /// Lowest terrain in the chunk, metres.
    pub min_height: f32,
    /// Highest terrain, metres.
    pub max_height: f32,
    /// Fraction of the chunk under standing water, `0..=1`.
    pub water_fraction: f32,
}

/// One chunk's lifecycle record.
#[derive(Debug)]
struct ChunkRecord {
    summary: ChunkSummary,
    coarse: Option<Vec<f32>>,
    /// Water at Coarse resolution, when the chunk is Coarse or above and has
    /// any. Rides with `coarse` rather than having its own residency level.
    coarse_water: Option<ChunkWater>,
    active: Option<ActiveChunk>,
}

/// What an Active chunk holds resident.
#[derive(Debug)]
pub struct ActiveChunk {
    /// Full-resolution baked elevation.
    pub elevation: ChunkElevation,
    /// Slope and aspect, quantised.
    pub fields: DerivedFields,
    /// Lakes and channels on the 2 m grid — `None` when the chunk is dry,
    /// which most are.
    pub water: Option<ChunkWater>,
}

impl ChunkRecord {
    fn residency(&self) -> Residency {
        if self.active.is_some() {
            Residency::Active
        } else if self.coarse.is_some() {
            Residency::Coarse
        } else {
            // Generated and Dormant share a representation; the distinction is
            // whether the machine has decided this chunk is worth remembering
            // at all, and with the summary this cheap it always is.
            Residency::Dormant
        }
    }

    /// Bytes this record keeps resident, for the budget test.
    fn resident_bytes(&self) -> usize {
        let mut bytes = size_of::<Self>();
        if let Some(coarse) = &self.coarse {
            bytes += coarse.len() * size_of::<f32>();
        }
        if let Some(active) = &self.active {
            bytes += size_of_val(active.elevation.as_slice());
            bytes += active.fields.slopes().len() + active.fields.aspects().len();
            if let Some(water) = &active.water {
                bytes += water.resident_bytes();
            }
        }
        if let Some(water) = &self.coarse_water {
            bytes += water.resident_bytes();
        }
        bytes
    }
}

/// How the lifecycle behaves. Every number is a knob on purpose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LifecycleSettings {
    /// Chunks within this Chebyshev distance of the interest point are Active.
    pub active_radius: i32,
    /// ... within this, at least Coarse. Beyond it, Dormant — the floor.
    pub coarse_radius: i32,
    /// Most chunks Active at once, whatever the radius asks for. Nearest win.
    pub active_cap: usize,
    /// Promotions applied per update. The knob that keeps a new region from
    /// baking 25 chunks in one frame.
    pub promotions_per_tick: usize,
    /// Demotions applied per update.
    pub demotions_per_tick: usize,
    /// Blocks kept resident for baking. Each is ~430 MB.
    pub resident_blocks: usize,
    /// How the frontier aims.
    pub frontier: FrontierSettings,
    /// How chunks bake.
    pub bake: BakeSettings,
    /// What counts as water when a chunk's water is read out.
    pub water: WaterSettings,
}

impl LifecycleSettings {
    /// Defaults: a 5x5 Active neighbourhood inside a 13x13 Coarse ring —
    /// everything further is Dormant — one promotion and four demotions a
    /// tick, two resident blocks.
    ///
    /// One promotion, not two: a promotion to Active costs ~7 ms of baking on
    /// the calling thread, and the M2 traversal criterion charges that to the
    /// frame. Two promotions put the worst tick at ~17 ms before the frame
    /// drew anything; one keeps the whole frame under the 20 ms budget, and a
    /// 5x5 neighbourhood still activates in under half a second.
    pub const DEFAULT: Self = Self {
        active_radius: 2,
        coarse_radius: 6,
        active_cap: 32,
        promotions_per_tick: 1,
        demotions_per_tick: 4,
        resident_blocks: 2,
        frontier: FrontierSettings::DEFAULT,
        bake: BakeSettings::SMOOTH,
        water: WaterSettings::DEFAULT,
    };
}

impl Default for LifecycleSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What one update did — the numbers the budgets are held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LifecycleReport {
    /// Chunks promoted this tick.
    pub promoted: usize,
    /// Chunks demoted this tick.
    pub demoted: usize,
    /// Chunks currently Active.
    pub active: usize,
    /// Blocks currently resident in memory.
    pub resident_blocks: usize,
    /// Blocks wanted but not yet delivered.
    pub pending_blocks: usize,
}

/// The chunk lifecycle manager. One per world.
#[derive(Debug)]
pub struct ChunkLifecycle {
    pool: GenerationPool,
    settings: LifecycleSettings,
    /// Resident blocks, with the tick each was last needed.
    blocks: BTreeMap<BlockCoord, (GeneratedBlock, u64)>,
    chunks: BTreeMap<ChunkCoord, ChunkRecord>,
    tick: u64,
}

impl ChunkLifecycle {
    /// Starts the lifecycle and its background pool.
    pub fn start(
        seed: u64,
        world: WorldSettings,
        settings: LifecycleSettings,
        cache: Option<BlockCache>,
    ) -> Self {
        Self {
            pool: GenerationPool::start(seed, world, cache),
            settings,
            blocks: BTreeMap::new(),
            chunks: BTreeMap::new(),
            tick: 0,
        }
    }

    /// One tick of lifecycle work: aim the frontier, absorb arrived blocks,
    /// and apply a budget's worth of promotions and demotions.
    ///
    /// `interest` and `velocity` are world metres and metres per second — the
    /// camera, today; any set of interest points, later.
    pub fn update(&mut self, interest: (f32, f32), velocity: (f32, f32)) -> LifecycleReport {
        self.tick += 1;

        // Aim the background work. The pool skips everything already
        // delivered, so resending the whole list every tick is free.
        self.pool
            .set_wanted(wanted_blocks(interest, velocity, self.settings.frontier));

        // Absorb whatever finished.
        for block in self.pool.poll() {
            self.absorb(block);
        }

        let interest_chunk = chunk_containing(interest.0, interest.1);
        let demoted = self.demote(interest_chunk);
        let promoted = self.promote(interest_chunk);
        self.evict_blocks();

        LifecycleReport {
            promoted,
            demoted,
            active: self.active_count(),
            resident_blocks: self.blocks.len(),
            pending_blocks: self.pool.pending(),
        }
    }

    /// A chunk's current residency, or `None` if its block was never made.
    pub fn residency(&self, chunk: ChunkCoord) -> Option<Residency> {
        self.chunks.get(&chunk).map(ChunkRecord::residency)
    }

    /// An Active chunk's data, for rendering and the sim.
    pub fn active(&self, chunk: ChunkCoord) -> Option<&ActiveChunk> {
        self.chunks.get(&chunk)?.active.as_ref()
    }

    /// A Coarse chunk's height grid, when one is resident.
    ///
    /// [`COARSE_EDGE`] cells to a side, row-major with +X fastest, 4 m per
    /// cell, heights in metres — the shape [`ChunkSummary`] summarises and a
    /// far-terrain mesh is built from. An Active chunk may or may not also
    /// hold one; render from [`ChunkLifecycle::active`] first.
    pub fn coarse(&self, chunk: ChunkCoord) -> Option<&[f32]> {
        self.chunks.get(&chunk)?.coarse.as_deref()
    }

    /// A Coarse chunk's water, when it is resident and the chunk has any.
    ///
    /// Half the resolution of [`ChunkWater`]'s native grid — 128 cells to a
    /// side at 4 m — downsampled wettest-cell-first so narrow rivers survive.
    /// An Active chunk's full-resolution water is on [`ActiveChunk::water`].
    pub fn coarse_water(&self, chunk: ChunkCoord) -> Option<&ChunkWater> {
        self.chunks.get(&chunk)?.coarse_water.as_ref()
    }

    /// A chunk's summary — present from the moment its block first arrived.
    pub fn summary(&self, chunk: ChunkCoord) -> Option<ChunkSummary> {
        self.chunks.get(&chunk).map(|record| record.summary)
    }

    /// How many chunks have records at all — everything a block ever delivered.
    pub fn known_chunks(&self) -> usize {
        self.chunks.len()
    }

    /// Total bytes resident across every chunk record, for the budget test.
    pub fn resident_chunk_bytes(&self) -> usize {
        self.chunks.values().map(ChunkRecord::resident_bytes).sum()
    }

    /// Stops the background pool, waiting out at most one in-flight block.
    pub fn shutdown(self) {
        self.pool.shutdown();
    }

    fn active_count(&self) -> usize {
        self.chunks
            .values()
            .filter(|record| record.active.is_some())
            .count()
    }

    /// Takes delivery of a block: summaries for all 256 chunks, then keep the
    /// block resident for baking.
    fn absorb(&mut self, mut block: GeneratedBlock) {
        // ~100 MB of drainage-order data that only erosion ever walks.
        block.network.shed_erosion_order();

        // All 256 summaries, computed in parallel. Serially this was the worst
        // single tick the traversal measured — a couple of million grid reads
        // landing in whatever frame the block happened to arrive on. Each
        // summary is a pure function of the block, so threads split the list
        // and the merge below is in fixed chunk order.
        let origin = block.coordinates.block().origin_chunk();
        let chunks: Vec<ChunkCoord> = (0..BLOCK_CHUNKS as i32)
            .flat_map(|dz| {
                (0..BLOCK_CHUNKS as i32)
                    .map(move |dx| ChunkCoord::new(origin.x + dx, origin.z + dz))
            })
            .collect();

        let workers = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1)
            .min(chunks.len().max(1));
        let mut summaries: Vec<(ChunkCoord, ChunkSummary)> = Vec::with_capacity(chunks.len());
        std::thread::scope(|scope| {
            let block = &block;
            let mut handles = Vec::new();
            for worker in 0..workers {
                let mine: Vec<ChunkCoord> = chunks
                    .iter()
                    .copied()
                    .skip(worker)
                    .step_by(workers)
                    .collect();
                handles.push(scope.spawn(move || {
                    mine.into_iter()
                        .map(|chunk| (chunk, summarise(block, chunk)))
                        .collect::<Vec<_>>()
                }));
            }
            for handle in handles {
                if let Ok(list) = handle.join() {
                    summaries.extend(list);
                }
            }
        });

        for (chunk, summary) in summaries {
            self.chunks
                .entry(chunk)
                .or_insert(ChunkRecord {
                    summary,
                    coarse: None,
                    coarse_water: None,
                    active: None,
                })
                .summary = summary;
        }

        self.blocks
            .insert(block.coordinates.block(), (block, self.tick));
    }

    /// Applies up to the demotion budget, farthest chunks first.
    fn demote(&mut self, interest: ChunkCoord) -> usize {
        // Candidates: resident chunks above their desired state.
        let mut candidates: Vec<(i32, ChunkCoord)> = self
            .chunks
            .iter()
            .filter_map(|(chunk, record)| {
                let distance = chebyshev(*chunk, interest);
                let desired = self.desired(distance);
                (record.residency() > desired).then_some((distance, *chunk))
            })
            .collect();

        // Farthest first; coordinate tie-break keeps it deterministic.
        candidates
            .sort_by_key(|(distance, chunk)| (std::cmp::Reverse(*distance), chunk.x, chunk.z));

        let mut demoted = 0;
        for (_, chunk) in candidates {
            if demoted >= self.settings.demotions_per_tick {
                break;
            }
            if let Some(record) = self.chunks.get_mut(&chunk) {
                // One step down per tick: Active sheds to Coarse (downsampling
                // what it already holds), Coarse sheds to Dormant. Gradual on
                // purpose — a sharp turn should not dump 6 MB x N in a frame.
                //
                // Only real work counts against the budget. A candidate with
                // nothing to shed consuming a budget slot is exactly how the
                // collapsed-state bug starved every genuine demotion.
                if let Some(active) = record.active.take() {
                    record.coarse = Some(downsample_active(&active.elevation));
                    // The water steps down with the heights, halved the same
                    // way — no re-read of the block, which may be long gone.
                    record.coarse_water =
                        active.water.as_ref().and_then(|water| water.downsample(2));
                    demoted += 1;
                } else if record.coarse.take().is_some() {
                    record.coarse_water = None;
                    demoted += 1;
                }
            }
        }
        demoted
    }

    /// Applies up to the promotion budget, nearest chunks first.
    fn promote(&mut self, interest: ChunkCoord) -> usize {
        let active_room = self.settings.active_cap.saturating_sub(self.active_count());

        let mut candidates: Vec<(i32, ChunkCoord)> = self
            .chunks
            .iter()
            .filter_map(|(chunk, record)| {
                let distance = chebyshev(*chunk, interest);
                let desired = self.desired(distance);
                (record.residency() < desired).then_some((distance, *chunk))
            })
            .collect();
        candidates.sort_by_key(|(distance, chunk)| (*distance, chunk.x, chunk.z));

        let mut promoted = 0;
        let mut activated = 0;
        for (distance, chunk) in candidates {
            if promoted >= self.settings.promotions_per_tick {
                break;
            }
            let desired = self.desired(distance);

            // Everything above Dormant needs the source block in memory. Not
            // resident is not an error: the frontier already wants it, and this
            // chunk simply waits for a later tick.
            let block_coord = chunk.block();
            let Some((block, last_used)) = self.blocks.get_mut(&block_coord) else {
                continue;
            };
            *last_used = self.tick;

            let step = match desired {
                Residency::Active if activated < active_room => {
                    let baked = bake_chunk(
                        &block.terrain,
                        &block.network,
                        &block.generator,
                        block.coordinates,
                        chunk,
                        self.settings.bake,
                    );
                    baked.map(|elevation| {
                        let fields = derive_fields(&elevation);
                        let water = bake_water(block, chunk, self.settings.water);
                        ActiveChunk {
                            elevation,
                            fields,
                            water,
                        }
                    })
                }
                _ => None,
            };

            if let Some(record) = self.chunks.get_mut(&chunk) {
                match step {
                    Some(active) => {
                        record.active = Some(active);
                        activated += 1;
                        promoted += 1;
                    }
                    None if desired >= Residency::Coarse && record.coarse.is_none() => {
                        record.coarse = Some(coarse_from_block(block, chunk));
                        record.coarse_water = bake_water(block, chunk, self.settings.water)
                            .and_then(|water| water.downsample(2));
                        promoted += 1;
                    }
                    None => {}
                }
            }
        }
        promoted
    }

    /// The residency a chunk at `distance` should have. Dormant is the floor
    /// — see [`Residency`] for why there is nothing below it.
    fn desired(&self, distance: i32) -> Residency {
        if distance <= self.settings.active_radius {
            Residency::Active
        } else if distance <= self.settings.coarse_radius {
            Residency::Coarse
        } else {
            Residency::Dormant
        }
    }

    /// Evicts least-recently-needed blocks past the residency cap.
    fn evict_blocks(&mut self) {
        while self.blocks.len() > self.settings.resident_blocks.max(1) {
            let oldest = self
                .blocks
                .iter()
                .min_by_key(|(coord, (_, used))| (*used, coord.x, coord.z))
                .map(|(coord, _)| *coord);
            let Some(coord) = oldest else { break };
            self.blocks.remove(&coord);
            // Re-openable: if a chunk near here needs promoting later, the
            // frontier re-wants the block and the pool reloads it from cache.
            self.pool.forget(coord);
        }
    }
}

/// Chebyshev distance in chunks — rings, matching the square radii.
fn chebyshev(a: ChunkCoord, b: ChunkCoord) -> i32 {
    (a.x - b.x).abs().max((a.z - b.z).abs())
}

fn chunk_containing(x: f32, z: f32) -> ChunkCoord {
    ChunkCoord::new(
        (x / CHUNK_SIZE).floor() as i32,
        (z / CHUNK_SIZE).floor() as i32,
    )
}

/// A chunk's summary, read straight off the block's erosion grid.
fn summarise(block: &GeneratedBlock, chunk: ChunkCoord) -> ChunkSummary {
    let Some(origin) = block.coordinates.chunk_origin_cell(chunk) else {
        return ChunkSummary {
            min_height: 0.0,
            max_height: 0.0,
            water_fraction: 0.0,
        };
    };

    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut wet = 0u32;
    let mut counted = 0u32;

    // Every 4th cell: a summary needs the shape, not the census, and this runs
    // 256 times per arriving block.
    for dz in (0..EROSION_CELLS_PER_CHUNK).step_by(4) {
        for dx in (0..EROSION_CELLS_PER_CHUNK).step_by(4) {
            let Some(cell) = ErosionCell::new(origin.x() + dx, origin.z() + dz) else {
                continue;
            };
            let height = block.terrain.get(cell);
            min = min.min(height);
            max = max.max(height);
            if block.water_depth(cell) > 0.05 {
                wet += 1;
            }
            counted += 1;
        }
    }

    ChunkSummary {
        min_height: if counted == 0 { 0.0 } else { min },
        max_height: if counted == 0 { 0.0 } else { max },
        water_fraction: if counted == 0 {
            0.0
        } else {
            wet as f32 / counted as f32
        },
    }
}

/// A Coarse grid straight from the block's erosion-grid slice: 2x2 averages of
/// 2 m cells, so 4 m per coarse cell.
fn coarse_from_block(block: &GeneratedBlock, chunk: ChunkCoord) -> Vec<f32> {
    let mut cells = vec![0.0f32; (COARSE_EDGE * COARSE_EDGE) as usize];
    let Some(origin) = block.coordinates.chunk_origin_cell(chunk) else {
        return cells;
    };

    let step = EROSION_CELLS_PER_CHUNK / COARSE_EDGE;
    for z in 0..COARSE_EDGE {
        for x in 0..COARSE_EDGE {
            let mut sum = 0.0f32;
            let mut count = 0u32;
            for dz in 0..step {
                for dx in 0..step {
                    if let Some(cell) =
                        ErosionCell::new(origin.x() + x * step + dx, origin.z() + z * step + dz)
                    {
                        sum += block.terrain.get(cell);
                        count += 1;
                    }
                }
            }
            if let Some(slot) = cells.get_mut((z * COARSE_EDGE + x) as usize) {
                *slot = if count == 0 { 0.0 } else { sum / count as f32 };
            }
        }
    }
    cells
}

/// A Coarse grid from an Active chunk being demoted: 8x8 averages of the baked
/// 0.5 m cells, so 4 m per coarse cell — same resolution as
/// [`coarse_from_block`], different (finer) source.
fn downsample_active(elevation: &ChunkElevation) -> Vec<f32> {
    let mut cells = vec![0.0f32; (COARSE_EDGE * COARSE_EDGE) as usize];
    let step = CELLS_PER_CHUNK_EDGE / COARSE_EDGE;

    for z in 0..COARSE_EDGE {
        for x in 0..COARSE_EDGE {
            let mut sum = 0.0f32;
            let mut count = 0u32;
            for dz in 0..step {
                for dx in 0..step {
                    if let Some(height) = elevation.get(x * step + dx, z * step + dz) {
                        sum += height;
                        count += 1;
                    }
                }
            }
            if let Some(slot) = cells.get_mut((z * COARSE_EDGE + x) as usize) {
                *slot = if count == 0 { 0.0 } else { sum / count as f32 };
            }
        }
    }
    cells
}
