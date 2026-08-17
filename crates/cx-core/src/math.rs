//! Coordinates, units, and the world-space conventions from `03-conventions.md`.
//!
//! Y-up, right-handed, counter-clockwise winding — matching glTF, so there is no
//! import-time conversion anywhere in the engine.
//!
//! The rule that shapes this module: **never store an absolute `f32` world
//! position**. At 100 km from the origin an `f32` has roughly 1 cm of
//! resolution, and the jitter is visible. A position is therefore a
//! [`ChunkCoord`] plus a local offset, and [`WorldPos`] is the only type that
//! should appear in a component or a field sample.

/// glam is re-exported rather than depended on directly by other crates, so the
/// version is pinned in exactly one place (S01).
pub use glam;
pub use glam::{IVec2, Quat, Vec2, Vec3, Vec3Swizzles};

/// Edge length of a chunk, in metres.
pub const CHUNK_SIZE: f32 = 512.0;

/// Edge length of one field cell, in metres.
pub const CELL_SIZE: f32 = 0.5;

/// Cells along one chunk edge. `CHUNK_SIZE / CELL_SIZE`.
pub const CELLS_PER_CHUNK_EDGE: u32 = 1024;

/// Cells in one chunk. 1,048,576 — a single `f32` field for one chunk is 4 MB,
/// which is why quantization is mandatory (`bench/memory-budget.md`).
pub const CELLS_PER_CHUNK: u32 = CELLS_PER_CHUNK_EDGE * CELLS_PER_CHUNK_EDGE;

/// Chunks along one block edge. Blocks are the unit of *generation*.
pub const BLOCK_CHUNKS: u32 = 16;

/// Edge length of a block, in metres.
pub const BLOCK_SIZE: f32 = CHUNK_SIZE * BLOCK_CHUNKS as f32;

/// Chunks of discarded margin around a generated block (`ADR-0006`).
pub const GENERATION_HALO_CHUNKS: u32 = 2;

/// Cells along one tile edge. The tile is the dirty-tracking unit (`ADR-0011`).
pub const TILE_CELLS: u32 = 64;

/// Edge length of a tile, in metres.
pub const TILE_SIZE: f32 = TILE_CELLS as f32 * CELL_SIZE;

/// Tiles along one chunk edge.
pub const TILES_PER_CHUNK_EDGE: u32 = CELLS_PER_CHUNK_EDGE / TILE_CELLS;

/// Tiles in one chunk.
pub const TILES_PER_CHUNK: u32 = TILES_PER_CHUNK_EDGE * TILES_PER_CHUNK_EDGE;

/// Edge length of a world-map region cell, in metres.
pub const REGION_SIZE: f32 = 1024.0;

/// Which chunk, in chunk units. Signed: the world extends in every direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ChunkCoord {
    /// Chunk index along +X.
    pub x: i32,
    /// Chunk index along +Z.
    pub z: i32,
}

impl ChunkCoord {
    /// A chunk coordinate.
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// The block containing this chunk.
    ///
    /// Uses floor division rather than truncation, so chunk -1 belongs to block
    /// -1 and not to block 0. Truncating here would put a seam through the
    /// origin — the classic negative-coordinate worldgen bug.
    pub const fn block(self) -> BlockCoord {
        BlockCoord {
            x: self.x.div_euclid(BLOCK_CHUNKS as i32),
            z: self.z.div_euclid(BLOCK_CHUNKS as i32),
        }
    }

    /// This chunk's position within its block, in `0..BLOCK_CHUNKS`.
    pub const fn offset_in_block(self) -> (u32, u32) {
        (
            self.x.rem_euclid(BLOCK_CHUNKS as i32) as u32,
            self.z.rem_euclid(BLOCK_CHUNKS as i32) as u32,
        )
    }

    /// The four edge-adjacent chunks, in a fixed order: -X, +X, -Z, +Z.
    ///
    /// Fixed order matters: halo exchange visits neighbours through this and a
    /// varying order would make float accumulation order-dependent (`ADR-0004`).
    pub const fn neighbours(self) -> [ChunkCoord; 4] {
        [
            ChunkCoord::new(self.x - 1, self.z),
            ChunkCoord::new(self.x + 1, self.z),
            ChunkCoord::new(self.x, self.z - 1),
            ChunkCoord::new(self.x, self.z + 1),
        ]
    }
}

/// Which block, in block units. The unit of generation (`ADR-0006`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BlockCoord {
    /// Block index along +X.
    pub x: i32,
    /// Block index along +Z.
    pub z: i32,
}

impl BlockCoord {
    /// A block coordinate.
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// The chunk at this block's minimum corner.
    pub const fn origin_chunk(self) -> ChunkCoord {
        ChunkCoord::new(self.x * BLOCK_CHUNKS as i32, self.z * BLOCK_CHUNKS as i32)
    }
}

/// A cell within a chunk. Always chunk-local, always in `0..CELLS_PER_CHUNK_EDGE`.
///
/// Deliberately not a global coordinate: field storage is per chunk, and a
/// global cell index would invite the absolute-position bug this module exists
/// to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CellCoord {
    /// Cell index along +X within the chunk.
    pub x: u32,
    /// Cell index along +Z within the chunk.
    pub z: u32,
}

impl CellCoord {
    /// A chunk-local cell coordinate, or `None` if either axis is out of range.
    pub const fn new(x: u32, z: u32) -> Option<Self> {
        if x < CELLS_PER_CHUNK_EDGE && z < CELLS_PER_CHUNK_EDGE {
            Some(Self { x, z })
        } else {
            None
        }
    }

    /// Row-major index into a chunk's field array, without halo.
    ///
    /// Row-major with X contiguous: field kernels walk +X in the inner loop, so
    /// this is the layout that makes them SIMD-friendly (`03-conventions.md`).
    pub const fn index(self) -> usize {
        (self.z * CELLS_PER_CHUNK_EDGE + self.x) as usize
    }

    /// The tile containing this cell.
    pub const fn tile(self) -> TileCoord {
        TileCoord {
            x: self.x / TILE_CELLS,
            z: self.z / TILE_CELLS,
        }
    }
}

/// A tile within a chunk. The unit of dirty tracking (`ADR-0011`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TileCoord {
    /// Tile index along +X within the chunk.
    pub x: u32,
    /// Tile index along +Z within the chunk.
    pub z: u32,
}

impl TileCoord {
    /// Index into a chunk's per-tile dirty bitset.
    pub const fn index(self) -> usize {
        (self.z * TILES_PER_CHUNK_EDGE + self.x) as usize
    }
}

/// A world position: which chunk, plus a local offset in `[0, CHUNK_SIZE)`.
///
/// The local offset keeps `f32` precision constant everywhere in the world
/// instead of degrading with distance from the origin. Floating origin is
/// applied at extract time only; the sim never rebases (`03-conventions.md`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldPos {
    /// The containing chunk.
    pub chunk: ChunkCoord,
    /// Offset within the chunk. X and Z are in `[0, CHUNK_SIZE)`; Y is absolute
    /// elevation in metres, which needs no rebasing because the world is not
    /// tall enough for `f32` to lose useful precision vertically.
    pub local: Vec3,
}

impl WorldPos {
    /// A world position, normalized so `local` lies within its chunk.
    pub fn new(chunk: ChunkCoord, local: Vec3) -> Self {
        Self { chunk, local }.normalized()
    }

    /// Moves any out-of-range local offset into the chunk coordinate.
    ///
    /// Call after arithmetic on `local`. Integrating movement without this is
    /// how an entity ends up with a local offset of 40,000 and the precision
    /// problem the type exists to avoid.
    pub fn normalized(self) -> Self {
        let carry_x = (self.local.x / CHUNK_SIZE).floor();
        let carry_z = (self.local.z / CHUNK_SIZE).floor();

        Self {
            chunk: ChunkCoord::new(
                self.chunk.x.wrapping_add(carry_x as i32),
                self.chunk.z.wrapping_add(carry_z as i32),
            ),
            local: Vec3::new(
                self.local.x - carry_x * CHUNK_SIZE,
                self.local.y,
                self.local.z - carry_z * CHUNK_SIZE,
            ),
        }
    }

    /// The cell containing this position.
    pub fn cell(self) -> CellCoord {
        let normalized = self.normalized();
        let x = (normalized.local.x / CELL_SIZE) as u32;
        let z = (normalized.local.z / CELL_SIZE) as u32;
        CellCoord {
            x: x.min(CELLS_PER_CHUNK_EDGE - 1),
            z: z.min(CELLS_PER_CHUNK_EDGE - 1),
        }
    }

    /// Offset by a delta in metres, renormalizing across chunk boundaries.
    pub fn offset(self, delta: Vec3) -> Self {
        Self {
            chunk: self.chunk,
            local: self.local + delta,
        }
        .normalized()
    }

    /// Displacement from `other` to `self`, in metres.
    ///
    /// Computed through the chunk difference rather than by materializing two
    /// absolute positions, so it stays exact far from the origin.
    pub fn delta(self, other: Self) -> Vec3 {
        let chunk_delta = Vec3::new(
            (self.chunk.x - other.chunk.x) as f32 * CHUNK_SIZE,
            0.0,
            (self.chunk.z - other.chunk.z) as f32 * CHUNK_SIZE,
        );
        chunk_delta + (self.local - other.local)
    }

    /// Absolute position in metres, for rendering and debug output only.
    ///
    /// Named to be conspicuous at a call site: if this appears in sim code, that
    /// code has the precision bug this type prevents.
    pub fn to_absolute_lossy(self) -> Vec3 {
        Vec3::new(
            self.chunk.x as f32 * CHUNK_SIZE + self.local.x,
            self.local.y,
            self.chunk.z as f32 * CHUNK_SIZE + self.local.z,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s01_acceptance_chunk_constants_agree() {
        assert!((CHUNK_SIZE / CELL_SIZE - CELLS_PER_CHUNK_EDGE as f32).abs() < f32::EPSILON);
        assert_eq!(CELLS_PER_CHUNK_EDGE / TILE_CELLS, TILES_PER_CHUNK_EDGE);
        assert_eq!(TILES_PER_CHUNK, 256);
        assert!((TILE_SIZE - 32.0).abs() < f32::EPSILON);
        assert!((BLOCK_SIZE - 8192.0).abs() < f32::EPSILON);
    }

    #[test]
    fn negative_chunks_belong_to_the_expected_block() {
        // The seam-at-the-origin bug: with truncating division, chunk -1 would
        // land in block 0 alongside chunk 0.
        assert_eq!(ChunkCoord::new(-1, -1).block(), BlockCoord::new(-1, -1));
        assert_eq!(ChunkCoord::new(0, 0).block(), BlockCoord::new(0, 0));
        assert_eq!(ChunkCoord::new(-16, 0).block(), BlockCoord::new(-1, 0));
        assert_eq!(ChunkCoord::new(-17, 0).block(), BlockCoord::new(-2, 0));
        assert_eq!(ChunkCoord::new(-1, 0).offset_in_block(), (15, 0));
    }

    #[test]
    fn world_pos_normalizes_across_chunk_boundaries() {
        let position = WorldPos::new(
            ChunkCoord::new(0, 0),
            Vec3::new(CHUNK_SIZE + 1.0, 5.0, -1.0),
        );
        assert_eq!(position.chunk, ChunkCoord::new(1, -1));
        assert!((position.local.x - 1.0).abs() < 1e-3);
        assert!((position.local.z - (CHUNK_SIZE - 1.0)).abs() < 1e-3);
        assert!((position.local.y - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn delta_stays_exact_far_from_the_origin() {
        // 100 km out: the distance an absolute f32 position starts losing
        // centimetres, which is the entire reason WorldPos exists.
        let far = ChunkCoord::new(200, 200);
        let a = WorldPos::new(far, Vec3::new(10.0, 0.0, 10.0));
        let b = WorldPos::new(far, Vec3::new(10.25, 0.0, 10.0));

        let delta = b.delta(a);
        assert!(
            (delta.x - 0.25).abs() < 1e-6,
            "25 cm at 100 km should survive exactly, got {}",
            delta.x
        );
    }

    #[test]
    fn cell_index_is_row_major_with_x_contiguous() {
        let a = CellCoord::new(1, 0).expect("in range");
        let b = CellCoord::new(0, 1).expect("in range");
        assert_eq!(a.index(), 1);
        assert_eq!(b.index(), CELLS_PER_CHUNK_EDGE as usize);
        assert_eq!(CellCoord::new(CELLS_PER_CHUNK_EDGE, 0), None);
    }

    #[test]
    fn cells_map_to_the_expected_tile() {
        let cell = CellCoord::new(65, 130).expect("in range");
        assert_eq!(cell.tile(), TileCoord { x: 1, z: 2 });
        assert_eq!(cell.tile().index(), 2 * TILES_PER_CHUNK_EDGE as usize + 1);
    }
}
