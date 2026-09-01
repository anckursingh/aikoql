//! SE2-M2 — memtable: the in-memory recent-write layer (design §10). A
//! BTreeMap on (key, seq) — deterministic, the doc's own order. The head
//! for a key is its highest-seq entry; a None value is a delete tombstone.
//! The byte accounting is approximate (reported, never asserted).

use std::collections::BTreeMap;

/// Approximate in-memory cost of one entry: key + value + seq + map node.
const ENTRY_OVERHEAD: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemEntry {
    /// None = delete tombstone.
    pub value: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct Memtable {
    map: BTreeMap<(Vec<u8>, u64), MemEntry>,
    bytes: usize,
}

impl Memtable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn apply(&mut self, key: Vec<u8>, seq: u64, value: Option<Vec<u8>>) {
        self.bytes += key.len() + value.as_ref().map_or(0, Vec::len) + ENTRY_OVERHEAD;
        self.map.insert((key, seq), MemEntry { value });
    }

    /// Head for a key: the highest-seq entry (BTreeMap order is key asc,
    /// seq asc, so the last in the key's range is the head).
    pub fn get(&self, key: &[u8]) -> Option<&MemEntry> {
        self.map
            .range((key.to_vec(), 0)..=(key.to_vec(), u64::MAX))
            .next_back()
            .map(|(_, e)| e)
    }

    /// All entries in (key, seq) order — the flush order.
    pub fn entries(&self) -> impl Iterator<Item = (&[u8], u64, &MemEntry)> {
        self.map.iter().map(|((k, s), e)| (k.as_slice(), *s, e))
    }
}
