//! L-10 (TDD-025) — placement direct-read equivalence: three read paths
//! must agree on every object row — the object read (identity directories),
//! the placement (rid → PhysicalLocation), and a DIRECT read of the anchor
//! (segment file → block → entry_offset) — through flush, a second flush
//! that moves the anchor, compaction (relocation), checkpoint, and reopen.
//! The anchor is the rid's max-seq surviving entry (§21–25): the direct
//! read must be exactly the row the object read answers.

mod common;

use aikoql_storage_v2::db::{segment_path, Config, Db};
use aikoql_storage_v2::identity::directory::{IdentityResolver, LocalIdentityDirectory};
use aikoql_storage_v2::identity::topology::{LocalReplicaDirectory, ReplicaDirectory};
use aikoql_storage_v2::identity::{ObjectId, ReplicaId};
use aikoql_storage_v2::placement::directory::{
    LocalPlacementResolver, Placement, PlacementResolver,
};
use aikoql_storage_v2::segment::{SegmentEntry, SegmentReader};
use common::dir;

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

/// The placement's anchor read DIRECT: the segment file at the recorded
/// (block, offset) — no directories, no db.
fn direct_read(dir: &std::path::Path, loc: &impl Fn() -> Option<Placement>) -> SegmentEntry {
    let Placement::Segment(loc) = loc().expect("the placement must be a Segment here") else {
        panic!("a Segment placement is required for a direct read");
    };
    SegmentReader::open(&segment_path(dir, loc.segment_id.0))
        .unwrap()
        .entry_at(loc.block_id, loc.entry_offset)
        .unwrap()
        .expect("the anchor names an entry")
}

/// The full three-way agreement for every object at this stage: the
/// direct anchor read must equal the object read of the object's max-seq
/// key, byte-exact — and every key's object read must hold its value.
fn assert_three_way(
    db: &Db,
    dir: &std::path::Path,
    objects: &[ObjectId],
    rows: &[(ObjectId, &'static [u8], &'static [u8])],
) {
    for &a in objects {
        let rid = rid_of(db, a);
        let placement = placement_of(db, rid);
        let entry = direct_read(dir, &|| placement);
        assert_eq!(entry.replica_id, rid, "the anchor carries the right rid");
        // The anchor is the object's max-seq row — the object read of
        // exactly that key answers the anchor's value, byte-exact.
        let key = entry.key.clone();
        let got = db
            .get_object(a, &key)
            .unwrap()
            .unwrap_or_else(|| panic!("anchor key {:?} unreadable", String::from_utf8_lossy(&key)));
        assert_eq!(got, entry.value, "direct read == object read");
        // Every other row agrees through the object path.
        for (oid, k, v) in rows {
            if *oid == a {
                assert_eq!(
                    db.get_object(*oid, k).unwrap().as_deref(),
                    Some(*v),
                    "row {:?} diverged",
                    String::from_utf8_lossy(k)
                );
            }
        }
    }
}

#[test]
fn object_placement_and_direct_read_agree_through_every_stage() {
    let d = dir("placement-equiv");
    let db = Db::open(Config::new(d.clone())).unwrap();
    let a = oid(0xA1);
    let b = oid(0xB2);
    let c = oid(0xC3);
    let objects = [a, b, c];

    // Stage 0 — memtable: the placement says Memtable (no direct surface
    // exists yet; the anchor materializes at the first flush).
    db.put_object(a, b"a1", b"va1").unwrap();
    db.put_object(b, b"b1", b"vb1").unwrap();
    db.put_object(c, b"c1", b"vc1").unwrap();
    for &o in &objects {
        assert!(
            matches!(
                placement_of(&db, rid_of(&db, o)),
                Some(Placement::Memtable { .. })
            ),
            "pre-flush placements are Memtable"
        );
    }
    let mut rows = vec![
        (a, b"a1".as_ref(), b"va1".as_ref()),
        (b, b"b1".as_ref(), b"vb1".as_ref()),
        (c, b"c1".as_ref(), b"vc1".as_ref()),
    ];

    // Stage 1 — flush: placements land on the flushed segment.
    db.flush().unwrap();
    assert_three_way(&db, &d, &objects, &rows);

    // Stage 2 — a second flush moves the anchors: newer max-seq rows.
    db.put_object(a, b"a2", b"va2").unwrap();
    rows.push((a, b"a2".as_ref(), b"va2".as_ref()));
    db.flush().unwrap();
    assert_three_way(&db, &d, &objects, &rows);

    // Stage 3 — compaction relocates: the anchors live in the merged L1.
    db.compact().unwrap();
    assert_three_way(&db, &d, &objects, &rows);

    // Stage 4 — checkpoint: the placement rides the directory checkpoint.
    db.checkpoint_now().unwrap();
    assert_three_way(&db, &d, &objects, &rows);

    // Stage 5 — reopen: the same agreement from the recovered state.
    drop(db);
    let db = Db::open(Config::new(d.clone())).unwrap();
    assert_three_way(&db, &d, &objects, &rows);
}
