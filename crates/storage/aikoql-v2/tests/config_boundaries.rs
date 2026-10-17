//! F-01 (PR #7 fuzz review, PBT-10) — the configuration boundary matrix.
//!
//! Every public knob at 0, 1, b−1, b, b+1 (b = the shipped default) must
//! open, run a scripted workload, and answer identically to a byte-surface
//! reference model — through flush, compact, checkpoint and reopen. A
//! panic, an integer overflow, or a silent divergence at a boundary is the
//! RED this matrix exists to catch; a clean `Invalid` at open is an
//! acceptable fail-safe, never a panic.
//!
//! The workload is a fixed script (no RNG — the boundary is the variable):
//! the degenerate values are what trigger the stress (memtable_bytes=1
//! flushes per op, checkpoint_bytes=1 checkpoints per flush,
//! l0_compact_trigger=1 merges per flush, merge_chunk_bytes=1 splits the
//! merge into 1-byte chunks), so the interactive paths run under pressure.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::format::FormatError;
use common::dir;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy)]
enum Knob {
    MemtableBytes,
    BlockTarget,
    CacheBytes,
    MaxBatchOps,
    MaxBatchBytes,
    MergeChunkBytes,
    CheckpointBytes,
    L0CompactTrigger,
    L0TierRatio,
}

impl Knob {
    fn name(self) -> &'static str {
        match self {
            Knob::MemtableBytes => "memtable_bytes",
            Knob::BlockTarget => "block_target",
            Knob::CacheBytes => "cache_bytes",
            Knob::MaxBatchOps => "max_batch_ops",
            Knob::MaxBatchBytes => "max_batch_bytes",
            Knob::MergeChunkBytes => "merge_chunk_bytes",
            Knob::CheckpointBytes => "checkpoint_bytes",
            Knob::L0CompactTrigger => "l0_compact_trigger",
            Knob::L0TierRatio => "l0_tier_ratio",
        }
    }

    fn set(self, c: &mut Config, v: usize) {
        match self {
            Knob::MemtableBytes => c.memtable_bytes = v,
            Knob::BlockTarget => c.block_target = v,
            Knob::CacheBytes => c.cache_bytes = v,
            Knob::MaxBatchOps => c.max_batch_ops = v,
            Knob::MaxBatchBytes => c.max_batch_bytes = v,
            Knob::MergeChunkBytes => c.merge_chunk_bytes = v,
            Knob::CheckpointBytes => c.checkpoint_bytes = v,
            Knob::L0CompactTrigger => c.l0_compact_trigger = v,
            Knob::L0TierRatio => c.l0_tier_ratio = v,
        }
    }

    fn default(self) -> usize {
        let c = Config::new(PathBuf::new());
        match self {
            Knob::MemtableBytes => c.memtable_bytes,
            Knob::BlockTarget => c.block_target,
            Knob::CacheBytes => c.cache_bytes,
            Knob::MaxBatchOps => c.max_batch_ops,
            Knob::MaxBatchBytes => c.max_batch_bytes,
            Knob::MergeChunkBytes => c.merge_chunk_bytes,
            Knob::CheckpointBytes => c.checkpoint_bytes,
            Knob::L0CompactTrigger => c.l0_compact_trigger,
            Knob::L0TierRatio => c.l0_tier_ratio,
        }
    }
}

const KNOBS: [Knob; 9] = [
    Knob::MemtableBytes,
    Knob::BlockTarget,
    Knob::CacheBytes,
    Knob::MaxBatchOps,
    Knob::MaxBatchBytes,
    Knob::MergeChunkBytes,
    Knob::CheckpointBytes,
    Knob::L0CompactTrigger,
    Knob::L0TierRatio,
];

type Model = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

/// The fixed 16-op script over 8 keys: overwrites, deletes, one
/// resurrection — every op class the byte surface knows.
fn apply_script<F: FnMut(&[u8], Option<&[u8]>)>(mut op: F) {
    let script: Vec<(&str, Option<&str>)> = vec![
        ("k00", Some("v0")),
        ("k01", Some("v0")),
        ("k02", Some("v0")),
        ("k03", Some("v0")),
        ("k01", None),
        ("k04", Some("v0")),
        ("k01", Some("v1")),
        ("k05", Some("v0")),
        ("k00", Some("v1")),
        ("k04", None),
        ("k06", Some("v0")),
        ("k07", Some("v0")),
        ("k02", Some("v1")),
        ("k06", None),
        ("k03", Some("v1")),
        ("k07", Some("v1")),
    ];
    for (k, v) in script {
        op(k.as_bytes(), v.map(str::as_bytes));
    }
}

fn apply_to_model(model: &mut Model) {
    apply_script(|k, v| {
        model.insert(k.to_vec(), v.map(|v| v.to_vec()));
    });
}

fn apply_to_db(db: &Db) {
    apply_script(|k, v| match v {
        Some(v) => {
            db.put(k, v).unwrap();
        }
        None => {
            db.delete(k).unwrap();
        }
    });
}

fn check_model(db: &Db, model: &Model, tag: &str) {
    for (k, want) in model {
        let got = db.get(k).unwrap();
        assert_eq!(&got, want, "{tag}: get({k:?}) diverged");
    }
    // Full-range scan: tombstones suppressed, byte order = key order.
    let rows = db.scan(b"").unwrap();
    let want_rows: Vec<(&Vec<u8>, &Vec<u8>)> = model
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| (k, v)))
        .collect();
    assert_eq!(rows.len(), want_rows.len(), "{tag}: scan count");
    for ((rk, rv), (wk, wv)) in rows.iter().zip(want_rows.iter()) {
        assert_eq!(rk, *wk, "{tag}: scan key order");
        assert_eq!(rv, *wv, "{tag}: scan value");
    }
}

#[test]
fn config_boundary_matrix_runs_clean_and_answers_correctly() {
    for knob in KNOBS {
        let b = knob.default();
        let mut values = vec![0, 1, b.saturating_sub(1), b, b.saturating_add(1)];
        values.sort_unstable();
        values.dedup();
        for v in values {
            eprintln!("config {}={}", knob.name(), v);
            let make = |d: PathBuf| {
                let mut c = Config::new(d);
                knob.set(&mut c, v);
                c
            };
            let d = dir("config-boundary");
            let model = {
                let mut m = Model::new();
                apply_to_model(&mut m);
                m
            };
            let db = match Db::open(make(d.clone())) {
                Ok(db) => db,
                // A clean reject at a degenerate value is a fail-safe, not
                // a finding — a panic or a wrong answer is.
                Err(FormatError::Invalid(why)) => {
                    eprintln!("  clean Invalid at open: {why}");
                    continue;
                }
                Err(e) => panic!("{}={}: open failed with {e}", knob.name(), v),
            };
            apply_to_db(&db);
            db.flush().unwrap();
            db.compact().unwrap();
            db.checkpoint_now().unwrap();
            check_model(&db, &model, &format!("{}={} live", knob.name(), v));
            drop(db);
            let db2 = match Db::open(make(d.clone())) {
                Ok(db) => db,
                Err(FormatError::Invalid(why)) => {
                    eprintln!("  clean Invalid at reopen: {why}");
                    continue;
                }
                Err(e) => panic!("{}={}: reopen failed with {e}", knob.name(), v),
            };
            check_model(&db2, &model, &format!("{}={} reopened", knob.name(), v));
        }
    }
}
