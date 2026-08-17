//! Per-chunk field storage.
//!
//! One contiguous `Vec<f32>` per field per chunk, allocated lazily on first
//! write. A 1024×1024 chunk is 1,048,576 cells, so a single `f32` field for one
//! chunk is 4 MB — which is why an unwritten field allocating nothing is a
//! stated acceptance criterion rather than an optimisation.
//!
//! Arrays are stored **with a halo ring** already included, so a kernel reading
//! `index - stride` at the top row reads copied neighbour data rather than
//! running off the array. That is the whole reason halos exist: no bounds checks
//! and no neighbour lookups in the inner loop.

use cx_core::math::{CELLS_PER_CHUNK_EDGE, TILES_PER_CHUNK, TILES_PER_CHUNK_EDGE};

/// How a field's data is treated by persistence (S13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persistence {
    /// Recomputable from the seed; never saved.
    Regenerable,
    /// Saved as a delta against the generated value.
    DeltaPersisted,
    /// Rebuilt at load; never saved.
    Transient,
}

/// A field's declared shape and policy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldSpec {
    /// `SCREAMING_SNAKE`, unique across all modules (S06).
    pub name: &'static str,
    /// Value of a cell that has never been written.
    pub default: f32,
    /// How persistence treats this field.
    pub persistence: Persistence,
    /// Halo ring width in cells. A 5-point stencil needs 1.
    pub halo_width: u32,
    /// Whether writes mark tiles dirty for mesh, nav, and collider consumers.
    pub tile_dirty_tracking: bool,
}

impl FieldSpec {
    /// A transient `f32` field with a one-cell halo — the common case.
    pub const fn transient(name: &'static str, default: f32) -> Self {
        Self {
            name,
            default,
            persistence: Persistence::Transient,
            halo_width: 1,
            tile_dirty_tracking: false,
        }
    }

    /// Cells along one edge of the stored array, halo included.
    pub const fn stride(&self) -> usize {
        (CELLS_PER_CHUNK_EDGE + self.halo_width * 2) as usize
    }

    /// Total stored elements per chunk, halo included.
    pub const fn elements(&self) -> usize {
        self.stride() * self.stride()
    }

    /// Index of chunk-local cell `(x, z)` within the stored array.
    pub const fn index_of(&self, x: u32, z: u32) -> usize {
        let halo = self.halo_width as usize;
        (z as usize + halo) * self.stride() + (x as usize + halo)
    }
}

/// One chunk's storage for one field.
///
/// Double-buffered: kernels read `front` and write `back`, then the two swap.
/// A kernel that read and wrote the same array would make each cell's result
/// depend on how far the loop had already progressed, which is the classic
/// stencil bug and is invisible until the output looks subtly directional.
#[derive(Debug)]
pub struct ChunkField {
    front: Vec<f32>,
    back: Vec<f32>,
    dirty_tiles: [u64; TILES_PER_CHUNK.div_ceil(64) as usize],
    spec: FieldSpec,
}

impl ChunkField {
    /// Allocates and fills with the field's default.
    pub fn new(spec: FieldSpec) -> Self {
        Self {
            front: vec![spec.default; spec.elements()],
            back: vec![spec.default; spec.elements()],
            dirty_tiles: [0; TILES_PER_CHUNK.div_ceil(64) as usize],
            spec,
        }
    }

    /// The readable array, halo included.
    pub fn front(&self) -> &[f32] {
        &self.front
    }

    /// The readable array, mutably.
    pub fn front_mut(&mut self) -> &mut [f32] {
        &mut self.front
    }

    /// Both buffers at once, for running a kernel.
    pub fn buffers_mut(&mut self) -> (&[f32], &mut [f32]) {
        (&self.front, &mut self.back)
    }

    /// Swaps the buffers after a kernel has written `back`.
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.front, &mut self.back);
    }

    /// Bytes this chunk's storage occupies, both buffers.
    pub fn allocated_bytes(&self) -> usize {
        (self.front.len() + self.back.len()) * size_of::<f32>()
    }

    /// Reads a chunk-local cell.
    pub fn get(&self, x: u32, z: u32) -> f32 {
        self.front
            .get(self.spec.index_of(x, z))
            .copied()
            .unwrap_or(self.spec.default)
    }

    /// Writes a chunk-local cell, marking its tile dirty if the field tracks tiles.
    pub fn set(&mut self, x: u32, z: u32, value: f32) {
        let index = self.spec.index_of(x, z);
        if let Some(slot) = self.front.get_mut(index) {
            *slot = value;
        }
        if self.spec.tile_dirty_tracking {
            self.mark_tile_dirty(x / cx_core::math::TILE_CELLS, z / cx_core::math::TILE_CELLS);
        }
    }

    /// Fills every cell, halo included.
    pub fn fill(&mut self, value: f32) {
        self.front.fill(value);
        self.back.fill(value);
        if self.spec.tile_dirty_tracking {
            self.dirty_tiles.fill(u64::MAX);
        }
    }

    fn mark_tile_dirty(&mut self, tile_x: u32, tile_z: u32) {
        let tile = (tile_z * TILES_PER_CHUNK_EDGE + tile_x) as usize;
        if let Some(word) = self.dirty_tiles.get_mut(tile / 64) {
            *word |= 1u64 << (tile % 64);
        }
    }

    /// Whether a tile has been written since the last clear.
    pub fn is_tile_dirty(&self, tile_x: u32, tile_z: u32) -> bool {
        let tile = (tile_z * TILES_PER_CHUNK_EDGE + tile_x) as usize;
        self.dirty_tiles
            .get(tile / 64)
            .is_some_and(|word| word & (1u64 << (tile % 64)) != 0)
    }

    /// How many tiles are dirty.
    pub fn dirty_tile_count(&self) -> u32 {
        self.dirty_tiles.iter().map(|word| word.count_ones()).sum()
    }

    /// Clears dirty tracking, at end of tick once consumers have read it.
    pub fn clear_dirty_tiles(&mut self) {
        self.dirty_tiles.fill(0);
    }

    /// The spec this storage was built from.
    pub const fn spec(&self) -> &FieldSpec {
        &self.spec
    }
}
