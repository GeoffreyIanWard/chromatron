//! The background generation pool (S07/M2).
//!
//! Generation runs on a worker thread, never on the tick thread — a block takes
//! seconds even from cache and tens of seconds fresh, and either would be a
//! frozen frame if it ran inline. The tick thread's whole interface is three
//! non-blocking calls: hand over a want-list, poll for finished blocks, keep
//! going.
//!
//! # One worker, and why not eight
//!
//! The memory arithmetic in `crate::pipeline` settled this: one in-flight block
//! peaks at ~0.71 GB against a 0.8 GB budget, so 1.13 blocks fit. Eight blocks
//! at once would be 7x over budget. The eight cores go *inside* each block
//! instead (`crate::parallel`), which is what `ADR-0008` prescribed all along.
//!
//! # The want-list is replaced, not appended to
//!
//! The frontier recomputes priorities every time the camera moves, so the
//! natural interface is "here is the full list of what I want now, in order" —
//! not a stream of one-off requests that would go stale the moment the camera
//! turned. A block that drops off the list before the worker reaches it is
//! simply never made; the one mid-generation finishes (44 seconds of work is
//! not worth abandoning half-done) and is delivered anyway, for the caller to
//! keep or drop.
//!
//! # What the pool can and cannot affect
//!
//! Terrain bits: never. Blocks come from [`crate::cache::BlockCache`] or
//! [`crate::pipeline::generate_block`], both pure functions of their inputs, so
//! the pool decides only *when* a block exists, not *what* it contains. The
//! simulation must not read terrain that has not arrived — enforcing that is
//! the chunk lifecycle's job, not the pool's.

use std::collections::BTreeSet;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};

use cx_core::math::BlockCoord;

use crate::cache::BlockCache;
use crate::pipeline::{GeneratedBlock, WorldSettings, generate_block};

/// The background pool. One per world.
#[derive(Debug)]
pub struct GenerationPool {
    shared: Arc<Shared>,
    completed: mpsc::Receiver<GeneratedBlock>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<WantState>,
    wake: Condvar,
}

#[derive(Debug, Default)]
struct WantState {
    /// What the frontier wants, highest priority first.
    wanted: Vec<BlockCoord>,
    /// Everything ever completed and sent. Filters the want-list so a block
    /// the caller has not polled yet is not generated twice. `BTreeSet`, not
    /// `HashSet` — iteration order is unspecified there, and the workspace
    /// lint bans it outright (`ADR-0004`).
    delivered: BTreeSet<BlockCoord>,
    /// What the worker is generating right now, if anything.
    in_flight: Option<BlockCoord>,
    shutdown: bool,
}

impl GenerationPool {
    /// Starts the worker.
    ///
    /// With a cache, blocks load from disk when present and are stored after
    /// generating; without one, every block is generated fresh.
    pub fn start(seed: u64, settings: WorldSettings, cache: Option<BlockCache>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(WantState::default()),
            wake: Condvar::new(),
        });
        let (sender, completed) = mpsc::channel();

        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("cx-worldgen pool".to_owned())
            .spawn(move || worker_loop(&worker_shared, seed, settings, cache.as_ref(), &sender))
            .ok();

        if worker.is_none() {
            tracing::error!("could not spawn the generation worker; blocks will never arrive");
        }

        Self {
            shared,
            completed,
            worker,
        }
    }

    /// Replaces the want-list. Non-blocking; call as often as the camera moves.
    ///
    /// Blocks already delivered are skipped automatically, so the caller may
    /// pass the frontier's list verbatim without tracking what has arrived.
    pub fn set_wanted(&self, wanted: Vec<BlockCoord>) {
        let mut state = lock(&self.shared.state);
        state.wanted = wanted;
        drop(state);
        self.shared.wake.notify_one();
    }

    /// Every block finished since the last poll. Non-blocking.
    pub fn poll(&self) -> Vec<GeneratedBlock> {
        self.completed.try_iter().collect()
    }

    /// Blocks wanted but not yet delivered, the in-flight one included.
    pub fn pending(&self) -> usize {
        let state = lock(&self.shared.state);
        state
            .wanted
            .iter()
            .filter(|block| !state.delivered.contains(block))
            .count()
    }

    /// Forgets that a block was delivered, so a future want-list can request
    /// it again — for a caller that dropped the block from memory.
    pub fn forget(&self, block: BlockCoord) {
        let mut state = lock(&self.shared.state);
        state.delivered.remove(&block);
        drop(state);
        self.shared.wake.notify_one();
    }

    /// Stops the worker and waits for it.
    ///
    /// Waits out at most one in-flight block — the shutdown flag is checked
    /// between blocks, because abandoning 44 seconds of nearly-finished work
    /// buys nothing. Dropping the pool without calling this also stops the
    /// worker, but without waiting.
    pub fn shutdown(mut self) {
        self.signal_shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    fn signal_shutdown(&self) {
        let mut state = lock(&self.shared.state);
        state.shutdown = true;
        drop(state);
        self.shared.wake.notify_one();
    }
}

impl Drop for GenerationPool {
    fn drop(&mut self) {
        // Signal but do not join: the worker exits after its current block,
        // and its send into a closed channel is ignored. A drop that silently
        // blocked the main thread for 44 seconds would be worse than a worker
        // that outlives the pool by one block.
        self.signal_shutdown();
    }
}

/// Poison-tolerant lock. The worker holds the lock only to read or flip small
/// fields — never across generation — so a panic mid-lock leaves nothing
/// half-updated worth being poisoned about.
fn lock(mutex: &Mutex<WantState>) -> std::sync::MutexGuard<'_, WantState> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn worker_loop(
    shared: &Shared,
    seed: u64,
    settings: WorldSettings,
    cache: Option<&BlockCache>,
    sender: &mpsc::Sender<GeneratedBlock>,
) {
    loop {
        // Wait for something to do. The lock is held only while choosing.
        let job = {
            let mut state = lock(&shared.state);
            loop {
                if state.shutdown {
                    return;
                }
                let next = state
                    .wanted
                    .iter()
                    .copied()
                    .find(|block| !state.delivered.contains(block));
                if let Some(block) = next {
                    state.in_flight = Some(block);
                    break block;
                }
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };

        // The long part, lock not held: the frontier can keep re-prioritising
        // while this block is made.
        let block = match cache {
            Some(cache) => cache.get_or_generate(seed, job, settings),
            None => generate_block(seed, job, settings),
        };

        {
            let mut state = lock(&shared.state);
            state.delivered.insert(job);
            state.in_flight = None;
        }

        // The pool was dropped mid-generation: nothing is listening, stop.
        if sender.send(block).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::carve::CarveSettings;
    use crate::hydraulic::ErosionSettings;
    use crate::thermal::ThermalSettings;

    const SEED: u64 = 0x0BADC0DE;

    /// The cheapest real settings there are — pool tests are about plumbing,
    /// and every generated block still costs seconds of fill and routing.
    fn cheapest() -> WorldSettings {
        WorldSettings {
            erosion: ErosionSettings::NONE,
            thermal: ThermalSettings::NONE,
            carve: CarveSettings::NONE,
            // No regional model either: these blocks exist to test delivery
            // machinery, and the model was the marginal cost that pushed
            // three-block waits past CI patience.
            region: crate::region::RegionSettings::NONE,
            ..WorldSettings::default()
        }
    }

    /// Polls until `count` blocks have arrived or patience runs out.
    fn collect_blocks(pool: &GenerationPool, count: usize) -> Vec<GeneratedBlock> {
        let mut blocks = Vec::new();
        // Generous: a NO_EROSION block is seconds, CI machines are slow —
        // and the whole suite runs in parallel around this, so the budget is
        // sized for a contended 4-core runner, not for the block.
        for _ in 0..2_400 {
            blocks.extend(pool.poll());
            if blocks.len() >= count {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        blocks
    }

    /// A requested block arrives, off-thread, and is the right block.
    #[test]
    fn a_wanted_block_arrives() {
        let pool = GenerationPool::start(SEED, cheapest(), None);
        pool.set_wanted(vec![BlockCoord::new(0, 0)]);

        let blocks = collect_blocks(&pool, 1);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks.first().map(|b| b.coordinates.block()),
            Some(BlockCoord::new(0, 0))
        );
        assert_eq!(pool.pending(), 0);

        pool.shutdown();
    }

    /// Priority order is delivery order for a queued list.
    #[test]
    fn blocks_arrive_in_priority_order() {
        let pool = GenerationPool::start(SEED, cheapest(), None);
        pool.set_wanted(vec![
            BlockCoord::new(2, 0),
            BlockCoord::new(0, 2),
            BlockCoord::new(1, 1),
        ]);

        let blocks = collect_blocks(&pool, 3);
        let order: Vec<BlockCoord> = blocks.iter().map(|b| b.coordinates.block()).collect();
        assert_eq!(
            order,
            vec![
                BlockCoord::new(2, 0),
                BlockCoord::new(0, 2),
                BlockCoord::new(1, 1),
            ],
            "the pool delivered out of priority order"
        );

        pool.shutdown();
    }

    /// Re-sending a want-list never generates a block twice — the race this
    /// guards is real: the frontier resends its list every frame, far faster
    /// than the caller polls completions.
    #[test]
    fn resending_the_list_does_not_duplicate_work() {
        let pool = GenerationPool::start(SEED, cheapest(), None);

        for _ in 0..50 {
            pool.set_wanted(vec![BlockCoord::new(3, 3)]);
        }
        let blocks = collect_blocks(&pool, 1);

        // And after delivery, keep resending; nothing more may arrive.
        for _ in 0..50 {
            pool.set_wanted(vec![BlockCoord::new(3, 3)]);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let extra = pool.poll();

        assert_eq!(blocks.len(), 1);
        assert!(
            extra.is_empty(),
            "a delivered block was generated again: {} extras",
            extra.len()
        );
        assert_eq!(pool.pending(), 0);

        pool.shutdown();
    }

    /// `forget` re-opens a block for generation — the dropped-from-memory case.
    #[test]
    fn a_forgotten_block_can_be_wanted_again() {
        let pool = GenerationPool::start(SEED, cheapest(), None);
        pool.set_wanted(vec![BlockCoord::new(5, 5)]);
        let first = collect_blocks(&pool, 1);
        assert_eq!(first.len(), 1);

        pool.forget(BlockCoord::new(5, 5));
        pool.set_wanted(vec![BlockCoord::new(5, 5)]);
        let second = collect_blocks(&pool, 1);
        assert_eq!(second.len(), 1, "a forgotten block was never re-made");

        // Same bits both times, of course.
        assert_eq!(
            first.first().map(|b| b.terrain.as_slice().to_vec()),
            second.first().map(|b| b.terrain.as_slice().to_vec())
        );

        pool.shutdown();
    }

    /// The pool serves from the cache when one is attached.
    #[test]
    fn a_cached_block_is_served_from_disk() {
        let root = std::env::temp_dir()
            .join("cx-worldgen-pool-tests")
            .join(format!("cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cache = BlockCache::new(&root);

        let pool = GenerationPool::start(SEED, cheapest(), Some(cache.clone()));
        pool.set_wanted(vec![BlockCoord::new(7, 7)]);
        let generated = collect_blocks(&pool, 1);
        assert_eq!(generated.len(), 1);
        pool.shutdown();

        // A second pool over the same cache must agree bit-for-bit.
        let again = GenerationPool::start(SEED, cheapest(), Some(cache));
        again.set_wanted(vec![BlockCoord::new(7, 7)]);
        let loaded = collect_blocks(&again, 1);
        again.shutdown();

        assert_eq!(
            generated.first().map(|b| b.terrain.as_slice().to_vec()),
            loaded.first().map(|b| b.terrain.as_slice().to_vec()),
            "the cached copy differs from the generated one"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
