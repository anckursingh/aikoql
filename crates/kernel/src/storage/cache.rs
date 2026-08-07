//! Optional in-memory cache for hot repository reads.
//!
//! Sits inside `KnowledgeRepository` and is kept coherent by invalidating or
//! updating entries on every write path. Disabled by default; enable by calling
//! `KnowledgeRepository::with_cache` or `Kernel::with_cache`.
//!
//! ponytail: measure-driven feature. The implementation is intentionally small
//! (LRU for heads and object versions) because no production workload profile
//! has been supplied yet. Expand to query-result / ACL caches only after
//! profiling shows the bottleneck.

use crate::knowledge::kom::{KnowledgeObject, LifecycleState, KOID};
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Mutex};

/// Simple LRU cache bounded by `capacity`.
#[derive(Clone, Debug)]
struct Lru<K, V> {
    capacity: usize,
    order: VecDeque<K>,
    map: HashMap<K, V>,
}

impl<K: Eq + Hash + Clone, V: Clone> Lru<K, V> {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::with_capacity(capacity),
            map: HashMap::with_capacity(capacity),
        }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        if self.map.contains_key(key) {
            self.order.retain(|k| k != key);
            self.order.push_back(key.clone());
        }
        self.map.get(key).cloned()
    }

    fn put(&mut self, key: K, value: V) {
        if self.map.contains_key(&key) {
            self.order.retain(|k| k != &key);
        } else if self.capacity > 0 && self.order.len() >= self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }

    fn remove(&mut self, key: &K) {
        self.order.retain(|k| k != key);
        self.map.remove(key);
    }

    fn clear(&mut self) {
        self.order.clear();
        self.map.clear();
    }
}

#[derive(Clone, Debug)]
struct CacheInner {
    heads: Lru<KOID, (u64, u64, LifecycleState)>,
    objects: Lru<(KOID, u64), KnowledgeObject>,
}

impl CacheInner {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            heads: Lru::with_capacity(capacity),
            objects: Lru::with_capacity(capacity),
        }
    }
}

/// In-memory cache wired into `KnowledgeRepository`.
#[derive(Clone, Debug)]
pub struct KnowledgeCache {
    inner: Arc<Mutex<CacheInner>>,
}

impl KnowledgeCache {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheInner::with_capacity(capacity))),
        }
    }

    pub fn get_head(&self, koid: &KOID) -> Option<(u64, u64, LifecycleState)> {
        self.inner.lock().unwrap().heads.get(koid)
    }

    pub fn put_head(&self, koid: &KOID, version: u64, commit_ts: u64, state: LifecycleState) {
        self.inner
            .lock()
            .unwrap()
            .heads
            .put(*koid, (version, commit_ts, state));
    }

    pub fn delete_head(&self, koid: &KOID) {
        self.inner.lock().unwrap().heads.remove(koid);
    }

    pub fn get_object(&self, koid: &KOID, commit_ts: u64) -> Option<KnowledgeObject> {
        self.inner.lock().unwrap().objects.get(&(*koid, commit_ts))
    }

    pub fn put_object(&self, koid: &KOID, commit_ts: u64, ko: &KnowledgeObject) {
        self.inner
            .lock()
            .unwrap()
            .objects
            .put((*koid, commit_ts), ko.clone());
    }

    pub fn delete_object(&self, koid: &KOID, commit_ts: u64) {
        self.inner
            .lock()
            .unwrap()
            .objects
            .remove(&(*koid, commit_ts));
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.heads.clear();
        inner.objects.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::kom::{Lifecycle, Metadata, Origin, PropertyMap, SecurityDescriptor};
    use crate::KOID_LEN;

    fn dummy_ko(koid: KOID, version: u64) -> KnowledgeObject {
        KnowledgeObject {
            koid,
            version,
            commit_ts: 1,
            metadata: Metadata {
                type_name: "test".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: PropertyMap::new(),
            semantic: None,
            relationships: vec![],
            event_refs: vec![],
            security: SecurityDescriptor {
                owner: "test".into(),
                acl: vec![],
                classification: None,
            },
            lifecycle: Lifecycle {
                state: LifecycleState::Draft,
                origin: Origin::Human,
            },
            extensions: Default::default(),
        }
    }

    #[test]
    fn head_cache_hits_and_invalidates() {
        let c = KnowledgeCache::with_capacity(2);
        let k = KOID([1u8; KOID_LEN]);
        assert!(c.get_head(&k).is_none());
        c.put_head(&k, 1, 100, LifecycleState::Draft);
        assert_eq!(c.get_head(&k), Some((1, 100, LifecycleState::Draft)));
        c.delete_head(&k);
        assert!(c.get_head(&k).is_none());
    }

    #[test]
    fn object_cache_evicts_at_capacity() {
        let c = KnowledgeCache::with_capacity(2);
        let a = KOID([1u8; KOID_LEN]);
        let b = KOID([2u8; KOID_LEN]);
        let x = KOID([3u8; KOID_LEN]);
        c.put_object(&a, 1, &dummy_ko(a, 1));
        c.put_object(&b, 1, &dummy_ko(b, 2));
        c.put_object(&x, 1, &dummy_ko(x, 3));
        assert!(c.get_object(&a, 1).is_none());
        assert!(c.get_object(&b, 1).is_some());
        assert!(c.get_object(&x, 1).is_some());
    }
}
