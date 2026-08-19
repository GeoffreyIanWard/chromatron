//! Per-tile dirty tracking (`ADR-0011`).
//!
//! A chunk is 1024×1024 cells. When a player digs a hole, roughly a hundred of
//! them change. Rebuilding the mesh, the collider heights, and the nav costs for
//! all 1,048,576 is the cost `ADR-0011` exists to avoid, and the tile — 64×64
//! cells, 32 m, **256 per chunk** — is the granularity it settles on.
//!
//! # Why this is a fixed-size bitset and not a set of coordinates
//!
//! 256 bits is four `u64`s: the whole thing is 32 bytes, fits in a cache line
//! pair, copies trivially, and allocates never. A `HashSet<TileCoord>` would be
//! larger for any realistic edit, would allocate on the edit path, and — the
//! part that actually decides it — iterates in an unspecified order, which
//! `ADR-0004` forbids anywhere a result can reach the simulation. Rebuild order
//! is observable through float accumulation, so it has to be fixed.
//!
//! # Why it lives in `cx-core`
//!
//! Meshes, colliders, and nav grids all dirty per tile, in three different
//! crates. One shared primitive means one definition of "which tiles changed"
//! rather than three that drift.
//!
//! # Fixed before M1, deliberately
//!
//! `ADR-0011` says the granularity must be settled before mesh layout depends on
//! it, because M4B cannot retrofit a different tile size. This is the structure
//! that makes the choice concrete.

use crate::math::{CellCoord, TILES_PER_CHUNK, TILES_PER_CHUNK_EDGE, TileCoord};

/// `u64`s needed to hold one bit per tile.
const WORDS: usize = (TILES_PER_CHUNK as usize).div_ceil(u64::BITS as usize);

/// Which tiles of one chunk have changed since the last rebuild.
///
/// Cheap to copy and to clear, which matters because the consumer clears it
/// every time it catches up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TileDirty {
    words: [u64; WORDS],
}

impl TileDirty {
    /// Nothing dirty.
    pub const fn new() -> Self {
        Self { words: [0; WORDS] }
    }

    /// Every tile dirty.
    ///
    /// What a freshly generated or freshly loaded chunk starts as: everything
    /// needs building once, and saying so here avoids a separate "have I ever
    /// been built" flag that could disagree with this one.
    #[allow(
        clippy::indexing_slicing,
        reason = "WORDS is a compile-time constant and WORDS - 1 is its last element; the \
                  array cannot be empty because TILES_PER_CHUNK is not zero"
    )]
    pub const fn all() -> Self {
        let mut words = [u64::MAX; WORDS];

        // Clear any bits past the last tile in the final word. They would
        // otherwise be counted and iterated as tiles that do not exist — and
        // `TILES_PER_CHUNK` is a multiple of 64 today, so this branch is dead
        // *at present*, which is exactly why it is written now rather than
        // discovered later by whoever changes `TILE_CELLS`.
        let used = TILES_PER_CHUNK as usize % u64::BITS as usize;
        if used != 0 {
            words[WORDS - 1] = (1u64 << used) - 1;
        }

        Self { words }
    }

    /// Marks one tile dirty.
    #[allow(
        clippy::indexing_slicing,
        reason = "the bounds check on the line above proves index < TILES_PER_CHUNK, and \
                  WORDS is TILES_PER_CHUNK / 64, so index / 64 is always in range. Slice::get \
                  is not available in a const fn, and this is a per-tile hot path"
    )]
    pub const fn mark(&mut self, tile: TileCoord) {
        let index = tile.index();
        if index < TILES_PER_CHUNK as usize {
            self.words[index / 64] |= 1u64 << (index % 64);
        }
    }

    /// Marks the tile containing `cell`.
    ///
    /// The common case: an edit knows which cells it touched, not which tiles.
    pub const fn mark_cell(&mut self, cell: CellCoord) {
        self.mark(cell.tile());
    }

    /// Marks every tile overlapping the inclusive cell rectangle from `from` to
    /// `to`.
    ///
    /// Order-independent: the corners are sorted, so a caller that hands them
    /// over in the other order marks the same tiles rather than nothing. That
    /// mistake produces an edit whose mesh silently never updates, which is
    /// indistinguishable from the edit not applying.
    pub fn mark_region(&mut self, from: CellCoord, to: CellCoord) {
        let (min_x, max_x) = ordered(from.x, to.x);
        let (min_z, max_z) = ordered(from.z, to.z);

        let first = TileCoord {
            x: min_x / crate::math::TILE_CELLS,
            z: min_z / crate::math::TILE_CELLS,
        };
        let last = TileCoord {
            x: max_x / crate::math::TILE_CELLS,
            z: max_z / crate::math::TILE_CELLS,
        };

        for z in first.z..=last.z {
            for x in first.x..=last.x {
                self.mark(TileCoord { x, z });
            }
        }
    }

    /// Whether a tile is dirty.
    #[allow(
        clippy::indexing_slicing,
        reason = "the bounds check on the early return above proves index < TILES_PER_CHUNK, and \
                  WORDS is TILES_PER_CHUNK / 64, so index / 64 is always in range. Slice::get \
                  is not available in a const fn, and this is a per-tile hot path"
    )]
    pub const fn is_dirty(&self, tile: TileCoord) -> bool {
        let index = tile.index();
        if index >= TILES_PER_CHUNK as usize {
            return false;
        }
        self.words[index / 64] & (1u64 << (index % 64)) != 0
    }

    /// Marks a tile clean, normally after rebuilding it.
    #[allow(
        clippy::indexing_slicing,
        reason = "the bounds check on the line above proves index < TILES_PER_CHUNK, and \
                  WORDS is TILES_PER_CHUNK / 64, so index / 64 is always in range. Slice::get \
                  is not available in a const fn, and this is a per-tile hot path"
    )]
    pub const fn clear_tile(&mut self, tile: TileCoord) {
        let index = tile.index();
        if index < TILES_PER_CHUNK as usize {
            self.words[index / 64] &= !(1u64 << (index % 64));
        }
    }

    /// Marks everything clean.
    pub const fn clear(&mut self) {
        self.words = [0; WORDS];
    }

    /// Whether anything is dirty.
    ///
    /// The question asked once per chunk per frame, so it is a four-word test
    /// rather than a scan.
    #[allow(
        clippy::indexing_slicing,
        reason = "the loop condition bounds the index to WORDS; slice::get is not available \
                  in a const fn"
    )]
    pub const fn any(&self) -> bool {
        let mut index = 0;
        while index < WORDS {
            if self.words[index] != 0 {
                return true;
            }
            index += 1;
        }
        false
    }

    /// How many tiles are dirty.
    #[allow(
        clippy::indexing_slicing,
        reason = "the loop condition bounds the index to WORDS; slice::get is not available \
                  in a const fn"
    )]
    pub const fn count(&self) -> u32 {
        let mut total = 0;
        let mut index = 0;
        while index < WORDS {
            total += self.words[index].count_ones();
            index += 1;
        }
        total
    }

    /// Every dirty tile, in ascending index order.
    ///
    /// Ordered because rebuild order is observable: float accumulation over a
    /// region depends on the order the pieces are summed, so an unspecified
    /// order here would be a determinism bug that only appears on some machines.
    pub fn iter(&self) -> impl Iterator<Item = TileCoord> + '_ {
        (0..TILES_PER_CHUNK).filter_map(move |index| {
            let tile = TileCoord {
                x: index % TILES_PER_CHUNK_EDGE,
                z: index / TILES_PER_CHUNK_EDGE,
            };
            self.is_dirty(tile).then_some(tile)
        })
    }

    /// Everything dirty in either.
    ///
    /// For merging an edit's tiles into a chunk's outstanding set.
    #[allow(
        clippy::indexing_slicing,
        reason = "the loop condition bounds the index to WORDS; slice::get is not available \
                  in a const fn"
    )]
    pub const fn union(&self, other: &Self) -> Self {
        let mut words = [0; WORDS];
        let mut index = 0;
        while index < WORDS {
            words[index] = self.words[index] | other.words[index];
            index += 1;
        }
        Self { words }
    }
}

/// The pair in ascending order.
const fn ordered(a: u32, b: u32) -> (u32, u32) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{CELLS_PER_CHUNK_EDGE, TILE_CELLS};

    fn cell(x: u32, z: u32) -> CellCoord {
        CellCoord::new(x, z).expect("in range")
    }

    #[test]
    fn a_new_set_is_clean_and_all_is_full() {
        let clean = TileDirty::new();
        assert!(!clean.any());
        assert_eq!(clean.count(), 0);
        assert_eq!(clean.iter().count(), 0);

        let dirty = TileDirty::all();
        assert!(dirty.any());
        assert_eq!(
            dirty.count(),
            TILES_PER_CHUNK,
            "all() must mean every tile and no phantom bits past the last one"
        );
        assert_eq!(dirty.iter().count(), TILES_PER_CHUNK as usize);
    }

    #[test]
    fn marking_and_clearing_one_tile_leaves_the_rest_alone() {
        let mut dirty = TileDirty::new();
        let tile = TileCoord { x: 3, z: 7 };

        dirty.mark(tile);
        assert!(dirty.is_dirty(tile));
        assert_eq!(dirty.count(), 1);
        assert!(!dirty.is_dirty(TileCoord { x: 4, z: 7 }));

        dirty.clear_tile(tile);
        assert!(!dirty.is_dirty(tile));
        assert!(!dirty.any());
    }

    #[test]
    fn every_tile_gets_its_own_bit() {
        // The bug this catches: an index calculation that collides two tiles
        // onto one bit shows up as a tile that never rebuilds, one chunk edge
        // away from the edit.
        for index in 0..TILES_PER_CHUNK {
            let tile = TileCoord {
                x: index % TILES_PER_CHUNK_EDGE,
                z: index / TILES_PER_CHUNK_EDGE,
            };

            let mut dirty = TileDirty::new();
            dirty.mark(tile);
            assert_eq!(dirty.count(), 1, "{tile:?} shares a bit with another tile");
            assert!(dirty.is_dirty(tile));
        }
    }

    #[test]
    fn a_cell_dirties_the_tile_that_contains_it() {
        let mut dirty = TileDirty::new();
        dirty.mark_cell(cell(0, 0));
        assert!(dirty.is_dirty(TileCoord { x: 0, z: 0 }));

        // The last cell of a tile belongs to that tile, not the next one — the
        // off-by-one that leaves a one-cell seam unrebuilt along every tile
        // boundary.
        let mut edge = TileDirty::new();
        edge.mark_cell(cell(TILE_CELLS - 1, TILE_CELLS - 1));
        assert!(edge.is_dirty(TileCoord { x: 0, z: 0 }));
        assert_eq!(edge.count(), 1);

        let mut next = TileDirty::new();
        next.mark_cell(cell(TILE_CELLS, TILE_CELLS));
        assert!(next.is_dirty(TileCoord { x: 1, z: 1 }));

        let mut last = TileDirty::new();
        last.mark_cell(cell(CELLS_PER_CHUNK_EDGE - 1, CELLS_PER_CHUNK_EDGE - 1));
        assert!(last.is_dirty(TileCoord {
            x: TILES_PER_CHUNK_EDGE - 1,
            z: TILES_PER_CHUNK_EDGE - 1,
        }));
    }

    #[test]
    fn a_region_dirties_every_tile_it_touches() {
        // A 3x2 tile span, starting mid-tile so the partial tiles at both ends
        // are included — a region that only marked whole tiles would leave the
        // edges of an edit unrebuilt.
        let mut dirty = TileDirty::new();
        dirty.mark_region(
            cell(TILE_CELLS - 1, TILE_CELLS * 2 - 1),
            cell(TILE_CELLS * 2 + 1, TILE_CELLS * 3),
        );

        // Three tiles across (cells 63..129 land in tiles 0, 1, 2) and three
        // down (cells 127..192 land in tiles 1, 2, 3).
        assert_eq!(dirty.count(), 3 * 3, "should span 3 tiles by 3");
        for z in 1..=3 {
            for x in 0..=2 {
                assert!(
                    dirty.is_dirty(TileCoord { x, z }),
                    "tile ({x}, {z}) should be dirty"
                );
            }
        }
    }

    #[test]
    fn a_region_given_backwards_marks_the_same_tiles() {
        // Handing the corners over in the other order is an easy mistake, and
        // one that would silently mark nothing: the mesh never updates, which
        // looks exactly like the edit failing to apply.
        let mut forwards = TileDirty::new();
        forwards.mark_region(cell(10, 20), cell(300, 400));

        let mut backwards = TileDirty::new();
        backwards.mark_region(cell(300, 400), cell(10, 20));

        assert_eq!(forwards, backwards);
        assert!(forwards.any());
    }

    #[test]
    fn a_single_cell_region_dirties_exactly_one_tile() {
        let mut dirty = TileDirty::new();
        dirty.mark_region(cell(100, 100), cell(100, 100));
        assert_eq!(dirty.count(), 1);
    }

    #[test]
    fn iteration_is_ordered_and_complete() {
        // Rebuild order is observable through float accumulation, so it has to
        // be fixed rather than merely consistent-on-this-machine (ADR-0004).
        let mut dirty = TileDirty::new();
        let marked = [
            TileCoord { x: 15, z: 15 },
            TileCoord { x: 0, z: 0 },
            TileCoord { x: 5, z: 2 },
            TileCoord { x: 0, z: 1 },
        ];
        for tile in marked {
            dirty.mark(tile);
        }

        let seen: Vec<TileCoord> = dirty.iter().collect();
        assert_eq!(seen.len(), marked.len());

        let mut indices: Vec<usize> = seen.iter().map(|tile| tile.index()).collect();
        let sorted = {
            let mut copy = indices.clone();
            copy.sort_unstable();
            copy
        };
        assert_eq!(indices, sorted, "iteration must be in index order");

        indices.sort_unstable();
        let mut expected: Vec<usize> = marked.iter().map(|tile| tile.index()).collect();
        expected.sort_unstable();
        assert_eq!(indices, expected);
    }

    #[test]
    fn union_merges_without_losing_either_side() {
        let mut left = TileDirty::new();
        left.mark(TileCoord { x: 1, z: 1 });
        left.mark(TileCoord { x: 2, z: 2 });

        let mut right = TileDirty::new();
        right.mark(TileCoord { x: 2, z: 2 });
        right.mark(TileCoord { x: 9, z: 4 });

        let merged = left.union(&right);
        assert_eq!(merged.count(), 3, "the shared tile should not count twice");
        assert!(merged.is_dirty(TileCoord { x: 1, z: 1 }));
        assert!(merged.is_dirty(TileCoord { x: 9, z: 4 }));

        assert_eq!(
            merged,
            right.union(&left),
            "union should not depend on which side it is called from"
        );
    }

    #[test]
    fn out_of_range_tiles_are_ignored_rather_than_corrupting_a_neighbour() {
        // TileCoord's fields are public, so an out-of-range one is constructible.
        // Wrapping it into some other tile's bit would dirty an unrelated part of
        // the chunk, which is worse than dropping it.
        let mut dirty = TileDirty::new();
        let outside = TileCoord {
            x: TILES_PER_CHUNK_EDGE + 4,
            z: TILES_PER_CHUNK_EDGE * 4,
        };

        dirty.mark(outside);
        assert!(!dirty.any(), "an out-of-range tile must not mark anything");
        assert!(!dirty.is_dirty(outside));

        dirty.mark(TileCoord { x: 1, z: 1 });
        dirty.clear_tile(outside);
        assert_eq!(
            dirty.count(),
            1,
            "clearing out of range must not clear a real tile"
        );
    }

    #[test]
    fn the_whole_set_is_small_enough_to_copy_freely() {
        // The reason this is a bitset rather than a collection. If it ever grows
        // past a couple of cache lines, the "copy it, clear it, pass it around"
        // assumptions elsewhere stop being free.
        assert_eq!(size_of::<TileDirty>(), 32);
        assert_eq!(TILES_PER_CHUNK, 256, "ADR-0011 fixes this at 256 per chunk");
    }
}
