//! P3-M8 §71 acceptance cell — W8 write-tail, background vs synchronous
//! baseline. The M8 claim is that the trigger-crossing write no longer
//! bears the merge: in synchronous mode (compact_background = false) the
//! crossing put's ack includes the whole compaction; in background mode
//! it includes at most a kick (+ the hard-bound wait, which the probe's
//! steady state never hits). Reported, not asserted — the probe prints
//! p50/p99/max of write acks in both modes; the honesty asserts are the
//! op accounting and zero compaction errors. Strict opt-in: unset
//! `M8_W8_NIGHTLY` skips (the SE2M14 pattern).

mod common;

use aikoql_storage_v2::db::{Config, Db};
use common::{dir, percentiles};
use std::time::Instant;

/// The W8 mixed shape (kse_m7_v2_workloads): 70% put, 20% get, 10% delete
/// over a small memtable so every ~8 puts flush and every 4th flush
/// crosses the L0 trigger — the crossing put is where the tail lives.
fn run(mode: &str, background: bool) -> (usize, Vec<u128>) {
    let mut cfg = Config::new(dir(&format!("w8-tail-{mode}")));
    cfg.memtable_bytes = 512; // the tiered fixture: ~8 puts per flush
    cfg.compact_background = background;
    let db = Db::open(cfg).unwrap();
    let mut next = 42u64;
    let mut lat = Vec::new();
    for i in 0..2000u32 {
        let k = format!("k{:03}", next % 200).into_bytes();
        next ^= next << 13;
        next ^= next >> 7;
        next ^= next << 17;
        match next % 10 {
            0..=6 => {
                let v = format!("v{i:04}{}", "y".repeat(40)).into_bytes();
                let t = Instant::now();
                db.put(&k, &v).unwrap();
                lat.push(t.elapsed().as_micros());
            }
            7..=8 => {
                db.get(&k).unwrap();
            }
            _ => {
                let t = Instant::now();
                db.delete(&k).unwrap();
                lat.push(t.elapsed().as_micros());
            }
        }
    }
    db.wait_compactor_idle();
    let errors = db.stats().write.compaction_error_count;
    assert_eq!(errors, 0, "probe runs must be clean: {errors} merge errors");
    drop(db);
    (lat.len(), lat)
}

#[test]
fn w8_write_tail_background_vs_synchronous() {
    if std::env::var_os("M8_W8_NIGHTLY").is_none() {
        eprintln!("SKIPPED (set M8_W8_NIGHTLY=1 for the W8 write-tail cell)");
        return;
    }
    let (n_sync, sync) = run("sync", false);
    let (n_bg, bg) = run("bg", true);
    assert_eq!(
        n_sync, n_bg,
        "same op draw must produce the same write count"
    );
    let (p50s, _, p99s) = percentiles(sync.clone());
    let (p50b, _, p99b) = percentiles(bg.clone());
    let maxs = sync.iter().max().copied().unwrap_or(0);
    let maxb = bg.iter().max().copied().unwrap_or(0);
    eprintln!(
        "W8 write-tail: n={n_bg} | synchronous p50={p50s}µs p99={p99s}µs max={maxs}µs | \
         background p50={p50b}µs p99={p99b}µs max={maxb}µs"
    );
}
