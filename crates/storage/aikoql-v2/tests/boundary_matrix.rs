//! L-12 (TDD-027/028) — the restart/block-boundary matrix and the length
//! boundary matrix.
//!
//! TDD-027's fixtures run through every read path (point get, object get,
//! scan, prefix scan, placement-direct read): entries 15/16 and 16/17
//! (the interval boundary around RESTART_INTERVAL = 16), a key whose head
//! lands exactly on a restart, a version run crossing a restart (the
//! restart defers past the run and repositions to the run head — SE2-M38),
//! a v4 dense cadence point inside a run (an identity row's anchor decodes
//! standalone), and a run whose versions split across data blocks.
//!
//! TDD-028's length fixtures: 1-byte, 4 KiB and 65535-byte keys round-trip;
//! one byte past must fail deterministically (Invalid) — the format's key
//! fields are u16 and a silent `as u16` truncation at publish is exactly
//! the corruption this row exists to catch. The value dimension's
//! serialize ceiling (>u32::MAX) is pinned by wal_boundary.rs (L-08);
//! here 1-byte and 64 KiB values round-trip.
//!
//! The byte/object surface split is the scan rows' invariant (stor006):
//! a flushed object row must never answer a byte scan, and the byte head
//! is the newest BYTE row even when an object row of the key is newer.

mod common;

use aikoql_storage_v2::db::{segment_path, Config, Db};
use aikoql_storage_v2::format::FormatError;
use aikoql_storage_v2::identity::directory::{IdentityResolver, LocalIdentityDirectory};
use aikoql_storage_v2::identity::topology::{LocalReplicaDirectory, ReplicaDirectory};
use aikoql_storage_v2::identity::{ObjectId, ReplicaId};
use aikoql_storage_v2::placement::directory::LocalPlacementResolver;
use aikoql_storage_v2::placement::{Placement, PlacementResolver};
use aikoql_storage_v2::segment::{SegmentEntry, SegmentReader, SegmentWriter, FLAG_PUT};
use common::dir;
use std::path::Path;

fn oid(byte: u8) -> ObjectId {
    ObjectId([byte; 16])
}

fn rid_of(db: &Db, a: ObjectId) -> ReplicaId {
    let lid = LocalIdentityDirectory::new(db).resolve(a).unwrap().unwrap();
    LocalReplicaDirectory::new(db)
        .resolve_local(lid)
        .unwrap()
        .unwrap()
}

fn placement_of(db: &Db, rid: ReplicaId) -> Option<Placement> {
    LocalPlacementResolver::new(db).resolve(rid).unwrap()
}

/// L-12 — the placement-direct read of the L-10 equivalence suite: the
/// stored anchor decodes to the row itself (v4 dense base).
fn direct_read(dir: &Path, loc: &Placement) -> SegmentEntry {
    let Placement::Segment(loc) = loc else {
        panic!("expected a Segment placement, got {loc:?}");
    };
    SegmentReader::open(&segment_path(dir, loc.segment_id.0))
        .unwrap()
        .entry_at(loc.block_id, loc.entry_offset)
        .unwrap()
        .expect("the anchor names an entry")
}

/// The data-block format versions of the segment holding `loc`, read from
/// the raw file: an `AKBL | version u16 | type u8` header at each block
/// boundary. The fixtures' payloads are ASCII and cannot contain the magic,
/// so the plain scan is exact (the reader exposes no block-version view).
fn data_block_versions(dir: &Path, loc: &Placement) -> Vec<u16> {
    let Placement::Segment(loc) = loc else {
        panic!("expected a Segment placement, got {loc:?}");
    };
    let raw = std::fs::read(segment_path(dir, loc.segment_id.0)).unwrap();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = raw[i..].windows(4).position(|w| w == b"AKBL") {
        let p = i + off;
        if p + 6 < raw.len() && raw[p + 6] == 0 {
            out.push(u16::from_le_bytes([raw[p + 4], raw[p + 5]]));
        }
        i = p + 4;
    }
    out
}

fn key(i: u32) -> Vec<u8> {
    format!("k/{i:02}").into_bytes()
}

fn val(i: u32) -> Vec<u8> {
    format!("v/{i:02}").into_bytes()
}

/// TDD-027 — entries 15/16 (the last of an interval and its restart) and
/// 16/17 (a restart entry and the first of the next) answer every read
/// path, before and after a flush, and after a restart (reopen). The keys
/// are strictly increasing, so every 16th entry is a restart: restarts
/// land at k00/k16/k32.
#[test]
fn restart_boundary_entries_answer_every_path() {
    let d = dir("l12-restart-windows");
    let db = Db::open(Config::new(d.clone())).unwrap();
    for i in 0..40u32 {
        db.put(&key(i), &val(i)).unwrap();
    }
    // memtable-stage reads at the boundary positions
    assert_eq!(db.get(&key(15)).unwrap(), Some(val(15)));
    assert_eq!(db.get(&key(16)).unwrap(), Some(val(16)));
    assert_eq!(db.get(&key(17)).unwrap(), Some(val(17)));
    // an object read on a never-created oid is no answer, not an error
    assert_eq!(db.get_object(oid(0x00), &key(16)).unwrap(), None);
    db.flush().unwrap();
    // segment-stage reads — the restart table decodes every interval
    for i in 0..40u32 {
        assert_eq!(db.get(&key(i)).unwrap(), Some(val(i)), "key {i}");
    }
    // scan: all 40 in key order
    let rows = db.scan(b"k/").unwrap();
    assert_eq!(rows.len(), 40);
    for (i, (k, v)) in rows.iter().enumerate() {
        assert_eq!(
            (k.as_slice(), v.as_slice()),
            (key(i as u32).as_slice(), val(i as u32).as_slice())
        );
    }
    // prefix scan: k/1 → k/10..k/19 exactly
    let rows = db.scan(b"k/1").unwrap();
    assert_eq!(rows.len(), 10);
    for (i, (k, v)) in rows.iter().enumerate() {
        assert_eq!(
            (k.as_slice(), v.as_slice()),
            (key(10 + i as u32).as_slice(), val(10 + i as u32).as_slice())
        );
    }
    drop(db);
    // restart — the same fixture set reopens
    let db = Db::open(Config::new(d)).unwrap();
    for i in 0..40u32 {
        assert_eq!(
            db.get(&key(i)).unwrap(),
            Some(val(i)),
            "key {i} after reopen"
        );
    }
    assert_eq!(db.scan(b"k/").unwrap().len(), 40);
    assert_eq!(db.scan(b"k/1").unwrap().len(), 10);
}

/// TDD-027 — a version run crossing a restart: 13 single-version keys
/// occupy entries 0..12, k99's five-version run occupies 13..17 (the
/// cadence point at 16 lands inside the run — the restart defers past it
/// and repositions to the run head, SE2-M38), then ten more keys. Every
/// path must see exactly one k99 row: the newest.
#[test]
fn version_run_crossing_a_restart_keeps_its_head() {
    let d = dir("l12-run-crosses-restart");
    let db = Db::open(Config::new(d.clone())).unwrap();
    for i in 0..13u32 {
        db.put(&key(i), &val(i)).unwrap();
    }
    for s in 1..=5u32 {
        db.put(&key(99), &format!("kx/{s}").into_bytes()).unwrap();
    }
    for i in 14..24u32 {
        db.put(&key(i), &val(i)).unwrap();
    }
    db.flush().unwrap();
    assert_eq!(db.get(&key(99)).unwrap(), Some(b"kx/5".to_vec()));
    assert_eq!(db.get(&key(12)).unwrap(), Some(val(12)));
    assert_eq!(db.get(&key(14)).unwrap(), Some(val(14)));
    let rows = db.scan(b"k/").unwrap();
    assert_eq!(rows.len(), 24);
    let k99: Vec<_> = rows.iter().filter(|(k, _)| k == &key(99)).collect();
    assert_eq!(k99.len(), 1);
    assert_eq!(k99[0].1, b"kx/5".to_vec());
    assert_eq!(db.scan(b"k/9").unwrap().len(), 1);
    drop(db);
    let db = Db::open(Config::new(d)).unwrap();
    assert_eq!(db.get(&key(99)).unwrap(), Some(b"kx/5".to_vec()));
    assert_eq!(db.scan(b"k/").unwrap().len(), 24);
}

/// TDD-027 — the v4 dense cadence point inside a version run: an
/// identity-carrying flush writes v4 blocks whose dense table records a
/// full key every 16th entry even mid-run, so a stored anchor decodes
/// standalone. The fixture's byte/object interleave also pins stor006's
/// surface split on the segment path: the byte scan yields the newest
/// BYTE row even when an object row is newer, and an object-only key
/// never appears in a byte scan.
#[test]
fn v4_dense_cadence_inside_a_run_and_the_byte_surface_split() {
    let d = dir("l12-v4-dense-mid-run");
    let db = Db::open(Config::new(d.clone())).unwrap();
    let ox = db.create_object().unwrap();
    for s in 0..18u8 {
        db.put_object(ox, b"hot", &[s]).unwrap(); // entries 0..17 — the dense point at 16 lands mid-run
    }
    db.put(b"hot", b"byte").unwrap(); // entry 18 — the byte head
    db.put_object(ox, b"hot", &[18]).unwrap(); // entry 19 — an object row NEWER than the byte head
    let oy = db.create_object().unwrap();
    db.put_object(oy, b"cold", b"v").unwrap(); // entry 20 — after the dense point
    db.flush().unwrap();

    // the flush really wrote v4 data blocks (the dense table exists)
    let loc = placement_of(&db, rid_of(&db, ox)).unwrap();
    assert!(data_block_versions(&d, &loc).iter().any(|&v| v == 4));

    // object gets answer the newest rows
    assert_eq!(db.get_object(ox, b"hot").unwrap(), Some(vec![18]));
    assert_eq!(db.get_object(oy, b"cold").unwrap(), Some(b"v".to_vec()));
    // the byte surface: the byte head, not the newer object row
    assert_eq!(db.get(b"hot").unwrap(), Some(b"byte".to_vec()));
    // scan: one row per key — the BYTE row, never the object row
    assert_eq!(
        db.scan(b"hot").unwrap(),
        vec![(b"hot".to_vec(), b"byte".to_vec())]
    );
    // object-only keys never leak into byte scans
    assert_eq!(db.scan(b"cold").unwrap().len(), 0);

    // placement-direct reads decode via the dense base (entries 19/20 ≥ 16)
    let e = direct_read(&d, &loc);
    assert_eq!(e.key, b"hot");
    assert_eq!(e.value, vec![18]);
    let loc_oy = placement_of(&db, rid_of(&db, oy)).unwrap();
    let e = direct_read(&d, &loc_oy);
    assert_eq!(e.key, b"cold");
    assert_eq!(e.value, b"v".to_vec());

    drop(db);
    // restart — the same fixtures re-answer through every path
    let db = Db::open(Config::new(d.clone())).unwrap();
    assert_eq!(db.get_object(ox, b"hot").unwrap(), Some(vec![18]));
    assert_eq!(db.get(b"hot").unwrap(), Some(b"byte".to_vec()));
    assert_eq!(
        db.scan(b"hot").unwrap(),
        vec![(b"hot".to_vec(), b"byte".to_vec())]
    );
    assert_eq!(db.scan(b"cold").unwrap().len(), 0);
    let e = direct_read(&d, &placement_of(&db, rid_of(&db, oy)).unwrap());
    assert_eq!(e.key, b"cold");
    assert_eq!(e.value, b"v".to_vec());
}

/// TDD-027 — a key crossing a block boundary: a tiny block target splits
/// each 1 KiB version into its own block, so one key's version run spans
/// several data blocks. The index lookup must land the HEAD's block (the
/// newest version), scans must collapse the run to one row, and the
/// placement-direct anchor (in the newest block) must decode.
#[test]
fn key_crossing_a_block_boundary_answers_every_path() {
    let d = dir("l12-key-crosses-block");
    let mut cfg = Config::new(d.clone());
    cfg.block_target = 128; // every 1 KiB version is its own block
    let db = Db::open(cfg).unwrap();
    for s in 0..4u8 {
        db.put(b"kx", &vec![s; 1024]).unwrap(); // kx versions in blocks 0..3
    }
    let o = db.create_object().unwrap();
    for s in 0..4u8 {
        db.put_object(o, b"ox", &vec![s; 1024]).unwrap(); // ox versions in blocks 4..7
    }
    db.flush().unwrap();
    // the head (newest version) answers, wherever the index lands
    assert_eq!(db.get(b"kx").unwrap(), Some(vec![3u8; 1024]));
    assert_eq!(db.get_object(o, b"ox").unwrap(), Some(vec![3u8; 1024]));
    // scans collapse the run to one row — the head
    let rows = db.scan(b"kx").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, vec![3u8; 1024]);
    assert_eq!(db.scan(b"ox").unwrap().len(), 0); // object rows never leak
    assert_eq!(db.scan(b"k").unwrap().len(), 1); // prefix over the byte surface
                                                 // the placement-direct anchor of ox sits in the newest block
    let loc = placement_of(&db, rid_of(&db, o)).unwrap();
    let e = direct_read(&d, &loc);
    assert_eq!(e.key, b"ox");
    assert_eq!(e.value, vec![3u8; 1024]);
    drop(db);
    let db = Db::open(Config::new(d)).unwrap();
    assert_eq!(db.get(b"kx").unwrap(), Some(vec![3u8; 1024]));
    assert_eq!(db.get_object(o, b"ox").unwrap(), Some(vec![3u8; 1024]));
    assert_eq!(db.scan(b"kx").unwrap().len(), 1);
}

/// TDD-028 — the length boundaries: 1-byte, 4 KiB and the near-limit
/// 65535-byte key round-trip; one byte past the limit must fail
/// deterministically (Invalid) at the API trust boundary — the format's
/// key fields are u16 and the old path silently truncated at publish.
/// The value dimension round-trips 1 byte and 64 KiB; the value-side
/// serialize ceiling (>u32::MAX) is pinned by wal_boundary.rs (L-08).
#[test]
fn length_boundaries_round_trip_or_fail_deterministically() {
    let d = dir("l12-lengths");
    let db = Db::open(Config::new(d.clone())).unwrap();
    db.put(b"x", b"y").unwrap();
    assert_eq!(db.get(b"x").unwrap(), Some(b"y".to_vec()));
    let k4k = vec![b'a'; 4096];
    db.put(&k4k, b"v4k").unwrap();
    assert_eq!(db.get(&k4k).unwrap(), Some(b"v4k".to_vec()));
    let k_max = vec![b'b'; u16::MAX as usize];
    db.put(&k_max, b"vmax").unwrap();
    assert_eq!(db.get(&k_max).unwrap(), Some(b"vmax".to_vec()));
    // one past — Invalid at the API boundary, never a silent truncation
    let k_over = vec![b'c'; u16::MAX as usize + 1];
    assert!(matches!(
        db.put(&k_over, b"v"),
        Err(FormatError::Invalid(_))
    ));
    // the object surface carries the same guard
    let o = db.create_object().unwrap();
    assert!(matches!(
        db.put_object(o, &k_over, b"v"),
        Err(FormatError::Invalid(_))
    ));
    // the format boundary rejects a direct writer fed an oversized key
    // (publish is the choke point for callers that bypass Db::write)
    let mut w = SegmentWriter::new_v2(4096);
    w.push(SegmentEntry {
        key: k_over.clone(),
        value: b"v".to_vec(),
        seq: 1,
        flags: FLAG_PUT,
        replica_id: ReplicaId(0),
    });
    let stray = d.join("stray.seg");
    assert!(matches!(w.publish(&stray), Err(FormatError::Invalid(_))));
    assert!(!stray.exists(), "a rejected publish writes nothing");
    // the value dimension
    db.put(b"v1", b"z").unwrap();
    assert_eq!(db.get(b"v1").unwrap(), Some(b"z".to_vec()));
    let v64k = vec![b'd'; 64 * 1024];
    db.put(b"v64k", &v64k).unwrap();
    assert_eq!(db.get(b"v64k").unwrap(), Some(v64k.clone()));
    db.flush().unwrap();
    // the extremes survive the segment encode
    assert_eq!(db.get(&k_max).unwrap(), Some(b"vmax".to_vec()));
    assert_eq!(db.get(&k4k).unwrap(), Some(b"v4k".to_vec()));
    assert_eq!(db.get(b"v64k").unwrap(), Some(v64k.clone()));
    let rows = db.scan(b"").unwrap();
    assert!(rows.iter().any(|(k, v)| k == &k_max && v == b"vmax"));
}
