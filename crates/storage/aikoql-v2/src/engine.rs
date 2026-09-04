//! V2-Adopt — the kernel `StorageEngine` adapter over the v2 Db.
//!
//! The v2 Db owns its own locking (RwLock<State> per Db, the committer
//! thread, the OS file lock), so the adapter's RwLock only guards the
//! `&mut self` writes: readers share, writers serialize — the kernel's
//! KSE-13 concurrency contract pinned behaviorally at the engine boundary.
//! Defaults are the v2 defaults (Sync durability, 64 MiB memtable, 8 MiB
//! block cache) — one Config knob away for a caller that wants others
//! (V2-Adopt gate: memory limits configurable). Writes go through
//! `Db::write` — one frame per batch, durable before the ack, all-or-
//! nothing by construction (M2). An empty batch is a no-op (KSE-005 —
//! `Db::write` rejects empty frames, so the adapter never forwards one).
//! REC-002 snapshot/restore ride the trait defaults (full scan + redb
//! snapshot) — the adapter needs no override.

use crate::db::{Config, Db};
use crate::stats::ReadPathStats;
use crate::wal::Op;
use aikoql_kernel::knowledge::kom::{KError, KResult};
use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};
use std::path::Path;
use std::sync::RwLock;

fn se(e: impl std::fmt::Display) -> KError {
    KError::Store(format!("aikoql-v2: {e}"))
}

fn poisoned() -> KError {
    KError::Store("aikoql-v2: db lock poisoned".into())
}

/// AIKOQL v2 engine: bounded WAL → memtable → immutable segments, served
/// through the kernel's storage contract. NOT the production default —
/// the V2-Adopt gate (KSE-20 conformance + §26 matrix) decides that.
pub struct AikoqlStorageEngineV2 {
    db: RwLock<Db>,
}

impl AikoqlStorageEngineV2 {
    /// Open (or create) a durable database at `path` with the v2 defaults.
    pub fn open(path: impl AsRef<Path>) -> KResult<Self> {
        let db = Db::open(Config::new(path.as_ref().to_path_buf())).map_err(se)?;
        Ok(AikoqlStorageEngineV2 {
            db: RwLock::new(db),
        })
    }

    /// Open with an explicit Config — the memory-limit knobs (memtable
    /// bytes, block cache bytes) live here (§26: configurable memory).
    pub fn open_with_config(config: Config) -> KResult<Self> {
        let db = Db::open(config).map_err(se)?;
        Ok(AikoqlStorageEngineV2 {
            db: RwLock::new(db),
        })
    }

    /// SE2-M21 — the Db's cumulative read-path counters, reachable through
    /// the adapter: the attribution probe measures the kernel leg (kernel
    /// op → engine gets) against the engine leg (this engine's gets) on
    /// one dataset.
    pub fn read_path_stats(&self) -> KResult<ReadPathStats> {
        self.db
            .read()
            .map_err(|_| poisoned())
            .map(|d| d.read_path_stats())
    }
}

impl StorageEngine for AikoqlStorageEngineV2 {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>> {
        self.db.read().map_err(|_| poisoned())?.get(key).map_err(se)
    }

    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.db
            .read()
            .map_err(|_| poisoned())?
            .scan(prefix)
            .map_err(se)
    }

    fn write_batch(&self, batch: &WriteBatch) -> KResult<()> {
        if batch.is_empty() {
            return Ok(()); // KSE-005: no state change
        }
        // Puts before dels — the shared contract order (KSE-006).
        let mut ops = Vec::with_capacity(batch.puts.len() + batch.dels.len());
        for (k, v) in &batch.puts {
            ops.push(Op::Put(k.clone(), v.clone()));
        }
        for k in &batch.dels {
            ops.push(Op::Delete(k.clone()));
        }
        self.db
            .write()
            .map_err(|_| poisoned())?
            .write(&ops)
            .map_err(se)?;
        Ok(())
    }
}
