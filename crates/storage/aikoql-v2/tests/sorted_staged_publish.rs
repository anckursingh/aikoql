//! P5-M41 (R4-P1-02) — sorted staged compaction publish pins
//! (`publish_with_anchors_sorted_staged`).
//!
//! The compaction merge heap emits key asc + seq desc — exactly the
//! publish's contract — so the staged publish takes that order straight
//! through: the M29 sorted path plus the SE2-M36 park stage. No sort: the
//! heap did the ordering once, at merge time (the sorting staged variant's
//! full O(n log n) sort over heap-ordered entries is the R4 review's
//! finding this pin eliminates). The pins hold the staged sorted writer
//! byte-, order- and anchor-identical to the sorting writer over the same
//! corpus, plus the misuse guards unchanged.

mod common;

use aikoql_storage_v2::format::FormatError;
use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::segment::{
    SegmentEntry, SegmentWriter, FLAG_DELETE, FLAG_PUT,
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

/// Version-heavy corpus in PUBLISH order (key asc, seq desc within key) —
/// the merge heap's emission order. Values vary so the block splits land
/// mid-key-run, not at run boundaries (16 KiB target).
fn publish_order_corpus() -> Vec<SegmentEntry> {
    vec![
        entry("alpha", 4096, 9, FLAG_DELETE, 7),
        entry("alpha", 4096, 4, FLAG_PUT, 7),
        entry("alpha", 2048, 1, FLAG_PUT, 0),
        entry("beta", 4096, 2, FLAG_PUT, 9),
        entry("gamma", 3072, 5, FLAG_PUT, 9),
        entry("gamma", 4096, 3, FLAG_PUT, 0),
        entry("delta", 4096, 6, FLAG_PUT, 9),
        entry("epsilon", 4096, 8, FLAG_PUT, 9),
        entry("epsilon", 4096, 7, FLAG_PUT, 7),
    ]
}

#[test]
fn staged_sorted_publish_is_byte_identical_to_the_sorting_publish() {
    let corpus = publish_order_corpus();
    // The sorting writer's input: deliberately unsorted (reversed +
    // rotated — the sort must earn its keep there).
    let mut unsorted = corpus.clone();
    unsorted.reverse();
    unsorted.rotate_left(3);
    assert_ne!(unsorted, corpus, "fixture must really be unsorted");

    let pa = dir("sst-eq-a").join("SEGMENT-001.log");
    let mut wa = SegmentWriter::new_v4(16 << 10);
    for e in unsorted {
        wa.push(e);
    }
    let (size_a, ck_a, anchors_a) = wa.publish_with_anchors(&pa).unwrap();

    // The staged sorted writer's input: heap order straight in, plus the
    // SE2-M36 park stage — the compaction path's exact call shape.
    let pb = dir("sst-eq-b").join("SEGMENT-001.log");
    let mut wb = SegmentWriter::new_v4(16 << 10);
    for e in corpus {
        wb.push(e);
    }
    let (size_b, ck_b, mut anchors_b) =
        wb.publish_with_anchors_sorted_staged(&pb, Some("SEGMENT")).unwrap();

    // The anchor Vec's order is HashMap iteration order (the compaction
    // sorts it by rid itself) — compare as equal SETS.
    let mut anchors_a = anchors_a;
    anchors_a.sort_by_key(|a| a.replica_id);
    anchors_b.sort_by_key(|a| a.replica_id);
    assert_eq!((size_a, ck_a, anchors_a), (size_b, ck_b, anchors_b));
    assert_eq!(
        std::fs::read(&pa).unwrap(),
        std::fs::read(&pb).unwrap(),
        "staged sorted and sorting publishes must be byte-identical"
    );
}

#[test]
fn staged_sorted_publish_rejects_duplicate_key_seq_pairs() {
    let d = dir("sst-dup");
    let path = d.join("SEGMENT-001.log");
    let mut w = SegmentWriter::new_v2(16 << 10);
    w.push(entry("dup", 8, 5, FLAG_PUT, 0));
    w.push(entry("dup", 8, 5, FLAG_PUT, 7)); // same (key, seq), different rid
    let err = w
        .publish_with_anchors_sorted_staged(&path, Some("SEGMENT"))
        .unwrap_err();
    assert!(
        matches!(err, FormatError::Invalid(_)),
        "duplicate (key, seq) must be Invalid, got {err:?}"
    );
    assert!(!path.exists(), "a rejected publish must not write a file");
}
