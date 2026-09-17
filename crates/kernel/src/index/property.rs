//! P5-M8 (ND-07) — property + composite hash indexes: in-memory equality
//! indexes over a catalog-declared property tuple, maintained asynchronously
//! by the scheduler's IndexMaintainer — never on the commit path. The key is
//! the Debug-derived string of the value tuple (Value is not Hash — the
//! P5-M5 group-key precedent); ANY key property missing → the row is not
//! indexed. Contents are never persisted: they replay from the journal
//! (idx2-007).

use crate::index::unified::{ConsistencyLevel, Index, VerifyReport};
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
}

impl PropertyIndex {
    pub fn new(name: &str, type_name: &str, properties: &[&str]) -> Self {
        PropertyIndex {
            name: name.into(),
            type_name: type_name.into(),
            properties: properties.iter().map(|p| p.to_string()).collect(),
            map: RwLock::new(HashMap::new()),
            applied: AtomicU64::new(0),
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
}

impl Index for PropertyIndex {
    fn name(&self) -> &str {
        &self.name
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
        // P5-M17b: the stamp invalidates FIRST (0) so a concurrent query
        // cannot short-circuit against a half-reseeded map, then the map
        // reseeds, then the stamp lands on the head captured BEFORE the
        // scan — every commit ≤ h0 is visible to the reseed; commits after
        // h0 reach this index through the maintainer (the index is
        // registered before rebuild), which stamps them as it applies them.
        let h0 = kernel.journal_head()?.0;
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
}
