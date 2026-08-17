//! The field store: registration, chunks, kernels, halos, and sampling.

use std::collections::BTreeMap;

use bevy_tasks::{ComputeTaskPool, TaskPoolBuilder};
use cx_core::math::{CELL_SIZE, CELLS_PER_CHUNK_EDGE, ChunkCoord, WorldPos};

use crate::storage::{ChunkField, FieldSpec};

/// Names one dense array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldId(pub u16);

/// A stencil kernel.
///
/// Flat slices, explicit stride, an index range per call. No branches in the
/// inner loop and no bounds checks — boundaries are handled by the halo ring
/// rather than by conditionals (`03-conventions.md`).
///
/// `input` and `output` are different arrays: kernels never read and write the
/// same one.
pub type Kernel =
    fn(input: &[f32], output: &mut [f32], stride: usize, range: std::ops::Range<usize>);

/// How a [`FieldStore`] is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreConfig {
    /// Worker threads for kernel and halo work. From config, never `num_cpus`.
    pub threads: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self { threads: 8 }
    }
}

/// Chunked SoA storage for dense fields.
///
/// The dense half of the architecture (`ADR-0003`). Field cells outnumber
/// entities by two orders of magnitude, so this is where the engine's scale
/// claim is won or lost.
#[derive(Debug)]
pub struct FieldStore {
    config: StoreConfig,
    specs: BTreeMap<FieldId, FieldSpec>,
    /// Keyed by field, then chunk. Ordered, because iteration order reaches
    /// float accumulation and therefore results (`ADR-0004`).
    data: BTreeMap<FieldId, BTreeMap<ChunkCoord, ChunkField>>,
    chunks: Vec<ChunkCoord>,
}

impl FieldStore {
    /// Builds a store and initializes the shared task pool.
    pub fn new(config: StoreConfig) -> Self {
        ComputeTaskPool::get_or_init(|| {
            TaskPoolBuilder::new()
                .num_threads(config.threads.max(1))
                .thread_name("cx-fields".to_owned())
                .build()
        });

        Self {
            config,
            specs: BTreeMap::new(),
            data: BTreeMap::new(),
            chunks: Vec::new(),
        }
    }

    /// Registers a field.
    ///
    /// Registration allocates nothing: a field belonging to a disabled module is
    /// never registered, and a registered field that is never written still
    /// costs zero bytes (S06, S20).
    pub fn register(&mut self, field: FieldId, spec: FieldSpec) -> &mut Self {
        self.specs.insert(field, spec);
        self.data.entry(field).or_default();
        self
    }

    /// Whether a field is registered.
    pub fn is_registered(&self, field: FieldId) -> bool {
        self.specs.contains_key(&field)
    }

    /// A registered field's spec.
    pub fn spec(&self, field: FieldId) -> Option<&FieldSpec> {
        self.specs.get(&field)
    }

    /// Adds a chunk. Allocates nothing until a field in it is written.
    pub fn insert_chunk(&mut self, chunk: ChunkCoord) -> &mut Self {
        if !self.chunks.contains(&chunk) {
            self.chunks.push(chunk);
            self.chunks.sort_unstable();
        }
        self
    }

    /// Loaded chunks, in coordinate order.
    pub fn chunks(&self) -> &[ChunkCoord] {
        &self.chunks
    }

    /// Bytes allocated for a field across every chunk.
    ///
    /// Zero for a registered-but-never-written field, which is the acceptance
    /// criterion that makes the memory budget achievable.
    pub fn allocated_bytes(&self, field: FieldId) -> usize {
        self.data
            .get(&field)
            .map(|chunks| chunks.values().map(ChunkField::allocated_bytes).sum())
            .unwrap_or(0)
    }

    /// Bytes allocated across every field.
    pub fn total_allocated_bytes(&self) -> usize {
        self.specs
            .keys()
            .map(|field| self.allocated_bytes(*field))
            .sum()
    }

    /// Allocates storage for a field in a chunk, if not already present.
    fn ensure(&mut self, field: FieldId, chunk: ChunkCoord) -> Option<&mut ChunkField> {
        let spec = *self.specs.get(&field)?;
        if !self.chunks.contains(&chunk) {
            return None;
        }
        Some(
            self.data
                .entry(field)
                .or_default()
                .entry(chunk)
                .or_insert_with(|| ChunkField::new(spec)),
        )
    }

    /// Fills one chunk's field with a value, allocating if needed.
    pub fn fill(&mut self, field: FieldId, chunk: ChunkCoord, value: f32) {
        if let Some(storage) = self.ensure(field, chunk) {
            storage.fill(value);
        }
    }

    /// Writes one cell, allocating if needed.
    pub fn set(&mut self, field: FieldId, chunk: ChunkCoord, x: u32, z: u32, value: f32) {
        if let Some(storage) = self.ensure(field, chunk) {
            storage.set(x, z, value);
        }
    }

    /// Reads one cell, or the field default where nothing is stored.
    pub fn get(&self, field: FieldId, chunk: ChunkCoord, x: u32, z: u32) -> f32 {
        let default = self
            .specs
            .get(&field)
            .map(|spec| spec.default)
            .unwrap_or(0.0);
        self.data
            .get(&field)
            .and_then(|chunks| chunks.get(&chunk))
            .map(|storage| storage.get(x, z))
            .unwrap_or(default)
    }

    /// Borrows a chunk's storage.
    pub fn chunk(&self, field: FieldId, chunk: ChunkCoord) -> Option<&ChunkField> {
        self.data.get(&field)?.get(&chunk)
    }

    /// Borrows a chunk's storage mutably.
    pub fn chunk_mut(&mut self, field: FieldId, chunk: ChunkCoord) -> Option<&mut ChunkField> {
        self.data.get_mut(&field)?.get_mut(&chunk)
    }

    /// Runs a kernel over every allocated chunk of a field, then swaps buffers.
    ///
    /// Parallelised **by chunk**. `03-conventions.md` also calls for row-band
    /// splitting within a chunk; that is not implemented yet and is recorded as
    /// an open question in S06 — it matters when chunk count drops below thread
    /// count, which the M0 workload (16 chunks, 8 threads) does not reach.
    pub fn run_kernel(&mut self, field: FieldId, kernel: Kernel) {
        let Some(spec) = self.specs.get(&field).copied() else {
            return;
        };
        let Some(chunks) = self.data.get_mut(&field) else {
            return;
        };

        let stride = spec.stride();
        let halo = spec.halo_width as usize;
        // Interior only: the halo ring is input, never output.
        let first = halo * stride + halo;
        let last = (stride - halo) * stride - halo;

        ComputeTaskPool::get().scope(|scope| {
            for storage in chunks.values_mut() {
                scope.spawn(async move {
                    let (input, output) = storage.buffers_mut();
                    kernel(input, output, stride, first..last);
                });
            }
        });

        for storage in chunks.values_mut() {
            storage.swap();
        }
    }

    /// Copies neighbour edge data into each chunk's halo ring.
    ///
    /// Its own sub-phase (S06), run before `FieldSolve`. Two passes: gather every
    /// neighbour edge, then write. A single fused pass would need simultaneous
    /// mutable and immutable access to two chunks, and the read-then-write split
    /// is the same discipline the tick phases use.
    pub fn exchange_halos(&mut self, field: FieldId) {
        let Some(spec) = self.specs.get(&field).copied() else {
            return;
        };
        if spec.halo_width == 0 {
            return;
        }

        let edge = CELLS_PER_CHUNK_EDGE;
        let mut gathered: Vec<(ChunkCoord, [Option<Vec<f32>>; 4])> = Vec::new();

        {
            let Some(chunks) = self.data.get(&field) else {
                return;
            };

            for coord in chunks.keys() {
                // Fixed neighbour order: -X, +X, -Z, +Z (cx_core::ChunkCoord).
                let neighbours = coord.neighbours();
                let mut sides: [Option<Vec<f32>>; 4] = [None, None, None, None];

                for (slot, neighbour) in neighbours.iter().enumerate() {
                    let Some(source) = chunks.get(neighbour) else {
                        continue;
                    };
                    let mut strip = Vec::with_capacity(edge as usize);
                    for i in 0..edge {
                        // Take the neighbour's touching edge line.
                        let value = match slot {
                            0 => source.get(edge - 1, i),
                            1 => source.get(0, i),
                            2 => source.get(i, edge - 1),
                            _ => source.get(i, 0),
                        };
                        strip.push(value);
                    }
                    if let Some(cell) = sides.get_mut(slot) {
                        *cell = Some(strip);
                    }
                }

                gathered.push((*coord, sides));
            }
        }

        let Some(chunks) = self.data.get_mut(&field) else {
            return;
        };

        for (coord, sides) in gathered {
            let Some(storage) = chunks.get_mut(&coord) else {
                continue;
            };
            let stride = spec.stride();
            let halo = spec.halo_width as usize;

            for (slot, strip) in sides.iter().enumerate() {
                let Some(strip) = strip else {
                    continue;
                };
                for (i, value) in strip.iter().enumerate() {
                    // Halo ring cell just outside the corresponding edge.
                    let (hx, hz) = match slot {
                        0 => (halo - 1, i + halo),
                        1 => (halo + edge as usize, i + halo),
                        2 => (i + halo, halo - 1),
                        _ => (i + halo, halo + edge as usize),
                    };
                    if let Some(cell) = storage.front_mut().get_mut(hz * stride + hx) {
                        *cell = *value;
                    }
                }
            }
        }
    }

    /// Samples a field at a world position with bilinear interpolation.
    ///
    /// Read-only and safe to call in parallel from `AgentSense`.
    pub fn sample(&self, field: FieldId, position: WorldPos) -> f32 {
        let position = position.normalized();
        let default = self
            .specs
            .get(&field)
            .map(|spec| spec.default)
            .unwrap_or(0.0);

        let Some(storage) = self.chunk(field, position.chunk) else {
            return default;
        };

        let fx = (position.local.x / CELL_SIZE).max(0.0);
        let fz = (position.local.z / CELL_SIZE).max(0.0);
        let x0 = (fx as u32).min(CELLS_PER_CHUNK_EDGE - 1);
        let z0 = (fz as u32).min(CELLS_PER_CHUNK_EDGE - 1);
        let x1 = (x0 + 1).min(CELLS_PER_CHUNK_EDGE - 1);
        let z1 = (z0 + 1).min(CELLS_PER_CHUNK_EDGE - 1);
        let tx = fx - x0 as f32;
        let tz = fz - z0 as f32;

        let v00 = storage.get(x0, z0);
        let v10 = storage.get(x1, z0);
        let v01 = storage.get(x0, z1);
        let v11 = storage.get(x1, z1);

        let top = v00 + (v10 - v00) * tx;
        let bottom = v01 + (v11 - v01) * tx;
        top + (bottom - top) * tz
    }

    /// Samples without interpolation, for discrete fields like `BIOME`.
    pub fn sample_nearest(&self, field: FieldId, position: WorldPos) -> f32 {
        let position = position.normalized();
        let cell = position.cell();
        self.get(field, position.chunk, cell.x, cell.z)
    }

    /// The configuration this store was built with.
    pub const fn config(&self) -> &StoreConfig {
        &self.config
    }
}
