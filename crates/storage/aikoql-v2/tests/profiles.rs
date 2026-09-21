//! P5-M35 (P1-06 + P1-07) — control-plane lock-wait instrumentation and
//! the write-path scan_l0 cost profile. prof001 pins the counters exist
//! and are readable (the RED is a compile error: `DbStats` carries no
//! `control` block yet). prof002 pins the scan-cost evidence cell
//! (env-gated): the A/B wall delta (trigger 0 = the scan never runs vs
//! a trigger out of reach = the scan runs on every write, compaction
//! never fires) plus the direct `scan_l0_*` counters — the data the
//! publication-time-counter decision (the review's own gate) needs.
//!
//! prof002 runs only with AIKOQL_V2_PROF_CELLS_PROFILE set — 200k
//! writes total, CI never pays for it. Run:
//!
//!   AIKOQL_V2_PROF_CELLS_PROFILE=1 cargo test -p aikoql-storage-v2 \
//!     --test profiles -- --nocapture

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::identity::ObjectId;
use common::dir;
use std::time::Instant;

#[test]
fn prof001_control_plane_lock_wait_counters_are_readable() {
    let db = Db::open(Config::new(dir("prof001"))).unwrap();
    // The control-plane surface: stats() + the resolve paths. Each takes
    // the global state read lock — the counters record the acquisition
    // wait (the M21 lock_wait_ns pattern), pooled per family.
    let first = db.stats();
    assert!(
        first.control.stats_waits >= 1,
        "stats() counts its own lock wait"
    );
    let _ = db.resolve_object(ObjectId([7u8; 16]));
    let _ = db.resolve_object(ObjectId([8u8; 16]));
    let second = db.stats();
    assert!(
        second.control.stats_waits > first.control.stats_waits,
        "every stats() call counts one wait"
    );
    assert!(
        second.control.resolve_waits >= 2,
        "the resolve paths counted their waits"
    );
    // The wait spans ride the same snapshots (advisory ns totals — an
    // uncontended acquire may read 0 at clock resolution; the contention
    // evidence is the prof002 cell's job).
    let _ = (first.control.stats_wait_ns, second.control.resolve_wait_ns);
}

/// One A/B regime: 100k Async writes (fsync would dominate the cell),
/// 64 KiB memtable (~64 flushes — a realistic segment pile for the scan
/// to walk). Returns (wall µs, scan calls, scan ns) from the write
/// stats.
fn run_regime(name: &str, trigger: usize) -> (u64, u64, u64) {
    const WRITES: u64 = 100_000;
    let mut cfg = Config::new(dir(name));
    cfg.durability = DurabilityMode::Async;
    cfg.memtable_bytes = 64 << 10;
    cfg.l0_compact_trigger = trigger;
    cfg.compact_background = false;
    let db = Db::open(cfg).unwrap();
    let t = Instant::now();
    for i in 0..WRITES {
        db.put(format!("k{i:06}").as_bytes(), b"v").unwrap();
    }
    let wall_us = t.elapsed().as_micros() as u64;
    let w = db.stats().write;
    (wall_us, w.scan_l0_calls, w.scan_l0_ns)
}

#[test]
fn prof002_scan_l0_cost_per_write_cell() {
    if std::env::var_os("AIKOQL_V2_PROF_CELLS_PROFILE").is_none() {
        return; // env-gated — 200k writes across the two regimes
    }
    // A: trigger 0 — maybe_compact returns BEFORE the scan (baseline).
    // B: trigger 1<<30 — the scan runs on every write, never fires
    // (l1 empty ⇒ the tier gate passes, the count gate never does).
    // Same shape otherwise; the wall delta is the scan's end-to-end
    // price, the counters its direct price.
    let (a_wall, _, _) = run_regime("prof002-a", 0);
    let (b_wall, scans, scan_ns) = run_regime("prof002-b", 1 << 30);
    assert!(scans >= 100_000, "the B regime scanned on every write");
    assert!(scan_ns > 0, "the scan's direct cost is recorded");
    assert!(
        b_wall >= a_wall,
        "the scan is not free: B pays it, A does not"
    );
    // The evidence cell — the decision input, not a gate: per-write scan
    // cost at 100k ops throughput.
    let cells = dir("prof002-cells").join("cells.json");
    let body = format!(
        "{{\"writes\":100000,\"a_wall_us\":{a_wall},\"b_wall_us\":{b_wall},\
         \"scan_calls\":{scans},\"scan_ns\":{scan_ns},\"per_write_scan_ns\":{}}}",
        scan_ns / scans.max(1)
    );
    std::fs::write(&cells, &body).unwrap();
    eprintln!("[prof002 cells] {body}");
}
