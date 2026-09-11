//! SE2-M2 — memtable: the in-memory recent-write layer (design §10). A
//! BTreeMap on (key, seq) — deterministic, the doc's own order. The head
//! for a key is its highest-seq entry; a None value is a delete tombstone.
//! The byte accounting is approximate (reported, never asserted).

use crate::identity::ReplicaId;
use std::collections::BTreeMap;

/// Approximate in-memory cost of one entry: key + value + seq + map node.
const ENTRY_OVERHEAD: usize = 24;

/// SE2-M2 — a byte-API row. TDD-STOR-006 (P4-M7) type-level pin: there is
/// NO `replica_id` field here, so a byte row can never be constructed with
/// (or answer reads as) an identity row — the byte surface simply cannot
/// name one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteRow {
    /// None = delete tombstone.
    pub value: Option<Vec<u8>>,
}

/// SE2-M33 — an object row: carries the owning replica (the §17 target
/// shape, the persisted relocation handle — the oid→lid map stays in the
/// directory). Entries of other replicas — and byte rows — are another
/// layer's rows (§11) and never answer an object read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRow {
    /// None = delete tombstone.
    pub value: Option<Vec<u8>>,
    pub replica_id: ReplicaId,
}

/// One memtable entry — a byte row or an identity row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemEntry {
    Byte(ByteRow),
    Object(ObjectRow),
}

impl MemEntry {
    pub fn value(&self) -> Option<&Vec<u8>> {
        match self {
            MemEntry::Byte(b) => b.value.as_ref(),
            MemEntry::Object(o) => o.value.as_ref(),
        }
    }

    /// 0 for byte rows — they carry no identity by construction.
    pub fn replica_id(&self) -> ReplicaId {
        match self {
            MemEntry::Byte(_) => ReplicaId(0),
            MemEntry::Object(o) => o.replica_id,
        }
    }

    /// SE2-M34 — any identity-carrying entry: the flush writes v4 blocks
    /// iff this is true (rid-0 rows alone stay v2, byte-identical to M9).
    pub fn is_identity(&self) -> bool {
        matches!(self, MemEntry::Object(o) if o.replica_id != ReplicaId(0))
    }

    /// SE2-M15 — moved-out parts for the flush writer, no clones.
    pub fn into_parts(self) -> (Option<Vec<u8>>, ReplicaId) {
        match self {
            MemEntry::Byte(b) => (b.value, ReplicaId(0)),
            MemEntry::Object(o) => (o.value, o.replica_id),
        }
    }
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

    /// SE2-M34 — any identity-carrying entry: the flush writes v4 blocks
    /// iff this is true (rid-0 rows alone stay v2, byte-identical to M9).
    pub fn has_identity(&self) -> bool {
        self.map.values().any(MemEntry::is_identity)
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn apply(&mut self, key: Vec<u8>, seq: u64, value: Option<Vec<u8>>) {
        self.bytes += key.len() + value.as_ref().map_or(0, Vec::len) + ENTRY_OVERHEAD;
        self.map
            .insert((key, seq), MemEntry::Byte(ByteRow { value }));
    }

    /// SE2-M33 — an object write: the entry carries the owning replica id
    /// (the §17 target shape), so the identity read path can filter the
    /// object's own entries out of the key's run.
    pub fn apply_object(
        &mut self,
        key: Vec<u8>,
        seq: u64,
        value: Option<Vec<u8>>,
        replica_id: ReplicaId,
    ) {
        self.bytes += key.len() + value.as_ref().map_or(0, Vec::len) + ENTRY_OVERHEAD;
        self.map.insert(
            (key, seq),
            MemEntry::Object(ObjectRow { value, replica_id }),
        );
    }

    /// Head for a key on the BYTE surface: the highest-seq byte row
    /// (BTreeMap order is key asc, seq asc, so the last byte row of the
    /// run is the head). Object rows are invisible here — the byte API
    /// cannot answer object reads (TDD-STOR-006). SE2-M10 — ONE allocation
    /// (the owned range start): `RangeFrom` + take-while covers the key's
    /// run without building both bounds. (`last()`, not `next_back()`:
    /// TakeWhile's DoubleEndedIterator impl needs an ExactSize inner, which
    /// a BTreeMap range is not.)
    pub fn get(&self, key: &[u8]) -> Option<&ByteRow> {
        self.map
            .range((key.to_vec(), 0)..)
            .take_while(|((k, _), _)| k.as_slice() == key)
            .filter_map(|(_, e)| match e {
                MemEntry::Byte(b) => Some(b),
                MemEntry::Object(_) => None,
            })
            .last()
    }

    /// SE2-M33 — the object's head: the newest entry in the key's run whose
    /// replica_id matches (the run is seq-ascending, so the last match is
    /// the head). Byte rows carry no identity and never answer an object
    /// read — structural (TDD-STOR-006), not a filter.
    pub fn get_by_rid(&self, key: &[u8], rid: ReplicaId) -> Option<&ObjectRow> {
        self.map
            .range((key.to_vec(), 0)..)
            .take_while(|((k, _), _)| k.as_slice() == key)
            .filter_map(|(_, e)| match e {
                MemEntry::Object(o) if o.replica_id == rid => Some(o),
                _ => None,
            })
            .last()
    }

    /// All entries in (key, seq) order — the flush order.
    pub fn entries(&self) -> impl Iterator<Item = (&[u8], u64, &MemEntry)> {
        self.map.iter().map(|((k, s), e)| (k.as_slice(), *s, e))
    }

    /// All entries in (key, seq) order, moved out — the table is consumed,
    /// so the flush takes keys and values by move instead of cloning
    /// (SE2-M15).
    pub fn into_entries(self) -> impl Iterator<Item = ((Vec<u8>, u64), MemEntry)> {
        self.map.into_iter()
    }

    /// V2-Adopt — one entry per key with the prefix, key-ascending: the
    /// highest-seq BYTE row for that key (its head in this layer). Seeks
    /// directly to the prefix range — the kernel's scan contract forbids
    /// walking the whole key space. Keys holding only object rows never
    /// appear (TDD-STOR-006: the byte scan cannot answer object data).
    pub fn prefix_heads<'a>(
        &'a self,
        prefix: &'a [u8],
    ) -> impl Iterator<Item = (&'a [u8], &'a ByteRow)> {
        // SE2-M10 — one allocation: the owned range start moves into the
        // iterator; the closure compares against the borrowed prefix.
        let mut it = self
            .map
            .range((prefix.to_vec(), 0)..)
            .map(|((k, _), e)| (k.as_slice(), e))
            .peekable();
        std::iter::from_fn(move || loop {
            let (first_key, first_e) = it.next()?;
            if !first_key.starts_with(prefix) {
                return None;
            }
            // (key, seq) order is seq-ascending within a key — the last
            // byte row of the run is the byte-API head.
            let mut head: Option<&ByteRow> = None;
            if let MemEntry::Byte(b) = first_e {
                head = Some(b);
            }
            while let Some((next_key, next_e)) = it.peek() {
                if next_key != &first_key {
                    break;
                }
                if let MemEntry::Byte(b) = next_e {
                    head = Some(b);
                }
                it.next();
            }
            if let Some(h) = head {
                return Some((first_key, h));
            }
            // Key holds only object rows — skip it; next key.
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(n: u64) -> ReplicaId {
        ReplicaId(n)
    }

    #[test]
    fn stor006_byte_api_cannot_answer_object_reads() {
        let mut m = Memtable::new();
        // Object-only key: invisible to the byte surface.
        m.apply_object(b"o".to_vec(), 2, Some(b"obj-v2".to_vec()), rid(7));
        let head: Option<&ByteRow> = m.get(b"o"); // the type-level pin: the byte
                                                  // surface can only name a ByteRow
        assert!(head.is_none(), "object-only keys never answer byte reads");
        assert_eq!(
            m.prefix_heads(b"o").count(),
            0,
            "and never appear in byte scans"
        );

        // Interleaved: the newer OBJECT row must not shadow the byte head.
        m.apply(b"k".to_vec(), 1, Some(b"byte-v1".to_vec()));
        m.apply_object(b"k".to_vec(), 2, Some(b"obj-v2".to_vec()), rid(7));
        assert_eq!(
            m.get(b"k").and_then(|e| e.value.as_deref()),
            Some(b"byte-v1".as_slice()),
            "the byte head is the newest BYTE row, not the newer object row"
        );
        assert_eq!(
            m.prefix_heads(b"k").map(|(_, e)| e.value.as_deref()).next(),
            Some(Some(b"byte-v1".as_slice()))
        );

        // The object surface sees only object rows of the exact replica.
        let o: Option<&ObjectRow> = m.get_by_rid(b"k", rid(7));
        assert_eq!(
            o.and_then(|e| e.value.as_deref()),
            Some(b"obj-v2".as_slice())
        );
        assert!(
            m.get_by_rid(b"k", rid(8)).is_none(),
            "other replicas never answer"
        );
        assert!(
            m.get_by_rid(b"k", rid(0)).is_none(),
            "byte rows carry no identity"
        );

        // Byte-only keys are invisible to the object surface.
        m.apply(b"b".to_vec(), 1, Some(b"bv".to_vec()));
        assert!(m.get_by_rid(b"b", rid(7)).is_none());
        assert!(
            m.get_by_rid(b"b", rid(0)).is_none(),
            "rid 0 names no object row"
        );
    }
}
