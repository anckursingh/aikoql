//! L-09 (TDD-019) — the per-replica winner matrix: a key is a shared byte
//! namespace (compaction.rs — SE2-M38), so the merge's winner is per
//! (key, rid): each rid's NEWEST entry survives, older same-rid versions
//! lose, rid-0 byte rows form one last-writer-wins group. Exact winners
//! through compaction for the 5-row interleave (the L-04 shape) in ONE
//! segment and across segments (the advance-first trap), plus the
//! tombstone / resurrect / Drop / Archive / Retired legs.

mod common;

use aikoql_storage_v2::compaction::{KeepAll, Retention, RetentionPolicy};
use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::identity::directory::{IdentityResolver, LocalIdentityDirectory};
use aikoql_storage_v2::identity::topology::{LocalReplicaDirectory, ReplicaDirectory};
use aikoql_storage_v2::identity::{ObjectId, ReplicaId};
use aikoql_storage_v2::placement::directory::{
    LocalPlacementResolver, Placement, PlacementResolver,
};
use aikoql_storage_v2::segment::SegmentReader;
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

/// The 5-row interleave on key `k` (byte rows rid-0, object rows rid 7/8)
/// plus a byte-only key `b` — memtable order, so seqs ascend per write.
/// Expected winners: rid0 = v3, rid7 = o7b, rid8 = o8, b = byte-only.
fn write_matrix(db: &Db) {
    let oid7 = oid(0x07);
    let oid8 = oid(0x08);
    db.put(b"k", b"v0").unwrap();
    db.put_object(oid7, b"k", b"o7a").unwrap();
    db.put(b"k", b"v3").unwrap();
    db.put_object(oid8, b"k", b"o8").unwrap();
    db.put_object(oid7, b"k", b"o7b").unwrap();
    db.put(b"b", b"byte-only").unwrap();
}

fn assert_winners(db: &Db) {
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"v3".as_ref()));
    assert_eq!(
        db.get(b"b").unwrap().as_deref(),
        Some(b"byte-only".as_ref())
    );
    assert_eq!(
        db.get_object(oid(0x07), b"k").unwrap().as_deref(),
        Some(b"o7b".as_ref())
    );
    assert_eq!(
        db.get_object(oid(0x08), b"k").unwrap().as_deref(),
        Some(b"o8".as_ref())
    );
}

/// Winner per (key, rid): every rid's newest entry survives, older
/// same-rid versions lose, the byte surface answers the rid-0 winner.
#[test]
fn winner_matrix_in_one_segment() {
    let d = dir("winner-one-seg");
    let db = Db::open(Config::new(d.clone())).unwrap();
    write_matrix(&db);
    db.flush().unwrap();
    // compact() skips a single segment (the PR#2 SE-05 advisory pre-check);
    // compact_with is the caller's explicit request, which this is.
    let stats = db.compact_with(&KeepAll).unwrap();
    assert_eq!(stats.entries_in, 6);
    assert_eq!(stats.entries_out, 4, "the exact 4 winners survive");
    assert_winners(&db);
    // Both replicas anchored at their surviving entries, not Retired.
    assert!(matches!(
        placement_of(&db, rid_of(&db, oid(0x07))),
        Some(Placement::Segment(_))
    ));
    assert!(matches!(
        placement_of(&db, rid_of(&db, oid(0x08))),
        Some(Placement::Segment(_))
    ));
    drop(db);
    let db = Db::open(Config::new(d)).unwrap();
    assert_winners(&db);
}

/// The advance-first trap (compaction.rs:215): a rid's winner lives in a
/// LATER segment than its loser — a version not yet in the heap must not
/// pop later as a fresh winner.
#[test]
fn winner_matrix_across_segments() {
    let d = dir("winner-two-seg");
    let db = Db::open(Config::new(d.clone())).unwrap();
    let oid7 = oid(0x07);
    let oid8 = oid(0x08);
    db.put(b"k", b"v0").unwrap();
    db.put_object(oid7, b"k", b"o7a").unwrap();
    db.put(b"k", b"v3").unwrap();
    db.put(b"b", b"byte-only").unwrap();
    db.flush().unwrap();
    db.put_object(oid8, b"k", b"o8").unwrap();
    db.put_object(oid7, b"k", b"o7b").unwrap();
    db.flush().unwrap();
    let stats = db.compact().unwrap();
    assert_eq!(stats.entries_in, 6);
    assert_eq!(stats.entries_out, 4, "one winner per rid across segments");
    assert_winners(&db);
    drop(db);
    let db = Db::open(Config::new(d)).unwrap();
    assert_winners(&db);
}

/// A tombstone is a winner only when it IS the rid's newest row: it kills
/// every older same-rid version and retires the replica — and nothing
/// else. A newer value written after it resurrects the row.
#[test]
fn tombstone_winner_retires_only_its_replica_and_can_resurrect() {
    let d = dir("winner-tomb");
    let db = Db::open(Config::new(d.clone())).unwrap();
    let oid7 = oid(0x07);
    let oid8 = oid(0x08);
    db.put(b"k", b"v3").unwrap();
    db.put_object(oid7, b"k", b"o7").unwrap();
    db.put_object(oid8, b"k", b"o8").unwrap();
    db.put(b"b", b"byte-only").unwrap();
    db.flush().unwrap();
    db.delete_object(oid8, b"k").unwrap();
    db.flush().unwrap();
    let rid7 = rid_of(&db, oid7);
    let rid8 = rid_of(&db, oid8);
    let stats = db.compact().unwrap();
    // The rid-8 tombstone wins rid-8's group; rid-0 and rid-7 untouched.
    assert_eq!(stats.entries_out, 3, "v3 + o7 + b");
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"v3".as_ref()));
    assert_eq!(
        db.get_object(oid7, b"k").unwrap().as_deref(),
        Some(b"o7".as_ref())
    );
    assert_eq!(db.get_object(oid8, b"k").unwrap(), None);
    assert!(matches!(
        placement_of(&db, rid7),
        Some(Placement::Segment(_))
    ));
    assert!(
        matches!(placement_of(&db, rid8), Some(Placement::Retired { .. })),
        "the tombstoned replica retires (§16)"
    );

    // A newer value resurrects: the tombstone was not the newest anymore.
    db.put_object(oid8, b"k", b"o8b").unwrap();
    db.flush().unwrap();
    let stats = db.compact().unwrap();
    assert_eq!(
        db.get_object(oid8, b"k").unwrap().as_deref(),
        Some(b"o8b".as_ref())
    );
    assert_eq!(stats.entries_out, 4, "v3 + o7 + o8b + b");
    assert!(
        matches!(placement_of(&db, rid8), Some(Placement::Segment(_))),
        "the resurrected replica re-anchors"
    );
}

/// A policy drop of a key drops EVERY replica's rows of that key — and
/// nothing else; every replica of the key retires.
#[test]
fn drop_policy_drops_every_replica_of_the_key() {
    struct DropKey;
    impl RetentionPolicy for DropKey {
        fn classify(&self, key: &[u8]) -> Retention {
            if key == b"k" {
                Retention::Drop
            } else {
                Retention::Keep
            }
        }
    }
    let d = dir("winner-drop");
    let db = Db::open(Config::new(d.clone())).unwrap();
    write_matrix(&db);
    db.flush().unwrap();
    let rid7 = rid_of(&db, oid(0x07));
    let rid8 = rid_of(&db, oid(0x08));
    let stats = db.compact_with(&DropKey).unwrap();
    assert_eq!(stats.entries_in, 6);
    assert_eq!(stats.entries_out, 1, "only b survives");
    assert_eq!(db.get(b"k").unwrap(), None);
    assert_eq!(db.get_object(oid(0x07), b"k").unwrap(), None);
    assert_eq!(db.get_object(oid(0x08), b"k").unwrap(), None);
    assert_eq!(
        db.get(b"b").unwrap().as_deref(),
        Some(b"byte-only".as_ref())
    );
    assert!(matches!(
        placement_of(&db, rid7),
        Some(Placement::Retired { .. })
    ));
    assert!(matches!(
        placement_of(&db, rid8),
        Some(Placement::Retired { .. })
    ));
}

fn archive_segments(d: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(d.join("archive"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("ARCHIVE-"))
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

/// An archive of a key holds EVERY version of EVERY replica, byte-exact
/// with rids intact (a v4 archive); the live space loses the key entirely
/// and both replicas retire.
#[test]
fn archive_policy_archives_every_version_with_rids() {
    struct ArchiveKey;
    impl RetentionPolicy for ArchiveKey {
        fn classify(&self, key: &[u8]) -> Retention {
            if key == b"k" {
                Retention::Archive
            } else {
                Retention::Keep
            }
        }
    }
    let d = dir("winner-archive");
    let db = Db::open(Config::new(d.clone())).unwrap();
    write_matrix(&db);
    db.flush().unwrap();
    let rid7 = rid_of(&db, oid(0x07));
    let rid8 = rid_of(&db, oid(0x08));
    let stats = db.compact_with(&ArchiveKey).unwrap();
    assert_eq!(stats.entries_archived, 5, "all five k rows archived");
    assert_eq!(stats.entries_out, 1, "only b survives live");
    assert_eq!(db.get(b"k").unwrap(), None);
    assert_eq!(db.get_object(oid(0x07), b"k").unwrap(), None);
    assert_eq!(db.get_object(oid(0x08), b"k").unwrap(), None);
    assert_eq!(
        db.get(b"b").unwrap().as_deref(),
        Some(b"byte-only".as_ref())
    );
    assert!(matches!(
        placement_of(&db, rid7),
        Some(Placement::Retired { .. })
    ));
    assert!(matches!(
        placement_of(&db, rid8),
        Some(Placement::Retired { .. })
    ));

    // The archive holds every version, byte-exact, rids intact.
    let files = archive_segments(&d);
    assert_eq!(files.len(), 1);
    let reader = SegmentReader::open(&files[0]).unwrap();
    assert_eq!(reader.entry_count(), 5);
    let r7 = rid_of(&db, oid(0x07)).0;
    let r8 = rid_of(&db, oid(0x08)).0;
    let mut got: Vec<(Vec<u8>, u64, u64, Vec<u8>)> = reader
        .scan(b"", b"~")
        .unwrap()
        .into_iter()
        .map(|e| (e.value, e.seq, e.replica_id.0, e.key))
        .collect();
    got.sort();
    let mut want: Vec<(Vec<u8>, u64, u64, Vec<u8>)> = vec![
        (b"v0".to_vec(), 1, 0, b"k".to_vec()),
        (b"o7a".to_vec(), 2, r7, b"k".to_vec()),
        (b"v3".to_vec(), 3, 0, b"k".to_vec()),
        (b"o8".to_vec(), 4, r8, b"k".to_vec()),
        (b"o7b".to_vec(), 5, r7, b"k".to_vec()),
    ];
    want.sort();
    assert_eq!(
        got, want,
        "the archive is the exact pre-compaction multiset"
    );
    // The per-rid surface still answers from the archive directly.
    assert_eq!(
        reader
            .get_by_rid(b"k", rid7)
            .unwrap()
            .expect("rid-7 row in the archive")
            .value,
        b"o7b"
    );
}
