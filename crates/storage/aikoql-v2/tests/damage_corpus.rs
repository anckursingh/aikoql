//! PR6-F3 (P0-3) — deterministic damage corpus.
//!
//! Recovery tests hand-rolled corruption inline (segment_corrupt, ckp001,
//! db_recovery). One shared corpus — `common::damage::Damage` — applies the
//! mutation classes (bit-flip, truncation, trailing bytes, zeroed region)
//! deterministically, and this matrix runs the full corpus through the
//! FormatError classifiers: Io vs Corrupt vs Unsupported, and the WAL's
//! torn-tail distinction. Every new codec path inherits the classifier
//! coverage by adding its fixture here.
//!
//! Pinned semantics (from the decoders' own doc comments):
//! - Decode order is magic → version → type → checksum, so a raw flip of a
//!   VERSION byte (or of a checkpoint placement-variant byte) classifies as
//!   Unsupported (the checksum is never reached); a checksum-repaired
//!   version change is not byte damage and is out of corpus. Every other
//!   flip is Corrupt.
//! - WAL: damage with a valid frame after it is Corrupt (KSE-082B); damage
//!   in the FINAL frame with nothing valid after it is a torn tail — the
//!   crash window — replayed as a truncated prefix, never as silent data.
//! - Stale is a lifecycle class (generation moved), not byte damage — the
//!   corpus cannot produce it; phy001–005 pin it.

mod common;

use aikoql_storage_v2::checkpoint::{test_support, DirectoryCheckpoint};
use aikoql_storage_v2::db::{Config, Db, WAL_FILE};
use aikoql_storage_v2::format::{Current, FormatError};
use aikoql_storage_v2::identity::{LogicalId, ObjectId, ReplicaId};
use aikoql_storage_v2::placement::directory::{PhysicalLocation, Placement};
use aikoql_storage_v2::placement::{BlockId, SegmentId};
use aikoql_storage_v2::wal::{encode_frame, replay_frames, replay_reader, Op, WalFrame};
use common::damage::Damage;
use common::dir;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;

const FRAME_HEADER_LEN: usize = 19; // magic 4 + version 2 + type 1 + seq 8 + payload_len 4

/// Start offset of every frame, plus the total length as the sentinel end.
fn frame_bounds(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        starts.push(pos);
        let payload_len =
            u32::from_le_bytes(bytes[pos + 15..pos + 19].try_into().expect("frame header"))
                as usize;
        pos += FRAME_HEADER_LEN + payload_len + 8;
    }
    assert_eq!(pos, bytes.len(), "fixture frames must be clean");
    starts.push(bytes.len());
    starts
}

/// Three synthetic frames (seqs 10/11/12) with known boundaries — the
/// decoder-level sweeps run on this, not on a Db fixture.
fn synthetic_wal() -> (Vec<u8>, Vec<WalFrame>) {
    let frames = vec![
        WalFrame {
            seq: 10,
            ops: vec![Op::Put(b"a".to_vec(), b"1".to_vec())],
        },
        WalFrame {
            seq: 11,
            ops: vec![
                Op::PutObject(ReplicaId(2), b"b".to_vec(), b"2".to_vec()),
                Op::Delete(b"c".to_vec()),
            ],
        },
        WalFrame {
            seq: 12,
            ops: vec![Op::DeleteObject(ReplicaId(3), b"d".to_vec())],
        },
    ];
    let mut bytes = Vec::new();
    for f in &frames {
        bytes.extend_from_slice(&encode_frame(f.seq, &f.ops).unwrap());
    }
    (bytes, frames)
}

/// The ckp001 frozen golden, rebuilt through the same fixture path.
fn checkpoint_fixture() -> Vec<u8> {
    let mut identity = HashMap::new();
    identity.insert(ObjectId([0x11; 16]), LogicalId(1));
    identity.insert(ObjectId([0x22; 16]), LogicalId(2));
    let mut replicas = HashMap::new();
    replicas.insert(LogicalId(1), ReplicaId(10));
    replicas.insert(LogicalId(2), ReplicaId(20));
    let mut placements = HashMap::new();
    placements.insert(
        ReplicaId(10),
        Placement::Segment(PhysicalLocation {
            segment_id: SegmentId(5),
            block_id: BlockId(3),
            entry_offset: 7,
            generation: 9,
        }),
    );
    placements.insert(ReplicaId(20), Placement::Retired { generation: 11 });
    placements.insert(ReplicaId(30), Placement::Memtable { generation: 4 });
    let checkpoint =
        DirectoryCheckpoint::from_state(7, &identity, &replicas, &placements, 3, 30, 12, 0, 0, 0);
    test_support::encode_for_tests(&checkpoint)
}

/// A real seeded Db: two batch writes (one WAL frame each — puts sit in
/// the memtable until flush), then closed. Returns the dir + the on-disk
/// WAL bytes (replay_frames gives the frames for expectations).
fn seeded_wal_fixture(tag: &str) -> (PathBuf, Vec<u8>, Vec<WalFrame>) {
    let d = dir(&format!("corpus-{tag}"));
    let db = Db::open(Config::new(d.clone())).unwrap();
    db.write(&[Op::Put(b"k1".to_vec(), b"v1".to_vec())])
        .unwrap();
    db.write(&[Op::Put(b"k2".to_vec(), b"v2".to_vec())])
        .unwrap();
    drop(db);
    let bytes = std::fs::read(d.join(WAL_FILE)).unwrap();
    let (frames, consumed) = replay_frames(&bytes).unwrap();
    assert_eq!(consumed, bytes.len(), "clean fixture must replay fully");
    assert!(
        frames.len() >= 2,
        "corpus fixture needs >=2 frames to exercise the non-final legs"
    );
    (d, bytes, frames)
}

#[test]
fn wal_flip_corpus_classifies_every_byte() {
    let (bytes, frames) = synthetic_wal();
    let bounds = frame_bounds(&bytes);
    for o in 0..bytes.len() {
        let damaged = Damage::BitFlip { offset: o }.apply(&bytes);
        let frame_idx = bounds
            .windows(2)
            .position(|w| w[0] <= o && o < w[1])
            .expect("every byte is inside a frame");
        if frame_idx + 1 == frames.len() {
            // Final frame + nothing valid after = torn tail (the crash
            // window), replayed as the exact prefix.
            let (replayed, consumed) = replay_frames(&damaged).unwrap();
            assert_eq!(
                replayed,
                frames[..frames.len() - 1],
                "flip at {o} (final frame): torn tail must replay the exact prefix"
            );
            assert_eq!(
                consumed, bounds[frame_idx],
                "consumed must stop at the tail"
            );
        } else {
            assert!(
                matches!(replay_frames(&damaged), Err(FormatError::Corrupt(_))),
                "flip at {o} (frame {frame_idx}): damage with a valid frame after must be Corrupt"
            );
        }
    }
}

#[test]
fn wal_truncation_corpus_replays_exact_prefix() {
    let (bytes, frames) = synthetic_wal();
    let bounds = frame_bounds(&bytes);
    for cut in 0..bytes.len() {
        let damaged = Damage::Truncate(cut).apply(&bytes);
        let (replayed, consumed) = replay_frames(&damaged).unwrap();
        let kept = bounds.windows(2).filter(|w| w[1] <= cut).count();
        assert_eq!(
            replayed,
            frames[..kept],
            "truncation at {cut}: replay must yield exactly the {kept} complete frames"
        );
        assert_eq!(consumed, bounds[kept]);
    }
}

#[test]
fn wal_trailing_bytes_are_a_torn_tail() {
    let (bytes, frames) = synthetic_wal();
    for extra in [0x00u8, 0xFF] {
        let damaged = Damage::TrailingByte(extra).apply(&bytes);
        let (replayed, consumed) = replay_frames(&damaged).unwrap();
        assert_eq!(
            replayed, frames,
            "trailing garbage must not drop valid frames"
        );
        assert_eq!(consumed, bytes.len());
    }
}

#[test]
fn wal_zero_region_corpus() {
    let (bytes, frames) = synthetic_wal();
    // Magic of a non-final frame: Corrupt (a valid frame follows).
    let damaged = Damage::ZeroRegion { from: 0, len: 4 }.apply(&bytes);
    assert!(matches!(
        replay_frames(&damaged),
        Err(FormatError::Corrupt(_))
    ));
    // Checksum region of the final frame: torn tail, exact prefix.
    let damaged = Damage::ZeroRegion {
        from: bytes.len() - 8,
        len: 8,
    }
    .apply(&bytes);
    let (replayed, _) = replay_frames(&damaged).unwrap();
    assert_eq!(replayed, frames[..frames.len() - 1]);
}

/// Keys of the frames fully contained in bytes[..cut]: the data the Db must
/// still serve after a truncation — no more, no less.
fn surviving_keys(frames: &[WalFrame], bounds: &[usize], cut: usize) -> Vec<Vec<u8>> {
    let mut keys = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        if bounds[i + 1] <= cut {
            keys.extend(f.ops.iter().map(|op| op.key().to_vec()));
        }
    }
    keys.sort();
    keys
}

#[test]
fn db_reopen_after_wal_damage_spot_checks() {
    // Mid-frame-0 flip: Corrupt at open (KSE-082B at the Db boundary).
    let (d, bytes, _) = seeded_wal_fixture("mid");
    let damaged = Damage::BitFlip { offset: 20 }.apply(&bytes); // inside frame 0's payload
    std::fs::write(d.join(WAL_FILE), &damaged).unwrap();
    assert!(matches!(
        Db::open(Config::new(d.clone())),
        Err(FormatError::Corrupt(_))
    ));

    // Final-frame flip: torn tail — opens, the last frame's data is gone,
    // the prefix survives.
    let (d, bytes, frames) = seeded_wal_fixture("tail");
    let bounds = frame_bounds(&bytes);
    let damaged = Damage::BitFlip {
        offset: bytes.len() - 1,
    }
    .apply(&bytes);
    std::fs::write(d.join(WAL_FILE), &damaged).unwrap();
    let db = Db::open(Config::new(d.clone())).unwrap();
    let last_start = bounds[bounds.len() - 2];
    let expect = surviving_keys(&frames, &bounds, last_start);
    for k in [b"k1".to_vec(), b"k2".to_vec()] {
        let got = db.get(&k).unwrap();
        let want = expect.contains(&k);
        assert_eq!(got.is_some(), want, "key {k:?} after torn tail");
    }

    // Truncation mid-frame-0: opens, both puts dropped with the torn frame.
    let (d, bytes, frames) = seeded_wal_fixture("trunc");
    let bounds = frame_bounds(&bytes);
    let mid0 = bounds[0] + FRAME_HEADER_LEN;
    let damaged = Damage::Truncate(mid0).apply(&bytes);
    std::fs::write(d.join(WAL_FILE), &damaged).unwrap();
    let db = Db::open(Config::new(d.clone())).unwrap();
    assert_eq!(
        surviving_keys(&frames, &bounds, mid0),
        Vec::<Vec<u8>>::new(),
        "cut inside frame 0 keeps nothing"
    );
    assert!(db.get(b"k1").unwrap().is_none());

    // Truncation at 0: empty WAL, opens empty.
    let (d, bytes, _) = seeded_wal_fixture("empty");
    std::fs::write(d.join(WAL_FILE), &bytes[..0]).unwrap();
    let db = Db::open(Config::new(d.clone())).unwrap();
    assert!(db.get(b"k1").unwrap().is_none());
    assert!(db.get(b"k2").unwrap().is_none());
}

// ---------------------------------------------------------------------------
// M28 — the reader-based replay (`replay_reader`, P0-01) must be verdict-
// and byte-equivalent to the in-memory decoder this corpus pins, at the
// function boundary and at the Db boundary.
// ---------------------------------------------------------------------------

/// The reader's applied stream, flattened to (seq, op) pairs — the same
/// flattening as `replay_stream` so the two are comparable.
fn reader_stream(bytes: &[u8]) -> Result<(Vec<(u64, Op)>, u64), FormatError> {
    let mut cur = Cursor::new(bytes);
    let mut stream = Vec::new();
    let (consumed, end) = replay_reader(&mut cur, |seq, op| {
        stream.push((seq, op));
        Ok(())
    })?;
    assert_eq!(end, bytes.len() as u64, "the reader must see the whole file");
    Ok((stream, consumed))
}

/// The in-memory reference — `replay_frames` flattened the same way.
fn replay_stream(bytes: &[u8]) -> Result<(Vec<(u64, Op)>, u64), FormatError> {
    let (frames, consumed) = replay_frames(bytes)?;
    let stream = frames
        .into_iter()
        .flat_map(|f| f.ops.into_iter().map(move |op| (f.seq, op)))
        .collect();
    Ok((stream, consumed as u64))
}

#[test]
fn wal_reader_matches_in_memory_replay_over_the_damage_corpus() {
    let (bytes, _) = synthetic_wal();
    let mut cases: Vec<Vec<u8>> = vec![bytes.clone()]; // the clean corpus
    for o in 0..bytes.len() {
        cases.push(Damage::BitFlip { offset: o }.apply(&bytes));
    }
    for cut in 0..bytes.len() {
        cases.push(Damage::Truncate(cut).apply(&bytes));
    }
    for extra in [0x00u8, 0xFF] {
        cases.push(Damage::TrailingByte(extra).apply(&bytes));
    }
    cases.push(Damage::ZeroRegion { from: 0, len: 4 }.apply(&bytes)); // magic
    cases.push(
        Damage::ZeroRegion {
            from: bytes.len() - 8,
            len: 8,
        }
        .apply(&bytes), // checksum
    );
    cases.push(
        Damage::ZeroRegion {
            from: 30,
            len: 12,
        }
        .apply(&bytes), // payload
    );
    // One big-frame case: frame 0 sized so frame 1's magic starts exactly
    // on the reader probe's 64 KiB chunk boundary (the probe's overlap
    // scan must not miss a valid frame straddling a chunk — a missed
    // straddle would misread damage-with-valid-after as a torn tail).
    let f0 = encode_frame(1, &[Op::Put(b"k".to_vec(), vec![b'v'; 65_495])]).unwrap();
    assert_eq!(f0.len(), 65_536, "frame 0 must end on the chunk boundary");
    let mut big = f0;
    big.extend_from_slice(&encode_frame(2, &[Op::Put(b"a".to_vec(), b"1".to_vec())]).unwrap());
    cases.push(Damage::BitFlip { offset: 0 }.apply(&big));

    for (i, case) in cases.iter().enumerate() {
        let r = reader_stream(case);
        let m = replay_stream(case);
        match (r, m) {
            (Ok((rs, rc)), Ok((ms, mc))) => {
                assert_eq!(rc, mc, "case {i}: consumed diverged");
                assert_eq!(rs, ms, "case {i}: applied stream diverged");
            }
            (Err(re), Err(me)) => {
                assert!(matches!(re, FormatError::Corrupt(_)), "case {i}: reader {re:?}");
                assert!(matches!(me, FormatError::Corrupt(_)), "case {i}: replay {me:?}");
            }
            (r, m) => panic!("case {i}: verdict diverged — reader {r:?} vs replay {m:?}"),
        }
    }
}

#[test]
fn wal_reader_torn_tail_truncates_to_the_valid_prefix() {
    let (d, bytes, frames) = seeded_wal_fixture("reader-tail");
    let bounds = frame_bounds(&bytes);
    let last_start = bounds[bounds.len() - 2];
    // Tear the FINAL frame (partial payload — the crash window).
    let cut = last_start + FRAME_HEADER_LEN;
    std::fs::write(d.join(WAL_FILE), &bytes[..cut]).unwrap();
    let db = Db::open(Config::new(d.clone())).unwrap();
    // The torn tail is physically gone — the on-disk WAL IS the valid
    // prefix, not just logically skipped.
    let on_disk = std::fs::read(d.join(WAL_FILE)).unwrap();
    assert_eq!(
        on_disk,
        &bytes[..last_start],
        "the WAL must be truncated to the valid prefix"
    );
    // The prefix data survives, the torn frame's does not.
    let expect = surviving_keys(&frames, &bounds, last_start);
    for k in [b"k1".to_vec(), b"k2".to_vec()] {
        let got = db.get(&k).unwrap();
        assert_eq!(got.is_some(), expect.contains(&k), "key {k:?} after torn tail");
    }
    drop(db);
    // Reopen clean: the truncation left the store consistent.
    let db = Db::open(Config::new(d.clone())).unwrap();
    for k in [b"k1".to_vec(), b"k2".to_vec()] {
        let got = db.get(&k).unwrap();
        assert_eq!(got.is_some(), expect.contains(&k), "key {k:?} after clean reopen");
    }
}

#[test]
fn wal_reader_damage_then_valid_fails_closed() {
    // Reader level: damage in frame 0 with frames 1-2 valid after it —
    // Corrupt, never a truncated Ok.
    let (bytes, _) = synthetic_wal();
    let damaged = Damage::BitFlip { offset: 25 }.apply(&bytes);
    assert!(
        matches!(reader_stream(&damaged), Err(FormatError::Corrupt(_))),
        "damage followed by a valid frame must be Corrupt, never a torn-tail Ok"
    );
    // Db boundary: open fails closed and the WAL is untouched on disk —
    // the failed open must not have truncated.
    let (d, bytes, _) = seeded_wal_fixture("reader-mid");
    let damaged = Damage::BitFlip { offset: 20 }.apply(&bytes);
    std::fs::write(d.join(WAL_FILE), &damaged).unwrap();
    assert!(matches!(
        Db::open(Config::new(d.clone())),
        Err(FormatError::Corrupt(_))
    ));
    assert_eq!(
        std::fs::read(d.join(WAL_FILE)).unwrap(),
        damaged,
        "a failed open must never truncate the WAL"
    );
}

#[test]
fn wal_reader_enforces_strictly_increasing_sequences() {
    // Equal seqs across frames: Corrupt.
    let w = [
        encode_frame(7, &[Op::Put(b"a".to_vec(), b"1".to_vec())]).unwrap(),
        encode_frame(7, &[Op::Put(b"b".to_vec(), b"2".to_vec())]).unwrap(),
    ]
    .concat();
    assert!(
        matches!(reader_stream(&w), Err(FormatError::Corrupt(_))),
        "equal seqs must be Corrupt"
    );
    // Decreasing: Corrupt.
    let w = [
        encode_frame(7, &[Op::Put(b"a".to_vec(), b"1".to_vec())]).unwrap(),
        encode_frame(3, &[Op::Put(b"b".to_vec(), b"2".to_vec())]).unwrap(),
    ]
    .concat();
    assert!(
        matches!(reader_stream(&w), Err(FormatError::Corrupt(_))),
        "decreasing seqs must be Corrupt"
    );
    // Strictly increasing: both frames decode and apply in order.
    let w = [
        encode_frame(7, &[Op::Put(b"a".to_vec(), b"1".to_vec())]).unwrap(),
        encode_frame(8, &[Op::Put(b"b".to_vec(), b"2".to_vec())]).unwrap(),
    ]
    .concat();
    let (stream, consumed) = reader_stream(&w).unwrap();
    assert_eq!(consumed, w.len() as u64);
    assert_eq!(stream.len(), 2);
    assert_eq!(stream[0].0, 7);
    assert_eq!(stream[1].0, 8);
}

#[test]
fn checkpoint_flip_corpus() {    let bytes = checkpoint_fixture();
    let mut unsupported = 0;
    for o in 0..bytes.len() {
        let damaged = Damage::BitFlip { offset: o }.apply(&bytes);
        let err = DirectoryCheckpoint::decode(&damaged).unwrap_err();
        if (4..6).contains(&o) {
            assert!(matches!(err, FormatError::Unsupported(_)), "offset {o}");
            unsupported += 1;
        } else if matches!(err, FormatError::Unsupported(_)) {
            // A placement VARIANT byte (one per record) — the decoder's
            // other Unsupported site. Its position moves with the record
            // layout, so the corpus pins the COUNT, not the offsets.
            unsupported += 1;
        } else {
            assert!(
                matches!(err, FormatError::Corrupt(_)),
                "offset {o}: {err:?}"
            );
        }
    }
    assert_eq!(
        unsupported, 5,
        "2 version bytes + 3 placement-variant bytes classify Unsupported — \
         a codec change here must update this pin deliberately"
    );
}

#[test]
fn checkpoint_truncation_and_trailing() {
    let bytes = checkpoint_fixture();
    for cut in 0..bytes.len() {
        let damaged = Damage::Truncate(cut).apply(&bytes);
        assert!(
            matches!(
                DirectoryCheckpoint::decode(&damaged),
                Err(FormatError::Corrupt(_))
            ),
            "truncation at {cut}"
        );
    }
    let damaged = Damage::TrailingByte(0x00).apply(&bytes);
    assert!(matches!(
        DirectoryCheckpoint::decode(&damaged),
        Err(FormatError::Corrupt(_))
    ));
}

#[test]
fn checkpoint_missing_file_is_io() {
    let d = dir("corpus-ckpt-io");
    assert!(matches!(
        DirectoryCheckpoint::read(&d.join("never-written")),
        Err(FormatError::Io(_))
    ));
}

#[test]
fn current_flip_corpus() {
    let (d, bytes) = {
        let d = dir("corpus-current");
        let db = Db::open(Config::new(d.clone())).unwrap();
        db.put(b"k1", b"v1").unwrap();
        db.flush().unwrap();
        drop(db);
        (d.clone(), std::fs::read(d.join("CURRENT")).unwrap())
    };
    assert_eq!(bytes.len(), 22, "CURRENT is fixed 22 bytes");
    for o in 0..bytes.len() {
        let damaged = Damage::BitFlip { offset: o }.apply(&bytes);
        let path = d.join("CURRENT-damaged");
        std::fs::write(&path, &damaged).unwrap();
        let err = Current::read(&path).unwrap_err();
        if (4..6).contains(&o) {
            assert!(matches!(err, FormatError::Unsupported(_)), "offset {o}");
        } else {
            assert!(
                matches!(err, FormatError::Corrupt(_)),
                "offset {o}: {err:?}"
            );
        }
    }
}

#[test]
fn current_truncation_and_trailing() {
    let d = dir("corpus-current-len");
    let db = Db::open(Config::new(d.clone())).unwrap();
    db.put(b"k1", b"v1").unwrap();
    db.flush().unwrap();
    drop(db);
    let bytes = std::fs::read(d.join("CURRENT")).unwrap();
    for cut in 0..bytes.len() {
        let damaged = Damage::Truncate(cut).apply(&bytes);
        std::fs::write(d.join("CURRENT-damaged"), &damaged).unwrap();
        assert!(matches!(
            Current::read(&d.join("CURRENT-damaged")),
            Err(FormatError::Corrupt(_))
        ));
    }
    let damaged = Damage::TrailingByte(0x00).apply(&bytes);
    std::fs::write(d.join("CURRENT-damaged"), &damaged).unwrap();
    assert!(matches!(
        Current::read(&d.join("CURRENT-damaged")),
        Err(FormatError::Corrupt(_))
    ));
}

#[test]
fn current_missing_file_is_io() {
    let d = dir("corpus-current-io");
    assert!(matches!(
        Current::read(&d.join("never-written")),
        Err(FormatError::Io(_))
    ));
}
