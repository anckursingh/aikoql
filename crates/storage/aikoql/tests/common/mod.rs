//! Shared harness pieces for the KSE measurement suites (kse5, kse6, …).
#![allow(dead_code)] // each suite uses only the pieces it needs

use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Pass-through engine that counts every kernel→engine request.
pub struct CountingEngine {
    pub inner: Arc<dyn StorageEngine>,
    gets: AtomicU64,
    scan_calls: AtomicU64,
    scan_pairs: AtomicU64,
    bytes_returned: AtomicU64,
    write_batches: AtomicU64,
    puts: AtomicU64,
    dels: AtomicU64,
}

impl CountingEngine {
    pub fn new(inner: Arc<dyn StorageEngine>) -> Arc<Self> {
        Arc::new(CountingEngine {
            inner,
            gets: AtomicU64::new(0),
            scan_calls: AtomicU64::new(0),
            scan_pairs: AtomicU64::new(0),
            bytes_returned: AtomicU64::new(0),
            write_batches: AtomicU64::new(0),
            puts: AtomicU64::new(0),
            dels: AtomicU64::new(0),
        })
    }
}

impl StorageEngine for CountingEngine {
    fn get(&self, key: &[u8]) -> aikoql_kernel::KResult<Option<Vec<u8>>> {
        self.gets.fetch_add(1, Ordering::Relaxed);
        let v = self.inner.get(key)?;
        if let Some(v) = &v {
            self.bytes_returned
                .fetch_add(v.len() as u64, Ordering::Relaxed);
        }
        Ok(v)
    }

    fn scan(&self, prefix: &[u8]) -> aikoql_kernel::KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_calls.fetch_add(1, Ordering::Relaxed);
        let rows = self.inner.scan(prefix)?;
        self.scan_pairs
            .fetch_add(rows.len() as u64, Ordering::Relaxed);
        let bytes: u64 = rows.iter().map(|(k, v)| (k.len() + v.len()) as u64).sum();
        self.bytes_returned.fetch_add(bytes, Ordering::Relaxed);
        Ok(rows)
    }

    fn write_batch(&self, batch: &WriteBatch) -> aikoql_kernel::KResult<()> {
        self.write_batches.fetch_add(1, Ordering::Relaxed);
        self.puts
            .fetch_add(batch.puts.len() as u64, Ordering::Relaxed);
        self.dels
            .fetch_add(batch.dels.len() as u64, Ordering::Relaxed);
        self.inner.write_batch(batch)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalCounts {
    pub gets: u64,
    pub scans: u64,
    pub pairs: u64,
    pub bytes: u64,
}

impl LogicalCounts {
    pub fn snapshot(c: &CountingEngine) -> LogicalCounts {
        LogicalCounts {
            gets: c.gets.load(Ordering::Relaxed),
            scans: c.scan_calls.load(Ordering::Relaxed),
            pairs: c.scan_pairs.load(Ordering::Relaxed),
            bytes: c.bytes_returned.load(Ordering::Relaxed),
        }
    }

    pub fn delta(&self, before: LogicalCounts) -> LogicalCounts {
        LogicalCounts {
            gets: self.gets - before.gets,
            scans: self.scans - before.scans,
            pairs: self.pairs - before.pairs,
            bytes: self.bytes - before.bytes,
        }
    }

    pub fn writes(c: &CountingEngine) -> (u64, u64, u64) {
        (
            c.write_batches.load(Ordering::Relaxed),
            c.puts.load(Ordering::Relaxed),
            c.dels.load(Ordering::Relaxed),
        )
    }
}

impl std::fmt::Display for LogicalCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} gets + {} scans ({} pairs, {} B returned)",
            self.gets, self.scans, self.pairs, self.bytes
        )
    }
}

pub fn percentiles(mut xs: Vec<u128>) -> (u128, u128, u128) {
    xs.sort_unstable();
    let p = |q: f64| xs[((xs.len() - 1) as f64 * q).round() as usize];
    (p(0.50), p(0.95), p(0.99))
}

pub fn tmp(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("aikoql_kse_unit_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_dir_all(&p);
    p
}
