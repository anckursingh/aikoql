//! PR6-003 — the review's P0 Placement finding: the checkpoint must
//! preserve SEMANTIC state, not just decode success. The invariant under
//! proof is
//!
//! ```text
//! live placement map == checkpoint/recovered placement map
//! ```
//!
//! field for field — segment id, block id, entry offset, generation,
//! retired generation, memtable generation. One reusable helper
//! (`assert_directory_equivalent`) compares full directory snapshots
//! (identity + replica + placement maps — `Db::directory_snapshot`, the
//! checkpoint builder's own view), then each placement variant round-trips
//! through checkpoint + reopen, and a mixed state survives checkpoint +
//! prune.

mod common;

use aikoql_storage_v2::checkpoint::DirectoryCheckpoint;
use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::identity::ObjectId;
use aikoql_storage_v2::placement::{Placement, PlacementRecord};
use common::dir;
use std::path::Path;

fn cfg(path: &Path) -> Config {
    let mut c = Config::new(path.to_path_buf());
    c.checkpoint_bytes = 0; // explicit checkpoint_now() only — determinism
    c.compact_background = false; // compact() stays synchronous
    c
}

fn oid(byte: u8) -> ObjectId {
    ObjectId([byte; 16])
}

/// PR6-003 — the one reusable comparison helper: the live directory and
/// the recovered one must be semantically identical, field for field.
/// Every variant field the review lists (segment id, block id, entry
/// offset, generation, retired/memtable generation) rides Placement's
/// PartialEq — decode success is NOT enough.
fn assert_directory_equivalent(before: &DirectoryCheckpoint, after: &DirectoryCheckpoint) {
    assert_eq!(
        before.identities, after.identities,
        "identity map diverged across checkpoint/reopen"
    );
    assert_eq!(
        before.replicas, after.replicas,
        "replica map diverged across checkpoint/reopen"
    );
    assert_eq!(
        before.placements, after.placements,
        "placement map diverged across checkpoint/reopen"
    );
}

/// Reopen on the same directory (the caller's Db is already dropped —
/// the lock file forbids two opens) and assert the snapshot survived.
fn reopen_and_assert(d: &Path, before: DirectoryCheckpoint) {
    let after = Db::open(cfg(d)).unwrap();
    assert_directory_equivalent(&before, &after.directory_snapshot());
}

/// The delta-log file count across all three families — the prune proof
/// (a checkpointed directory holds no subsumed history).
fn delta_log_count(d: &Path) -> usize {
    ["IDENTITY-", "REPLICA-", "PLACEMENT-"]
        .iter()
        .map(|stem| {
            std::fs::read_dir(d)
                .unwrap()
                .flatten()
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with(stem) && n.ends_with(".log")
                })
                .count()
        })
        .sum()
}

#[test]
fn memtable_placement_round_trips() {
    // §14 — a never-flushed replica stays Memtable; the generation the
    // live apply allocated must be the one recovered (it rode the
    // checkpoint, and the WAL replay must not displace it).
    let d = dir("pr6-003-memtable");
    let before = {
        let db = Db::open(cfg(&d)).unwrap();
        let a = oid(0xA1);
        db.put_object(a, b"k", b"v").unwrap();
        db.checkpoint_now().unwrap();
        let snap = db.directory_snapshot();
        assert!(matches!(
            snap.placements.as_slice(),
            [PlacementRecord {
                placement: Placement::Memtable { .. },
                ..
            }]
        ));
        snap
    };
    reopen_and_assert(&d, before);
}

#[test]
fn segment_placement_round_trips() {
    // §25 — flush anchors the replica: segment id, block id, entry offset
    // and generation all round-trip.
    let d = dir("pr6-003-segment");
    let before = {
        let db = Db::open(cfg(&d)).unwrap();
        let a = oid(0xA2);
        db.put_object(a, b"k", b"v").unwrap();
        db.flush().unwrap();
        db.checkpoint_now().unwrap();
        let snap = db.directory_snapshot();
        assert!(matches!(
            snap.placements.as_slice(),
            [PlacementRecord { placement: Placement::Segment(loc), .. }]
                if loc.segment_id.0 > 0 && loc.generation > 0
        ));
        snap
    };
    reopen_and_assert(&d, before);
}

#[test]
fn retired_placement_round_trips() {
    // §16 — compaction dropped the replica's last live entry (cp007's
    // recipe): Retired must survive checkpoint + prune + reopen — the
    // pruned history is the only other place the record ever was.
    let d = dir("pr6-003-retired");
    let before = {
        let db = Db::open(cfg(&d)).unwrap();
        let a = oid(0xA3);
        db.put_object(a, b"k", b"v1").unwrap();
        db.flush().unwrap();
        db.put_object(a, b"k", b"v2").unwrap();
        db.flush().unwrap();
        db.delete_object(a, b"k").unwrap();
        db.flush().unwrap();
        db.compact().unwrap();
        assert!(matches!(
            db.directory_snapshot().placements.as_slice(),
            [PlacementRecord {
                placement: Placement::Retired { .. },
                ..
            }]
        ));
        db.checkpoint_now().unwrap();
        let snap = db.directory_snapshot();
        assert_eq!(delta_log_count(&d), 0, "the delta history must be pruned");
        snap
    };
    reopen_and_assert(&d, before);
}

#[test]
fn mixed_placement_state_round_trips_after_prune() {
    // All three variants in one live map (a: Segment via flush, b:
    // Retired via cp007's recipe, c: Memtable — written after the
    // compaction, so its WAL frame replays on reopen), then checkpoint +
    // prune + reopen: the whole mixed state must come back identical.
    let d = dir("pr6-003-mixed");
    let before = {
        let db = Db::open(cfg(&d)).unwrap();
        let a = oid(0xB1);
        let b = oid(0xB2);
        let c = oid(0xB3);
        db.put_object(a, b"k", b"v").unwrap();
        db.put_object(b, b"k", b"v1").unwrap();
        db.flush().unwrap();
        db.put_object(b, b"k", b"v2").unwrap();
        db.flush().unwrap();
        db.delete_object(b, b"k").unwrap();
        db.flush().unwrap();
        db.compact().unwrap();
        db.put_object(c, b"k", b"v").unwrap();
        db.checkpoint_now().unwrap();
        let snap = db.directory_snapshot();
        assert!(snap
            .placements
            .iter()
            .any(|r| matches!(r.placement, Placement::Memtable { .. })));
        assert!(snap
            .placements
            .iter()
            .any(|r| matches!(r.placement, Placement::Segment(_))));
        assert!(snap
            .placements
            .iter()
            .any(|r| matches!(r.placement, Placement::Retired { .. })));
        assert_eq!(delta_log_count(&d), 0, "the delta history must be pruned");
        snap
    };
    reopen_and_assert(&d, before);
}
