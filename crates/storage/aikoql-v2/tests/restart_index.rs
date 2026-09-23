//! P5-M44 (R4-P1-05) — restart-index preparse.
//! `block_get_v2` rebuilds the restart-key Vec and revalidates every
//! offset on EVERY point lookup (segment.rs): one heap alloc + an
//! O(restarts) key-parse walk per lookup — immutable metadata
//! reparsed per read. The deliverable parses each block's restart
//! table once (a OnceLock per block) into offsets + compact owned
//! keys; lookups partition-point over the parsed keys and decode from
//! the block as today. The representation is selected on the memory
//! tradeoff the review demanded: measure the metadata footprint
//! FIRST, no blind per-segment bloat.
//!
//! Pins:
//!   - the reparse pin: two point lookups on the same block (no
//!     block cache — SegmentReader::open, so every get re-reads the
//!     block; the re-read noise is identical in both arms). The
//!     second warm lookup must allocate STRICTLY LESS than the first:
//!     the first touch parses the table once, the re-reads rebuild
//!     nothing. The old code rebuilds the keys Vec on every lookup —
//!     equal allocs, the pin fails. A floor pins the steady state
//!     (block re-read 2 + scratch extend 1 + key clone 1 = 4; the old
//!     steady state adds the keys Vec = 5).
//!   - parity: point reads answer by construction across block
//!     boundaries — puts with empty values, tombstones, absent keys
//!     (empty values: a value copy would allocate and mask the pin).
//!   - fail-closed: a corrupted restart offset still fails — the
//!     validation moves to parse time, but a damaged table must
//!     never decode silently (the block checksum is re-stamped after
//!     the patch so the corruption reaches the restart validation,
//!     not the checksum).
//!
//! Cells (AIKOQL_V2_RIDX_CELLS=1): the metadata footprint — restart
//! table bytes + restart key bytes + restart count vs the segment's
//! file bytes — plus open wall and warm per-lookup allocs + wall
//! percentiles. The resident preparse cost is table + key bytes +
//! ~28 B per restart (offsets + box overhead); the cells gate the
//! representation decision.

mod common;

use aikoql_storage_v2::db::{manifest_path, segment_path, Config, Db, DurabilityMode};
use aikoql_storage_v2::format::{checksum8, Current, Manifest};
use aikoql_storage_v2::segment::SegmentReader;
use common::dir;
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
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

fn open_db(d: &Path) -> Db {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.durability = DurabilityMode::Async;
    cfg.memtable_bytes = usize::MAX; // no auto-flush: explicit flush
    Db::open(cfg).unwrap()
}

/// The L0 segment's on-disk path after a flush (the bgc003b pattern).
fn l0_segment_path(d: &Path) -> std::path::PathBuf {
    let current = Current::read(&d.join("CURRENT")).unwrap();
    let manifest = Manifest::read(&manifest_path(d, current.manifest_generation)).unwrap();
    let l0 = manifest
        .segments
        .iter()
        .find(|r| r.level == 0)
        .expect("one L0 segment");
    segment_path(d, l0.segment_id)
}

#[test]
fn restart_index_reparse_pin() {
    const N: usize = 2000;
    let d = dir("m44-pin");
    let db = open_db(&d);
    for i in 0..N {
        let key = format!("k{i:08}");
        db.put(key.as_bytes(), b"").unwrap();
    }
    db.flush().unwrap();
    let path = l0_segment_path(&d);
    drop(db);

    // No block cache: every get re-reads the block (identical noise in
    // both arms). keys[16] sits at a restart position — its decode
    // window holds exactly one entry.
    let reader = SegmentReader::open(&path).unwrap();
    let target = format!("k{:08}", 16);
    let measure = || {
        ARMED.store(1, Ordering::Relaxed);
        ALLOCS.store(0, Ordering::Relaxed);
        let out = reader.get(target.as_bytes()).unwrap();
        ARMED.store(0, Ordering::Relaxed);
        assert!(out.is_some(), "the corpus key resolves");
        ALLOCS.load(Ordering::Relaxed) as u64
    };
    let a1 = measure();
    let a2 = measure();
    assert!(
        a2 < a1,
        "restart index: warm re-lookup {a2} allocs >= first {a1} — the restart \
         keys Vec rebuilds on every point lookup (the first touch must parse \
         once, the re-reads must rebuild nothing)"
    );
    assert!(
        a2 <= 4,
        "restart index: warm steady state {a2} allocs — block re-read (2) + \
         scratch extend (1) + key clone (1) = 4; anything more is per-lookup \
         table work"
    );
}

#[test]
fn restart_index_parity() {
    const N: usize = 600;
    let d = dir("m44-parity");
    let db = open_db(&d);
    for i in 0..N {
        let key = format!("k{i:08}");
        if i % 7 == 6 {
            db.delete(key.as_bytes()).unwrap();
        } else {
            db.put(key.as_bytes(), b"").unwrap();
        }
    }
    db.flush().unwrap();
    for i in 0..N {
        let key = format!("k{i:08}");
        let got = db.get(key.as_bytes()).unwrap();
        if i % 7 == 6 {
            assert_eq!(got, None, "key {key}: the tombstone suppresses");
        } else {
            assert_eq!(got.as_deref(), Some(&b""[..]), "key {key}: the put answers");
        }
    }
    assert_eq!(
        db.get(b"k0999999").unwrap(),
        None,
        "absent key answers None"
    );
    assert_eq!(db.get(b"").unwrap(), None, "empty key answers None");
}

#[test]
fn restart_index_fail_closed() {
    const N: usize = 17; // k0 + k16 = two restart points in one block
    let d = dir("m44-corrupt");
    let db = open_db(&d);
    for i in 0..N {
        let key = format!("k{i:08}");
        db.put(key.as_bytes(), b"").unwrap();
    }
    db.flush().unwrap();
    let path = l0_segment_path(&d);
    drop(db);

    let mut raw = std::fs::read(&path).unwrap();
    let (payload, data_headers_end, index_header, bloom_end, footer_start) = block_chain(&raw);
    let payload_len = u32::from_le_bytes(
        raw[payload - 28 + 12..payload - 28 + 16]
            .try_into()
            .expect("u32 slice"),
    ) as usize;
    // Corrupt the first restart offset slot (interval u16 + count u32 at
    // the payload head), then re-stamp the block checksum AND the footer
    // skeleton checksum — the block checksum field lives inside the data
    // block header, which the skeleton covers, so both must pass for the
    // damage to reach the restart validation.
    raw[payload + 6..payload + 10].copy_from_slice(&((payload_len + 1000) as u32).to_le_bytes());
    let mut hashed = Vec::with_capacity(20 + payload_len);
    hashed.extend_from_slice(&raw[payload - 28..payload - 8]);
    hashed.extend_from_slice(&raw[payload..payload + payload_len]);
    raw[payload - 28 + 20..payload - 28 + 28].copy_from_slice(&checksum8(&hashed));
    let mut skeleton = Vec::with_capacity(data_headers_end + (bloom_end - index_header) + 14);
    skeleton.extend_from_slice(&raw[..data_headers_end]);
    skeleton.extend_from_slice(&raw[index_header..bloom_end]);
    skeleton.extend_from_slice(&raw[footer_start..footer_start + 14]);
    raw[footer_start + 14..footer_start + 22].copy_from_slice(&checksum8(&skeleton));
    std::fs::write(&path, &raw).unwrap();

    let reader = SegmentReader::open(&path).expect("the directory walk passes");
    let err = reader
        .get(format!("k{:08}", 0).as_bytes())
        .expect_err("a corrupted restart table fails closed, never decodes silently");
    assert!(
        format!("{err:?}").contains("restart offset"),
        "the restart-table validation names the damage, got {err:?}"
    );
}

/// Walks the segment's block chain — segment header (magic 4 | version 2
/// | count 4 | entries 8 | key_min_len 4 | key_min | key_max_len 4 |
/// key_max | seq_lo 8 | seq_hi 8 | checksum 8), then 28-byte block
/// headers until kind 0 (BLOCK_DATA), kind 1 (BLOCK_INDEX), kind 2
/// (BLOCK_BLOOM, last — the footer follows). Returns (first data payload
/// offset, end of the data-block headers, index header, bloom end,
/// footer start) — the skeleton spans the first, second and third.
fn block_chain(raw: &[u8]) -> (usize, usize, usize, usize, usize) {
    let key_min_len = u32::from_le_bytes(raw[18..22].try_into().expect("u32 slice")) as usize;
    let mut cur = 22 + key_min_len;
    let key_max_len = u32::from_le_bytes(raw[cur..cur + 4].try_into().expect("u32 slice")) as usize;
    cur += 4 + key_max_len + 24;
    let mut first_data_payload = 0;
    let mut n_data = 0usize;
    let mut index_header = 0;
    let mut bloom_end = 0;
    loop {
        assert_eq!(&raw[cur..cur + 4], b"AKBL", "block magic at {cur}");
        let kind = raw[cur + 6];
        let payload_len =
            u32::from_le_bytes(raw[cur + 12..cur + 16].try_into().expect("u32 slice")) as usize;
        match kind {
            0 => {
                if n_data == 0 {
                    first_data_payload = cur + 28;
                }
                n_data += 1;
            }
            1 => index_header = cur,
            2 => bloom_end = cur + 28 + payload_len,
            _ => unreachable!("block kind {kind}"),
        }
        cur += 28 + payload_len;
        if kind == 2 {
            return (
                first_data_payload,
                first_data_payload - 28 + n_data * 28,
                index_header,
                bloom_end,
                cur,
            );
        }
    }
}

/// The review's measurement-first cells: the metadata footprint vs the
/// segment's file bytes, open wall, and warm per-lookup allocs + wall
/// percentiles. The resident preparse cost = table + key bytes +
/// ~28 B/restart — the cells gate the representation decision.
#[test]
fn m44_restart_index_cells() {
    if std::env::var_os("AIKOQL_V2_RIDX_CELLS").is_none() {
        return;
    }
    const N: usize = 50_000;
    let d = dir("m44-cells");
    let t_open = Instant::now();
    let db = open_db(&d);
    let open_ms = t_open.elapsed().as_millis() as u64;
    for i in 0..N {
        let key = format!("k{i:08}");
        db.put(key.as_bytes(), b"").unwrap();
    }
    db.flush().unwrap();
    let path = l0_segment_path(&d);
    let file_bytes = std::fs::metadata(&path).unwrap().len();
    let (table_bytes, key_bytes, restarts) = db.debug_restart_metadata().unwrap();
    drop(db);

    let reader = SegmentReader::open(&path).unwrap();
    let target = format!("k{:08}", 16);
    // One warm-up lookup absorbs the one-time parse; the cell measures
    // the steady state the pin asserts (the pin's a2).
    reader.get(target.as_bytes()).unwrap();
    let measure = || {
        ARMED.store(1, Ordering::Relaxed);
        ALLOCS.store(0, Ordering::Relaxed);
        let out = reader.get(target.as_bytes()).unwrap();
        ARMED.store(0, Ordering::Relaxed);
        assert!(out.is_some());
        ALLOCS.load(Ordering::Relaxed) as u64
    };
    let warm_allocs = measure();

    // 1000 warm point reads over the corpus → per-lookup wall cells.
    let mut walls: Vec<u64> = Vec::with_capacity(1000);
    for i in 0..1000 {
        let key = format!("k{:08}", (i * 31) % N);
        let t = Instant::now();
        let out = reader.get(key.as_bytes()).unwrap();
        let ns = t.elapsed().as_nanos() as u64;
        assert!(out.is_some(), "corpus key {i} resolves");
        walls.push(ns);
    }
    walls.sort_unstable();
    let pct = |p: usize| walls[walls.len() * p / 100];

    let cells = format!(
        "{{\"file_bytes\":{file_bytes},\"table_bytes\":{table_bytes},\"key_bytes\":{key_bytes},\
         \"restarts\":{restarts},\"open_ms\":{open_ms},\"warm_lookup_allocs\":{warm_allocs},\
         \"lookup_p50_ns\":{},\"lookup_p95_ns\":{},\"lookup_p99_ns\":{}}}",
        pct(50),
        pct(95),
        pct(99),
    );
    let cells_path = d.join("cells.json");
    std::fs::write(&cells_path, &cells).unwrap();
    eprintln!("[m44 cells] {cells}");
}
