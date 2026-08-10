//! RocksDB storage backend for Mnemosyne (MVP).
//!
//! Implements the `StorageEngine` trait using RocksDB — a battle-tested
//! LSM-tree engine with concurrent readers AND writers (unlike redb's
//! single-writer limitation).
//!
//! ## Contract mapping
//!
//! - `write_batch` → `rocksdb::WriteBatch` + `WriteOptions`: atomic by
//!   RocksDB's WriteBatch guarantee. Durability via `set_sync(true)`.
//! - `scan` → prefix iterator over default column family. Keys are sorted
//!   ascending by RocksDB's internal comparator (lexicographic byte order).
//! - `get` → single-key point lookup.

use mnemosyne_kernel::knowledge::kom::{KError, KResult};
use mnemosyne_kernel::storage::store::{StorageEngine, WriteBatch};
use rocksdb::{Options, WriteBatch as Rwb, WriteOptions, DB};
use std::path::Path;

fn se(e: impl std::fmt::Display) -> KError {
    KError::Store(format!("rocksdb: {}", e))
}

/// RocksDB-backed storage engine. Thread-safe, supports concurrent
/// readers and writers.
pub struct RocksDbEngine {
    db: DB,
}

impl RocksDbEngine {
    /// Open (or create) a RocksDB database at `path`.
    ///
    /// Creates the default column family and applies performance-oriented
    /// defaults: 64 MB write buffer, 4 background threads, snappy compression.
    pub fn open(path: impl AsRef<Path>) -> KResult<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        // Performance defaults for write-heavy knowledge workloads.
        opts.set_write_buffer_size(64 * 1024 * 1024); // 64 MB
        opts.set_max_background_jobs(4);
        opts.set_compression_type(rocksdb::DBCompressionType::Snappy);
        opts.increase_parallelism(4);

        let db = DB::open(&opts, path).map_err(se)?;
        Ok(RocksDbEngine { db })
    }
}

impl StorageEngine for RocksDbEngine {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>> {
        self.db.get(key).map_err(se)
    }

    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let iter = self.db.iterator(rocksdb::IteratorMode::Start);
        let mut out = Vec::new();
        for item in iter {
            let (k, v) = item.map_err(se)?;
            if !k.starts_with(prefix) {
                // Past the prefix range — short-circuit on first non-matching key.
                // Relies on RocksDB's lexicographic ordering.
                if k.as_ref() > prefix {
                    break;
                }
                continue;
            }
            out.push((k.to_vec(), v.to_vec()));
        }
        Ok(out)
    }

    fn write_batch(&self, batch: &WriteBatch) -> KResult<()> {
        let mut wb = Rwb::default();
        for (k, v) in &batch.puts {
            wb.put(k, v);
        }
        for k in &batch.dels {
            wb.delete(k);
        }
        let mut wo = WriteOptions::default();
        wo.set_sync(true); // fsync for durability (MRFC-0008 contract)
        self.db.write_opt(wb, &wo).map_err(se)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mnemosyne_rocksdb_unit_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn persists_across_reopen() {
        let p = tmp("persist");
        {
            let e = RocksDbEngine::open(&p).unwrap();
            let mut b = WriteBatch::new();
            b.put(b"k1".to_vec(), vec![1, 2, 3]);
            e.write_batch(&b).unwrap();
        }
        let e2 = RocksDbEngine::open(&p).unwrap();
        assert_eq!(e2.get(b"k1").unwrap(), Some(vec![1, 2, 3]));
        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn scan_is_sorted_and_prefix_limited() {
        let p = tmp("scan");
        let e = RocksDbEngine::open(&p).unwrap();
        let mut b = WriteBatch::new();
        for k in [&b"p/c"[..], &b"p/a"[..], &b"p/b"[..], &b"q/a"[..]] {
            b.put(k.to_vec(), vec![0]);
        }
        e.write_batch(&b).unwrap();
        let got: Vec<Vec<u8>> = e.scan(b"p/").unwrap().into_iter().map(|(k, _)| k).collect();
        assert_eq!(got, vec![b"p/a".to_vec(), b"p/b".to_vec(), b"p/c".to_vec()]);
        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn batch_delete_is_atomic() {
        let p = tmp("batch");
        let e = RocksDbEngine::open(&p).unwrap();
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
        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn get_missing_returns_none() {
        let p = tmp("missing");
        let e = RocksDbEngine::open(&p).unwrap();
        assert_eq!(e.get(b"nope").unwrap(), None);
        let _ = std::fs::remove_dir_all(&p);
    }
}
