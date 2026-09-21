//! M29 (P0-02) — sorted-input publish pins (`publish_with_anchors_sorted`).
//!
//! The memtable's iteration order (key asc, seq asc within key) plus an
//! in-place reversal of each key's version run is EXACTLY the publish's
//! key asc + seq desc contract — the sorted writer takes that input
//! directly, no sort. These pins hold the sorted writer byte-, order- and
//! anchor-identical to the sorting writer over the same corpus, plus the
//! misuse guards the sorting writer already has.

mod common;

use aikoql_storage_v2::format::FormatError;
use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::segment::{
    SegmentEntry, SegmentReader, SegmentWriter, FLAG_DELETE, FLAG_PUT,
};
use common::dir;

fn entry(key: &str, value_len: usize, seq: u64, flags: u8, rid: u64) -> SegmentEntry {
    SegmentEntry {
        key: key.as_bytes().to_vec(),
        value: vec![b'v'; value_len],
        seq,
        flags,
        replica_id: ReplicaId(rid),
    }
}

/// Version-heavy corpus in MEMTABLE order (key asc, seq asc within key).
/// Values are 4 KiB so the corpus spans block boundaries (16 KiB target).
fn version_corpus() -> Vec<SegmentEntry> {
    vec![
        entry("alpha", 4096, 1, FLAG_PUT, 0),
        entry("alpha", 4096, 4, FLAG_PUT, 7),
        entry("alpha", 4096, 9, FLAG_DELETE, 7),
        entry("beta", 4096, 2, FLAG_PUT, 9),
        entry("gamma", 4096, 3, FLAG_PUT, 0),
        entry("gamma", 4096, 5, FLAG_PUT, 9),
    ]
}

#[test]
fn sorted_publish_is_byte_identical_to_the_sorting_publish() {
    let corpus = version_corpus();
    // The sorting writer's input: deliberately unsorted (reversed +
    // rotated — the sort must earn its keep there).
    let mut unsorted = corpus.clone();
    unsorted.reverse();
    unsorted.rotate_left(2);
    assert_ne!(unsorted, corpus, "fixture must really be unsorted");

    let pa = dir("sorted-eq-a").join("SEGMENT-001.log");
    let mut wa = SegmentWriter::new_v4(16 << 10);
    for e in unsorted {
        wa.push(e);
    }
    let (size_a, ck_a, anchors_a) = wa.publish_with_anchors(&pa).unwrap();

    // The sorted writer's input: memtable order straight in.
    let pb = dir("sorted-eq-b").join("SEGMENT-001.log");
    let mut wb = SegmentWriter::new_v4(16 << 10);
    for e in corpus {
        wb.push(e);
    }
    let (size_b, ck_b, anchors_b) = wb.publish_with_anchors_sorted(&pb).unwrap();

    assert_eq!((size_a, ck_a, anchors_a), (size_b, ck_b, anchors_b));
    assert_eq!(
        std::fs::read(&pa).unwrap(),
        std::fs::read(&pb).unwrap(),
        "sorted and sorting publishes must be byte-identical"
    );
}

#[test]
fn sorted_publish_rejects_duplicate_key_seq_pairs() {
    let d = dir("sorted-dup");
    let path = d.join("SEGMENT-001.log");
    let mut w = SegmentWriter::new_v2(16 << 10);
    w.push(entry("dup", 8, 5, FLAG_PUT, 0));
    w.push(entry("dup", 8, 5, FLAG_PUT, 7)); // same (key, seq), different rid
    let err = w.publish_with_anchors_sorted(&path).unwrap_err();
    assert!(
        matches!(err, FormatError::Invalid(_)),
        "duplicate (key, seq) must be Invalid, got {err:?}"
    );
    assert!(!path.exists(), "a rejected publish must not write a file");
}

#[test]
fn sorted_publish_decodes_to_key_asc_seq_desc_within_key() {
    let d = dir("sorted-order");
    let path = d.join("SEGMENT-001.log");
    let mut w = SegmentWriter::new_v2(16 << 10);
    for e in version_corpus() {
        w.push(e);
    }
    w.publish_with_anchors_sorted(&path).unwrap();

    let reader = SegmentReader::open(&path).unwrap();
    let got: Vec<(String, u64, u64)> = reader
        .scan(b"", b"~")
        .unwrap()
        .into_iter()
        .map(|e| {
            (
                String::from_utf8(e.key).unwrap(),
                e.seq,
                e.replica_id.0,
            )
        })
        .collect();
    let want = vec![
        ("alpha".to_string(), 9, 7), // the run reversed: seq desc
        ("alpha".to_string(), 4, 7),
        ("alpha".to_string(), 1, 0),
        ("beta".to_string(), 2, 9),
        ("gamma".to_string(), 5, 9),
        ("gamma".to_string(), 3, 0),
    ];
    assert_eq!(got, want, "key asc, seq desc within key, rids preserved");
}

#[test]
fn sorted_publish_anchors_the_max_seq_entry_per_rid() {
    // Small values: one block, five entries. Reversal yields
    // a-9, a-5, a-3, b-4, b-2 — rid 7's max seq is the FIRST entry,
    // rid 8's is entry 3.
    let corpus = vec![
        entry("a", 8, 3, FLAG_PUT, 7),
        entry("a", 8, 5, FLAG_PUT, 7),
        entry("a", 8, 9, FLAG_PUT, 7),
        entry("b", 8, 2, FLAG_PUT, 8),
        entry("b", 8, 4, FLAG_PUT, 8),
    ];
    let d = dir("sorted-anchors");
    let path = d.join("SEGMENT-001.log");
    let mut w = SegmentWriter::new_v4(16 << 10);
    for e in corpus {
        w.push(e);
    }
    let (_size, _ck, anchors) = w.publish_with_anchors_sorted(&path).unwrap();

    let a7 = anchors
        .iter()
        .find(|a| a.replica_id == ReplicaId(7))
        .expect("rid 7 anchored");
    assert_eq!((a7.seq, a7.block_id.0, a7.entry_offset), (9, 0, 0));
    let a8 = anchors
        .iter()
        .find(|a| a.replica_id == ReplicaId(8))
        .expect("rid 8 anchored");
    assert_eq!((a8.seq, a8.block_id.0, a8.entry_offset), (4, 0, 3));
}
