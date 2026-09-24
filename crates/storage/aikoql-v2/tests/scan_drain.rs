//! P5-M43 (R4-P1-04) — allocation-free scan equal-key drain.
//! The scan drains every equal-key group into a `Vec` then clones
//! every candidate's value through `max_by_key` (db.rs scan()): one
//! allocation per group + one clone per candidate on the W5 range
//! path. The deliverable resolves the newest-layer winner inline
//! while draining — winner + consumed stream indices only, never a
//! candidate clone.
//!
//! Pins:
//!   - the zero-alloc drain delta (counting allocator): scan a corpus
//!     with every key in ONE stream vs the SAME corpus with every key
//!     in EIGHT streams (rotate x8). Both scans collect each entry as
//!     (key.to_vec, value.clone) — 2 allocs — so the W8 scan decodes
//!     7N more entries = 14N more allocs. On top of that floor the old
//!     drain adds one group Vec + one candidate clone per entry beyond
//!     the first per group: W1 = N vecs + N clones, W8 = N vecs + 8N
//!     clones → old delta 21N. The inline drain adds nothing → 14N.
//!     Bound 16N with a 12N floor (the pin is vacuous if the corpus
//!     drifts).
//!   - winner parity: overwrites + tombstones across four rounds,
//!     answers by construction (newest round wins; a tombstone in the
//!     newest round suppresses; a newer put resurrects).
//!
//! Env-gated cells (the review's dedicated scan milestone):
//! AIKOQL_V2_SCAN_CELLS=1 runs 10K/100K, _FULL=1 adds 1M (CI).
//! Version-heavy corpus (4 rounds of overwrites → 4 streams). Per
//! point: full-scan wall + rows/sec, allocations, read-path
//! bytes/decodes, and per-row p50/p95/p99 from 100 chunked scans.

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use common::{dir, stats_delta};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) == 1 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) == 1 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static A: Counting = Counting;

fn open_db(tag: &str) -> Db {
    let mut cfg = Config::new(dir(tag));
    cfg.durability = DurabilityMode::Async; // fsync per batch would dominate
    cfg.memtable_bytes = usize::MAX; // no auto-flush: explicit rotates
    Db::open(cfg).unwrap()
}

/// One round of puts over the same key range — non-empty values so a
/// clone allocates (empty values would make the drain's clones free,
/// and the delta pin non-discriminating).
fn write_round(db: &Db, n: usize, round: usize) {
    for i in 0..n {
        let key = format!("k{i:08}");
        let val = format!("v{i:08}-r{round}");
        db.put(key.as_bytes(), val.as_bytes()).unwrap();
    }
}

#[test]
fn scan_drain_allocs_scale_with_entries_not_groups() {
    const N: usize = 1000;

    // W1: every key once, one (active) stream.
    let db_w1 = open_db("m43-w1");
    write_round(&db_w1, N, 0);
    let w1 = {
        ARMED.store(1, Ordering::Relaxed);
        ALLOCS.store(0, Ordering::Relaxed);
        let rows = db_w1.scan(b"k").unwrap();
        ARMED.store(0, Ordering::Relaxed);
        assert_eq!(rows.len(), N);
        ALLOCS.load(Ordering::Relaxed) as u64
    };

    // W8: the same N keys, eight rounds with a rotate between —
    // 8 streams, 8 entries per key, still N answers.
    let db_w8 = open_db("m43-w8");
    for r in 0..8 {
        write_round(&db_w8, N, r);
        db_w8.rotate();
    }
    let w8 = {
        ARMED.store(1, Ordering::Relaxed);
        ALLOCS.store(0, Ordering::Relaxed);
        let rows = db_w8.scan(b"k").unwrap();
        ARMED.store(0, Ordering::Relaxed);
        assert_eq!(rows.len(), N);
        ALLOCS.load(Ordering::Relaxed) as u64
    };

    let delta = w8.saturating_sub(w1);
    eprintln!(
        "[m43 drain] W1 {w1} allocs, W8 {w8} allocs — delta {delta} \
         (decode floor 14N = {}, bound 16N = {})",
        14 * N,
        16 * N
    );
    assert!(
        delta >= 12 * N as u64,
        "scan drain: delta {delta} below the 14N decode floor (W1 {w1}, W8 {w8}) — \
         the corpus drifted and the pin is vacuous"
    );
    assert!(
        delta <= 16 * N as u64,
        "scan drain: W8 {w8} vs W1 {w1} — delta {delta} over the 14N decode floor; \
         the per-group Vec + candidate clones are still allocating (bound {})",
        16 * N
    );
}

#[test]
fn scan_drain_winner_parity() {
    let db = open_db("m43-parity");
    // Round 0: 0..60 = r0
    for i in 0..60 {
        db.put(format!("k{i:08}").as_bytes(), b"r0").unwrap();
    }
    db.rotate();
    // Round 1: 0..20 → r1 (overwrites); 50..55 deleted.
    for i in 0..20 {
        db.put(format!("k{i:08}").as_bytes(), b"r1").unwrap();
    }
    for i in 50..55 {
        db.delete(format!("k{i:08}").as_bytes()).unwrap();
    }
    db.rotate();
    // Round 2: 10..15 → r2 (wins over r1); 55..57 → r2 (resurrects the
    // round-1 delete); 20..25 deleted (suppresses r0).
    for i in 10..15 {
        db.put(format!("k{i:08}").as_bytes(), b"r2").unwrap();
    }
    for i in 55..57 {
        db.put(format!("k{i:08}").as_bytes(), b"r2").unwrap();
    }
    for i in 20..25 {
        db.delete(format!("k{i:08}").as_bytes()).unwrap();
    }
    db.rotate();
    // Round 3 (active, the newest stream): 40..45 → r3.
    for i in 40..45 {
        db.put(format!("k{i:08}").as_bytes(), b"r3").unwrap();
    }

    let rows = db.scan(b"k").unwrap();
    assert_eq!(
        rows.len(),
        50,
        "60 keys − 5 deleted (20..25) − 5 deleted (50..55)"
    );
    let answers: std::collections::HashMap<&str, &str> = rows
        .iter()
        .map(|(k, v)| {
            (
                std::str::from_utf8(k).unwrap(),
                std::str::from_utf8(v).unwrap(),
            )
        })
        .collect();
    let expect = |i: usize, v: &str| {
        let k = format!("k{i:08}");
        assert_eq!(
            answers.get(k.as_str()),
            Some(&v),
            "key {k}: the newest round's row wins"
        );
    };
    let absent = |i: usize| {
        let k = format!("k{i:08}");
        assert!(
            !answers.contains_key(k.as_str()),
            "key {k}: the newest round's tombstone suppresses"
        );
    };
    for i in 0..10 {
        expect(i, "r1");
    }
    for i in 10..15 {
        expect(i, "r2");
    }
    for i in 15..20 {
        expect(i, "r1");
    }
    for i in 20..25 {
        absent(i);
    }
    for i in 25..40 {
        expect(i, "r0");
    }
    for i in 40..45 {
        expect(i, "r3");
    }
    for i in 45..50 {
        expect(i, "r0");
    }
    for i in 50..55 {
        absent(i);
    }
    for i in 55..57 {
        expect(i, "r2");
    }
    for i in 57..60 {
        expect(i, "r0");
    }
}

/// The review's dedicated scan milestone cells. 10K/100K laptop,
/// +1M CI. Per point: full-scan wall + rows/sec, allocations,
/// read-path bytes/decodes, and per-row p50/p95/p99 ns from 100
/// chunked prefix scans (per-chunk wall / chunk rows — includes the
/// stream build, so it is a per-row proxy, not a pure row cost).
#[test]
fn m43_scan_cells() {
    if std::env::var_os("AIKOQL_V2_SCAN_CELLS").is_none() {
        return;
    }
    let full = std::env::var_os("AIKOQL_V2_SCAN_CELLS_FULL").is_some();
    let mut points: Vec<(String, usize)> = vec![("n10k".into(), 10_000), ("n100k".into(), 100_000)];
    if full {
        points.push(("n1m".into(), 1_000_000));
    }

    let mut cells = String::from("{");
    for (tag, n) in points {
        let db = open_db(&format!("m43-cells-{tag}"));
        for r in 0..4 {
            write_round(&db, n, r);
            if r < 3 {
                db.rotate();
            }
        }
        // 4 streams: the 3 rotated rounds publish as segments (the real
        // range path — bytes/decodes only count segment reads), the
        // round-3 active stays in the memtable.
        db.flush().unwrap();
        let before = db.read_path_stats();
        ARMED.store(1, Ordering::Relaxed);
        ALLOCS.store(0, Ordering::Relaxed);
        let t = Instant::now();
        let rows = db.scan(b"k").unwrap();
        let wall_ns = t.elapsed().as_nanos();
        ARMED.store(0, Ordering::Relaxed);
        let delta = stats_delta(db.read_path_stats(), before);
        assert_eq!(rows.len(), n, "{tag}: one row per key");
        let wall_ms = (wall_ns / 1_000_000) as u64;
        let rows_per_sec = n as u64 * 1_000_000_000 / wall_ns.max(1) as u64;

        // 100 chunked scans → per-row ns samples. chunk = 10^d, so the
        // block [c·chunk, (c+1)·chunk) is exactly the keys sharing the
        // first 8−d digits of their 8-digit form.
        let chunk = n / 100;
        let width = 8 - chunk.ilog10() as usize;
        let mut per_row: Vec<u64> = Vec::with_capacity(100);
        for c in 0..100 {
            let full = format!("k{:08}", c * chunk);
            let prefix = full[..1 + width].to_string();
            let t = Instant::now();
            let got = db.scan(prefix.as_bytes()).unwrap();
            let ns = t.elapsed().as_nanos() as u64;
            assert_eq!(got.len(), chunk, "{tag} chunk {c}: prefix row count");
            per_row.push(ns / chunk as u64);
        }
        per_row.sort_unstable();
        let pct = |p: usize| per_row[per_row.len() * p / 100];

        cells.push_str(&format!(
            "\"{tag}_wall_ms\":{wall_ms},\"{tag}_rows_per_sec\":{rows_per_sec},\
             \"{tag}_allocs\":{},\"{tag}_bytes\":{},\"{tag}_decodes\":{},\
             \"{tag}_p50_ns\":{},\"{tag}_p95_ns\":{},\"{tag}_p99_ns\":{},",
            ALLOCS.load(Ordering::Relaxed) as u64,
            delta.bytes_read,
            delta.entries_decoded,
            pct(50),
            pct(95),
            pct(99),
        ));
    }
    cells.push('}');
    let cells_path = dir("m43-cells").join("cells.json");
    std::fs::write(&cells_path, &cells).unwrap();
    eprintln!("[m43 cells] {cells}");
}
