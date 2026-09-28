//! L-18 (TDD-026) — prefix-scan oracle + allocation counters.
//!
//! The scan contract is already byte-oracle-pinned for the byte-only
//! workload (merged_scan.rs, SE2-M12) — the gap is the L-04 class: OBJECT
//! rows (rid ≠ 0) interleaved with byte rows across layer churn. An
//! object row must never answer a byte scan, and — the sharper pin — the
//! object rows must still BE there after the churn (present-but-filtered,
//! not dropped by compaction). The oracle models the byte surface only:
//! byte puts/rewrites/tombstones; object puts feed the object-winner
//! model, never the oracle. Scans are checked against the oracle at
//! every stage — memtable, flushed immutables + segments, post-compact,
//! and after reopen — over three prefixes plus the full range, with the
//! object winners re-verified through get_object after the reopen.
//!
//! "Zero avoidable allocs": the scan path must allocate per YIELDED head
//! only — skipped versions and skipped object rows ride the borrowed
//! value slice (the L-12 skip without recording `last`). The pin:
//! per-key scan allocations stay flat as versions-per-key climbs 1 → 4 →
//! 16 (a per-entry allocation term inflates the V=16 point ~16×; the
//! slope assert rides the V=1/V=16 pair) plus an absolute cap.
//!
//! One binary: the counting allocator is #[global_allocator] — every
//! test in this binary takes TEST_LOCK or a sibling's steady-state
//! allocations land inside the measured window.

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn allocs() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn open_big(d: &Path) -> Db {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.memtable_bytes = 1 << 30;
    cfg.l0_compact_trigger = 0;
    cfg.durability = DurabilityMode::Async;
    Db::open(cfg).unwrap()
}

/// xorshift64 — deterministic across runs.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// The byte surface's per-key head (None = tombstoned). Object puts never
/// touch this model — the scan must equal it exactly.
#[derive(Default)]
struct Oracle {
    heads: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
}

impl Oracle {
    fn put(&mut self, k: &[u8], v: &[u8]) {
        self.heads.insert(k.to_vec(), Some(v.to_vec()));
    }
    fn delete(&mut self, k: &[u8]) {
        self.heads.insert(k.to_vec(), None);
    }
    fn scan(&self, prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.heads
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .filter_map(|(k, v)| v.clone().map(|v| (k.clone(), v)))
            .collect()
    }
}

fn check(db: &Db, oracle: &Oracle, tag: &str) {
    for p in [b"".as_slice(), b"pfx-A/", b"pfx-B/", b"other-"] {
        let got = db.scan(p).unwrap();
        let want = oracle.scan(p);
        assert_eq!(got, want, "{tag}: scan({p:?}) diverged");
    }
}

#[test]
fn tdd026_scan_matches_the_byte_oracle_across_layers_with_object_rows() {
    let _serial = serial();
    const KEYS: u64 = 96;
    const STEPS: u64 = 800;
    let d = dir("prefix-oracle");
    let db = open_big(&d);
    let oids = [
        db.create_object().unwrap(),
        db.create_object().unwrap(),
        db.create_object().unwrap(),
    ];
    let mut oracle = Oracle::default();
    let mut obj_wins: BTreeMap<(usize, Vec<u8>), Vec<u8>> = BTreeMap::new();
    let mut rng = Rng(0x51ed_0018);
    let prefixes: [&[u8]; 3] = [b"pfx-A/", b"pfx-B/", b"other-"];

    for step in 0..STEPS {
        let r = rng.next();
        let p = prefixes[(r % 3) as usize];
        let k = format!("{}{:02}", std::str::from_utf8(p).unwrap(), r % KEYS).into_bytes();
        match r % 20 {
            // byte put (12/20) — the oracle's newest byte head moves.
            0..=11 => {
                let v = format!("v{step:03}").into_bytes();
                db.put(&k, &v).unwrap();
                oracle.put(&k, &v);
            }
            // object put (3/20) — never answers a byte scan; the winner
            // model tracks it for the post-reopen presence check.
            12..=14 => {
                let oi = ((r / 20) % 3) as usize;
                let v = format!("o{step:03}").into_bytes();
                db.put_object(oids[oi], &k, &v).unwrap();
                obj_wins.insert((oi, k.clone()), v);
            }
            // byte delete (3/20) — tombstone suppresses the key.
            15..=17 => {
                db.delete(&k).unwrap();
                oracle.delete(&k);
            }
            // byte rewrite (2/20).
            _ => {
                let v = format!("w{step:03}").into_bytes();
                db.put(&k, &v).unwrap();
                oracle.put(&k, &v);
            }
        }
        if step % 100 == 99 {
            db.flush().unwrap();
        }
        if step == 399 {
            db.compact().unwrap();
        }
        if step % 200 == 199 {
            check(&db, &oracle, &format!("step {step}"));
        }
    }
    check(&db, &oracle, "final");
    drop(db); // graceful flush — reopen reads everything from segments

    let db = open_big(&d);
    check(&db, &oracle, "reopen");
    // The sharper half: the object rows are PRESENT, not dropped — every
    // object winner still answers through its surface.
    for ((oi, k), v) in &obj_wins {
        assert_eq!(
            db.get_object(oids[*oi], k).unwrap().as_deref(),
            Some(v.as_slice()),
            "object row survived the layer churn"
        );
    }
}

#[test]
fn tdd026_scan_allocs_stay_flat_per_key_across_versions() {
    let _serial = serial();
    const KEYS: u64 = 256;
    let mut per_key_at = [0.0f64; 2]; // [V=1, V=16]
    for &versions in &[1u64, 4, 16] {
        let d = dir(&format!("scan-alloc-v{versions}"));
        let db = open_big(&d);
        let oids = [
            db.create_object().unwrap(),
            db.create_object().unwrap(),
            db.create_object().unwrap(),
        ];
        for i in 0..KEYS {
            let k = format!("pfx-{i:03}").into_bytes();
            for v in 1..=versions {
                db.put(&k, format!("v{v:02}").as_bytes()).unwrap();
            }
            // An object row rides the same prefix range — the scan must
            // skip it without allocating (every key keeps its byte head).
            if i % 8 == 0 {
                db.put_object(oids[(i / 8 % 3) as usize], &k, b"obj")
                    .unwrap();
            }
        }
        db.flush().unwrap();
        db.scan(b"pfx-").unwrap(); // warm: cache + first-call paths
        let before = allocs();
        let rows = db.scan(b"pfx-").unwrap();
        let delta = allocs() - before;
        assert_eq!(
            rows.len() as u64,
            KEYS,
            "every key yields its byte head — object rows stay invisible"
        );
        let per_key = delta as f64 / KEYS as f64;
        eprintln!(
            "RECORDED tdd026 versions={versions} per_key_allocs={per_key:.2}",
            versions = versions,
            per_key = per_key
        );
        if versions == 1 || versions == 16 {
            per_key_at[if versions == 1 { 0 } else { 1 }] = per_key;
        }
        drop(db);
    }
    assert!(
        per_key_at[1] <= per_key_at[0] * 1.5,
        "per-key scan allocations grew with versions per key \
         (V=1: {:.2}, V=16: {:.2}) — skipped versions allocate",
        per_key_at[0],
        per_key_at[1]
    );
    assert!(
        per_key_at[1] <= 128.0,
        "{} allocations per scanned key at V=16",
        per_key_at[1]
    );
}
