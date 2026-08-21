//! Row-band parallelism for block grids, with a determinism guarantee.
//!
//! Generation was single-threaded, and most of its work is "compute something
//! for every one of 26 million cells where no cell depends on another" —
//! elevation sampling, hardness sampling, flow directions, thermal deltas,
//! carve stamps. That work splits cleanly across CPU cores by rows.
//!
//! # The rule that shapes everything here
//!
//! `ADR-0004` promises bit-identical output from the same build **at any thread
//! count**. A thread pool that split work by "however many cores there are"
//! would break that the moment floating-point results were combined, because
//! float addition gives slightly different answers in different orders.
//!
//! So the split is by a **fixed number of bands** ([`BANDS`]) that never varies
//! with the machine, and anything a band produces is merged on the calling
//! thread **in band order**. Threads decide only *who* computes each band,
//! never *what* is computed or *in what order* results combine. One core and
//! sixteen cores produce the same bits.
//!
//! Plain `std::thread::scope` — no thread-pool dependency, nothing new for the
//! supply chain to carry.

use crate::block::EDGE;

/// How many row bands a grid is split into, regardless of machine.
///
/// 64 bands of 80 rows each. Enough that eight cores stay busy even when band
/// costs vary (erosion-heavy rows cost more than flat ones); few enough that
/// per-band overhead is noise.
pub(crate) const BANDS: usize = 64;

/// How many worker threads to use.
///
/// Cores, capped at the band count. This number affects speed only — see the
/// module docs for why it cannot affect output.
fn workers() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1)
        .min(BANDS)
}

/// Fills a block-sized row-major grid in parallel: `fill(z, row)` writes one
/// row of [`EDGE`] cells.
pub(crate) fn fill_grid<T, F>(out: &mut [T], fill: F)
where
    T: Send,
    F: Fn(u32, &mut [T]) + Sync,
{
    fill_rows(out, EDGE as usize, fill);
}

/// Fills any row-major grid in parallel: `fill(z, row)` writes one row of
/// `width` cells. Rows come from `out.len() / width`.
///
/// Each band owns a disjoint slice of `out`, so bands cannot race, and each
/// cell's value depends only on `fill` and its coordinates — thread assignment
/// cannot reach the arithmetic. The band count stays fixed regardless of grid
/// size, so a chunk-sized grid (1024 rows) and a block-sized one (5120) both
/// keep the same-bits-at-any-thread-count guarantee.
pub(crate) fn fill_rows<T, F>(out: &mut [T], width: usize, fill: F)
where
    T: Send,
    F: Fn(u32, &mut [T]) + Sync,
{
    if width == 0 {
        return;
    }
    let rows = out.len() / width;
    let per_band = rows.div_ceil(BANDS).max(1);

    let mut bands: Vec<(usize, &mut [T])> = out.chunks_mut(per_band * width).enumerate().collect();

    std::thread::scope(|scope| {
        let workers = workers();
        let fill = &fill;

        // Static round-robin: band i goes to worker i % workers. No queue, no
        // locks, and the assignment is irrelevant to the result anyway.
        let mut per_worker: Vec<Vec<(usize, &mut [T])>> =
            (0..workers).map(|_| Vec::new()).collect();
        for entry in bands.drain(..) {
            let worker = entry.0 % workers;
            if let Some(list) = per_worker.get_mut(worker) {
                list.push(entry);
            }
        }

        for list in per_worker {
            scope.spawn(move || {
                for (band, slice) in list {
                    let start = band * per_band;
                    for (offset, row) in slice.chunks_mut(width).enumerate() {
                        fill((start + offset) as u32, row);
                    }
                }
            });
        }
    });
}

fn band_rows_for(band: usize, per_band: usize) -> (u32, u32) {
    let rows = EDGE as usize;
    let start = (band * per_band).min(rows);
    let end = ((band + 1) * per_band).min(rows);
    (start as u32, end as u32)
}

/// Like [`fill_grid`], but the closure runs once per band (not per row) and
/// returns a result — collected **in band order**.
///
/// For stages whose work spills past a cell: a band writes its own rows
/// directly and returns whatever landed outside them (a spill row, a partial
/// sum), and the caller merges those in band order so floats always combine
/// the same way.
pub(crate) fn fill_bands_map<T, R, F>(out: &mut [T], work: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(u32, &mut [T]) -> R + Sync,
{
    let rows = EDGE as usize;
    let per_band = rows.div_ceil(BANDS);

    let mut bands: Vec<(usize, &mut [T])> = out
        .chunks_mut(per_band * EDGE as usize)
        .enumerate()
        .collect();

    let mut results: Vec<Option<R>> = (0..BANDS).map(|_| None).collect();

    std::thread::scope(|scope| {
        let workers = workers();
        let work = &work;

        let mut per_worker: Vec<Vec<(usize, &mut [T])>> =
            (0..workers).map(|_| Vec::new()).collect();
        for entry in bands.drain(..) {
            let worker = entry.0 % workers;
            if let Some(list) = per_worker.get_mut(worker) {
                list.push(entry);
            }
        }

        let mut handles = Vec::new();
        for list in per_worker {
            handles.push(scope.spawn(move || {
                let mut mine = Vec::new();
                for (band, slice) in list {
                    let (start, _) = band_rows_for(band, per_band);
                    mine.push((band, work(start, slice)));
                }
                mine
            }));
        }

        for handle in handles {
            if let Ok(list) = handle.join() {
                for (band, result) in list {
                    if let Some(slot) = results.get_mut(band) {
                        *slot = Some(result);
                    }
                }
            }
        }
    });

    results.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row is written exactly once, with the right row index.
    #[test]
    fn fill_grid_covers_every_cell_once() {
        let mut grid = vec![u32::MAX; (EDGE as usize) * (EDGE as usize)];
        fill_grid(&mut grid, |z, row| {
            for (x, cell) in row.iter_mut().enumerate() {
                *cell = z * EDGE + x as u32;
            }
        });

        for (index, value) in grid.iter().enumerate() {
            assert_eq!(
                *value as usize, index,
                "cell {index} written wrongly or not at all"
            );
        }
    }

    /// Two runs are bit-identical — the scheduler cannot reach the output.
    #[test]
    fn parallel_fills_are_reproducible() {
        let fill = |z: u32, row: &mut [f32]| {
            for (x, cell) in row.iter_mut().enumerate() {
                // Float math that would expose any ordering difference.
                *cell = (z as f32 * 1.7 + x as f32 * 0.3).sin();
            }
        };

        let mut a = vec![0.0f32; (EDGE as usize) * (EDGE as usize)];
        let mut b = vec![0.0f32; (EDGE as usize) * (EDGE as usize)];
        fill_grid(&mut a, fill);
        fill_grid(&mut b, fill);
        assert_eq!(a, b);
    }
}
