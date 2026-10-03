//! The F-05 fuzz surface (PR #7 fuzz review, the deferred FZ-01..07
//! targets): the pure decode logic the storage crate runs, in functions
//! shared with the cargo-fuzz targets. The fuzz crate is a separate,
//! non-workspace package (its own `[workspace]` table) that can only
//! reach `pub` API — hence this doc(hidden) module. The seven `check_*`
//! entry points are the FZ-01..07 target bodies: each restates a frozen
//! format property over the real shared logic and panics on violation
//! (a crash to libFuzzer). Valid inputs are embedded in the checks so the
//! engine reaches the deep paths without a seed corpus.

use crate::checkpoint::test_support::encode_for_tests;
use crate::checkpoint::DirectoryCheckpoint;
use crate::format::{self, Current, Manifest, SegmentRecord};
use crate::identity::directory::{IdentityLog, IdentityRecord, ReplicaLog, ReplicaRecord};
use crate::identity::{LogicalId, NodeId, ObjectId, ReplicaId};
use crate::legacy_envelope::{self, ParseOutcome};
use crate::placement::directory::{PlacementLog, PlacementRecord};
use crate::placement::Placement;
use crate::snapshot::{SnapshotFile, SnapshotMarker};
use crate::wal::{self, Op};

/// FZ-01 — CURRENT: any accepted input re-encodes byte-identically.
pub fn check_current(data: &[u8]) {
    if let Ok(c) = Current::decode(data) {
        let enc = c.encode();
        let d = Current::decode(&enc).expect("re-decode of the encode must succeed");
        assert_eq!(d.encode(), enc, "CURRENT re-encode must be byte-identical");
    }
    let valid = Current::new(format::FORMAT_VERSION, 1).encode();
    round_trip_current(&valid);
}

fn round_trip_current(data: &[u8]) {
    let c = Current::decode(data).expect("the valid seed must decode");
    let enc = c.encode();
    let d = Current::decode(&enc).expect("re-decode of the encode must succeed");
    assert_eq!(d.encode(), enc, "CURRENT re-encode must be byte-identical");
}

/// FZ-02 — MANIFEST: byte-identical re-encode, and `verify_pair` over the
/// decoded value against a generation-matched CURRENT is total.
pub fn check_manifest(data: &[u8]) {
    if let Ok(m) = Manifest::decode(data) {
        let enc = m.encode();
        let d = Manifest::decode(&enc).expect("re-decode of the encode must succeed");
        assert_eq!(d.encode(), enc, "MANIFEST re-encode must be byte-identical");
        let _ = format::verify_pair(&Current::new(format::FORMAT_VERSION, m.generation), &m);
    }
    let valid = Manifest {
        format_version: format::FORMAT_VERSION,
        generation: 1,
        segments: vec![SegmentRecord {
            segment_id: 0,
            level: 0,
            key_min: vec![],
            key_max: vec![],
            seq_lo: 0,
            seq_hi: 0,
            record_count: 0,
            file_size: 0,
            checksum: 0,
        }],
        wal_ids: vec![0],
        identity_floor: 0,
        replica_floor: 0,
        placement_floor: 0,
        identity_chain: 0,
        replica_chain: 0,
        placement_chain: 0,
    }
    .encode();
    round_trip_manifest(&valid);
}

fn round_trip_manifest(data: &[u8]) {
    let m = Manifest::decode(data).expect("the valid seed must decode");
    let enc = m.encode();
    let d = Manifest::decode(&enc).expect("re-decode of the encode must succeed");
    assert_eq!(d.encode(), enc, "MANIFEST re-encode must be byte-identical");
    let _ = format::verify_pair(&Current::new(format::FORMAT_VERSION, m.generation), &m);
}

/// FZ-03 — one WAL frame: the consumed length stays inside the input and
/// the frame round-trips identically.
pub fn check_wal_frame(data: &[u8]) {
    if let Ok((frame, consumed)) = wal::decode_frame(data) {
        assert!(consumed <= data.len(), "consumed must not exceed the input");
        let enc = wal::encode_frame(frame.seq, &frame.ops)
            .expect("re-encode of a decoded frame must succeed");
        let (d, c2) = wal::decode_frame(&enc).expect("re-decode of the encode must succeed");
        assert_eq!(c2, enc.len(), "the frame must consume its whole encode");
        assert_eq!(d, frame, "the frame must round-trip identically");
    }
    for ops in [
        vec![Op::Put(b"k".to_vec(), b"v".to_vec())],
        vec![
            Op::Delete(b"k".to_vec()),
            Op::PutObject(ReplicaId::from_bytes([0; 8]), b"k".to_vec(), b"v".to_vec()),
        ],
    ] {
        let valid = wal::encode_frame(1, &ops).expect("seed frame must encode");
        round_trip_frame(&valid);
    }
}

fn round_trip_frame(data: &[u8]) {
    let (frame, consumed) = wal::decode_frame(data).expect("the valid seed must decode");
    assert_eq!(
        consumed,
        data.len(),
        "the frame must consume its whole encode"
    );
    let enc = wal::encode_frame(frame.seq, &frame.ops)
        .expect("re-encode of a decoded frame must succeed");
    let (d, c2) = wal::decode_frame(&enc).expect("re-decode of the encode must succeed");
    assert_eq!(c2, enc.len(), "the frame must consume its whole encode");
    assert_eq!(d, frame, "the frame must round-trip identically");
}

/// FZ-04 — WAL replay: the batch and streaming entry points agree on the
/// outcome and the consumed bytes.
pub fn check_wal_replay(data: &[u8]) {
    replay_consistency(data);
    let valid = wal::encode_frame(1, &[Op::Put(b"k".to_vec(), b"v".to_vec())])
        .expect("seed frame must encode");
    replay_consistency(&valid);
}

fn replay_consistency(data: &[u8]) {
    let batch = wal::replay_frames(data);
    let streaming = wal::replay_frames_streaming(data, |_| Ok(()));
    match (batch, streaming) {
        (Ok((_, n1)), Ok(n2)) => {
            assert_eq!(
                n1, n2,
                "batch and streaming replay must agree on the consumed bytes"
            );
        }
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => panic!("batch and streaming replay diverged"),
        (Err(_), Err(_)) => {}
    }
}

/// FZ-05 — directory checkpoint: an accepted checkpoint round-trips
/// identically through the test encoder.
pub fn check_checkpoint(data: &[u8]) {
    if let Ok(cp) = DirectoryCheckpoint::decode(data) {
        let enc = encode_for_tests(&cp);
        let d = DirectoryCheckpoint::decode(&enc).expect("re-decode of the encode must succeed");
        assert_eq!(d, cp, "the checkpoint must round-trip identically");
    }
    let valid = DirectoryCheckpoint {
        format_version: format::FORMAT_VERSION,
        generation: 1,
        identities: vec![IdentityRecord {
            oid: ObjectId::from_bytes([0; 16]),
            lid: LogicalId::from_bytes([0; 8]),
        }],
        replicas: vec![ReplicaRecord {
            lid: LogicalId::from_bytes([0; 8]),
            node: NodeId::from_bytes([0; 8]),
            rid: ReplicaId::from_bytes([0; 8]),
        }],
        placements: vec![PlacementRecord {
            rid: ReplicaId::from_bytes([0; 8]),
            placement: Placement::Memtable { generation: 1 },
        }],
        next_logical_id: 1,
        next_replica_id: 1,
        next_placement_generation: 1,
        identity_chain: 0,
        replica_chain: 0,
        placement_chain: 0,
    };
    let enc = encode_for_tests(&valid);
    round_trip_checkpoint(&enc);
}

fn round_trip_checkpoint(data: &[u8]) {
    let cp = DirectoryCheckpoint::decode(data).expect("the valid seed must decode");
    let enc = encode_for_tests(&cp);
    let d = DirectoryCheckpoint::decode(&enc).expect("re-decode of the encode must succeed");
    assert_eq!(d, cp, "the checkpoint must round-trip identically");
}

/// FZ-06 — the identity/replica/placement directory logs: any accepted
/// log re-encodes byte-identically.
pub fn check_directories(data: &[u8]) {
    if let Ok(l) = IdentityLog::decode(data) {
        let enc = l.encode();
        let d = IdentityLog::decode(&enc).expect("re-decode of the encode must succeed");
        assert_eq!(
            d.encode(),
            enc,
            "identity log re-encode must be byte-identical"
        );
    }
    if let Ok(l) = ReplicaLog::decode(data) {
        let enc = l.encode();
        let d = ReplicaLog::decode(&enc).expect("re-decode of the encode must succeed");
        assert_eq!(
            d.encode(),
            enc,
            "replica log re-encode must be byte-identical"
        );
    }
    if let Ok(l) = PlacementLog::decode(data) {
        let enc = l.encode();
        let d = PlacementLog::decode(&enc).expect("re-decode of the encode must succeed");
        assert_eq!(
            d.encode(),
            enc,
            "placement log re-encode must be byte-identical"
        );
    }
    let seeds = [
        IdentityLog {
            format_version: format::FORMAT_VERSION,
            generation: 1,
            records: vec![IdentityRecord {
                oid: ObjectId::from_bytes([0; 16]),
                lid: LogicalId::from_bytes([0; 8]),
            }],
        }
        .encode(),
        ReplicaLog {
            format_version: format::FORMAT_VERSION,
            generation: 1,
            records: vec![ReplicaRecord {
                lid: LogicalId::from_bytes([0; 8]),
                node: NodeId::from_bytes([0; 8]),
                rid: ReplicaId::from_bytes([0; 8]),
            }],
        }
        .encode(),
        PlacementLog {
            format_version: format::FORMAT_VERSION,
            generation: 1,
            records: vec![PlacementRecord {
                rid: ReplicaId::from_bytes([0; 8]),
                placement: Placement::Segment(crate::placement::PhysicalLocation {
                    segment_id: crate::placement::SegmentId(0),
                    block_id: crate::placement::BlockId(0),
                    entry_offset: 0,
                    generation: 1,
                }),
            }],
        }
        .encode(),
    ];
    // Each log has its own magic — a seed decodes only under its own type.
    round_trip_identity(&seeds[0]);
    round_trip_replica(&seeds[1]);
    round_trip_placement(&seeds[2]);
}

fn round_trip_identity(data: &[u8]) {
    let l = IdentityLog::decode(data).expect("the valid identity seed must decode");
    let enc = l.encode();
    let d = IdentityLog::decode(&enc).expect("re-decode of the encode must succeed");
    assert_eq!(
        d.encode(),
        enc,
        "identity log re-encode must be byte-identical"
    );
}

fn round_trip_replica(data: &[u8]) {
    let l = ReplicaLog::decode(data).expect("the valid replica seed must decode");
    let enc = l.encode();
    let d = ReplicaLog::decode(&enc).expect("re-decode of the encode must succeed");
    assert_eq!(
        d.encode(),
        enc,
        "replica log re-encode must be byte-identical"
    );
}

fn round_trip_placement(data: &[u8]) {
    let l = PlacementLog::decode(data).expect("the valid placement seed must decode");
    let enc = l.encode();
    let d = PlacementLog::decode(&enc).expect("re-decode of the encode must succeed");
    assert_eq!(
        d.encode(),
        enc,
        "placement log re-encode must be byte-identical"
    );
}

/// FZ-07 — the legacy v1 WAL envelope parser and the snapshot marker:
/// `parse_at`'s accepted `Complete` stays inside the input; an accepted
/// marker re-encodes byte-identically.
pub fn check_envelope_snapshot(data: &[u8]) {
    if let Ok(ParseOutcome::Complete { end, .. }) = legacy_envelope::parse_at(data, 0) {
        assert!(
            end <= data.len(),
            "the record end must not exceed the input"
        );
    }
    if let Ok(m) = SnapshotMarker::decode(data) {
        let enc = m.encode();
        let d = SnapshotMarker::decode(&enc).expect("re-decode of the encode must succeed");
        assert_eq!(
            d.encode(),
            enc,
            "snapshot marker re-encode must be byte-identical"
        );
    }
    let record = legacy_envelope::encode_record(legacy_envelope::TYPE_BATCH, b"payload");
    round_trip_envelope(&record);
    let marker = SnapshotMarker {
        format_version: format::FORMAT_VERSION,
        generation: 1,
        files: vec![SnapshotFile {
            name: "a".into(),
            size: 1,
            checksum: [0; 8],
        }],
    }
    .encode();
    round_trip_marker(&marker);
}

fn round_trip_envelope(data: &[u8]) {
    match legacy_envelope::parse_at(data, 0).expect("the valid seed must parse") {
        ParseOutcome::Complete { end, .. } => {
            assert_eq!(end, data.len(), "one record, whole input");
        }
        ParseOutcome::TornTail => panic!("a whole record must not read as a torn tail"),
    }
}

fn round_trip_marker(data: &[u8]) {
    let m = SnapshotMarker::decode(data).expect("the valid seed must decode");
    let enc = m.encode();
    let d = SnapshotMarker::decode(&enc).expect("re-decode of the encode must succeed");
    assert_eq!(
        d.encode(),
        enc,
        "snapshot marker re-encode must be byte-identical"
    );
}
