//! V2-Adopt — `AikoqlStorageEngineV2`: the kernel `StorageEngine` adapter
//! over the v2 Db. The six KSE-1 asserts (the shared definition) run here
//! per-backend as granular tests; the KSE-20 matrix
//! (`kse20_backend_conformance.rs`) runs the same definition across all
//! backends. Persistence across reopen is the one divergence surface the
//! six asserts cannot see — pinned per engine below.

mod common;

use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};
use aikoql_storage_v2::AikoqlStorageEngineV2;
use common::{kse, tmp};

#[test]
fn kse001_get() {
    kse::kse001_get(&AikoqlStorageEngineV2::open(tmp("engine-kse1")).unwrap());
}

#[test]
fn kse002_missing_key() {
    kse::kse002_missing_key(&AikoqlStorageEngineV2::open(tmp("engine-kse2")).unwrap());
}

#[test]
fn kse003_prefix_scan() {
    kse::kse003_prefix_scan(&AikoqlStorageEngineV2::open(tmp("engine-kse3")).unwrap());
}

#[test]
fn kse004_atomic_batch() {
    kse::kse004_atomic_batch(&AikoqlStorageEngineV2::open(tmp("engine-kse4")).unwrap());
}

#[test]
fn kse005_empty_batch() {
    kse::kse005_empty_batch(&AikoqlStorageEngineV2::open(tmp("engine-kse5")).unwrap());
}

#[test]
fn kse006_conflicting_put_delete() {
    kse::kse006_conflicting_put_delete(&AikoqlStorageEngineV2::open(tmp("engine-kse6")).unwrap());
}

#[test]
fn reopen_serves_durable_state() {
    let path = tmp("engine-reopen");
    {
        let e = AikoqlStorageEngineV2::open(&path).unwrap();
        let mut b = WriteBatch::new();
        b.put(b"keep".to_vec(), b"v".to_vec());
        b.put(b"gone".to_vec(), b"v".to_vec());
        e.write_batch(&b).unwrap();
        let mut d = WriteBatch::new();
        d.del(b"gone".to_vec());
        e.write_batch(&d).unwrap();
    } // drop the handle — reopen must serve the committed state
    let e = AikoqlStorageEngineV2::open(&path).unwrap();
    assert_eq!(e.get(b"keep").unwrap(), Some(b"v".to_vec()));
    assert_eq!(e.get(b"gone").unwrap(), None);
    let rows: Vec<Vec<u8>> = e.scan(b"").unwrap().into_iter().map(|(k, _)| k).collect();
    assert_eq!(rows, vec![b"keep".to_vec()]);
}

#[test]
fn multi_put_batch_last_wins_and_is_atomic() {
    let path = tmp("engine-multiput");
    let e = AikoqlStorageEngineV2::open(&path).unwrap();
    let mut b = WriteBatch::new();
    b.put(b"d".to_vec(), vec![1]);
    b.put(b"d".to_vec(), vec![2]); // same key twice — last put wins
    b.put(b"e".to_vec(), vec![3]);
    e.write_batch(&b).unwrap();
    assert_eq!(e.get(b"d").unwrap(), Some(vec![2]));
    assert_eq!(e.get(b"e").unwrap(), Some(vec![3]));
    // all-or-nothing survives reopen: both keys came from one batch
    drop(e);
    let e = AikoqlStorageEngineV2::open(&path).unwrap();
    assert_eq!(e.get(b"d").unwrap(), Some(vec![2]));
    assert_eq!(e.get(b"e").unwrap(), Some(vec![3]));
}
