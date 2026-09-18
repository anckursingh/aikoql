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
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

pub struct PropertyIndex {
    name: String,
    type_name: String,
    properties: Vec<String>,
    map: RwLock<HashMap<String, Vec<KOID>>>,
    /// P5-M17b — the freshness stamp: the last committed event seq the
    /// index has fully applied. Set by `rebuild` (the head it reseeded
    /// from) and by the maintainer after a successful batch; 0 means no
    /// proof, and `verify` then walks.
    applied: AtomicU64,
    /// P5-M22 — the DDL lifecycle state (P1-07/P1-16). The catalog row
    /// carries the durable copy; this is the live registry's. Only `Ready`
    /// indexes may be CBO-chosen (P1-16).
    state: RwLock<IndexState>,
}

impl PropertyIndex {
    pub fn new(name: &str, type_name: &str, properties: &[&str]) -> Self {
        PropertyIndex {
            name: name.into(),
            type_name: type_name.into(),
            properties: properties.iter().map(|p| p.to_string()).collect(),
            map: RwLock::new(HashMap::new()),
            applied: AtomicU64::new(0),
            state: RwLock::new(IndexState::Declared),
        }
    }

    /// The Debug-derived key of the property tuple, or None when any key
    /// property is missing (the row is not indexed).
    fn key_of(&self, ko: &KnowledgeObject) -> Option<String> {
        let mut vals = Vec::with_capacity(self.properties.len());
        for p in &self.properties {
            vals.push(ko.properties.get(p)?.clone());
        }
        Some(format!("{vals:?}"))
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
        for bucket in map.values_mut() {
            bucket.retain(|k| *k != koid);
        }
        if ko.metadata.type_name != self.type_name {
            return Ok(());
        }
        if let Some(key) = self.key_of(ko) {
            map.entry(key).or_default().push(koid);
        }
        Ok(())
    }

    fn remove(&self, koid: &KOID) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        for bucket in self.map.write().unwrap().values_mut() {
            bucket.retain(|k| k != koid);
        }
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
        let mut out = self
            .map
            .read()
            .unwrap()
            .get(&format!("{key:?}"))
            .cloned()
            .unwrap_or_default();
        out.sort();
        Ok(out)
    }

    fn covers(&self, type_name: &str, property: &str) -> bool {
        self.type_name == type_name && self.properties.len() == 1 && self.properties[0] == property
    }

    fn len(&self) -> usize {
        // justified: RwLock poison is unrecoverable
        self.map.read().unwrap().values().map(|b| b.len()).sum()
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
                if !map.get(&key).is_some_and(|b| b.contains(&koid)) {
                    missing.push(koid);
                }
            }
        }
        let mut stale: Vec<KOID> = map
            .values()
            .flatten()
            .copied()
            .filter(|k| !live.contains(k))
            .collect();
        stale.sort();
        stale.dedup();
        let indexed = map.values().map(|b| b.len()).sum();
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
                map.entry(key).or_default().push(koid);
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
        out.extend_from_slice(b"AKPI");
        out.extend_from_slice(&(map.len() as u64).to_le_bytes());
        for (key, bucket) in map.iter() {
            out.extend_from_slice(&(key.len() as u64).to_le_bytes());
            out.extend_from_slice(key.as_bytes());
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
        if take(&bytes, &mut pos, 4)? != b"AKPI" {
            return Err(KError::Store(format!(
                "property checkpoint '{}' corrupt: bad magic",
                self.name
            )));
        }
        let n_buckets = u64::from_le_bytes(take(&bytes, &mut pos, 8)?.try_into().unwrap()) as usize;
        let mut map: HashMap<String, Vec<KOID>> = HashMap::new();
        for _ in 0..n_buckets {
            let key_len =
                u64::from_le_bytes(take(&bytes, &mut pos, 8)?.try_into().unwrap()) as usize;
            let key =
                String::from_utf8(take(&bytes, &mut pos, key_len)?.to_vec()).map_err(|_| {
                    KError::Store(format!(
                        "property checkpoint '{}' corrupt: bad key",
                        self.name
                    ))
                })?;
            let n = u64::from_le_bytes(take(&bytes, &mut pos, 8)?.try_into().unwrap()) as usize;
            let mut bucket = Vec::with_capacity(n);
            for _ in 0..n {
                let mut buf = [0u8; KOID_LEN];
                buf.copy_from_slice(take(&bytes, &mut pos, KOID_LEN)?);
                bucket.push(KOID::from_bytes(buf));
            }
            map.insert(key, bucket);
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
        assert_eq!(idx2.applied_seq(), 7, "restore stamps the water");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
