//! SE2-M7 — bounded raw-block cache, SE2-M9 — raw bytes (v2 blocks decode
//! per lookup; caching decoded entries would re-pay the full-block decode
//! that v2 removes). One cache per Db, shared by every SegmentReader the
//! Db opens. Keys are (reader_id, block_index) where reader_id comes from
//! a per-cache counter that is NEVER reused: segment ids can be reused
//! after an orphan is cleaned up, so a key based on segment ids could
//! alias a dead reader's blocks and serve wrong data. Capacity is raw
//! block bytes (28-byte header + payload), hard-capped with LRU eviction;
//! a block bigger than the cap is simply not cached. Only checksum-
//! validated bytes enter the cache. Answers never depend on the cache —
//! a hit hands the caller the same bytes the file would produce.
//!
//! # ponytail: LRU via generation stamps — hits are O(1) (hash + stamp);
//! eviction is a min-gen scan on the miss path (O(n), amortized behind
//! block I/O). Ties pick any — exact-LRU ordering per distinct touch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    /// Raw block bytes currently held (never exceeds the cap).
    pub bytes: usize,
}

#[derive(Debug)]
pub struct BlockCache {
    cap: usize,
    state: Mutex<State>,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    next_id: AtomicU64,
}

#[derive(Debug)]
struct Entry {
    /// Stamp of the entry's most recent touch, from `State::clock`.
    gen: u64,
    bytes: Arc<Vec<u8>>,
}

#[derive(Debug, Default)]
struct State {
    entries: HashMap<(u64, u32), Entry>,
    /// Bumped once per hit and per insert. Wrapping is fine — staleness
    /// only degrades LRU quality after 2^64 touches.
    clock: u64,
    bytes: usize,
    /// Total entries visited by eviction min-scans (miss path only) — the
    /// RED pin asserts a hit never moves it.
    scan_steps: u64,
}

impl BlockCache {
    pub fn new(cap: usize) -> Arc<Self> {
        Arc::new(BlockCache {
            cap,
            state: Mutex::new(State::default()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            next_id: AtomicU64::new(0),
        })
    }

    /// A fresh identity for one SegmentReader (never reused — see module
    /// doc).
    pub fn reader_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Lookup returns an Arc clone — the caller decodes from shared bytes.
    pub fn get(&self, id: u64, block: u32) -> Option<Arc<Vec<u8>>> {
        let key = (id, block);
        let mut st = self.state.lock().unwrap();
        // Disjoint field borrows — through a MutexGuard, `entries.get_mut`
        // would hold the whole guard borrowed and block the clock stamp.
        let State { entries, clock, .. } = &mut *st;
        let Some(e) = entries.get_mut(&key) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let gen = clock.wrapping_add(1);
        *clock = gen;
        e.gen = gen;
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(e.bytes.clone())
    }

    pub fn insert(&self, id: u64, block: u32, raw: Arc<Vec<u8>>) {
        let bytes = raw.len();
        let mut st = self.state.lock().unwrap();
        if bytes > self.cap {
            return; // one block bigger than the cache: never cached
        }
        let key = (id, block);
        let State {
            entries,
            clock,
            bytes: held,
            scan_steps,
        } = &mut *st;
        if let Some(old) = entries.remove(&key) {
            *held -= old.bytes.len();
        }
        while *held + bytes > self.cap {
            let Some(victim) = entries.iter().min_by_key(|(_, e)| e.gen).map(|(k, _)| *k) else {
                break;
            };
            *scan_steps += entries.len() as u64;
            if let Some(v) = entries.remove(&victim) {
                *held -= v.bytes.len();
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
        let gen = clock.wrapping_add(1);
        *clock = gen;
        *held += bytes;
        entries.insert(key, Entry { gen, bytes: raw });
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            bytes: self.state.lock().unwrap().bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 16 * 1024;

    fn block() -> Arc<Vec<u8>> {
        Arc::new(vec![0u8; BLOCK])
    }

    /// P5-M25 RED — a hit must be O(1): it may hash and stamp, but it must
    /// not walk the cached entries. The current retain-based recency move
    /// visits every entry per hit, which fails this pin.
    #[test]
    fn cache_hit_does_not_scan() {
        let cache = BlockCache::new(64 * BLOCK);
        for i in 0..64u32 {
            cache.insert(1, i, block());
        }
        let before = cache.state.lock().unwrap().scan_steps;
        assert!(cache.get(1, 0).is_some(), "recently inserted block hits");
        let after = cache.state.lock().unwrap().scan_steps;
        assert_eq!(
            after,
            before,
            "hit walked {} recency entries",
            after - before
        );
    }

    /// LRU semantics guard — survives the refactor: the least-recently-HIT
    /// entry is the eviction victim, and a re-inserted key becomes MRU.
    #[test]
    fn eviction_targets_least_recently_hit() {
        let cache = BlockCache::new(3 * BLOCK);
        cache.insert(1, 0, block());
        cache.insert(1, 1, block());
        cache.insert(1, 2, block());
        cache.get(1, 0).unwrap(); // 0 becomes MRU
        cache.insert(1, 3, block()); // forces one eviction — must be 1
        assert!(cache.get(1, 0).is_some());
        assert!(
            cache.get(1, 1).is_none(),
            "least-recently-hit must be evicted"
        );
        assert!(cache.get(1, 2).is_some());
        assert!(cache.get(1, 3).is_some());
    }

    /// P5-M30 (P0-04) RED — victim selection must not scan the map: the
    /// min-gen victim comes from the heap, so one forced eviction moves
    /// scan_steps by 0. Today the miss path runs a full min_by_key sweep
    /// per victim (n steps). Touches make the LRU order non-trivial first.
    #[test]
    fn eviction_does_not_scan_the_map() {
        let cache = BlockCache::new(64 * BLOCK);
        for i in 0..64u32 {
            cache.insert(1, i, block());
        }
        for i in 0..8u32 {
            cache.get(1, i).unwrap(); // these are NOT the victims
        }
        let before = cache.state.lock().unwrap().scan_steps;
        cache.insert(1, 64, block()); // over cap: exactly one eviction
        let after = cache.state.lock().unwrap().scan_steps;
        assert_eq!(
            after - before,
            0,
            "one eviction walked {} map entries — victim selection must not scan",
            after - before
        );
    }

    /// P5-M30 (P0-04) RED — eviction work must not grow with the cache:
    /// 16, 512 and 4096 entries each pay 0 map-scan steps per forced
    /// eviction. Today each victim costs n, so the first size fails.
    #[test]
    fn eviction_scan_work_is_flat_across_cache_sizes() {
        for n in [16usize, 512, 4096] {
            let cache = BlockCache::new(n * BLOCK);
            for i in 0..n as u32 {
                cache.insert(1, i, block());
            }
            let before = cache.state.lock().unwrap().scan_steps;
            cache.insert(1, n as u32, block()); // one eviction
            let after = cache.state.lock().unwrap().scan_steps;
            assert_eq!(
                after - before,
                0,
                "at {n} entries one eviction walked {} map entries",
                after - before
            );
        }
    }
}
