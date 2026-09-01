//! SE2-M7 — bounded decoded-block cache. One cache per Db, shared by every
//! SegmentReader the Db opens. Keys are (reader_id, block_index) where
//! reader_id comes from a per-cache counter that is NEVER reused: segment
//! ids can be reused after an orphan is cleaned up, so a key based on
//! segment ids could alias a dead reader's blocks and serve wrong data.
//! Capacity is decoded entry bytes (key + value + seq/flags), hard-capped
//! with LRU eviction; a block bigger than the cap is simply not cached.
//! Answers never depend on the cache — a hit returns a clone of the same
//! entries the file would produce.
//!
//! # ponytail: O(n) recency move per hit (n = cached blocks, ~128 at
//! 8 MiB / 64 KiB blocks) — swap for a linked LRU if blocks get tiny.

use crate::segment::SegmentEntry;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    /// Decoded entry bytes currently held (never exceeds the cap).
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

#[derive(Debug, Default)]
struct State {
    entries: HashMap<(u64, u32), Vec<SegmentEntry>>,
    recency: VecDeque<(u64, u32)>,
    bytes: usize,
}

/// Decoded size of the entries — key + value + seq(8) + flags(1).
fn entry_bytes(entries: &[SegmentEntry]) -> usize {
    entries
        .iter()
        .map(|e| e.key.len() + e.value.len() + 9)
        .sum()
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

    /// Lookup returns a clone: the caller consumes the Vec.
    pub fn get(&self, id: u64, block: u32) -> Option<Vec<SegmentEntry>> {
        let key = (id, block);
        let mut st = self.state.lock().unwrap();
        let Some(e) = st.entries.get(&key) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let hit = e.clone();
        self.hits.fetch_add(1, Ordering::Relaxed);
        st.recency.retain(|k| k != &key);
        st.recency.push_back(key);
        Some(hit)
    }

    pub fn insert(&self, id: u64, block: u32, entries: Vec<SegmentEntry>) {
        let bytes = entry_bytes(&entries);
        let mut st = self.state.lock().unwrap();
        if bytes > self.cap {
            return; // one block bigger than the cache: never cached
        }
        let key = (id, block);
        if let Some(old) = st.entries.remove(&key) {
            st.bytes -= entry_bytes(&old);
            st.recency.retain(|k| k != &key);
        }
        while st.bytes + bytes > self.cap {
            let Some(victim) = st.recency.pop_front() else {
                break;
            };
            if let Some(v) = st.entries.remove(&victim) {
                st.bytes -= entry_bytes(&v);
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
        st.bytes += bytes;
        st.entries.insert(key, entries);
        st.recency.push_back(key);
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
