//! P5-M8 (ND-07) — property + composite hash indexes: in-memory equality
//! indexes over a catalog-declared property tuple, maintained asynchronously
//! by the scheduler's IndexMaintainer — never on the commit path. The key is
//! the Debug-derived string of the value tuple (Value is not Hash — the
//! P5-M5 group-key precedent); ANY key property missing → the row is not
//! indexed. Contents are never persisted: they replay from the journal
//! (idx2-007).

use crate::index::unified::{Index, VerifyReport};
use crate::knowledge::kom::*;
use crate::transaction::kernel::Kernel;
use std::collections::{BTreeSet, HashMap};
use std::sync::RwLock;

pub struct PropertyIndex {
    name: String,
    type_name: String,
    properties: Vec<String>,
    map: RwLock<HashMap<String, Vec<KOID>>>,
}

impl PropertyIndex {
    pub fn new(name: &str, type_name: &str, properties: &[&str]) -> Self {
        PropertyIndex {
            name: name.into(),
            type_name: type_name.into(),
            properties: properties.iter().map(|p| p.to_string()).collect(),
            map: RwLock::new(HashMap::new()),
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
        if ko.metadata.type_name != self.type_name {
            return Ok(());
        }
        // justified: RwLock poison is unrecoverable
        let mut map = self.map.write().unwrap();
        // upsert = replace: the koid first drops from every key it may hold
        // (an update moves the entry between keys — idx2-004)
        for bucket in map.values_mut() {
            bucket.retain(|k| *k != koid);
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

    fn verify(&self, kernel: &Kernel) -> KResult<VerifyReport> {
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
        Ok(())
    }
}
