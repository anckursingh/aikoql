//! Durable `StorageEngine` backend over redb (pure-Rust ACID embedded KV).
//!
//! Contract mapping (see `store.rs`):
//! - `write_batch` -> one redb write transaction: atomic by ACID, fsync'd at
//!   commit => committed batches survive abrupt process termination.
//! - `scan` -> redb range iterator over an ordered B-tree: ascending key order.
//! - redb `Database` is `Send + Sync`: readers use short-lived read txns.
//!
//! The kernel sees only the `StorageEngine` trait; swapping MemoryEngine ->
//! RedbEngine changes nothing above this seam (conformance-verified).

use crate::knowledge::kom::{KError, KResult};
use crate::storage::store::{StorageEngine, WriteBatch};
use redb::{Database, TableDefinition};
use std::path::Path;

const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("aikoql_kv");

fn se(e: impl std::fmt::Display) -> KError {
    KError::Store(format!("redb: {}", e))
}

pub struct RedbEngine {
    db: Database,
}

impl RedbEngine {
    /// Open (or create) a durable store at `path`.
    ///
    /// Fail-closed on corrupt/truncated files (FAULT-009): redb asserts
    /// internally on short/corrupt headers (page-manager layout check) and
    /// panics instead of erroring — a panic must never escape the storage
    /// boundary as an embedder process crash. An interrupted
    /// backup/restore is rejected, not fatal.
    pub fn open(path: impl AsRef<Path>) -> KResult<Self> {
        let open_inner = || -> KResult<Self> {
            let db = Database::create(path.as_ref()).map_err(se)?;
            // ensure the table exists, even on a fresh file
            let tx = db.begin_write().map_err(se)?;
            {
                let _ = tx.open_table(TABLE).map_err(se)?;
            }
            tx.commit().map_err(se)?;
            Ok(RedbEngine { db })
        };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(open_inner)).unwrap_or_else(|_| {
            Err(KError::Store(format!(
                "redb: corrupt or truncated database file: {}",
                path.as_ref().display()
            )))
        })
    }
}

impl StorageEngine for RedbEngine {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>> {
        let tx = self.db.begin_read().map_err(se)?;
        let t = tx.open_table(TABLE).map_err(se)?;
        Ok(t.get(key).map_err(se)?.map(|g| g.value().to_vec()))
    }

    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let tx = self.db.begin_read().map_err(se)?;
        let t = tx.open_table(TABLE).map_err(se)?;
        let range = t.range::<&[u8]>(prefix..).map_err(se)?;
        let mut out = Vec::new();
        for item in range {
            let (k, v) = item.map_err(se)?;
            let kb = k.value();
            if !kb.starts_with(prefix) {
                break;
            }
            out.push((kb.to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }

    fn write_batch(&self, batch: &WriteBatch) -> KResult<()> {
        let tx = self.db.begin_write().map_err(se)?;
        {
            let mut t = tx.open_table(TABLE).map_err(se)?;
            for (k, v) in &batch.puts {
                t.insert(k.as_slice(), v.as_slice()).map_err(se)?;
            }
            for k in &batch.dels {
                t.remove(k.as_slice()).map_err(se)?;
            }
        }
        // Atomic commit + fsync: this is the durability boundary.
        tx.commit().map_err(se)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aikoql_redb_unit_{}_{}.redb",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn persists_across_reopen() {
        let p = tmp("persist");
        {
            let e = RedbEngine::open(&p).unwrap();
            let mut b = WriteBatch::new();
            b.put(b"k1".to_vec(), vec![1, 2, 3]);
            e.write_batch(&b).unwrap();
        }
        let e2 = RedbEngine::open(&p).unwrap();
        assert_eq!(e2.get(b"k1").unwrap(), Some(vec![1, 2, 3]));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn scan_is_sorted_and_prefix_limited() {
        let p = tmp("scan");
        let e = RedbEngine::open(&p).unwrap();
        let mut b = WriteBatch::new();
        for k in [&b"p/c"[..], &b"p/a"[..], &b"p/b"[..], &b"q/a"[..]] {
            b.put(k.to_vec(), vec![0]);
        }
        e.write_batch(&b).unwrap();
        let got: Vec<Vec<u8>> = e.scan(b"p/").unwrap().into_iter().map(|(k, _)| k).collect();
        assert_eq!(got, vec![b"p/a".to_vec(), b"p/b".to_vec(), b"p/c".to_vec()]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn batch_delete_is_atomic() {
        let p = tmp("batch");
        let e = RedbEngine::open(&p).unwrap();
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
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn snapshot_and_restore_round_trip() {
        // REC-002 engine contract: snapshot_to works while the live db stays
        // open (no file-level copy), and restore_from atomically swaps the
        // contents back — rows absent from the snapshot disappear.
        let p = tmp("snap_src");
        let snap = tmp("snap_dest");
        let e = RedbEngine::open(&p).unwrap();
        let mut b = WriteBatch::new();
        b.put(b"a/1".to_vec(), vec![1]);
        b.put(b"a/2".to_vec(), vec![2]);
        e.write_batch(&b).unwrap();

        e.snapshot_to(&snap).unwrap();

        let mut b = WriteBatch::new();
        b.put(b"a/3".to_vec(), vec![3]);
        b.del(b"a/1".to_vec());
        e.write_batch(&b).unwrap();

        e.restore_from(&snap).unwrap();
        assert_eq!(e.get(b"a/1").unwrap(), Some(vec![1]));
        assert_eq!(e.get(b"a/2").unwrap(), Some(vec![2]));
        assert_eq!(
            e.get(b"a/3").unwrap(),
            None,
            "rows not in the snapshot must be gone after restore"
        );

        // A missing source must be rejected — never treated as an empty wipe.
        let missing = tmp("snap_missing");
        assert!(e.restore_from(&missing).is_err());

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&snap);
    }
}
