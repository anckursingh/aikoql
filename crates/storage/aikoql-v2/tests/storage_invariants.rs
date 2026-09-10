//! P4-M3 — storage invariant validator (TDD-STOR-005): the manifest is
//! cross-checked at open — generation > 0, unique segment ids, key range
//! and sequence order sane, level supported, record_count > 0, every named
//! segment file present at the recorded size — and each record must agree
//! with the segment reader's own header metadata. Impossible metadata
//! fails closed before any reader opens — Corrupt for metadata disagreement,
//! Io for a missing file (the SE2-M1 taxonomy). The RED list:
//! iv001 flipped key range → open refuses; iv002 missing segment file →
//! refuses; iv003 size mismatch → refuses; iv004 duplicate segment id →
//! refuses; iv005 valid manifest passes.

mod common;

use aikoql_storage_v2::db::{manifest_path, Config, Db};
use aikoql_storage_v2::format::{validate_manifest, Current, FormatError, Manifest};
use aikoql_storage_v2::segment::segment_path;
use common::dir;
use std::path::Path;

fn oid(byte: u8) -> aikoql_storage_v2::identity::ObjectId {
    aikoql_storage_v2::identity::ObjectId([byte; 16])
}

/// Seed a Db with flushed data, then hand back its manifest + generation
/// so a test can mutate and re-publish the SAME generation.
fn seeded(d: &Path) -> (Manifest, u64) {
    let db = Db::open(Config::new(d.to_path_buf())).unwrap();
    db.put_object(oid(0x11), b"k1", b"v1").unwrap();
    db.put_object(oid(0x22), b"k2", b"v2").unwrap();
    db.flush().unwrap();
    drop(db);
    let current = Current::read(&d.join("CURRENT")).unwrap();
    let manifest = Manifest::read(&manifest_path(d, current.manifest_generation)).unwrap();
    (manifest, current.manifest_generation)
}

fn republish(d: &Path, gen: u64, manifest: &Manifest) {
    Manifest::publish(&manifest_path(d, gen), manifest).unwrap();
}

#[test]
fn iv001_flipped_key_range_refuses_open() {
    let d = dir("iv001");
    let (mut m, gen) = seeded(&d);
    let (kmin, kmax) = (m.segments[0].key_min.clone(), m.segments[0].key_max.clone());
    m.segments[0].key_min = kmax;
    m.segments[0].key_max = kmin;
    republish(&d, gen, &m);
    let err = Db::open(Config::new(d)).err().expect("open must refuse");
    assert!(
        matches!(err, FormatError::Corrupt(_)),
        "flipped key range must refuse open, got {err:?}"
    );

    // Reader-agreement leg: a manifest claiming MORE than the segment's own
    // header (static range still ordered) is caught by the open-loop
    // cross-check, not the static pass.
    let d = dir("iv001b");
    let (mut m, gen) = seeded(&d);
    m.segments[0].key_max = vec![0xFF, 0xFF, 0xFF];
    republish(&d, gen, &m);
    let err = Db::open(Config::new(d)).err().expect("open must refuse");
    assert!(
        matches!(err, FormatError::Corrupt(_)),
        "manifest-record/segment-header disagreement must refuse open, got {err:?}"
    );
}

#[test]
fn iv002_missing_segment_file_refuses_open() {
    // Refusal itself was pre-covered (db_recovery's `missing_segment_fails_closed`
    // pins Io — a missing file is a filesystem condition, not metadata damage);
    // P4-M3 routes the refusal through the explicit validator.
    let d = dir("iv002");
    let (m, gen) = seeded(&d);
    std::fs::remove_file(segment_path(&d, m.segments[0].segment_id)).unwrap();
    republish(&d, gen, &m);
    let err = Db::open(Config::new(d)).err().expect("open must refuse");
    assert!(
        matches!(err, FormatError::Io(_)),
        "manifest naming a missing segment file must refuse open as Io, got {err:?}"
    );
}

#[test]
fn iv003_size_mismatch_refuses_open() {
    let d = dir("iv003");
    let (mut m, gen) = seeded(&d);
    m.segments[0].file_size += 1000;
    republish(&d, gen, &m);
    let err = Db::open(Config::new(d)).err().expect("open must refuse");
    assert!(
        matches!(err, FormatError::Corrupt(_)),
        "recorded size differing from the file must refuse open, got {err:?}"
    );
}

#[test]
fn iv004_duplicate_segment_id_refuses_open() {
    let d = dir("iv004");
    let (mut m, gen) = seeded(&d);
    let dup = m.segments[0].clone();
    m.segments.push(dup);
    republish(&d, gen, &m);
    let err = Db::open(Config::new(d)).err().expect("open must refuse");
    assert!(
        matches!(err, FormatError::Corrupt(_)),
        "duplicate segment id must refuse open, got {err:?}"
    );
}

#[test]
fn iv005_valid_manifest_passes() {
    let d = dir("iv005");
    let (m, gen) = seeded(&d);
    // The validator accepts the seeded manifest directly…
    assert!(matches!(validate_manifest(&m, &d), Ok(())));
    // …and open still succeeds end to end.
    let db = Db::open(Config::new(d)).unwrap();
    assert_eq!(
        db.get_object(oid(0x11), b"k1").unwrap(),
        Some(b"v1".to_vec())
    );
    let _ = gen;
}
