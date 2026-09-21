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

    /// M31 (P0-03) mtr003 — prefix_heads parity across the refactor: key
    /// asc, the newest BYTE row per key (a newer object row must not
    /// shadow it), object-only keys skipped. Green on the flat map today;
    /// it guards the chain shape's scan.
    #[test]
    fn prefix_heads_parity_under_the_chain_shape() {
        let mut m = Memtable::new();
        m.apply(b"aa".to_vec(), 1, Some(b"a1".to_vec()));
        m.apply(b"aa".to_vec(), 2, Some(b"a2".to_vec()));
        m.apply_object(b"aa".to_vec(), 3, Some(b"obj".to_vec()), rid(7));
        m.apply(b"ab".to_vec(), 1, Some(b"b1".to_vec()));
        m.apply_object(b"ac".to_vec(), 1, Some(b"c-obj".to_vec()), rid(7));
        m.apply(b"ba".to_vec(), 1, Some(b"x".to_vec())); // outside the prefix
        let got: Vec<(String, Option<String>)> = m
            .prefix_heads(b"a")
            .map(|(k, r)| {
                (
                    String::from_utf8_lossy(k).into_owned(),
                    r.value.as_deref().map(|v| String::from_utf8_lossy(v).into_owned()),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("aa".to_string(), Some("a2".to_string())),
                ("ab".to_string(), Some("b1".to_string())),
            ],
            "key asc, newest byte row per key, object-only keys invisible"
        );
    }

    /// M31 (P0-03) mtr004 — the flat map accepted ANY (key, seq) order and
    /// a re-apply of the same seq REPLACED; the chain shape must keep both
    /// (out-of-order arrivals binary-search their place). Guards the
    /// semantics under high cardinality + version-heavy + object mixes.
    #[test]
    fn out_of_order_seqs_and_replace_match_the_flat_map_semantics() {
        let mut m = Memtable::new();
        m.apply(b"k".to_vec(), 5, Some(b"v5".to_vec()));
        m.apply(b"k".to_vec(), 2, Some(b"v2".to_vec())); // arrives late
        m.apply(b"k".to_vec(), 7, Some(b"v7".to_vec()));
        m.apply(b"k".to_vec(), 7, Some(b"v7b".to_vec())); // same (key, seq): replaces
        m.apply_object(b"k".to_vec(), 6, Some(b"o6".to_vec()), rid(7));
        m.apply_object(b"k".to_vec(), 8, Some(b"o8".to_vec()), rid(7));
        assert_eq!(
            m.get(b"k").and_then(|e| e.value.as_deref()),
            Some(b"v7b".as_slice()),
            "the byte head is the highest-seq byte row, with replace"
        );
        assert_eq!(
            m.get_by_rid(b"k", rid(7)).and_then(|e| e.value.as_deref()),
            Some(b"o8".as_slice()),
            "the object head is the highest-seq matching object row"
        );
        let order: Vec<(String, u64)> = m
            .entries()
            .map(|(k, s, _)| (String::from_utf8_lossy(k).into_owned(), s))
            .collect();
        assert_eq!(
            order,
            vec![
                ("k".to_string(), 2),
                ("k".to_string(), 5),
                ("k".to_string(), 6),
                ("k".to_string(), 7),
                ("k".to_string(), 8),
            ],
            "the flush stream is key asc, seq asc within key"
        );
        let owned: Vec<(String, u64)> = m
            .into_entries()
            .map(|((k, s), _)| (String::from_utf8_lossy(&k).into_owned(), s))
            .collect();
        assert_eq!(owned, order, "into_entries moves in the same order");

        // High-cardinality interleave: 1k keys x 2 versions stays exact.
        let mut big = Memtable::new();
        for i in 0..1_000u32 {
            let key = format!("k{i:04}").into_bytes();
            big.apply(key.clone(), 1, Some(vec![i as u8]));
            big.apply(key, 2, Some(vec![i as u8]));
        }
        assert_eq!(
            big.get(b"k0999").and_then(|e| e.value.as_deref()),
            Some([231u8].as_slice()),
            "999 as u8 = 231; both versions present, head = seq 2"
        );
        assert_eq!(big.entries().count(), 2_000);
    }
}
