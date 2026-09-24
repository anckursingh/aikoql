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
//! # ponytail: LRU via generation stamps + a lazy-deletion min-heap on
//! gen — hits stay O(1) (hash + stamp only); eviction pops the min-gen
//! heap node (O(log n)) and discards stale nodes (a key touched since
//! its push, or already gone). The heap rebuilds when it doubles past
//! the map, so garbage stays bounded; an intrusive list (O(1) evict,
//! unsafe) is the upgrade path only if a churn profile demands it.
//! Ties pick any — exact-LRU ordering per distinct touch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

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
    /// P5-M45 (R4-P1-06) — mutex-wait accumulation for the concurrency
    /// cells, gated: off (the default), one relaxed flag load per lock
    /// and no clock reads. The M45 harness enables it around its matrix.
    wait_enabled: AtomicBool,
    wait_ns: AtomicU64,
}

#[derive(Debug)]
struct Entry {
    /// Stamp of the entry's most recent touch, from `State::clock`.
    gen: u64,
    bytes: Arc<Vec<u8>>,
}

/// P5-M30 — one lazy-deletion heap node: the gen its key had when
/// pushed. A pop compares against the live entry's gen — equal means
/// the node is current (evict); lower means the key was touched since
/// (refresh the node in place); a missing entry means the key is gone
/// (drop the garbage).
#[derive(Debug, Clone, Copy)]
struct HeapNode {
    gen: u64,
    key: (u64, u32),
}

#[derive(Debug, Default)]
struct State {
    entries: HashMap<(u64, u32), Entry>,
    /// Bumped once per hit and per insert. Wrapping is fine — staleness
    /// only degrades LRU quality after 2^64 touches.
    clock: u64,
    bytes: usize,
    /// Total entries visited by eviction min-scans (miss path only) — the
    /// RED pins assert a hit never moves it and an eviction never moves
    /// it; only a heap rebuild scans (counted, amortized behind the 2x
    /// trigger).
    scan_steps: u64,
    /// Min-heap on gen; len <= 2x entries.len() (rebuild trigger).
    heap: Vec<HeapNode>,
}

impl State {
    fn heap_push(&mut self, gen: u64, key: (u64, u32)) {
        self.heap.push(HeapNode { gen, key });
        let mut i = self.heap.len() - 1;
        while i > 0 {
            let parent = (i - 1) / 2;
            if self.heap[parent].gen <= self.heap[i].gen {
                break;
            }
            self.heap.swap(parent, i);
            i = parent;
        }
    }

    fn sift_down(&mut self, mut i: usize) {
        loop {
            let l = 2 * i + 1;
            let r = l + 1;
            let mut smallest = i;
            if l < self.heap.len() && self.heap[l].gen < self.heap[smallest].gen {
                smallest = l;
            }
            if r < self.heap.len() && self.heap[r].gen < self.heap[smallest].gen {
                smallest = r;
            }
            if smallest == i {
                break;
            }
            self.heap.swap(i, smallest);
            i = smallest;
        }
    }

    fn heap_pop(&mut self) -> Option<HeapNode> {
        let mut last = self.heap.pop()?;
        if self.heap.is_empty() {
            return Some(last);
        }
        std::mem::swap(&mut self.heap[0], &mut last);
        self.sift_down(0);
        Some(last)
    }

    /// Pops min-gen nodes until `needed` bytes fit (or the heap is
    /// exhausted — unreachable: every live key keeps at least one node).
    /// A stale node is re-pushed with the live gen, so it can be popped
    /// again; each pop either frees bytes or moves a node's gen forward,
    /// so the loop terminates without a scan. Returns victims evicted.
    fn evict_for(&mut self, cap: usize, needed: usize) -> u64 {
        let mut evicted = 0;
        while self.bytes + needed > cap {
            let Some(node) = self.heap_pop() else { break };
            match self.entries.get(&node.key) {
                None => {} // key already gone: strand from an overwrite
                Some(e) if e.gen == node.gen => {
                    if let Some(v) = self.entries.remove(&node.key) {
                        self.bytes -= v.bytes.len();
                        evicted += 1;
                    }
                }
                Some(e) => self.heap_push(e.gen, node.key), // touched since push
            }
        }
        evicted
    }

    /// Rebuilds the heap from the live entries (O(n) heapify) — the
    /// rebuild itself is the one map scan on the evict path.
    fn rebuild_heap(&mut self) {
        self.heap.clear();
        for (&key, e) in &self.entries {
            self.heap.push(HeapNode { gen: e.gen, key });
        }
        for i in (0..self.heap.len() / 2).rev() {
            self.sift_down(i);
        }
        self.scan_steps += self.entries.len() as u64;
    }
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
            wait_enabled: AtomicBool::new(false),
            wait_ns: AtomicU64::new(0),
        })
    }

    /// P5-M45 cells — enables mutex-wait accumulation. Off by default;
    /// when off a lock pays one relaxed flag load and no clock read.
    pub fn set_wait_tracking(&self, on: bool) {
        self.wait_enabled.store(on, Ordering::Relaxed);
    }

    /// P5-M45 cells — total ns spent waiting on the state mutex since
    /// tracking was enabled (0 when tracking is off).
    pub fn wait_ns(&self) -> u64 {
        self.wait_ns.load(Ordering::Relaxed)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        if self.wait_enabled.load(Ordering::Relaxed) {
            let t0 = Instant::now();
            let guard = self.state.lock().unwrap();
            self.wait_ns
                .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
            guard
        } else {
            self.state.lock().unwrap()
        }
    }

    /// A fresh identity for one SegmentReader (never reused — see module
    /// doc).
    pub fn reader_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Lookup returns an Arc clone — the caller decodes from shared bytes.
    pub fn get(&self, id: u64, block: u32) -> Option<Arc<Vec<u8>>> {
        let key = (id, block);
        let mut st = self.lock();
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
        let mut st = self.lock();
        if bytes > self.cap {
            return; // one block bigger than the cache: never cached
        }
        let key = (id, block);
        if let Some(old) = st.entries.remove(&key) {
            st.bytes -= old.bytes.len(); // its heap node pops as garbage
        }
        let n = st.evict_for(self.cap, bytes);
        if n > 0 {
            self.evictions.fetch_add(n, Ordering::Relaxed);
        }
        let gen = st.clock.wrapping_add(1);
        st.clock = gen;
        st.bytes += bytes;
        st.entries.insert(key, Entry { gen, bytes: raw });
        st.heap_push(gen, key);
        if st.heap.len() > 2 * st.entries.len() {
            st.rebuild_heap();
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            bytes: self.lock().bytes,
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

    /// P5-M30 (P0-04) — the lazy-deletion heap's garbage stays bounded:
    /// overwrites strand one stale node each, and the 2x rebuild trigger
    /// keeps heap nodes within 2x the live entries.
    #[test]
    fn heap_garbage_stays_bounded_under_churn() {
        let cache = BlockCache::new(64 * BLOCK);
        for i in 0..64u32 {
            cache.insert(1, i, block());
        }
        for i in 0..32u32 {
            cache.insert(1, i, block()); // overwrite: strands a stale node
        }
        for i in 64..96u32 {
            cache.insert(1, i, block()); // evictions through the garbage
        }
        let st = cache.state.lock().unwrap();
        assert!(
            st.heap.len() <= 2 * st.entries.len(),
            "heap {} vs map {} — garbage outgrew the rebuild trigger",
            st.heap.len(),
            st.entries.len()
        );
        assert_eq!(st.entries.len(), 64, "the cap still holds");
    }
}
