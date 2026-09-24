//! P5-M40 (R4-P1-01) — O(1) L0/L1 backlog accounting REDs. Today
//! maybe_compact rescans segment_records on EVERY write (the M35 cell:
//! 345 ns/write, 0.7% at ~64 segments — regime-true, but O(segments),
//! and the segment count grows). M40 keeps three authoritative counters
//! (l0_count / l0_bytes / l1_bytes) in State — updated at open / flush /
//! compaction — and the write trigger reads them in O(1). scan_l0 stays
//! as the debug validator (`Db::debug_scan_l0`), and the parity pin
//! asserts counters ≡ scan_l0 after every structural change.
//!
//! lba001 — the write trigger never rescans: scan_l0_calls stays flat
//!   across writes at 10/100 segments (laptop) and 1,000/10,000
//!   (env-gated, AIKOQL_V2_LBA_CELLS_FULL=1 — the review's sweep pin).
//!   RED: every write with a non-zero trigger calls scan_l0 (the M35
//!   cell's own counter).
//! lba002 — parity: after every flush / compaction / reopen, the
//!   authoritative counters ≡ the scan_l0 recomputation. RED: the
//!   counters do not exist (compile error — SegmentStats carries no
//!   l0_* fields, Db has no debug_scan_l0).

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use common::dir;
use std::path::Path;

fn open_quiet(d: &Path, trigger: usize) -> Db {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.memtable_bytes = 4096;
    cfg.durability = DurabilityMode::Async;
    cfg.l0_compact_trigger = trigger;
    Db::open(cfg).unwrap()
}

/// The segment pile the scan would have to walk: `n` explicit flushes of
/// a handful of entries each (one L0 segment per flush).
fn seed_pile(db: &Db, n: usize) {
    for i in 0..n {
        db.put(format!("k{i:06}").as_bytes(), format!("v{i}").as_bytes())
            .unwrap();
        db.flush().unwrap();
    }
}

#[test]
fn lba001_write_trigger_never_rescans_l0() {
    let gated = std::env::var_os("AIKOQL_V2_LBA_CELLS_FULL").is_some();
    let sizes: &[usize] = if gated {
        &[10, 100, 1_000, 10_000]
    } else {
        &[10, 100]
    };
    for (idx, n) in sizes.iter().enumerate() {
        let d = dir(&format!("lba001-{idx}"));
        // trigger out of reach: the gate never fires, but TODAY the scan
        // still runs on every write before the gate is evaluated.
        let db = open_quiet(&d, usize::MAX);
        seed_pile(&db, *n);
        let count = db.stats().segments.count;
        assert!(
            count >= *n as u64,
            "the pile must have at least {n} segments, got {count}"
        );
        let before = db.stats().write.scan_l0_calls;
        for i in 0..20 {
            db.put(format!("w{i:03}").as_bytes(), b"v").unwrap();
        }
        let after = db.stats().write.scan_l0_calls;
        assert_eq!(
            after, before,
            "the write trigger must not rescan — {n} segments and \
             {before} → {after} scan_l0 calls (the M35 scan still runs \
             per write: the RED)"
        );
    }
}

/// The counters ≡ the debug validator's scan, right now.
fn assert_parity(db: &Db, why: &str) {
    let s = db.stats();
    let (l0, l0_bytes, l1_bytes) = db.debug_scan_l0();
    assert_eq!(
        s.segments.l0_count, l0 as u64,
        "l0_count diverged from the scan ({why})"
    );
    assert_eq!(
        s.segments.l0_bytes, l0_bytes,
        "l0_bytes diverged from the scan ({why})"
    );
    assert_eq!(
        s.segments.l1_bytes, l1_bytes,
        "l1_bytes diverged from the scan ({why})"
    );
}

#[test]
fn lba002_authoritative_counters_match_scan_l0_after_every_change() {
    let d = dir("lba002-parity");
    let db = open_quiet(&d, 0); // explicit compact only
    assert_parity(&db, "fresh open: nothing yet");
    let s = db.stats();
    assert_eq!(s.segments.l0_count, 0, "no L0 at open");

    seed_pile(&db, 2);
    assert_parity(&db, "two flushes");
    assert_eq!(db.stats().segments.l0_count, 2);

    seed_pile(&db, 1);
    assert_parity(&db, "a third flush");
    assert_eq!(db.stats().segments.l0_count, 3);

    db.compact().unwrap();
    assert_parity(&db, "compaction drained L0 into L1");
    let s = db.stats();
    assert_eq!(s.segments.l0_count, 0, "L0 is drained");
    assert!(s.segments.l1_bytes > 0, "L1 holds the merge");

    seed_pile(&db, 1);
    assert_parity(&db, "a flush after the merge");
    assert_eq!(db.stats().segments.l0_count, 1);

    drop(db);
    let db = open_quiet(&d, 0);
    assert_parity(&db, "reopen — the open recompute");
    assert_eq!(db.stats().segments.l0_count, 1, "the L0 pile survived");
}
