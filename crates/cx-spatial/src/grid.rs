//! The uniform spatial hash (S05).
//!
//! Answers "what is near here" for sparse entities. Dense field lookups do not
//! come through here — those are O(1) array indexing (S06).
//!
//! # Sorted arrays, not a hash map
//!
//! The obvious implementation is `HashMap<Cell, Vec<Entity>>`. `ADR-0004`
//! forbids it: iteration order is unspecified, and query results reach agent
//! decisions, so an unspecified order is a divergence between two runs of the
//! same seed.
//!
//! Instead entries are sorted by cell and stored flat, with lookup by binary
//! search over the sorted keys — the layout a CSR sparse matrix uses. Three
//! consequences, all wanted:
//!
//! - Iteration order is total and defined, so results are reproducible.
//! - Entries in a cell are contiguous, so a query walks memory in order.
//! - Rebuilding reuses the same two vectors, so the steady state allocates
//!   nothing (S05's fourth acceptance criterion).
//!
//! # What is deliberately not here
//!
//! The BVH for static geometry, `raycast`, `sweep`, and the coarse-to-fine path
//! that answers from S09 aggregates rather than activating a dormant chunk.
//! S05 is an M6 spec; this is the primary structure, built now because
//! `cx-agents` needs neighbour queries before then and because a wrong
//! *ordering* rule is much cheaper to fix now than after agents depend on it.

use cx_core::math::{CHUNK_SIZE, WorldPos};
use cx_ecs::Entity;

/// Which cell of the uniform grid a position falls in.
///
/// Absolute, not chunk-relative: a query radius crosses chunk boundaries
/// constantly, and a cell coordinate that restarted at every chunk would make
/// the neighbour walk wrong exactly at the seams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GridCell {
    /// Cell index along +X.
    pub x: i32,
    /// Cell index along +Z.
    pub z: i32,
}

/// One indexed entity.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    cell: GridCell,
    /// Sorted on after `cell`, so entries have a total order independent of
    /// insertion order.
    entity: Entity,
    position: WorldPos,
}

/// What a query found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Found {
    /// The entity.
    pub entity: Entity,
    /// Its position.
    pub position: WorldPos,
    /// Distance from the query centre, in metres.
    pub distance: f32,
}

/// A uniform spatial hash over one class of entity.
///
/// One index per class, per S05: an index mixing 4 m creatures with 128 m
/// buildings performs badly for both.
#[derive(Debug)]
pub struct SpatialGrid {
    cell_size: f32,
    entries: Vec<Entry>,
    /// Reused across queries so the steady state allocates nothing.
    scratch: Vec<Found>,
}

impl SpatialGrid {
    /// An empty index with the given cell size in metres.
    ///
    /// A cell size at or below zero is clamped rather than rejected: it comes
    /// from configuration, and a world that refuses to start because a tuning
    /// value is zero is worse than one that uses a sane cell and says so.
    pub fn new(cell_size: f32) -> Self {
        let cell_size = if cell_size.is_finite() && cell_size > 0.0 {
            cell_size
        } else {
            tracing::warn!(cell_size, "invalid spatial cell size; using 8 m");
            8.0
        };

        Self {
            cell_size,
            entries: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Cell size in metres.
    pub const fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// How many entities are indexed.
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is indexed.
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Rebuilds the index from `entities`.
    ///
    /// Rebuilt wholesale rather than updated incrementally: at the scale S05
    /// targets, most entities move every tick, so an incremental update is a
    /// rebuild with extra bookkeeping and a way to go stale.
    ///
    /// Reuses its buffers, so a steady state where the population is stable
    /// allocates nothing.
    pub fn rebuild(&mut self, entities: impl IntoIterator<Item = (Entity, WorldPos)>) {
        self.entries.clear();

        for (entity, position) in entities {
            self.entries.push(Entry {
                cell: self.cell_of(position),
                entity,
                position,
            });
        }

        // By cell, then by entity. The second key is what makes the order total:
        // without it, two entities in one cell would sit in whatever order they
        // arrived, which is the ECS's iteration order and therefore not
        // something to depend on.
        self.entries
            .sort_unstable_by(|a, b| a.cell.cmp(&b.cell).then_with(|| a.entity.cmp(&b.entity)));
    }

    /// The cell containing a position.
    pub fn cell_of(&self, position: WorldPos) -> GridCell {
        // Absolute metres, via the chunk. Computed in f64 so that a chunk index
        // far from the origin does not lose the local offset entirely — an f32
        // has about a centimetre of resolution at 100 km, and cell assignment
        // must not become ambiguous out there.
        let x = f64::from(position.chunk.x) * f64::from(CHUNK_SIZE) + f64::from(position.local.x);
        let z = f64::from(position.chunk.z) * f64::from(CHUNK_SIZE) + f64::from(position.local.z);
        let size = f64::from(self.cell_size);

        GridCell {
            x: (x / size).floor() as i32,
            z: (z / size).floor() as i32,
        }
    }

    /// Every entity within `radius` of `centre`, nearest first.
    ///
    /// Ordered by distance, then by entity as a tiebreak — S05's determinism
    /// criterion. Two entities at exactly the same distance are common (a grid
    /// formation, a stack of items), so the tiebreak is not a theoretical case.
    ///
    /// Returns a borrowed slice from a reused buffer, so a caller looping over
    /// queries allocates nothing.
    pub fn within_radius(&mut self, centre: WorldPos, radius: f32) -> &[Found] {
        // Cells the radius can reach, and where the centre sits. Both computed
        // before the fields are split apart below.
        let reach = (radius / self.cell_size).ceil() as i32;
        let origin = self.cell_of(centre);

        // Destructured so the entry list can be read while the result buffer is
        // written. Borrowing `self` for both at once is what the alternative
        // needs, and it is genuinely two disjoint fields.
        let Self {
            entries, scratch, ..
        } = self;

        scratch.clear();

        if radius <= 0.0 || !radius.is_finite() {
            return scratch;
        }

        let radius_squared = radius * radius;

        // Ascending cell order, matching how entries are sorted, so the runs
        // this visits are in the same order they appear in memory.
        for z in (origin.z - reach)..=(origin.z + reach) {
            for x in (origin.x - reach)..=(origin.x + reach) {
                for entry in cell_entries(entries, GridCell { x, z }) {
                    let offset = entry.position.delta(centre);
                    let distance_squared = offset.length_squared();
                    if distance_squared <= radius_squared {
                        scratch.push(Found {
                            entity: entry.entity,
                            position: entry.position,
                            distance: distance_squared.sqrt(),
                        });
                    }
                }
            }
        }

        sort_by_distance(scratch);
        scratch
    }

    /// The `k` nearest entities to `centre` within `radius`, nearest first.
    ///
    /// Bounded by a radius as well as a count, because an unbounded nearest-k
    /// over a sparse index degenerates into a scan of everything when the
    /// neighbourhood is empty.
    pub fn nearest_k(&mut self, centre: WorldPos, radius: f32, k: usize) -> &[Found] {
        let found = self.within_radius(centre, radius).len();
        let keep = found.min(k);
        self.scratch.truncate(keep);
        &self.scratch
    }

    /// Distance from `centre` to a position, in metres.
    ///
    /// Through the chunk-relative difference, so it stays exact far from the
    /// origin.
    pub fn distance(centre: WorldPos, position: WorldPos) -> f32 {
        position.delta(centre).length()
    }
}

/// Entries in one cell, as a contiguous slice.
///
/// Binary search over the sorted keys, which is why the entries are sorted
/// rather than hashed. A free function rather than a method so a query can read
/// it while writing the result buffer — two disjoint fields of the same struct.
fn cell_entries(entries: &[Entry], cell: GridCell) -> &[Entry] {
    let start = entries.partition_point(|entry| entry.cell < cell);
    let end = entries.partition_point(|entry| entry.cell <= cell);
    entries.get(start..end).unwrap_or_default()
}

/// Sorts results nearest first, with entity as the tiebreak.
///
/// A free function so the ordering rule has one definition. It is the rule S05
/// makes an acceptance criterion, and a second copy of it would be a second
/// chance to get it wrong.
fn sort_by_distance(found: &mut [Found]) {
    found.sort_unstable_by(|a, b| {
        a.distance
            .total_cmp(&b.distance)
            .then_with(|| a.entity.cmp(&b.entity))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_core::math::{ChunkCoord, Vec3};
    use cx_ecs::{SimWorld, WorldConfig};

    /// Entities, from a real world so their ids are the ids the ECS hands out.
    fn entities(count: usize) -> Vec<Entity> {
        let mut world = SimWorld::new(WorldConfig::default());
        (0..count).map(|_| world.spawn(())).collect()
    }

    fn at(x: f32, y: f32, z: f32) -> WorldPos {
        WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(x, y, z))
    }

    #[test]
    fn an_empty_grid_finds_nothing() {
        let mut grid = SpatialGrid::new(4.0);
        assert!(grid.is_empty());
        assert!(grid.within_radius(at(0.0, 0.0, 0.0), 100.0).is_empty());
    }

    #[test]
    fn a_query_finds_what_is_inside_and_not_what_is_outside() {
        let ids = entities(3);
        let mut grid = SpatialGrid::new(4.0);
        grid.rebuild([
            (ids[0], at(1.0, 0.0, 0.0)),
            (ids[1], at(5.0, 0.0, 0.0)),
            (ids[2], at(50.0, 0.0, 0.0)),
        ]);

        let found = grid.within_radius(at(0.0, 0.0, 0.0), 10.0);
        assert_eq!(found.len(), 2, "the entity 50 m away should not be found");
        assert_eq!(found[0].entity, ids[0]);
        assert_eq!(found[1].entity, ids[1]);
    }

    #[test]
    fn results_are_ordered_by_distance() {
        let ids = entities(4);
        let mut grid = SpatialGrid::new(4.0);
        // Inserted furthest-first, so an implementation that returns insertion
        // order fails rather than passing by luck.
        grid.rebuild([
            (ids[0], at(9.0, 0.0, 0.0)),
            (ids[1], at(6.0, 0.0, 0.0)),
            (ids[2], at(3.0, 0.0, 0.0)),
            (ids[3], at(1.0, 0.0, 0.0)),
        ]);

        let found: Vec<f32> = grid
            .within_radius(at(0.0, 0.0, 0.0), 20.0)
            .iter()
            .map(|found| found.distance)
            .collect();

        assert_eq!(found.len(), 4);
        for pair in found.windows(2) {
            let [near, far] = pair else { continue };
            assert!(near <= far, "results are not sorted: {found:?}");
        }
    }

    #[test]
    fn entities_at_equal_distance_are_ordered_by_entity() {
        // The tiebreak S05 makes an acceptance criterion. Equal distances are
        // not a theoretical case: a grid formation produces them constantly, and
        // without a tiebreak their order is the ECS's iteration order.
        let ids = entities(4);
        let mut grid = SpatialGrid::new(100.0);

        // All four at exactly 5 m, in the four cardinal directions.
        let forwards = [
            (ids[0], at(5.0, 0.0, 0.0)),
            (ids[1], at(-5.0, 0.0, 0.0)),
            (ids[2], at(0.0, 0.0, 5.0)),
            (ids[3], at(0.0, 0.0, -5.0)),
        ];
        grid.rebuild(forwards);
        let first: Vec<Entity> = grid
            .within_radius(at(0.0, 0.0, 0.0), 10.0)
            .iter()
            .map(|found| found.entity)
            .collect();

        let mut reversed = forwards;
        reversed.reverse();
        grid.rebuild(reversed);
        let second: Vec<Entity> = grid
            .within_radius(at(0.0, 0.0, 0.0), 10.0)
            .iter()
            .map(|found| found.entity)
            .collect();

        assert_eq!(
            first, second,
            "the same entities inserted in a different order returned a different order"
        );
        assert_eq!(first.len(), 4);

        // And specifically: ascending entity, since all distances are equal.
        let mut sorted = first.clone();
        sorted.sort_unstable();
        assert_eq!(first, sorted);
    }

    #[test]
    fn insertion_order_never_reaches_the_result() {
        // The general form of the test above, over a population large enough
        // that a stable-sort accident would not save it.
        let ids = entities(200);
        let placed: Vec<(Entity, WorldPos)> = ids
            .iter()
            .enumerate()
            .map(|(index, entity)| {
                let angle = index as f32 * 0.31;
                (*entity, at(angle.cos() * 20.0, 0.0, angle.sin() * 20.0))
            })
            .collect();

        let mut grid = SpatialGrid::new(4.0);
        grid.rebuild(placed.iter().copied());
        let forwards: Vec<Entity> = grid
            .within_radius(at(0.0, 0.0, 0.0), 25.0)
            .iter()
            .map(|found| found.entity)
            .collect();

        let mut shuffled = placed;
        shuffled.reverse();
        grid.rebuild(shuffled);
        let backwards: Vec<Entity> = grid
            .within_radius(at(0.0, 0.0, 0.0), 25.0)
            .iter()
            .map(|found| found.entity)
            .collect();

        assert_eq!(forwards, backwards);
        assert_eq!(forwards.len(), 200);
    }

    #[test]
    fn a_query_spanning_a_chunk_boundary_finds_both_sides() {
        // Cell coordinates are absolute for this reason. Chunk-relative ones
        // would restart at every seam, and the neighbour walk would be wrong
        // exactly where entities cross.
        let ids = entities(2);
        let mut grid = SpatialGrid::new(8.0);

        let west = WorldPos::new(
            ChunkCoord::new(0, 0),
            Vec3::new(CHUNK_SIZE - 2.0, 0.0, 10.0),
        );
        let east = WorldPos::new(ChunkCoord::new(1, 0), Vec3::new(2.0, 0.0, 10.0));
        grid.rebuild([(ids[0], west), (ids[1], east)]);

        // Four metres apart across the seam.
        assert!((SpatialGrid::distance(west, east) - 4.0).abs() < 1e-3);

        let found = grid.within_radius(west, 6.0);
        assert_eq!(
            found.len(),
            2,
            "a query at the seam should find both sides, found {found:?}"
        );
    }

    #[test]
    fn cells_are_continuous_across_a_chunk_boundary() {
        let grid = SpatialGrid::new(8.0);
        let west = WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(CHUNK_SIZE - 4.0, 0.0, 0.0));
        let east = WorldPos::new(ChunkCoord::new(1, 0), Vec3::new(4.0, 0.0, 0.0));

        let west_cell = grid.cell_of(west);
        let east_cell = grid.cell_of(east);
        assert_eq!(
            east_cell.x - west_cell.x,
            1,
            "cells eight metres apart across a seam should be adjacent: \
             {west_cell:?} then {east_cell:?}"
        );
    }

    #[test]
    fn negative_coordinates_get_their_own_cells() {
        // Truncation toward zero rather than flooring makes cell 0 twice as wide
        // as every other cell, straddling the origin — which shows up as a
        // query at the origin being subtly slower and subtly wrong.
        let grid = SpatialGrid::new(10.0);

        assert_eq!(grid.cell_of(at(5.0, 0.0, 0.0)).x, 0);
        assert_eq!(grid.cell_of(at(-5.0, 0.0, 0.0)).x, -1);
        assert_eq!(grid.cell_of(at(-15.0, 0.0, 0.0)).x, -2);
        assert_eq!(grid.cell_of(at(-0.001, 0.0, 0.0)).x, -1);
    }

    #[test]
    fn a_radius_smaller_than_a_cell_still_finds_neighbours() {
        // The reach calculation rounds up for this reason: a radius of 1 m in an
        // 8 m grid still has to look at the neighbouring cells, because the
        // query point can sit right against a cell boundary.
        let ids = entities(2);
        let mut grid = SpatialGrid::new(8.0);
        grid.rebuild([(ids[0], at(7.9, 0.0, 0.0)), (ids[1], at(8.1, 0.0, 0.0))]);

        let found = grid.within_radius(at(8.0, 0.0, 0.0), 0.5);
        assert_eq!(
            found.len(),
            2,
            "both sides of a cell boundary, found {found:?}"
        );
    }

    #[test]
    fn nearest_k_returns_the_closest_and_no_more() {
        let ids = entities(5);
        let mut grid = SpatialGrid::new(4.0);
        grid.rebuild(
            ids.iter()
                .enumerate()
                .map(|(index, entity)| (*entity, at((index + 1) as f32 * 2.0, 0.0, 0.0))),
        );

        let found = grid.nearest_k(at(0.0, 0.0, 0.0), 100.0, 3);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].entity, ids[0]);
        assert_eq!(found[2].entity, ids[2]);

        // Asking for more than exist returns what exists rather than failing.
        assert_eq!(grid.nearest_k(at(0.0, 0.0, 0.0), 100.0, 99).len(), 5);
        assert_eq!(grid.nearest_k(at(0.0, 0.0, 0.0), 100.0, 0).len(), 0);
    }

    #[test]
    fn a_rebuild_replaces_rather_than_appends() {
        let ids = entities(2);
        let mut grid = SpatialGrid::new(4.0);

        grid.rebuild([(ids[0], at(0.0, 0.0, 0.0))]);
        assert_eq!(grid.len(), 1);

        grid.rebuild([(ids[1], at(0.0, 0.0, 0.0))]);
        assert_eq!(grid.len(), 1, "the first entity should be gone");

        let found = grid.within_radius(at(0.0, 0.0, 0.0), 1.0);
        assert_eq!(found[0].entity, ids[1]);
    }

    #[test]
    fn the_steady_state_does_not_grow_its_buffers() {
        // S05's zero-allocation criterion, as far as a unit test can check it:
        // capacity must stop growing once the population is stable.
        let ids = entities(500);
        let placed: Vec<(Entity, WorldPos)> = ids
            .iter()
            .enumerate()
            .map(|(index, entity)| (*entity, at(index as f32 * 0.7, 0.0, index as f32 * 0.3)))
            .collect();

        let mut grid = SpatialGrid::new(4.0);
        for _ in 0..3 {
            grid.rebuild(placed.iter().copied());
            grid.within_radius(at(50.0, 0.0, 20.0), 30.0);
        }

        let entries = grid.entries.capacity();
        let scratch = grid.scratch.capacity();

        for _ in 0..20 {
            grid.rebuild(placed.iter().copied());
            grid.within_radius(at(50.0, 0.0, 20.0), 30.0);
        }

        assert_eq!(grid.entries.capacity(), entries, "the entry buffer regrew");
        assert_eq!(grid.scratch.capacity(), scratch, "the result buffer regrew");
    }

    #[test]
    fn a_degenerate_cell_size_is_survivable() {
        for size in [0.0, -1.0, f32::NAN] {
            let grid = SpatialGrid::new(size);
            assert!(
                grid.cell_size() > 0.0,
                "cell size {size} produced {}",
                grid.cell_size()
            );
        }
    }

    #[test]
    fn a_degenerate_radius_finds_nothing_rather_than_everything() {
        let ids = entities(1);
        let mut grid = SpatialGrid::new(4.0);
        grid.rebuild([(ids[0], at(0.0, 0.0, 0.0))]);

        for radius in [0.0, -1.0, f32::NAN] {
            assert!(
                grid.within_radius(at(0.0, 0.0, 0.0), radius).is_empty(),
                "radius {radius} should find nothing"
            );
        }
    }

    #[test]
    fn distance_is_measured_in_three_dimensions() {
        // A grid indexed on X and Z can quietly become a 2D query, which reads
        // as agents noticing things directly above them through a floor.
        let ids = entities(1);
        let mut grid = SpatialGrid::new(4.0);
        grid.rebuild([(ids[0], at(0.0, 100.0, 0.0))]);

        assert!(
            grid.within_radius(at(0.0, 0.0, 0.0), 10.0).is_empty(),
            "an entity 100 m above should be out of a 10 m radius"
        );
        assert_eq!(grid.within_radius(at(0.0, 0.0, 0.0), 150.0).len(), 1);
    }
}
