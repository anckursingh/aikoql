//! P5-M8 (ND-07) — property + composite hash indexes: in-memory equality
//! indexes over a catalog-declared property tuple, maintained asynchronously
//! by the scheduler's IndexMaintainer — never on the commit path. The key is
//! the Debug-derived string of the value tuple (Value is not Hash — the
//! P5-M5 group-key precedent); ANY key property missing → the row is not
//! indexed. Contents replay from the journal (idx2-007) and are persisted
//! in the maintainer checkpoint (P5-M26).

use crate::index::unified::{ConsistencyLevel, Index, IndexState, VerifyReport};
use crate::knowledge::kom::*;
use crate::transaction::kernel::Kernel;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

// AG-21c — order-preserving key bytes. Two values encode equal exactly when
// the runtime Filter calls them equal (Int and Float share one numeric
// domain — T-32's promotion; -0.0 is 0.0), and keys sort in the Filter's
// order within a domain, so one map answers Eq and ranges. Domains sort by
// tag; a range never crosses one (the Filter never orders across types).
const TAG_NULL: u8 = 0;
const TAG_BOOL: u8 = 1;
const TAG_NUM: u8 = 2;
const TAG_TEXT: u8 = 3;
const TAG_BYTES: u8 = 4;
const TAG_OTHER: u8 = 5;

/// Bytes with 0x00 escaped (0x00 0xFF) and a 0x00 0x00 terminator — a
/// shorter string sorts first, and a tuple's fields cannot run together.
fn put_escaped(out: &mut Vec<u8>, bytes: &[u8]) {
    for &b in bytes {
        out.push(b);
        if b == 0 {
            out.push(0xFF);
        }
    }
    out.extend_from_slice(&[0, 0]);
}

fn put_value(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => out.push(TAG_NULL),
        Value::Bool(b) => out.extend_from_slice(&[TAG_BOOL, *b as u8]),
        Value::Int(_) | Value::Float(_) => {
            let f = match v {
                Value::Int(i) => *i as f64, // > 2^53 ties: a superset, the Filter trims
                Value::Float(f) if f.is_nan() => f64::NAN,
                Value::Float(f) => *f + 0.0, // -0.0 -> 0.0
                _ => unreachable!(),
            };
            let bits = f.to_bits();
            let ordered = if bits >> 63 == 1 {
                !bits
            } else {
                bits ^ (1 << 63)
            };
            out.push(TAG_NUM);
            out.extend_from_slice(&ordered.to_be_bytes());
        }
        Value::Text(t) => {
            out.push(TAG_TEXT);
            put_escaped(out, t.as_bytes());
        }
        Value::Bytes(b) => {
            out.push(TAG_BYTES);
            put_escaped(out, b);
        }
        // shortcut: List/Map key by their Debug form (Eq only, never ranged);
        // a -0.0 inside one misses its 0.0 twin — a typed encoding if it bites
        other => {
            out.push(TAG_OTHER);
            put_escaped(out, format!("{other:?}").as_bytes());
        }
    }
}

fn encode(values: &[&Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        put_value(&mut out, v);
    }
    out
}

/// The index contents: each key's rows, and each row's key — the reverse
/// map makes a re-key or a remove touch only the row's own bucket.
#[derive(Default)]
struct Buckets {
    by_key: BTreeMap<Vec<u8>, BTreeSet<KOID>>,
    by_koid: HashMap<KOID, Vec<u8>>,
}

impl Buckets {
    fn insert(&mut self, koid: KOID, key: Vec<u8>) {
        self.by_key.entry(key.clone()).or_default().insert(koid);
        self.by_koid.insert(koid, key);
    }

    /// Drop the row's entry; returns the entries examined (0 or 1).
    fn remove(&mut self, koid: &KOID) -> u64 {
        let Some(key) = self.by_koid.remove(koid) else {
            return 0;
        };
        if let Some(bucket) = self.by_key.get_mut(&key) {
            bucket.remove(koid);
            if bucket.is_empty() {
                self.by_key.remove(&key);
            }
        }
        1
    }

    fn clear(&mut self) {
        self.by_key.clear();
        self.by_koid.clear();
    }
}

pub struct PropertyIndex {
    name: String,
    type_name: String,
    properties: Vec<String>,
    map: RwLock<Buckets>,
    /// P5-M17b — the freshness stamp: the last committed event seq the
    /// index has fully applied. Set by `rebuild` (the head it reseeded
    /// from) and by the maintainer after a successful batch; 0 means no
    /// proof, and `verify` then walks.
    applied: AtomicU64,
    /// P5-M22 — the DDL lifecycle state (P1-07/P1-16). The catalog row
    /// carries the durable copy; this is the live registry's. Only `Ready`
    /// indexes may be CBO-chosen (P1-16).
    state: RwLock<IndexState>,
    /// AG-21 probe — bucket entries examined by upsert/remove (the
    /// idx6-001 seam: maintenance must stay O(1) per row, not O(index)).
    steps: AtomicU64,
}

impl PropertyIndex {
    pub fn new(name: &str, type_name: &str, properties: &[&str]) -> Self {
        PropertyIndex {
            name: name.into(),
            type_name: type_name.into(),
            properties: properties.iter().map(|p| p.to_string()).collect(),
            map: RwLock::new(Buckets::default()),
            applied: AtomicU64::new(0),
            state: RwLock::new(IndexState::Declared),
            steps: AtomicU64::new(0),
        }
    }

    /// AG-21 probe — bucket entries upsert/remove examined since creation.
    pub fn maintenance_steps(&self) -> u64 {
        self.steps.load(Ordering::Relaxed)
    }

    /// The ordered key of the property tuple, or None when any key
    /// property is missing (the row is not indexed).
    fn key_of(&self, ko: &KnowledgeObject) -> Option<Vec<u8>> {
        let mut vals = Vec::with_capacity(self.properties.len());
        for p in &self.properties {
            vals.push(ko.properties.get(p)?);
        }
        Some(encode(&vals))
    }

    /// P5-M26 — the checkpoint file name: the hex of the index name. Names
    /// are user-controlled catalog strings (any byte, path separators
    /// included); hex keeps them traversal- and collision-proof.
    fn checkpoint_file(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let mut hex = String::with_capacity(name.len() * 2);
        for b in name.as_bytes() {
            hex.push_str(&format!("{b:02x}"));
        }
        dir.join(hex)
    }
}

impl Index for PropertyIndex {
    fn name(&self) -> &str {
        &self.name
    }

    fn state(&self) -> IndexState {
        // justified: RwLock poison is unrecoverable
        *self.state.read().unwrap()
    }

    fn set_state(&self, state: IndexState) {
        // justified: RwLock poison is unrecoverable
        *self.state.write().unwrap() = state;
    }

    fn upsert(&self, koid: KOID, ko: &KnowledgeObject) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        let mut map = self.map.write().unwrap();
        // upsert = replace: the koid first drops from every key it may hold
        // (an update moves the entry between keys — idx2-004). The sweep
        // runs for foreign types too: a re-typed row must not keep
        // answering scans for its old type (P1-01).
        self.steps.fetch_add(map.remove(&koid), Ordering::Relaxed);
        if ko.metadata.type_name != self.type_name {
            return Ok(());
        }
        if let Some(key) = self.key_of(ko) {
            map.insert(koid, key);
        }
        Ok(())
    }

    fn remove(&self, koid: &KOID) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        let steps = self.map.write().unwrap().remove(koid);
        self.steps.fetch_add(steps, Ordering::Relaxed);
        Ok(())
    }

    fn scan_eq(&self, key: &[Value]) -> KResult<Vec<KOID>> {
        if key.len() != self.properties.len() {
            return Err(KError::InvalidQuery(format!(
                "index '{}' expects {} key properties, got {}",
                self.name,
                self.properties.len(),
                key.len()
            )));
        }
        // justified: RwLock poison is unrecoverable
        // a BTreeSet iterates in KOID order — the sorted contract
        Ok(self
            .map
            .read()
            .unwrap()
            .by_key
            .get(&encode(&key.iter().collect::<Vec<_>>()))
            .map(|b| b.iter().copied().collect())
            .unwrap_or_default())
    }

    fn scan_range(&self, lo: Option<&Value>, hi: Option<&Value>) -> KResult<Option<Vec<KOID>>> {
        if self.properties.len() != 1 {
            return Ok(None);
        }
        let domain = |v: &Value| -> Option<u8> {
            match v {
                Value::Int(_) | Value::Float(_) => Some(TAG_NUM),
                Value::Text(_) => Some(TAG_TEXT),
                Value::Bool(_) => Some(TAG_BOOL),
                _ => None, // the Filter orders nothing else
            }
        };
        let tags: Vec<u8> = match lo.iter().chain(hi.iter()).map(|v| domain(v)).collect() {
            Some(t) => t,
            None => return Ok(None),
        };
        let Some(&tag) = tags.first() else {
            return Ok(None); // no bound at all: not a range
        };
        if tags.iter().any(|t| *t != tag) {
            return Ok(Some(Vec::new())); // across domains nothing compares
        }
        let start = lo.map_or(Bound::Included(vec![tag]), |v| {
            Bound::Included(encode(&[v]))
        });
        let end = hi.map_or(Bound::Excluded(vec![tag + 1]), |v| {
            Bound::Included(encode(&[v]))
        });
        if let (Bound::Included(a), Bound::Included(b)) = (&start, &end) {
            if a > b {
                return Ok(Some(Vec::new()));
            }
        }
        // justified: RwLock poison is unrecoverable
        let map = self.map.read().unwrap();
        let mut out: Vec<KOID> = map
            .by_key
            .range((start, end))
            .flat_map(|(_, b)| b.iter().copied())
            .collect();
        out.sort();
        Ok(Some(out))
    }

    fn covers(&self, type_name: &str, property: &str) -> bool {
        self.type_name == type_name && self.properties.len() == 1 && self.properties[0] == property
    }

    fn len(&self) -> usize {
        // justified: RwLock poison is unrecoverable
        self.map.read().unwrap().by_koid.len()
    }

    fn set_applied_seq(&self, seq: u64) {
        self.applied.store(seq, Ordering::SeqCst);
    }

    fn applied_seq(&self) -> u64 {
        self.applied.load(Ordering::SeqCst)
    }

    /// P1-04: asynchronously maintained but stamp-verified — a fresh stamp
    /// proves every committed event applied, so clean-verified scans answer
    /// the committed truth (SnapshotExact).
    fn consistency(&self) -> ConsistencyLevel {
        ConsistencyLevel::SnapshotExact
    }

    fn verify(&self, kernel: &Kernel) -> KResult<VerifyReport> {
        // P5-M17b — the O(1) freshness proof. rebuild stamps the head it
        // reseeded from and the maintainer stamps the last seq of every
        // successful batch, so stamp == head means every committed event
        // was applied: nothing missing (all upserts/removes applied) and
        // nothing stale (live rows are re-keyed by upsert, dead rows
        // removed). Any other stamp state walks — fail-closed, the M9/M15
        // exactness contract unchanged.
        if self.applied_seq() == kernel.journal_head()?.0 {
            return Ok(VerifyReport {
                name: self.name.clone(),
                indexed: self.len(),
                missing: Vec::new(),
                stale: Vec::new(),
                verified: true,
            });
        }
        // justified: RwLock poison is unrecoverable
        let map = self.map.read().unwrap();
        let mut live: BTreeSet<KOID> = BTreeSet::new();
        let mut missing = Vec::new();
        for (koid, _version, ts, state) in kernel.scan_heads()? {
            if state == LifecycleState::Deleted {
                continue; // tombstoned — an entry left for it is stale
            }
            let Some(ko) = kernel.raw_object_at(&koid, ts)? else {
                continue;
            };
            if ko.metadata.type_name != self.type_name {
                continue;
            }
            if let Some(key) = self.key_of(&ko) {
                live.insert(koid);
                if !map.by_key.get(&key).is_some_and(|b| b.contains(&koid)) {
                    missing.push(koid);
                }
            }
        }
        let mut stale: Vec<KOID> = map
            .by_key
            .values()
            .flatten()
            .copied()
            .filter(|k| !live.contains(k))
            .collect();
        stale.sort();
        stale.dedup();
        let indexed = map.by_koid.len();
        Ok(VerifyReport {
            name: self.name.clone(),
            indexed,
            missing,
            stale,
            verified: true,
        })
    }

    fn rebuild(&self, kernel: &Kernel) -> KResult<()> {
        // P5-M26: the freshness guard — stamp == head proves every committed
        // event applied (the M17b proof), so a caught-up rebuild is pure
        // waste. The SDK re-declares the same-shape index on every connect;
        // at 1M heads that reseed dominated the open cost. Sound: the stamp
        // only ever advances past actually-applied events (P0-01) and the
        // head only grows, so an equal stamp cannot miss a commit. The
        // state gate keeps non-Ready indexes converging (P1-07).
        let h0 = kernel.journal_head()?.0;
        if self.applied_seq() == h0 && self.state() == IndexState::Ready {
            return Ok(());
        }
        // P5-M26 (idx5-001) test hook: park a rebuild for THIS index so a
        // test can prove a caught-up rebuild is skipped entirely. The
        // guard above runs first, so a skip never parks.
        if std::env::var_os("INDEX_REBUILD_PARK").is_some_and(|v| v == self.name.as_str()) {
            std::env::set_var("INDEX_REBUILD_PARK_AT", "1");
            let mut waited = 0u64;
            while std::env::var_os("INDEX_REBUILD_PARK").is_some_and(|v| v == self.name.as_str())
                && waited < 500
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
                waited += 10;
            }
        }
        // P5-M17b: the stamp invalidates FIRST (0) so a concurrent query
        // cannot short-circuit against a half-reseeded map, then the map
        // reseeds, then the stamp lands on the head captured BEFORE the
        // scan — every commit ≤ h0 is visible to the reseed; commits after
        // h0 reach this index through the maintainer (the index is
        // registered before rebuild), which stamps them as it applies them.
        self.applied.store(0, Ordering::SeqCst);
        // justified: RwLock poison is unrecoverable
        let mut map = self.map.write().unwrap();
        map.clear();
        for (koid, _version, ts, state) in kernel.scan_heads()? {
            if state == LifecycleState::Deleted {
                continue;
            }
            let Some(ko) = kernel.raw_object_at(&koid, ts)? else {
                continue;
            };
            if ko.metadata.type_name != self.type_name {
                continue;
            }
            if let Some(key) = self.key_of(&ko) {
                map.insert(koid, key);
            }
        }
        self.applied.store(h0, Ordering::SeqCst);
        Ok(())
    }

    /// P5-M26 — persist the index into `dir` (created on demand): "AKPI"
    /// magic, then per bucket a length-prefixed key and its KOIDs — binary,
    /// so Debug-derived keys of any byte content round-trip. The scheduler
    /// wraps this in its tmp-dir + rename protocol, so torn writes cannot
    /// reach the published checkpoint.
    fn checkpoint(&self, dir: &std::path::Path) -> KResult<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| KError::Store(format!("create property checkpoint dir: {}", e)))?;
        // justified: RwLock poison is unrecoverable
        let map = self.map.read().unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(b"AKP2"); // AG-21c: ordered byte keys
        out.extend_from_slice(&(map.by_key.len() as u64).to_le_bytes());
        for (key, bucket) in map.by_key.iter() {
            out.extend_from_slice(&(key.len() as u64).to_le_bytes());
            out.extend_from_slice(key);
            out.extend_from_slice(&(bucket.len() as u64).to_le_bytes());
            for koid in bucket {
                out.extend_from_slice(koid.as_bytes());
            }
        }
        std::fs::write(Self::checkpoint_file(dir, &self.name), out)
            .map_err(|e| KError::Store(format!("write property checkpoint: {}", e)))
    }

    /// P5-M26 — restore the index from `dir`. `Ok(false)` = no file for
    /// this index (the caller reseeds it). Parse is fail-closed: any
    /// malformed stream errors, and the host falls back to a full replay —
    /// never a half-restored index. The restored contents are stamped
    /// exactly at `water`; the caller replays the tail on top.
    fn restore(&self, dir: &std::path::Path, water: u64) -> KResult<bool> {
        let path = Self::checkpoint_file(dir, &self.name);
        if !path.exists() {
            return Ok(false);
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| KError::Store(format!("read property checkpoint: {}", e)))?;
        fn take<'a>(bytes: &'a [u8], pos: &mut usize, n: usize) -> KResult<&'a [u8]> {
            if *pos + n > bytes.len() {
                return Err(KError::Store(
                    "property checkpoint corrupt: truncated".into(),
                ));
            }
            let s = &bytes[*pos..*pos + n];
            *pos += n;
            Ok(s)
        }
        let mut pos = 0usize;
        let magic = take(&bytes, &mut pos, 4)?;
        if magic == b"AKPI" {
            // a pre-AG-21c checkpoint keyed by Debug strings: reseed once
            return Ok(false);
        }
        if magic != b"AKP2" {
            return Err(KError::Store(format!(
                "property checkpoint '{}' corrupt: bad magic",
                self.name
            )));
        }
        let n_buckets = u64::from_le_bytes(take(&bytes, &mut pos, 8)?.try_into().unwrap()) as usize;
        let mut map = Buckets::default();
        for _ in 0..n_buckets {
            let key_len =
                u64::from_le_bytes(take(&bytes, &mut pos, 8)?.try_into().unwrap()) as usize;
            let key = take(&bytes, &mut pos, key_len)?.to_vec();
            let n = u64::from_le_bytes(take(&bytes, &mut pos, 8)?.try_into().unwrap()) as usize;
            for _ in 0..n {
                let mut buf = [0u8; KOID_LEN];
                buf.copy_from_slice(take(&bytes, &mut pos, KOID_LEN)?);
                map.insert(KOID::from_bytes(buf), key.clone());
            }
        }
        if pos != bytes.len() {
            return Err(KError::Store(format!(
                "property checkpoint '{}' corrupt: trailing bytes",
                self.name
            )));
        }
        // justified: RwLock poison is unrecoverable
        *self.map.write().unwrap() = map;
        self.applied.store(water, Ordering::SeqCst);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ko(koid: KOID, type_name: &str) -> KnowledgeObject {
        let mut ko = KnowledgeObject::new(
            koid,
            Metadata {
                type_name: type_name.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("title".into(), Value::Text("same title".into()));
        ko
    }

    // --- AG-21c — idx6-002: the key encoding agrees with the Filter. Equal
    // per the Filter -> equal keys (an index never misses an Eq row); a < b
    // per the Filter -> key(a) <= key(b) (a range never misses a row).
    // Unequal values may share a key (Int > 2^53): a superset, trimmed. ---
    fn filter_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x.partial_cmp(y),
            (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
            (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y),
            (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)),
            (Value::Text(x), Value::Text(y)) => Some(x.cmp(y)),
            (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
            _ => None,
        }
    }

    fn any_value() -> impl proptest::strategy::Strategy<Value = Value> {
        use proptest::prelude::*;
        prop_oneof![
            any::<i64>().prop_map(Value::Int),
            (-5i64..5).prop_map(Value::Int),
            any::<f64>().prop_map(Value::Float),
            prop::sample::select(vec![0.0, -0.0, 1.0, -1.0, 9007199254740993.0, f64::NAN])
                .prop_map(Value::Float),
            "[a\\x00b]{0,4}".prop_map(Value::Text),
            any::<bool>().prop_map(Value::Bool),
        ]
    }

    proptest::proptest! {
        #[test]
        fn idx6_002_key_order_agrees_with_the_filter(a in any_value(), b in any_value()) {
            let (ka, kb) = (encode(&[&a]), encode(&[&b]));
            match filter_cmp(&a, &b) {
                Some(std::cmp::Ordering::Equal) => proptest::prop_assert_eq!(ka, kb),
                Some(std::cmp::Ordering::Less) => proptest::prop_assert!(ka <= kb),
                Some(std::cmp::Ordering::Greater) => proptest::prop_assert!(ka >= kb),
                None => {}
            }
        }
    }

    // --- AG-21 — idx6-001: maintenance is O(1) per row. RED: upsert swept
    // every bucket to drop the row's old entry — 2 000 rows into one index
    // examined ~2 million entries; a 1M-row index build took ~an hour. ---
    #[test]
    fn idx6_001_maintenance_does_not_sweep_the_index() {
        let idx = PropertyIndex::new("by_title", "note", &["title"]);
        let n = 2000u64;
        for i in 0..n {
            let mut b = [0u8; KOID_LEN];
            b[..8].copy_from_slice(&i.to_le_bytes());
            idx.upsert(KOID::from_bytes(b), &ko(KOID::from_bytes(b), "note"))
                .unwrap();
        }
        let first = KOID::from_bytes([0u8; KOID_LEN]);
        idx.upsert(first, &ko(first, "note")).unwrap(); // a re-key
        idx.remove(&first).unwrap();
        let steps = idx.maintenance_steps();
        assert!(
            steps <= 4 * n,
            "{n} upserts examined {steps} bucket entries"
        );
        assert_eq!(idx.len() as u64, n - 1);
    }

    // --- P5-M19 — idx3-003 (PR6 P1-01): a foreign-type upsert must still
    // sweep stale membership. RED: the early return leaves the koid in the
    // buckets, so a re-typed row keeps answering scans for its old type. ---
    #[test]
    fn idx3_003_foreign_type_upsert_removes_stale_membership() {
        let idx = PropertyIndex::new("by_title", "note", &["title"]);
        let k1 = KOID::from_bytes([1u8; KOID_LEN]);
        let k2 = KOID::from_bytes([2u8; KOID_LEN]);
        idx.upsert(k1, &ko(k1, "note")).unwrap();
        idx.upsert(k2, &ko(k2, "note")).unwrap();
        idx.upsert(k2, &ko(k2, "memo")).unwrap(); // re-typed: must sweep
        let found = idx.scan_eq(&[Value::Text("same title".into())]).unwrap();
        assert_eq!(
            found,
            vec![k1],
            "the re-typed row no longer answers note scans"
        );
        assert_eq!(idx.len(), 1);
    }

    // --- P5-M26 — RED: index contents must survive a checkpoint → restore
    // round-trip, binary-unsafe keys included (the Debug-derived key string
    // can contain any byte). The trait defaults write nothing and restore
    // nothing, so the scan comes back empty. ---
    #[test]
    fn m26_property_index_checkpoint_restore_round_trip() {
        let idx = PropertyIndex::new("by_body", "note", &["body"]);
        let k1 = KOID::from_bytes([1u8; KOID_LEN]);
        let k2 = KOID::from_bytes([2u8; KOID_LEN]);
        let mut ko1 = ko(k1, "note");
        ko1.properties
            .insert("body".into(), Value::Text("line one\nline two".into()));
        let mut ko2 = ko(k2, "note");
        ko2.properties
            .insert("body".into(), Value::Text("tab\tand \"quotes\"".into()));
        idx.upsert(k1, &ko1).unwrap();
        idx.upsert(k2, &ko2).unwrap();

        let dir = std::env::temp_dir().join(format!(
            "aikoql-m26-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        idx.checkpoint(&dir).unwrap();

        let idx2 = PropertyIndex::new("by_body", "note", &["body"]);
        assert!(
            idx2.restore(&dir, 7).unwrap(),
            "a checkpointed index must restore its contents"
        );
        assert_eq!(
            idx2.scan_eq(&[Value::Text("line one\nline two".into())])
                .unwrap(),
            vec![k1],
            "binary-unsafe keys must round-trip"
        );
        assert_eq!(
            idx2.scan_eq(&[Value::Text("tab\tand \"quotes\"".into())])
                .unwrap(),
            vec![k2],
            "binary-unsafe keys must round-trip"
        );
    }

    // --- AG-21d — idx6-003: a range on the FIRST property of a composite
    // index serves as a byte-prefix range over the tuple keys. The hi
    // boundary is the exclusive successor of its key, so rows whose tuple
    // carries a suffix past hi are still included (the Filter re-checks).
    // RED pre-fix: scan_range refuses multi-property indexes and the trait
    // has no covers_range — the query full-scans.
    #[test]
    fn idx6_003_composite_prefix_range_serves_the_first_property() {
        let idx = PropertyIndex::new("by_team_points", "Player", &["team", "points"]);
        let row = |n: u8, team: i64, points: i64| {
            let koid = KOID::from_bytes([n; KOID_LEN]);
            let mut o = ko(koid, "Player");
            o.properties.insert("team".into(), Value::Int(team));
            o.properties.insert("points".into(), Value::Int(points));
            (koid, o)
        };
        for (koid, o) in [
            row(1, 3, 7),
            row(2, 5, 10),
            row(3, 8, 1),
            row(4, 8, 99),
            row(5, 9, 0),
        ] {
            idx.upsert(koid, &o).unwrap();
        }
        // team in [5, 8]: 5, 8+1, 8+99 — the 8 rows' suffixes must not
        // drop them at the hi boundary
        let got = idx
            .scan_range(Some(&Value::Int(5)), Some(&Value::Int(8)))
            .unwrap()
            .unwrap();
        assert_eq!(
            got,
            vec![
                KOID::from_bytes([2; KOID_LEN]),
                KOID::from_bytes([3; KOID_LEN]),
                KOID::from_bytes([4; KOID_LEN])
            ]
        );
        // open upper end: team >= 8
        let got = idx.scan_range(Some(&Value::Int(8)), None).unwrap().unwrap();
        assert_eq!(
            got,
            vec![
                KOID::from_bytes([3; KOID_LEN]),
                KOID::from_bytes([4; KOID_LEN]),
                KOID::from_bytes([5; KOID_LEN])
            ]
        );
        // the range covers only the FIRST property; Eq still needs the
        // full tuple
        assert!(idx.covers_range("Player", "team"));
        assert!(!idx.covers_range("Player", "points"));
        assert!(!idx.covers("Player", "team"), "Eq needs the full tuple");
    }

    #[test]
    fn idx6_003_composite_index_eq_scan_checks_arity() {
        let idx = PropertyIndex::new("by_team_points", "Player", &["team", "points"]);
        // an Eq probe with fewer key parts than the tuple is refused
        assert!(idx.scan_eq(&[Value::Int(1)]).is_err());
        assert_eq!(
            idx.scan_eq(&[Value::Int(1), Value::Int(2)]).unwrap(),
            Vec::<KOID>::new()
        );
    }
}
