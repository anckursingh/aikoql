//! KSE-1 — storage contract conformance (MRFC-KSE-001 §7).
//!
//! The six KSE asserts below run identically against every backend so the
//! custom engine passes exactly what MemoryEngine and RedbEngine pass.
//! KSE-20 extends this to the full cross-backend suite (RocksDB included).

use aikoql_kernel::storage::store::{MemoryEngine, StorageEngine, WriteBatch};
use aikoql_kernel::storage::store_redb::RedbEngine;
use aikoql_storage::AikoqlStorageEngine;
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "aikoql_kse_unit_{}_{}.redb",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

/// The six KSE asserts, shared verbatim by all backends.
mod kse {
    use super::{StorageEngine, WriteBatch};

    /// KSE-001: get returns the written value.
    pub fn kse001_get(e: &dyn StorageEngine) {
        let mut b = WriteBatch::new();
        b.put(b"k1".to_vec(), b"v1".to_vec());
        e.write_batch(&b).unwrap();
        assert_eq!(e.get(b"k1").unwrap(), Some(b"v1".to_vec()));
    }

    /// KSE-002: a missing key reads as None.
    pub fn kse002_missing_key(e: &dyn StorageEngine) {
        assert_eq!(e.get(b"missing").unwrap(), None);
    }

    /// KSE-003: prefix scan returns exactly the prefix's keys, sorted ascending.
    pub fn kse003_prefix_scan(e: &dyn StorageEngine) {
        let mut b = WriteBatch::new();
        for k in [&b"a/3"[..], &b"a/1"[..], &b"a/2"[..], &b"b/1"[..]] {
            b.put(k.to_vec(), vec![0]);
        }
        e.write_batch(&b).unwrap();
        let got: Vec<Vec<u8>> = e.scan(b"a/").unwrap().into_iter().map(|(k, _)| k).collect();
        assert_eq!(got, vec![b"a/1".to_vec(), b"a/2".to_vec(), b"a/3".to_vec()]);
    }

    /// KSE-004: puts and deletes in one batch become visible atomically.
    pub fn kse004_atomic_batch(e: &dyn StorageEngine) {
        let mut b = WriteBatch::new();
        b.put(b"x".to_vec(), vec![1]);
        b.put(b"y".to_vec(), vec![2]);
        e.write_batch(&b).unwrap();
        let mut d = WriteBatch::new();
        d.del(b"x".to_vec());
        d.put(b"z".to_vec(), vec![3]);
        e.write_batch(&d).unwrap();
        assert_eq!(e.get(b"x").unwrap(), None);
        assert_eq!(e.get(b"y").unwrap(), Some(vec![2]));
        assert_eq!(e.get(b"z").unwrap(), Some(vec![3]));
    }

    /// KSE-005: an empty batch produces no state change.
    pub fn kse005_empty_batch(e: &dyn StorageEngine) {
        let mut b = WriteBatch::new();
        b.put(b"k".to_vec(), vec![1]);
        e.write_batch(&b).unwrap();
        e.write_batch(&WriteBatch::new()).unwrap();
        assert_eq!(e.get(b"k").unwrap(), Some(vec![1]));
    }

    /// KSE-006: deterministic semantics for a key in both puts and deletes.
    ///
    /// All backends apply puts before dels (documented invariant in
    /// `store.rs`), so a put+del of the same key deletes it; duplicate puts
    /// resolve to the last value.
    pub fn kse006_conflicting_put_delete(e: &dyn StorageEngine) {
        let mut b = WriteBatch::new();
        b.put(b"c".to_vec(), vec![1]);
        b.del(b"c".to_vec());
        b.put(b"d".to_vec(), vec![1]);
        b.put(b"d".to_vec(), vec![2]);
        e.write_batch(&b).unwrap();
        assert_eq!(e.get(b"c").unwrap(), None); // put then del: deleted
        assert_eq!(e.get(b"d").unwrap(), Some(vec![2])); // last put wins
    }
}

/// Runs the six KSE asserts against one backend instance.
macro_rules! backend_tests {
    ($modname:ident, $open:expr) => {
        mod $modname {
            use super::*;

            // Backend-prefixed temp names: parallel tests must not share files.
            #[test]
            fn kse001_get() {
                kse::kse001_get(&*($open)(concat!(stringify!($modname), "_kse001")));
            }
            #[test]
            fn kse002_missing_key() {
                kse::kse002_missing_key(&*($open)(concat!(stringify!($modname), "_kse002")));
            }
            #[test]
            fn kse003_prefix_scan() {
                kse::kse003_prefix_scan(&*($open)(concat!(stringify!($modname), "_kse003")));
            }
            #[test]
            fn kse004_atomic_batch() {
                kse::kse004_atomic_batch(&*($open)(concat!(stringify!($modname), "_kse004")));
            }
            #[test]
            fn kse005_empty_batch() {
                kse::kse005_empty_batch(&*($open)(concat!(stringify!($modname), "_kse005")));
            }
            #[test]
            fn kse006_conflicting_put_delete() {
                kse::kse006_conflicting_put_delete(&*($open)(concat!(
                    stringify!($modname),
                    "_kse006"
                )));
            }
        }
    };
}

backend_tests!(aikoql, |name: &str| -> Box<dyn StorageEngine> {
    Box::new(AikoqlStorageEngine::open(tmp(name)).unwrap())
});
backend_tests!(memory, |_: &str| -> Box<dyn StorageEngine> {
    Box::new(MemoryEngine::new())
});
backend_tests!(redb, |name: &str| -> Box<dyn StorageEngine> {
    Box::new(RedbEngine::open(tmp(name)).unwrap())
});

/// Engine sanity (mirrors the rocksdb crate's): state survives a reopen.
#[test]
fn persists_across_reopen() {
    let p = tmp("reopen");
    {
        let e = AikoqlStorageEngine::open(&p).unwrap();
        let mut b = WriteBatch::new();
        b.put(b"k1".to_vec(), vec![1, 2, 3]);
        e.write_batch(&b).unwrap();
    }
    let e2 = AikoqlStorageEngine::open(&p).unwrap();
    assert_eq!(e2.get(b"k1").unwrap(), Some(vec![1, 2, 3]));
    let _ = std::fs::remove_file(&p);
}
