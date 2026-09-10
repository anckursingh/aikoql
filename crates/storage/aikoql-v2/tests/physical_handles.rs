//! P4-M2 — physical handle Db integration (TDD-ID-001): the handle layer
//! over the real directory — compaction flips handles while `ReplicaId`
//! stays put, and a pre-relocation handle fails closed afterwards. The
//! assertions branch on whether the merge actually relocated each replica,
//! so the test pins the contract either way.

mod common;

use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::format::FormatError;
use aikoql_storage_v2::identity::directory::{IdentityResolver, LocalIdentityDirectory};
use aikoql_storage_v2::identity::topology::{LocalReplicaDirectory, ReplicaDirectory};
use aikoql_storage_v2::identity::{ObjectId, ReplicaId};
use aikoql_storage_v2::placement::directory::{
    LocalPlacementResolver, Placement, PlacementResolver,
};
use aikoql_storage_v2::placement::{HandleRegistry, PhysicalHandle};
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

#[test]
fn phy005_db_compaction_flips_handles_not_replicas() {
    let d = dir("phy005");
    let db = Db::open(Config::new(d.clone())).unwrap();
    let a = oid(0xA1);
    let b = oid(0xB2);
    let c = oid(0xC3);
    db.put_object(a, b"a1", b"va1").unwrap();
    db.put_object(b, b"b1", b"vb1").unwrap();
    db.flush().unwrap();
    db.put_object(c, b"c1", b"vc1").unwrap();
    db.flush().unwrap();

    let rids = [rid_of(&db, a), rid_of(&db, b), rid_of(&db, c)];
    let pre: Vec<Placement> = rids
        .iter()
        .map(|r| placement_of(&db, *r).unwrap())
        .collect();

    let mut reg = HandleRegistry::new(LocalPlacementResolver::new(&db));
    let before: Vec<PhysicalHandle> = rids
        .iter()
        .map(|r| {
            reg.resolve(*r)
                .unwrap()
                .expect("segment placement after flush")
        })
        .collect();

    db.compact().unwrap();

    for (i, rid) in rids.iter().enumerate() {
        let now = placement_of(&db, *rid).expect("still placed after compact");
        let after = reg.resolve(*rid).unwrap().expect("handle after compact");
        if now.generation() != pre[i].generation() {
            // The merge relocated this replica — new generation, new handle,
            // and the old handle fails closed.
            assert_ne!(before[i], after, "relocation must flip the handle");
            assert!(
                matches!(reg.location(before[i]), Err(FormatError::Stale(_))),
                "pre-relocation handle must not resolve post-relocation"
            );
        } else {
            // Placement untouched — the handle stays stable.
            assert_eq!(before[i], after, "stable generation, stable handle");
        }
        // The current handle always resolves to the Db's current location.
        match now {
            Placement::Segment(loc) => assert_eq!(reg.location(after).unwrap(), loc),
            other => panic!("expected segment placement, got {other:?}"),
        }
    }
}
