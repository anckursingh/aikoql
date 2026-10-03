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
    let (size_b, ck_b, mut anchors_b) = wb.publish_with_anchors_sorted(&pb).unwrap();

    // The anchor Vec's order is HashMap iteration order (the flush sorts
    // it by rid itself) — compare as equal SETS.
    let mut anchors_a = anchors_a;
    anchors_a.sort_by_key(|a| a.replica_id);
    anchors_b.sort_by_key(|a| a.replica_id);
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
fn sorted_publish_rejects_out_of_order_input() {
    // L-01 (TDD-001) — the sorted-input contract must be a real precondition
    // in BOTH profiles: debug used to panic on a debug_assert, release used
    // to publish unsorted input silently. Two violations of memtable order
    // (key asc, seq asc within key): a key inversion and a same-key seq
    // inversion.
    for corpus in [
        vec![
            entry("beta", 8, 2, FLAG_PUT, 0),
            entry("alpha", 8, 1, FLAG_PUT, 0),
        ],
        vec![
            entry("alpha", 8, 4, FLAG_PUT, 0),
            entry("alpha", 8, 1, FLAG_PUT, 0),
        ],
    ] {
        let d = dir("sorted-unsorted");
        let path = d.join("SEGMENT-001.log");
        let mut w = SegmentWriter::new_v4(16 << 10);
        for e in corpus {
            w.push(e);
        }
        let err = w.publish_with_anchors_sorted(&path).unwrap_err();
        assert!(
            matches!(err, FormatError::Invalid(_)),
            "unsorted input must be Invalid, got {err:?}"
        );
        assert!(!path.exists(), "a rejected publish must not write a file");
    }
}

/// L-02 (TDD-002) — the three duplicate-placement classes the review's
/// matrix prescribes: the pair inside one block, the pair straddling a
/// block boundary, and the pair as a whole key run at the run edge. The
/// 16 KiB target + the dry-pass estimate (25 + keylen − shared + valuelen
/// for v4, distinct-key entries) place the across fixture's pair at
/// 16107/20135 bytes — straddling the 16384 boundary, verified by the
/// control test below.
fn dup_fixtures() -> Vec<Vec<SegmentEntry>> {
    vec![
        // inside-block: both duplicates well within the first block.
        vec![
            entry("dup", 8, 5, FLAG_PUT, 0),
            entry("dup", 8, 5, FLAG_PUT, 7), // same (key, seq), different rid
            entry("k0", 8, 1, FLAG_PUT, 0),
            entry("k0", 8, 2, FLAG_PUT, 0),
        ],
        // across-boundary: the pair would land in blocks 1 and 2.
        vec![
            entry("a0", 4000, 1, FLAG_PUT, 0),
            entry("a1", 4000, 2, FLAG_PUT, 0),
            entry("a2", 4000, 3, FLAG_PUT, 0),
            entry("dup", 4000, 5, FLAG_PUT, 7),
            entry("dup", 4000, 5, FLAG_PUT, 8),
        ],
        // run-edge: the pair is key "a"'s ENTIRE run (the reversal no-op
        // edge), immediately followed by the next key's run.
        vec![
            entry("a", 8, 5, FLAG_PUT, 0),
            entry("a", 8, 5, FLAG_PUT, 7),
            entry("b", 8, 2, FLAG_PUT, 0),
        ],
    ]
}

#[test]
fn duplicate_matrix_rejects_every_placement_class() {
    for corpus in dup_fixtures() {
        // The sorting writer: deliberately unsorted input — the sort must
        // still land the duplicate pair adjacently for the shared guard.
        let mut unsorted = corpus.clone();
        unsorted.reverse();
        let d = dir("dup-matrix-sort");
        let path = d.join("SEGMENT-001.log");
        let mut w = SegmentWriter::new_v4(16 << 10);
        for e in unsorted {
            w.push(e);
        }
        let err = w.publish_with_anchors(&path).unwrap_err();
        assert!(
            matches!(err, FormatError::Invalid(_)),
            "duplicate (key, seq) must be Invalid, got {err:?}"
        );
        assert!(
            std::fs::read_dir(&d).unwrap().next().is_none(),
            "a rejected publish must leave no segment visible"
        );

        // The sorted writer: memtable order straight in (the reversal runs
        // first, then the shared guard — the matrix pins the pair of them).
        let d = dir("dup-matrix-sorted");
        let path = d.join("SEGMENT-001.log");
        let mut w = SegmentWriter::new_v4(16 << 10);
        for e in corpus {
            w.push(e);
        }
        let err = w.publish_with_anchors_sorted(&path).unwrap_err();
        assert!(
            matches!(err, FormatError::Invalid(_)),
            "duplicate (key, seq) must be Invalid, got {err:?}"
        );
        assert!(
            std::fs::read_dir(&d).unwrap().next().is_none(),
            "a rejected publish must leave no segment visible"
        );
    }
}

#[test]
fn across_boundary_fixture_really_straddles() {
    // Control: the across fixture with the second duplicate's seq bumped —
    // no duplicate, the publish succeeds, and the pair's two rids land in
    // DIFFERENT blocks. The matrix's across-boundary leg is only honest if
    // this holds.
    let corpus = vec![
        entry("a0", 4000, 1, FLAG_PUT, 0),
        entry("a1", 4000, 2, FLAG_PUT, 0),
        entry("a2", 4000, 3, FLAG_PUT, 0),
        entry("dup", 4000, 5, FLAG_PUT, 7),
        entry("dup", 4000, 6, FLAG_PUT, 8),
    ];
    let d = dir("dup-matrix-straddle");
    let path = d.join("SEGMENT-001.log");
    let mut w = SegmentWriter::new_v4(16 << 10);
    for e in corpus {
        w.push(e);
    }
    let (_size, _ck, anchors) = w.publish_with_anchors_sorted(&path).unwrap();
    let b7 = anchors
        .iter()
        .find(|a| a.replica_id == ReplicaId(7))
        .expect("rid 7 anchored")
        .block_id;
    let b8 = anchors
        .iter()
        .find(|a| a.replica_id == ReplicaId(8))
        .expect("rid 8 anchored")
        .block_id;
    assert_ne!(b7, b8, "the control pair must straddle a block boundary");
}

#[test]
fn sorted_publish_decodes_to_key_asc_seq_desc_within_key() {
    let d = dir("sorted-order");
    let path = d.join("SEGMENT-001.log");
    // v4: rids persist in v3+ blocks only (v2 decodes them as 0).
    let mut w = SegmentWriter::new_v4(16 << 10);
    for e in version_corpus() {
        w.push(e);
    }
    w.publish_with_anchors_sorted(&path).unwrap();

    let reader = SegmentReader::open(&path).unwrap();
    let got: Vec<(String, u64, u64)> = reader
        .scan(b"", b"~")
        .unwrap()
        .into_iter()
        .map(|e| (String::from_utf8(e.key).unwrap(), e.seq, e.replica_id.0))
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
