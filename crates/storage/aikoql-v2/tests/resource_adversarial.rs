//! L-17 (TDD-017 + TDD-018) — replay memory and the compaction
//! allocation adversarial cases.
//!
//! TDD-017 (P0, "WAL replay memory scaling"): a crash-child writes a
//! 10 MB / 100 MB WAL (4 KiB values, Async mode) and exits WITHOUT a
//! Db::drop — the graceful drop would flush the memtable into a segment,
//! so the WAL stays the rows' only copy, exactly like a real crash. The
//! parent then reopens (recovery) inside a LIVE+PEAK byte-counting
//! window. The pins are structural: retained memory must be bounded by
//! the WAL size (the rebuilt memtable IS the decoded WAL — ~1×, headroom
//! 2×), the transient EXTRA — what replay held above the final retained
//! (PEAK − LIVE_before − retained) — must stay far below the WAL (a
//! read_to_end-style slurp would add a second full copy — cap at WAL/4 +
//! 4 MB), and the retained/WAL ratio must not grow between 10 MB and
//! 100 MB (that growth IS "overhead linear in WAL size" beyond the data
//! itself). PEAK is reset at the window start — it is a process-global
//! monotonic counter, and a sibling's prior peak would blind the assert.
//!
//! TDD-018 (P1, "compaction allocation adversarial"): four fixtures —
//! 1k keys × 1/4/16 versions and 100 keys × 1k versions — with mixed
//! replica IDs: every 8th (key + version) is an object put rotated
//! across three oids, the rest byte puts, so each surface's newest
//! version must win after flush → compact → reopen, and the object
//! surface must never leak to the byte surface. Per-entry compaction
//! allocations are measured per fixture with the slope assert on the
//! 16k → 100k entry pair (per-entry@100k ≤ 1.5 × per-entry@16k — a
//! quadratic term inflates it ~6×) plus an absolute cap; the compaction
//! transient peak is bounded by the data volume with slack.
//!
//! One binary: the counting allocator is #[global_allocator] — every
//! test in this binary takes TEST_LOCK, and the crash-child arm runs
//! BEFORE the lock (the parent holds it while waiting for the child).

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode, WAL_FILE};
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

// The counting allocator, live-bytes edition: LIVE nets out (dealloc
// subtracts), PEAK keeps the max of LIVE (a process-global monotonic
// counter — reset it at each window start), ALLOCS counts alloc+realloc
// events for the per-op scaling pins.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(live, Ordering::Relaxed);
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size >= layout.size() {
            let live = LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed) + new_size
                - layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// PEAK := LIVE now — the next peak reading measures only this window.
fn reset_peak() {
    PEAK.store(live(), Ordering::Relaxed);
}

fn allocs() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

/// Serial guard: the alloc counters are process-wide; a sibling running
/// concurrently would add its steady-state allocations to the window.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// No flush (the memtable sits above every workload here), no background
/// merge (l0_compact_trigger 0) — the measured window holds only the op
/// under test, never a compactor's steady-state allocations.
fn open_big(d: &Path) -> Db {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.memtable_bytes = 1 << 30;
    cfg.l0_compact_trigger = 0;
    cfg.durability = DurabilityMode::Async;
    Db::open(cfg).unwrap()
}

// ---------------------------------------------------------------------------
// TDD-017 — WAL replay memory scaling
// ---------------------------------------------------------------------------

const CHILD_ENV: &str = "AIKOQL_V2_REPLAY_CHILD";
const MB_ENV: &str = "AIKOQL_V2_REPLAY_MB";
const DIR_ENV: &str = "AIKOQL_V2_REPLAY_DIR";

#[test]
fn tdd017_replay_peak_overhead_stays_flat_across_wal_sizes() {
    // Crash-child arm FIRST — the parent holds TEST_LOCK while it waits
    // for us, so this must run before any lock.
    if env::var_os(CHILD_ENV).is_some() {
        let mb: u64 = env::var(MB_ENV).unwrap().parse().unwrap();
        let d = PathBuf::from(env::var(DIR_ENV).unwrap());
        let db = open_big(&d);
        let val = [b'x'; 4096];
        for i in 0..(mb * 256) {
            db.put(format!("k{i:08}").as_bytes(), &val).unwrap();
        }
        std::process::exit(0); // no Db::drop — the WAL stays the rows' only copy
    }
    let _serial = serial();
    let mut ratio_at = [0.0f64; 2]; // [10 MB, 100 MB]
    for (slot, mb) in [10u64, 100].into_iter().enumerate() {
        let d = dir(&format!("replay-{mb}mb"));
        let mut child = Command::new(env::current_exe().expect("current exe"))
            .arg("--exact")
            .arg("tdd017_replay_peak_overhead_stays_flat_across_wal_sizes")
            .env(CHILD_ENV, "1")
            .env(MB_ENV, mb.to_string())
            .env(DIR_ENV, &d)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");
        assert!(
            child.wait().expect("wait child").success(),
            "crash-child for {mb} MB"
        );
        let wal = std::fs::metadata(d.join(WAL_FILE)).expect("WAL").len();
        assert!(
            wal >= mb * 900_000,
            "child wrote only {wal} WAL bytes for {mb} MB"
        );
        let live_before = live();
        reset_peak();
        let db = open_big(&d); // recovery runs inside the window
        let retained = live() - live_before;
        // PEAK measures the max of LIVE — live rises monotonically as the
        // memtable rebuilds, so peak − live_before counts the retained
        // accumulation too. The slurp discriminator is the extra ABOVE
        // the final retained: what replay held and did not keep.
        let transient_extra = peak() - live_before - retained;
        let ratio = retained as f64 / wal as f64;
        eprintln!(
            "RECORDED tdd017 mb={mb} wal={wal} retained={retained} \
             transient_extra={transient_extra} ratio={ratio:.3}",
            mb = mb,
            wal = wal,
            retained = retained,
            transient_extra = transient_extra,
            ratio = ratio
        );
        assert!(
            retained as u64 <= wal * 2,
            "retained {retained} exceeds 2x the {wal}-byte WAL"
        );
        assert!(
            transient_extra as u64 <= wal / 4 + 4 * 1024 * 1024,
            "transient_extra {transient_extra} exceeds WAL/4 + 4 MB for a \
             {wal}-byte WAL — a slurp-level buffer exists"
        );
        let val = [b'x'; 4096];
        let ops = mb * 256;
        for k in [0, ops / 2, ops - 1] {
            assert_eq!(
                db.get(format!("k{k:08}").as_bytes()).unwrap().as_deref(),
                Some(val.as_slice()),
                "row {k} survived the crash replay"
            );
        }
        drop(db);
        ratio_at[slot] = ratio;
    }
    assert!(
        ratio_at[1] <= ratio_at[0] * 1.25,
        "retained/WAL ratio grew from {:.3} to {:.3} — overhead linear in WAL size",
        ratio_at[0],
        ratio_at[1]
    );
}

// ---------------------------------------------------------------------------
// TDD-018 — compaction allocation adversarial cases
// ---------------------------------------------------------------------------

#[test]
fn tdd018_compaction_allocation_scales_and_winners_survive() {
    let _serial = serial();
    let fixtures = [(1_000u64, 1u64), (1_000, 4), (1_000, 16), (100, 1_000)];
    let mut per_entry_at = [0.0f64; 2]; // [16k, 100k] entries for the slope
    for &(keys, versions) in fixtures.iter() {
        let d = dir(&format!("cadv-{keys}x{versions}"));
        let db = open_big(&d);
        let oids = [
            db.create_object().unwrap(),
            db.create_object().unwrap(),
            db.create_object().unwrap(),
        ];
        // Seed with winners tracked per surface. Mixed replica IDs: every
        // 8th (key + version) is an object put rotated across the three
        // oids; the rest are byte puts. Each surface's newest wins.
        let mut byte_wins: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        let mut obj_wins: BTreeMap<(usize, Vec<u8>), Vec<u8>> = BTreeMap::new();
        let mut data_bytes = 0u64;
        for key in 0..keys {
            for v in 1..=versions {
                let k = format!("k{key:05}").into_bytes();
                if (key + v) % 8 == 0 {
                    let oi = (((key + v) / 8) % 3) as usize;
                    let val = format!("o{key:05}v{v:04}").into_bytes();
                    db.put_object(oids[oi], &k, &val).unwrap();
                    data_bytes += (k.len() + val.len()) as u64;
                    obj_wins.insert((oi, k), val);
                } else {
                    let val = format!("b{key:05}v{v:04}").into_bytes();
                    db.put(&k, &val).unwrap();
                    data_bytes += (k.len() + val.len()) as u64;
                    byte_wins.insert(k, val);
                }
            }
        }
        db.flush().unwrap();
        // A second segment — compact() skips a single-segment set.
        for i in 0..10 {
            db.put(format!("f{i:02}").as_bytes(), b"filler").unwrap();
        }
        db.flush().unwrap();
        let live_before = live();
        reset_peak();
        let before = allocs();
        db.compact().unwrap();
        let delta = allocs() - before;
        let transient = peak() - live_before;
        let entries = keys * versions;
        let per_entry = delta as f64 / entries as f64;
        eprintln!(
            "RECORDED tdd018 keys={keys} versions={versions} entries={entries} \
             per_entry_allocs={per_entry:.2} transient={transient}",
            keys = keys,
            versions = versions,
            entries = entries,
            per_entry = per_entry,
            transient = transient
        );
        assert!(
            per_entry <= 2000.0,
            "{per_entry:.2} allocations per compacted entry at {entries} entries"
        );
        assert!(
            transient as u64 <= data_bytes * 2 + 8 * 1024 * 1024,
            "compact transient {transient} exceeds 2x data + 8 MB at {entries} entries"
        );
        drop(db); // the filler memtable flushes — small
        if entries == 16_000 || entries == 100_000 {
            per_entry_at[if entries == 16_000 { 0 } else { 1 }] = per_entry;
        }
        // Reopen: winners per surface, and object-only keys must NOT leak
        // onto the byte surface.
        let db = open_big(&d);
        for (k, v) in &byte_wins {
            assert_eq!(
                db.get(k).unwrap().as_deref(),
                Some(v.as_slice()),
                "byte winner survives compaction"
            );
        }
        for key in 0..keys {
            let k = format!("k{key:05}").into_bytes();
            if !byte_wins.contains_key(&k) {
                assert_eq!(
                    db.get(&k).unwrap(),
                    None,
                    "object-only key leaks onto the byte surface"
                );
            }
        }
        for ((oi, k), v) in &obj_wins {
            assert_eq!(
                db.get_object(oids[*oi], k).unwrap().as_deref(),
                Some(v.as_slice()),
                "object winner survives compaction"
            );
        }
    }
    assert!(
        per_entry_at[1] <= per_entry_at[0] * 1.5,
        "per-entry compaction allocations grew with the entry count (16k: {:.2}, 100k: {:.2})",
        per_entry_at[0],
        per_entry_at[1]
    );
}
