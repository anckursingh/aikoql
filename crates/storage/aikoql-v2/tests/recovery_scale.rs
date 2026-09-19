//! PR6-R2-007 — recovery performance after checkpoint (review P1 Recovery).
//!
//! The review's recipe: checkpoint at G, create N generations, reopen,
//! measure validation/recovery time at N = 100 / 10,000 / 100,000, with the
//! acceptance "historical generation count must not dominate recovery after
//! checkpoint". Env-gated (RECOVERY_PERF_NIGHTLY=1, strict opt-in).
//!
//! The structural pin rides the R2-002 fix: before every reopen, ALL
//! intermediate manifests (checkpoint+1 ..= CURRENT-1) are deleted. The
//! pre-R2-002 manifest-chain walk fails closed on the first missing
//! manifest (the R2-002 RED, Io("read MANIFEST-000005 ... os error 2")),
//! so this cell proves at scale that open-time coverage validation reads
//! zero historical manifests — the validator re-folds the surviving
//! post-checkpoint delta files against the checkpoint's chain bases and
//! requires equality with CURRENT's manifest (R2-002 commit). The budget
//! catches any accidental superlinear or history-replay IO; the timings
//! are reported for the N = 100 / 10k / 100k comparison the review asks for.

mod common;

use aikoql_storage_v2::db::{manifest_path, Config, Db};
use aikoql_storage_v2::identity::ObjectId;
use common::dir;
use std::path::Path;
use std::time::{Duration, Instant};

fn oid(i: u64) -> ObjectId {
    ObjectId(i.to_le_bytes().repeat(2).try_into().unwrap())
}

fn reopen_cfg(d: &Path) -> Config {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.checkpoint_bytes = 0; // no auto-checkpoint: the generations stay deltas
    cfg.l0_compact_trigger = 0;
    cfg
}

/// One generation: a NEW object (identity + replica + placement work),
/// flushed alone, so generation `g` publishes exactly one log per family.
fn write_generation(db: &Db, g: u64) {
    db.put_object(oid(g), b"k", &g.to_le_bytes()).unwrap();
    db.flush().unwrap();
}

/// The structural pin: remove every manifest between the checkpoint and
/// CURRENT. Recovery must not read any of them.
fn delete_intermediate_manifests(d: &Path, checkpoint_gen: u64, current_gen: u64) {
    for g in (checkpoint_gen + 1)..current_gen {
        let p = manifest_path(d, g);
        if p.exists() {
            std::fs::remove_file(p).unwrap();
        }
    }
}

fn open_timed(d: &Path) -> Duration {
    let t = Instant::now();
    Db::open(reopen_cfg(d)).unwrap();
    t.elapsed()
}

#[test]
fn recovery_scale_does_not_read_historical_manifests() {
    if std::env::var_os("RECOVERY_PERF_NIGHTLY").is_none() {
        return; // strict opt-in: builds up to 100k generations
    }
    // Default = the review's three data points; overridable so a laptop
    // smoke run can shrink the matrix without touching the CI defaults.
    let sizes: Vec<u64> = std::env::var("AIKOQL_RECOVERY_PERF_SIZES")
        .map(|s| s.split(',').map(|x| x.trim().parse().unwrap()).collect())
        .unwrap_or_else(|_| vec![100, 10_000, 100_000]);
    for n in sizes {
        let d = dir(&format!("r2-007-{n}"));
        let db = Db::open(reopen_cfg(&d)).unwrap();
        for i in 0..5u64 {
            db.put_object(oid(i), b"k", &[i as u8]).unwrap();
        }
        db.flush().unwrap(); // generation 2
        db.checkpoint_now().unwrap(); // CHECKPOINT-2 subsumes the gen-2 logs
        drop(db);
        let db = Db::open(reopen_cfg(&d)).unwrap();
        for g in 0..n {
            write_generation(&db, 0x1000 + g); // generations 3 ..= n+2
        }
        let current = n + 2;
        drop(db);
        delete_intermediate_manifests(&d, 2, current);
        let t = open_timed(&d);
        // Data survives: the baseline object and the newest generation.
        let db = Db::open(reopen_cfg(&d)).unwrap();
        assert_eq!(db.get_object(oid(0), b"k").unwrap(), Some(vec![0]));
        let last = 0x1000 + n - 1;
        assert_eq!(
            db.get_object(oid(last), b"k").unwrap(),
            Some(last.to_le_bytes().to_vec())
        );
        println!("R2-007 report: n={n} generations, reopen {t:?}");
        assert!(
            t < Duration::from_secs(600),
            "recovery at n={n} must stay budgeted: checkpoint + deltas + WAL, \
             not history (the deleted manifests already fail a history walk)"
        );
    }
}
