//! The disposable on-disk block cache (S07/M2).
//!
//! Generating a block costs ~44 seconds. Nothing about it ever changes for a
//! given seed — that is the whole point of positional generation — so paying
//! that cost more than once per block is pure waste. This cache pays it once
//! and reloads in a few seconds ever after, which speeds up every render, every
//! test session, and eventually every revisit in play.
//!
//! # Disposable, and why that is a feature
//!
//! The cache is *not* part of the save. Deleting it costs regeneration time and
//! nothing else, because `ADR-0006` guarantees regeneration reproduces the same
//! bits — and the equivalence test here holds that guarantee to account rather
//! than assuming it. That is what keeps an infinite world's save file finite:
//! terrain that can always be recomputed never needs to be kept.
//!
//! # What is stored: one surface, not three
//!
//! A generated block carries three big grids — final terrain, the ground under
//! its lakes, and the drainage network. Storing all of them would be ~300 MB per
//! block against the 100–200 MB the memory budget allows. But the terrain *is*
//! the ground with its basins filled, and the network is derived from the
//! terrain — both are recomputable in a few seconds by the same code that
//! produced them. So the file holds only the **ground** (~100 MB), and loading
//! re-runs the fill and routing. Bit-identical by construction, and tested.
//!
//! # Every mismatch is a miss
//!
//! Wrong pipeline version, different settings, different seed, truncated file,
//! flipped bit — the load path treats all of them the same way: as if the file
//! were not there. A cache must never be *wrong*; being absent is always safe,
//! because the generator is the source of truth and the cache is only a
//! shortcut to it. This is also why the entry is keyed on
//! [`crate::pipeline::GENERATOR_VERSION`]: a pipeline change silently
//! invalidates every cached block rather than silently serving terrain the
//! current code would not produce.
//!
//! # Bytes are native-endian
//!
//! The file is a snapshot for *this machine*, not an interchange format — a
//! cache moved between machines of different byte order would simply miss its
//! checksum and regenerate, which is the correct outcome.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cx_core::math::BlockCoord;

use crate::block::{BlockGrid, CELLS};
use crate::flow::FlowNetwork;
use crate::pipeline::{
    BlockReport, GENERATOR_VERSION, GeneratedBlock, WorldSettings, generate_block,
};

/// File magic: identifies the format and its layout revision in one place.
const MAGIC: &[u8; 8] = b"CXBLOCK1";

/// Default size cap. ~60 blocks at ~100 MB each — a healthy exploration radius
/// before the least-recently-touched blocks start being evicted.
pub const DEFAULT_CAP_BYTES: u64 = 6 * 1024 * 1024 * 1024;

/// The on-disk block cache.
#[derive(Debug, Clone)]
pub struct BlockCache {
    root: PathBuf,
    cap_bytes: u64,
}

impl BlockCache {
    /// A cache rooted at `root`, with the default size cap.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cap_bytes: DEFAULT_CAP_BYTES,
        }
    }

    /// A cache with an explicit size cap, for tests and constrained installs.
    pub fn with_cap(root: impl Into<PathBuf>, cap_bytes: u64) -> Self {
        Self {
            root: root.into(),
            cap_bytes,
        }
    }

    /// The block, from disk if cached or by generating (and then caching) it.
    ///
    /// A store failure is logged and swallowed: a full or read-only disk should
    /// cost the caching, never the block.
    pub fn get_or_generate(
        &self,
        seed: u64,
        block: BlockCoord,
        settings: WorldSettings,
    ) -> GeneratedBlock {
        if let Some(cached) = self.load(seed, block, settings) {
            return cached;
        }

        let generated = generate_block(seed, block, settings);
        if let Err(error) = self.store(seed, settings, &generated) {
            tracing::warn!(?block, %error, "could not cache a generated block");
        }
        generated
    }

    /// Loads a block, or `None` for any reason at all — see the module docs.
    pub fn load(
        &self,
        seed: u64,
        block: BlockCoord,
        settings: WorldSettings,
    ) -> Option<GeneratedBlock> {
        let path = self.entry_path(seed, block, settings);
        let mut file = std::io::BufReader::new(std::fs::File::open(&path).ok()?);

        let mut header = [0u8; HEADER_BYTES];
        file.read_exact(&mut header).ok()?;
        let expected = header_for(seed, block, settings);
        if header[..HEADER_KEY_BYTES] != expected[..HEADER_KEY_BYTES] {
            return None;
        }

        let report = read_report(&header)?;

        let mut ground_bytes = vec![0u8; CELLS * size_of::<f32>()];
        file.read_exact(&mut ground_bytes).ok()?;

        let mut checksum_bytes = [0u8; 8];
        file.read_exact(&mut checksum_bytes).ok()?;
        // And nothing after: a longer file than the format describes is as
        // suspect as a shorter one.
        if file.read(&mut [0u8; 1]).ok()? != 0 {
            return None;
        }

        let mut checksum = fnv(&header, FNV_SEED);
        checksum = fnv(&ground_bytes, checksum);
        if checksum != u64::from_le_bytes(checksum_bytes) {
            return None;
        }

        // Native-endian, matching the `cast_slice` on the write side; the
        // `unwrap_or` arm is unreachable because `chunks_exact` guarantees the
        // length, and a zero would fail the checksum anyway.
        let ground_cells: Vec<f32> = ground_bytes
            .chunks_exact(size_of::<f32>())
            .map(|chunk| f32::from_ne_bytes(chunk.try_into().unwrap_or([0; 4])))
            .collect();
        let ground = BlockGrid::from_cells(ground_cells)?;

        // The rest of a generated block is derived, not stored: the terrain is
        // the ground with basins filled, the network is routed from it, and the
        // generator rebuilds from the seed. Same code path carving used —
        // including the same regional boundary seal, or a loaded block's
        // basins would fill to different levels than the generated one's —
        // and `tests` proves the bit-identity rather than trusting this
        // comment.
        let generator = crate::elevation::ElevationGenerator::with_world(
            seed,
            settings.terrain,
            settings.world,
        );
        let coordinates = crate::block::BlockCoordinates::new(block);
        let region =
            crate::region::RegionalWater::for_block(&generator, coordinates, settings.region);
        let seal = region.boundary_seal(coordinates);
        let network = FlowNetwork::build_sealed(ground.clone(), &seal);

        Some(GeneratedBlock {
            coordinates: crate::block::BlockCoordinates::new(block),
            terrain: network.filled().clone(),
            ground,
            network,
            generator,
            report,
        })
    }

    /// Writes a block to the cache, evicting old entries past the size cap.
    pub fn store(
        &self,
        seed: u64,
        settings: WorldSettings,
        block: &GeneratedBlock,
    ) -> std::io::Result<()> {
        let path = self.entry_path(seed, block.coordinates.block(), settings);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let header = {
            let mut header = header_for(seed, block.coordinates.block(), settings);
            write_report(&mut header, &block.report);
            header
        };
        let ground_bytes: &[u8] = bytemuck::cast_slice(block.ground.as_slice());

        let mut checksum = fnv(&header, FNV_SEED);
        checksum = fnv(ground_bytes, checksum);

        // Written to a sibling temp file and renamed into place. A crash or a
        // full disk mid-write leaves a `.tmp` orphan, never a plausible-looking
        // half-file — torn writes are exactly the corruption the checksum is
        // for, and the cheapest checksum failure is the one that cannot happen.
        let temporary = path.with_extension("tmp");
        {
            let mut file = std::io::BufWriter::new(std::fs::File::create(&temporary)?);
            file.write_all(&header)?;
            file.write_all(ground_bytes)?;
            file.write_all(&checksum.to_le_bytes())?;
            file.into_inner()?.sync_all()?;
        }
        std::fs::rename(&temporary, &path)?;

        self.evict_past_cap();
        Ok(())
    }

    /// Where one block's entry lives.
    ///
    /// Version, seed, and settings are directories, so "everything stale" is a
    /// directory a person can recognise and delete — and so eviction naturally
    /// clears old versions first, since nothing refreshes their timestamps.
    fn entry_path(&self, seed: u64, block: BlockCoord, settings: WorldSettings) -> PathBuf {
        self.root
            .join(format!(
                "v{GENERATOR_VERSION}_{seed:016x}_{:016x}",
                settings.fingerprint()
            ))
            .join(format!("{}_{}.cxb", block.x, block.z))
    }

    /// Deletes least-recently-modified entries until the cache fits its cap.
    ///
    /// Best-effort throughout: an eviction failure costs disk space, not
    /// correctness, so nothing here propagates errors.
    fn evict_past_cap(&self) {
        let mut entries: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
        collect_entries(&self.root, &mut entries);

        let mut total: u64 = entries.iter().map(|(_, size, _)| size).sum();
        if total <= self.cap_bytes {
            return;
        }

        // Oldest first. The timestamp is filesystem metadata about the file,
        // not a wall-clock read into sim state — terrain bits never depend on
        // which entries happen to survive.
        entries.sort_by_key(|(modified, _, _)| *modified);

        for (_, size, path) in entries {
            if total <= self.cap_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                tracing::debug!(?path, "evicted a cached block");
                total = total.saturating_sub(size);
            }
        }
    }
}

/// Gathers every cache entry under `root` with its size and mtime.
fn collect_entries(root: &Path, out: &mut Vec<(std::time::SystemTime, u64, PathBuf)>) {
    let Ok(children) = std::fs::read_dir(root) else {
        return;
    };
    for child in children.flatten() {
        let path = child.path();
        if path.is_dir() {
            collect_entries(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "cxb")
            && let Ok(meta) = child.metadata()
            && let Ok(modified) = meta.modified()
        {
            out.push((modified, meta.len(), path));
        }
    }
}

// --- the fixed-layout header -------------------------------------------------
//
// magic(8) version(4) seed(8) x(4) z(4) fingerprint(8) cells(8)   = 44 key bytes
// then the stage reports: erosion 20 + thermal 36 + carve 28       = 84 bytes
//
// The first version said 76 here, miscounting the report fields. The `put`
// helper bounds-checks, so the overflow silently dropped the last field, the
// reader hit the short end and treated every entry as corrupt — the fail-safe
// design turned an arithmetic slip into loud misses instead of wrong terrain,
// which is the behaviour it exists for. The assertion below keeps the count
// honest against future report changes.
const HEADER_KEY_BYTES: usize = 44;
const REPORT_BYTES: usize = (4 + 4 + 4 + 8) + (4 + 8 + 8 + 8 + 8) + (8 + 8 + 4 + 8);
const HEADER_BYTES: usize = HEADER_KEY_BYTES + REPORT_BYTES;
const FNV_SEED: u64 = 0xcbf2_9ce4_8422_2325;

fn header_for(seed: u64, block: BlockCoord, settings: WorldSettings) -> [u8; HEADER_BYTES] {
    let mut header = [0u8; HEADER_BYTES];
    let mut at = 0usize;
    let mut put = |bytes: &[u8]| {
        if let Some(slot) = header.get_mut(at..at + bytes.len()) {
            slot.copy_from_slice(bytes);
        }
        at += bytes.len();
    };
    put(MAGIC);
    put(&GENERATOR_VERSION.to_le_bytes());
    put(&seed.to_le_bytes());
    put(&block.x.to_le_bytes());
    put(&block.z.to_le_bytes());
    put(&settings.fingerprint().to_le_bytes());
    put(&(CELLS as u64).to_le_bytes());
    header
}

fn write_report(header: &mut [u8; HEADER_BYTES], report: &BlockReport) {
    let mut at = HEADER_KEY_BYTES;
    let mut put = |header: &mut [u8; HEADER_BYTES], bytes: &[u8]| {
        if let Some(slot) = header.get_mut(at..at + bytes.len()) {
            slot.copy_from_slice(bytes);
        }
        at += bytes.len();
    };
    let e = &report.erosion;
    put(header, &e.rounds.to_le_bytes());
    put(header, &e.mean_lowering.to_le_bytes());
    put(header, &e.deepest.to_le_bytes());
    put(header, &(e.interior_sinks as u64).to_le_bytes());
    let t = &report.thermal;
    put(header, &t.rounds.to_le_bytes());
    put(header, &t.moved.to_le_bytes());
    put(header, &t.net_change.to_le_bytes());
    put(header, &(t.over_steep as u64).to_le_bytes());
    put(header, &t.excess.to_le_bytes());
    let c = &report.carve;
    put(header, &(c.channel_cells as u64).to_le_bytes());
    put(header, &(c.carved_cells as u64).to_le_bytes());
    put(header, &c.deepest.to_le_bytes());
    put(header, &(c.interior_sinks as u64).to_le_bytes());
}

fn read_report(header: &[u8; HEADER_BYTES]) -> Option<BlockReport> {
    let mut at = HEADER_KEY_BYTES;
    let mut take = |n: usize| -> Option<&[u8]> {
        let slice = header.get(at..at + n)?;
        at += n;
        Some(slice)
    };
    let read_u32 = |bytes: &[u8]| Some(u32::from_le_bytes(bytes.try_into().ok()?));
    let read_f32 = |bytes: &[u8]| Some(f32::from_le_bytes(bytes.try_into().ok()?));
    let read_f64 = |bytes: &[u8]| Some(f64::from_le_bytes(bytes.try_into().ok()?));
    let read_u64 = |bytes: &[u8]| Some(u64::from_le_bytes(bytes.try_into().ok()?));

    Some(BlockReport {
        erosion: crate::hydraulic::ErosionReport {
            rounds: read_u32(take(4)?)?,
            mean_lowering: read_f32(take(4)?)?,
            deepest: read_f32(take(4)?)?,
            interior_sinks: read_u64(take(8)?)? as usize,
        },
        thermal: crate::thermal::ThermalReport {
            rounds: read_u32(take(4)?)?,
            moved: read_f64(take(8)?)?,
            net_change: read_f64(take(8)?)?,
            over_steep: read_u64(take(8)?)? as usize,
            excess: read_f64(take(8)?)?,
        },
        carve: crate::carve::CarveReport {
            channel_cells: read_u64(take(8)?)? as usize,
            carved_cells: read_u64(take(8)?)? as usize,
            deepest: read_f32(take(4)?)?,
            interior_sinks: read_u64(take(8)?)? as usize,
        },
    })
}

/// FNV-1a over a byte slice, continuing from `state`.
fn fnv(bytes: &[u8], state: u64) -> u64 {
    let mut hash = state;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydraulic::ErosionSettings;
    use crate::thermal::ThermalSettings;

    const SEED: u64 = 0x0BADC0DE;

    /// One erosion round: these tests are about the cache, not the terrain.
    fn fast() -> WorldSettings {
        WorldSettings {
            erosion: ErosionSettings {
                rounds: 1,
                ..ErosionSettings::DEFAULT
            },
            thermal: ThermalSettings {
                rounds: 1,
                ..ThermalSettings::DEFAULT
            },
            ..WorldSettings::default()
        }
    }

    fn scratch_cache(name: &str) -> BlockCache {
        let root = std::env::temp_dir()
            .join("cx-worldgen-cache-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        BlockCache::new(root)
    }

    /// **M2's exit criterion**: delete the cache, replay, identical world.
    ///
    /// Proven as bit-equality in both directions: a loaded block matches the
    /// block that was stored, and a freshly regenerated block matches the
    /// loaded one. Together they are the criterion — the cache is a shortcut
    /// to the generator, never a second source of truth.
    #[test]
    fn a_cached_block_and_a_regenerated_block_are_identical() {
        let cache = scratch_cache("roundtrip");
        let settings = fast();

        let original = generate_block(SEED, BlockCoord::new(1, 2), settings);
        cache
            .store(SEED, settings, &original)
            .expect("the scratch directory is writable");

        let loaded = cache
            .load(SEED, BlockCoord::new(1, 2), settings)
            .expect("just stored");

        assert_eq!(loaded.terrain.as_slice(), original.terrain.as_slice());
        assert_eq!(loaded.ground.as_slice(), original.ground.as_slice());
        assert_eq!(loaded.report, original.report);

        // Delete the cache and replay: same world again.
        let _ = std::fs::remove_dir_all(&cache.root);
        let replayed = generate_block(SEED, BlockCoord::new(1, 2), settings);
        assert_eq!(replayed.terrain.as_slice(), loaded.terrain.as_slice());
    }

    /// Any key mismatch is a miss: seed, block, settings, or pipeline version.
    #[test]
    fn a_mismatched_key_never_serves_a_block() {
        let cache = scratch_cache("keys");
        let settings = fast();

        let block = generate_block(SEED, BlockCoord::new(0, 0), settings);
        cache
            .store(SEED, settings, &block)
            .expect("the scratch directory is writable");

        assert!(
            cache
                .load(SEED + 1, BlockCoord::new(0, 0), settings)
                .is_none()
        );
        assert!(cache.load(SEED, BlockCoord::new(0, 1), settings).is_none());

        let other = WorldSettings {
            erosion: ErosionSettings {
                erodibility: 5.0e-5,
                ..fast().erosion
            },
            ..fast()
        };
        assert!(cache.load(SEED, BlockCoord::new(0, 0), other).is_none());
        assert_ne!(
            settings.fingerprint(),
            other.fingerprint(),
            "two different settings share a fingerprint, so they would share \
             cache entries"
        );
    }

    /// A flipped bit is a miss, not wrong terrain.
    #[test]
    fn corruption_is_a_miss_rather_than_wrong_terrain() {
        let cache = scratch_cache("corruption");
        let settings = fast();

        let block = generate_block(SEED, BlockCoord::new(0, 0), settings);
        cache
            .store(SEED, settings, &block)
            .expect("the scratch directory is writable");
        let path = cache.entry_path(SEED, BlockCoord::new(0, 0), settings);

        // Flip one bit in the middle of the ground data.
        let mut bytes = std::fs::read(&path).expect("just written");
        let middle = bytes.len() / 2;
        if let Some(byte) = bytes.get_mut(middle) {
            *byte ^= 0x10;
        }
        std::fs::write(&path, &bytes).expect("writable");

        assert!(
            cache.load(SEED, BlockCoord::new(0, 0), settings).is_none(),
            "a corrupted entry was served as terrain"
        );

        // And a truncated file likewise.
        bytes.truncate(bytes.len() / 3);
        std::fs::write(&path, &bytes).expect("writable");
        assert!(cache.load(SEED, BlockCoord::new(0, 0), settings).is_none());
    }

    /// The cap evicts the oldest entries first, and only past the cap.
    #[test]
    fn eviction_removes_the_oldest_entries_past_the_cap() {
        let settings = fast();
        // Cap sized for roughly two entries, so storing three evicts one.
        let one_entry = (CELLS * size_of::<f32>() + HEADER_BYTES + 8) as u64;
        let root = std::env::temp_dir()
            .join("cx-worldgen-cache-tests")
            .join(format!("eviction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cache = BlockCache::with_cap(&root, one_entry * 2 + 1024);

        for (index, coord) in [(0, 0), (1, 0), (2, 0)].iter().enumerate() {
            let block = generate_block(SEED, BlockCoord::new(coord.0, coord.1), settings);
            cache
                .store(SEED, settings, &block)
                .expect("the scratch directory is writable");
            // Ensure mtimes are distinguishable even on coarse filesystems.
            let path = cache.entry_path(SEED, BlockCoord::new(coord.0, coord.1), settings);
            let time = std::fs::FileTimes::new().set_modified(
                std::time::SystemTime::UNIX_EPOCH
                    + std::time::Duration::from_secs(1_000_000 + index as u64 * 100),
            );
            if let Ok(file) = std::fs::File::options().append(true).open(&path) {
                let _ = file.set_times(time);
            }
            cache.evict_past_cap();
        }

        assert!(
            cache.load(SEED, BlockCoord::new(0, 0), settings).is_none(),
            "the oldest entry should have been evicted"
        );
        assert!(
            cache.load(SEED, BlockCoord::new(2, 0), settings).is_some(),
            "the newest entry should have survived"
        );
    }

    /// `get_or_generate` serves from disk on the second call.
    #[test]
    fn the_second_fetch_comes_from_the_cache() {
        let cache = scratch_cache("fetch");
        let settings = fast();

        let first = cache.get_or_generate(SEED, BlockCoord::new(3, 3), settings);
        let path = cache.entry_path(SEED, BlockCoord::new(3, 3), settings);
        assert!(
            path.exists(),
            "the first fetch should have populated the cache"
        );

        let second = cache.get_or_generate(SEED, BlockCoord::new(3, 3), settings);
        assert_eq!(first.terrain.as_slice(), second.terrain.as_slice());
    }
}
