//! P5-M34 (P1-05) — snapshot double-read measurement cells. The copy
//! hashes while writing, then the verify re-reads every copied byte —
//! ~2× read traffic at large sizes. MEASURE FIRST (the review's rule):
//! snp004 pins the cell harness (AIKOQL_V2_SNAP_CELLS=path writes a JSON
//! sidecar: phase walls + byte counts) and the cells' shape; the
//! double-read protocol is KEPT unless a single-pass replacement can
//! PROVE mutation detection (no such RED exists — do not weaken
//! integrity for speed).
//!
//! Env-gated: the body (a ~64 MiB generation) runs only with
//! AIKOQL_V2_SNAP_CELLS_PROFILE set — CI never pays for it. Run:
//!
//!   AIKOQL_V2_SNAP_CELLS_PROFILE=1 cargo test -p aikoql-storage-v2 \
//!     --test snapshot_cells -- --nocapture

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::snapshot::restore_from;
use common::dir;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn walk(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.scan(b"").unwrap().into_iter().collect()
}

/// One integer field of the fixed-format sidecar: `"key":value,`.
fn cell_value(raw: &str, key: &str) -> u64 {
    let needle = format!("\"{key}\":");
    let rest = raw
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("the sidecar must carry {key:?} — got {raw:?}"));
    rest.split([',', '}'])
        .next()
        .expect("a value after the key")
        .trim()
        .parse()
        .expect("an integer cell")
}

#[test]
fn snp004_cell_harness_writes_the_measurement_cells() {
    if std::env::var_os("AIKOQL_V2_SNAP_CELLS_PROFILE").is_none() {
        return; // env-gated — the body is a ~64 MiB snapshot
    }
    let d = dir("snp004-live");
    let mut cfg = Config::new(d.clone());
    cfg.memtable_bytes = 1 << 20; // a flush per put → the bulk is SEGMENTS
    cfg.checkpoint_bytes = 0;
    cfg.compact_background = false;
    let db = Db::open(cfg).unwrap();
    let val = vec![b'v'; 1 << 20];
    for i in 0..64u64 {
        db.put(format!("k{i:03}").as_bytes(), &val).unwrap();
    }
    let expected = walk(&db);

    let cells_path = dir("snp004-cells").join("cells.json");
    std::env::set_var("AIKOQL_V2_SNAP_CELLS", &cells_path);

    // The sampler: sfm009's pattern (sampling own RSS inline deadlocks).
    let base = common::self_rss_kb();
    let peak = Arc::new(AtomicU64::new(base));
    let stop = Arc::new(AtomicBool::new(false));
    let (p2, s2) = (Arc::clone(&peak), Arc::clone(&stop));
    let sampler = std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            p2.fetch_max(common::self_rss_kb(), Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(10));
        }
    });

    let snap = dir("snp004-snap");
    db.snapshot_to(&snap).unwrap();
    stop.store(true, Ordering::Relaxed);
    sampler.join().expect("sampler thread");

    // The RED: no sidecar exists today — the harness must write the cells.
    let raw = std::fs::read_to_string(&cells_path).expect("the harness writes the cells");
    eprintln!("[snp004 cells] {raw}");
    let copy_ms = cell_value(&raw, "copy_ms");
    let verify_ms = cell_value(&raw, "verify_ms");
    assert!(copy_ms > 0, "the copy phase wall is recorded");
    assert!(verify_ms > 0, "the verify phase wall is recorded");
    let bytes_copied = cell_value(&raw, "bytes_copied");
    assert!(
        cell_value(&raw, "read_bytes") == 2 * bytes_copied,
        "the double-read claim: every copied byte is read again by the verify"
    );
    let growth_kb = peak.load(Ordering::Relaxed).saturating_sub(base);
    assert!(
        growth_kb < 48 << 10,
        "the snapshot streams: RSS grew {growth_kb} KiB — the copy + verify \
         must not materialize the generation"
    );
    let restored = restore_from(&snap, dir("snp004-target")).unwrap();
    assert_eq!(walk(&restored), expected, "the double-read protocol held");
}
