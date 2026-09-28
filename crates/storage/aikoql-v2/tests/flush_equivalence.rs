//! L-05 (TDD-007) — flush equivalence: the generic publish (sorts its
//! input) and the sorted publish (takes memtable order) are the SAME
//! flush — randomized corpora (seeded xorshift64, P1-4) must land as
//! byte-identical files, equal (size, checksum, anchor-set) results, and
//! identical read/scan answers; duplicate (key, seq) corpora must be
//! rejected identically (Invalid, no file) by both entry points.

mod common;

use aikoql_storage_v2::format::FormatError;
use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::segment::{
    SegmentEntry, SegmentReader, SegmentWriter, FLAG_DELETE, FLAG_PUT,
};
use common::dir;

/// xorshift64 — deterministic across runs (the repo's seeded-RNG idiom).
fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed;
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    }
}

/// A random corpus in MEMTABLE order (key asc, seq asc within key): the
/// sorted writer's exact input contract, so a rejection would itself fail
/// the test. Values mix empty / small / 4 KiB (block-crossing), rids mix
/// byte rows and identity rows, tombstones sprinkled.
fn corpus(r: &mut impl FnMut() -> u64) -> Vec<SegmentEntry> {
    let n_keys = 2 + (r() % 6) as usize;
    let mut keys: Vec<Vec<u8>> = (0..n_keys)
        .map(|i| format!("k{i:02}").into_bytes())
        .collect();
    keys.sort();
    let mut out = Vec::new();
    for k in keys {
        let mut seq = 1 + r() % 10;
        for _ in 0..1 + (r() % 5) as usize {
            let vlen = match r() % 3 {
                0 => 0,
                1 => (r() % 32) as usize,
                _ => 4000,
            };
            let flags = if r() % 6 == 0 { FLAG_DELETE } else { FLAG_PUT };
            let rid = [0u64, 7, 8, 9][(r() % 4) as usize];
            out.push(SegmentEntry {
                key: k.clone(),
                value: vec![b'x'; vlen],
                seq,
                flags,
                replica_id: ReplicaId(rid),
            });
            seq += 1 + r() % 7; // strictly ascending within the key
        }
    }
    out
}

fn shuffled(r: &mut impl FnMut() -> u64, mut v: Vec<SegmentEntry>) -> Vec<SegmentEntry> {
    for i in (1..v.len()).rev() {
        let j = (r() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    v
}

/// The logical surface: (key, value, seq, flags, rid) tuples, scan order.
fn logical(path: &std::path::Path) -> Vec<(String, Vec<u8>, u64, u8, u64)> {
    SegmentReader::open(path)
        .unwrap()
        .scan(b"", b"~")
        .unwrap()
        .into_iter()
        .map(|e| {
            (
                String::from_utf8(e.key).unwrap(),
                e.value,
                e.seq,
                e.flags,
                e.replica_id.0,
            )
        })
        .collect()
}

#[test]
fn randomized_sorted_publish_equals_the_generic_publish() {
    let mut r = rng(0x5eed_0902);
    for _ in 0..64 {
        let c = corpus(&mut r);
        let generic_input = shuffled(&mut r, c.clone());
        let mut a = generic_input.clone();
        a.sort_by_key(|e| (e.key.clone(), e.seq));
        let mut b = c.clone();
        b.sort_by_key(|e| (e.key.clone(), e.seq));
        assert_eq!(a, b, "the shuffle must be a permutation");

        let da = dir("flush-eq-a");
        let pa = da.join("SEGMENT-001.log");
        let mut wa = SegmentWriter::new_v4(16 << 10);
        for e in generic_input {
            wa.push(e);
        }
        let (size_a, ck_a, mut anchors_a) = wa.publish_with_anchors(&pa).unwrap();

        let db_ = dir("flush-eq-b");
        let pb = db_.join("SEGMENT-001.log");
        let mut wb = SegmentWriter::new_v4(16 << 10);
        for e in c {
            wb.push(e);
        }
        let (size_b, ck_b, mut anchors_b) = wb.publish_with_anchors_sorted(&pb).unwrap();

        // Anchor order is map order — compare as sets.
        anchors_a.sort_by_key(|a| a.replica_id);
        anchors_b.sort_by_key(|a| a.replica_id);
        assert_eq!(
            (size_a, ck_a, anchors_a),
            (size_b, ck_b, anchors_b),
            "results must match"
        );
        assert_eq!(
            std::fs::read(&pa).unwrap(),
            std::fs::read(&pb).unwrap(),
            "byte-identical files"
        );
        assert_eq!(logical(&pa), logical(&pb), "identical read/scan answers");
    }
}

#[test]
fn duplicate_corpora_are_rejected_identically() {
    let mut r = rng(0x5eed_0717);
    for _ in 0..24 {
        let mut c = corpus(&mut r);
        // Force a duplicate (key, seq) while keeping memtable order: give
        // the first key's run one extra version carrying an earlier
        // version's seq (inserted at the run's end — the order guard must
        // let it through so the DUPLICATE guard is what fires).
        let k0 = c[0].key.clone();
        let first = c.iter().position(|e| e.key == k0).unwrap();
        let last = c.iter().rposition(|e| e.key == k0).unwrap();
        let mut extra = c[first].clone();
        extra.seq = c[first].seq;
        c.insert(last + 1, extra);

        let da = dir("flush-dup-a");
        let pa = da.join("SEGMENT-001.log");
        let mut wa = SegmentWriter::new_v4(16 << 10);
        for e in shuffled(&mut r, c.clone()) {
            wa.push(e);
        }
        let ea = wa.publish_with_anchors(&pa).unwrap_err();
        assert!(
            matches!(ea, FormatError::Invalid(_)),
            "generic must reject duplicates as Invalid, got {ea:?}"
        );
        assert!(!pa.exists(), "a rejected publish must not write a file");

        let db_ = dir("flush-dup-b");
        let pb = db_.join("SEGMENT-001.log");
        let mut wb = SegmentWriter::new_v4(16 << 10);
        for e in c {
            wb.push(e);
        }
        let eb = wb.publish_with_anchors_sorted(&pb).unwrap_err();
        assert!(
            matches!(eb, FormatError::Invalid(_)),
            "sorted must reject duplicates as Invalid, got {eb:?}"
        );
        assert!(!pb.exists(), "a rejected publish must not write a file");
    }
}
