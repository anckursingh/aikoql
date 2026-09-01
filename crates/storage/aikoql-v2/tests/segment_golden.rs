//! SE2-M1 — segment golden bytes + round-trip (TESTING-PLAN-V2 row V2-M1).
//!
//! Byte layout (design §11, pinned by the fixture): header `AKSE` (version,
//! block count, entry count, key range, seq range, sha256-8) → data blocks →
//! index block → bloom block → footer `AKFT` (skeleton sha256-8). Each
//! block: 20-byte header `AKBL` (version, type, compression, entry count,
//! sizes, sha256-8 over header+payload) + payload. Entries are
//! prefix-compressed and sorted (key asc, seq desc) — head = first version.
//!
//! The fixture below was computed independently in python (hashlib +
//! struct) before any Rust existed; a format change is a visible diff.

mod common;

use aikoql_storage_v2::format::FormatError;
use aikoql_storage_v2::segment::{
    SegmentReader, SegmentWriter, FLAG_DELETE, FLAG_PUT, FLAG_VERSION,
};
use common::{entry, hex, tmp};

#[test]
fn segment_golden_bytes() {
    // Writer input deliberately unsorted — publish must sort by
    // (key asc, seq desc) so the output is deterministic.
    let mut w = SegmentWriter::new(4096);
    w.push(entry("a3", "v3", 9, FLAG_DELETE));
    w.push(entry("a1", "v1", 5, FLAG_PUT));
    w.push(entry("a2", "v2", 7, FLAG_VERSION));
    let path = tmp("segment-golden");
    w.publish(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        hex(&bytes),
        concat!(
            "414b53450100010000000300000000000000020000006131020000006133050000",
            "00000000000900000000000000c4db5c102ef23785414b424c0100000003000000",
            "3d0000003d000000e028ebd981084a840000020061310200000076310500000000",
            "000000010100010032020000007632070000000000000004010001003302000000",
            "7633090000000000000002414b424c01000100010000001400000014000000742f",
            "cc49dbdc87320200613102006133360000000000000003000000414b424c010002",
            "00030000000800000008000000f6d1b0a32f1ff4ae1e0000000ca50139414b4654",
            "0100030000000000000004503fe3eec20615",
        ),
        "segment golden bytes changed — format break"
    );
}

#[test]
fn segment_round_trip() {
    let mut w = SegmentWriter::new(4096);
    w.push(entry("a3", "v3", 9, FLAG_DELETE));
    w.push(entry("a1", "v1", 5, FLAG_PUT));
    w.push(entry("a2", "v2", 7, FLAG_VERSION));
    let path = tmp("segment-roundtrip");
    w.publish(&path).unwrap();

    let r = SegmentReader::open(&path).unwrap();
    assert_eq!(r.entry_count(), 3);
    assert_eq!(r.block_count(), 1);
    assert_eq!(r.key_min(), b"a1");
    assert_eq!(r.key_max(), b"a3");
    assert_eq!(r.seq_lo(), 5);
    assert_eq!(r.seq_hi(), 9);

    assert_eq!(r.get(b"a1").unwrap(), Some(entry("a1", "v1", 5, FLAG_PUT)));
    assert_eq!(
        r.get(b"a2").unwrap(),
        Some(entry("a2", "v2", 7, FLAG_VERSION))
    );
    assert_eq!(
        r.get(b"a3").unwrap(),
        Some(entry("a3", "v3", 9, FLAG_DELETE))
    );
    assert_eq!(r.get(b"zz").unwrap(), None);
    assert_eq!(
        r.versions(b"a1").unwrap(),
        vec![entry("a1", "v1", 5, FLAG_PUT)]
    );
    assert!(r.bloom_may_contain(b"a1"));
}

#[test]
fn segment_reader_never_mutates() {
    let mut w = SegmentWriter::new(4096);
    w.push(entry("a1", "v1", 5, FLAG_PUT));
    w.push(entry("a2", "v2", 7, FLAG_VERSION));
    let path = tmp("segment-immutable");
    w.publish(&path).unwrap();

    let before = std::fs::read(&path).unwrap();
    let r = SegmentReader::open(&path).unwrap();
    let _ = r.get(b"a1");
    let _ = r.get(b"a2");
    let _ = r.versions(b"a1");
    let _ = r.scan(b"a", b"b");
    let _ = r.bloom_may_contain(b"a1");
    drop(r);
    let after = std::fs::read(&path).unwrap();
    assert_eq!(before, after, "reads must never mutate a published segment");
}

#[test]
fn segment_publish_validation() {
    // Caller misuse is rejected at publish, not written to disk.
    let empty = SegmentWriter::new(4096);
    assert!(matches!(
        empty.publish(&tmp("segment-empty")),
        Err(FormatError::Invalid(_))
    ));

    let mut dup = SegmentWriter::new(4096);
    dup.push(entry("a1", "v1", 5, FLAG_PUT));
    dup.push(entry("a1", "v2", 5, FLAG_PUT)); // same (key, seq)
    assert!(matches!(
        dup.publish(&tmp("segment-dup")),
        Err(FormatError::Invalid(_))
    ));

    let mut zero = SegmentWriter::new(0);
    zero.push(entry("a1", "v1", 5, FLAG_PUT));
    assert!(matches!(
        zero.publish(&tmp("segment-zero")),
        Err(FormatError::Invalid(_))
    ));
}
