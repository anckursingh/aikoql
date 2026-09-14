//! P5-M7 (ND-09) — database catalog. ct001–ct006.
//!
//! The catalog is the database's own metadata, stored as ordinary journaled
//! KOs in the SAME engine: reserved row type `aikoql:catalog`, reserved
//! tenant `aikoql:catalog`, written as the system principal (`aikoql:system`)
//! whose default owner-only ACL keeps catalog rows invisible to every normal
//! subject. One version row (kind `version`, name `catalog`,
//! `version: Int`) drives deterministic migrations at open; a corrupt or
//! unsupported version fails the open closed.
//!
//! Observable contract pinned here (the implementation must satisfy it):
//! - ct001 create/drop type round-trips across restart (type sugar + the
//!   generic entry API); the reserved row type never leaks into `list_types`
//! - ct002 schema evolution: add_property persists across restart; rows
//!   written before the change read back unchanged, new rows see the
//!   extended schema
//! - ct003 a corrupt version row (version not an Int) fails the open closed
//! - ct004 concurrent metadata changes serialize through the kernel's
//!   single-writer commit path — no lost entries, no torn rows
//! - ct005 ensure is deterministic (byte-stable version row) and idempotent
//!   (a reopen changes nothing)
//! - ct006 version compatibility: a pre-catalog database opens and
//!   re-initializes v1 without touching user data; a catalog version above
//!   what this build supports fails closed

use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use std::sync::Arc;

const CATALOG_TYPE: &str = "aikoql:catalog";
const CATALOG_TENANT: &str = "aikoql:catalog";
const SYSTEM: &str = "aikoql:system";

// --- helpers ------------------------------------------------------------------

fn mk() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
}

/// A kernel whose engine outlives it — the restart harness.
fn mk_shared() -> (Kernel, Arc<dyn StorageEngine>) {
    let engine: Arc<dyn StorageEngine> = Arc::new(MemoryEngine::new());
    let k = Kernel::open(
        Arc::clone(&engine),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap();
    (k, engine)
}

/// Restart the engine. The clock advances across a restart — a real wall
/// clock never rewinds — and koids are time-derived, so a same-time reopen
/// would regenerate the original koid sequence and collide on writes.
fn reopen(engine: &Arc<dyn StorageEngine>) -> KResult<Kernel> {
    Kernel::open(
        Arc::clone(engine),
        Arc::new(ManualClock::new(20_000)),
        0xC0FFEE,
    )
}

fn sys() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new(SYSTEM).in_tenant(CATALOG_TENANT))
}

fn alice() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new("alice"))
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

/// Write a raw catalog row through the ordinary remember path, the way a
/// corrupt or foreign tool could — the fail-closed pins need one. Returns
/// the row's koid (ct006b pins the in-place stamp on it).
fn raw_catalog_row(k: &Kernel, props: PropertyMap) -> KOID {
    let mut req = RememberRequest::create(
        sys(),
        Metadata {
            type_name: CATALOG_TYPE.into(),
            tenant: Some(CATALOG_TENANT.into()),
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties = props;
    k.remember(req).unwrap().koid
}

// --- ct001 — type create/drop round-trips across restart ------------------------

#[test]
fn ct001_type_create_drop_roundtrips_across_restart() {
    let (k, engine) = mk_shared();
    assert_eq!(k.catalog_version().unwrap(), 1, "fresh open initializes v1");
    k.catalog_create_type("Employee", &["name", "dept_id"])
        .unwrap();
    k.catalog_create_type("Department", &["id"]).unwrap();
    assert_eq!(
        k.catalog_list_types().unwrap(),
        vec!["Department", "Employee"],
        "types listed sorted"
    );
    assert_eq!(
        k.catalog_get_type("Employee").unwrap(),
        Some(vec!["name".into(), "dept_id".into()]),
        "property order preserved"
    );
    assert!(
        k.catalog_create_type("Employee", &["x"]).is_err(),
        "duplicate create fails closed"
    );
    // The generic entry API round-trips alongside the type sugar.
    let mut props = PropertyMap::new();
    props.insert("target".into(), Value::Text("v2".into()));
    k.catalog_create_entry("index", "emp_name", props.clone())
        .unwrap();
    assert_eq!(k.catalog_entry("index", "emp_name").unwrap(), Some(props));

    // Restart: everything persists.
    let k2 = reopen(&engine).unwrap();
    assert_eq!(k2.catalog_version().unwrap(), 1);
    assert_eq!(
        k2.catalog_list_types().unwrap(),
        vec!["Department", "Employee"]
    );
    assert_eq!(
        k2.catalog_get_type("Employee").unwrap(),
        Some(vec!["name".into(), "dept_id".into()])
    );
    assert!(k2.catalog_entry("index", "emp_name").unwrap().is_some());

    // Drop round-trips too; dropping an unknown entry fails closed.
    k2.catalog_drop_type("Department").unwrap();
    assert_eq!(k2.catalog_get_type("Department").unwrap(), None);
    assert!(k2.catalog_drop_type("Department").is_err());
    k2.catalog_drop_entry("index", "emp_name").unwrap();
    let k3 = reopen(&engine).unwrap();
    assert_eq!(k3.catalog_list_types().unwrap(), vec!["Employee"]);
    assert!(k3.catalog_entry("index", "emp_name").unwrap().is_none());

    // The reserved catalog row type never leaks into the user-facing list.
    assert!(
        !k3.list_types().unwrap().iter().any(|t| t == CATALOG_TYPE),
        "catalog rows are not user types"
    );
}

// --- ct002 — schema evolution -----------------------------------------------------

#[test]
fn ct002_add_property_persists_and_old_rows_read_unchanged() {
    let (k, engine) = mk_shared();
    k.catalog_create_type("Employee", &["name"]).unwrap();
    // A row written under the v1 schema.
    let mut req = RememberRequest::create(alice(), meta("Employee"));
    req.properties
        .insert("name".into(), Value::Text("A".into()));
    let old = k.remember(req).unwrap().koid;

    k.catalog_add_property("Employee", "dept_id").unwrap();
    assert_eq!(
        k.catalog_get_type("Employee").unwrap(),
        Some(vec!["name".into(), "dept_id".into()])
    );
    // New rows see the extended schema (write + read with the new property).
    let mut req = RememberRequest::create(alice(), meta("Employee"));
    req.properties
        .insert("name".into(), Value::Text("B".into()));
    req.properties.insert("dept_id".into(), Value::Int(7));
    k.remember(req).unwrap();
    let rows = k.scan_by_type(&Subject::new("alice"), "Employee").unwrap();
    assert_eq!(rows.len(), 2);
    let old_row = rows
        .iter()
        .find(|r| r.koid == old)
        .expect("old row readable");
    assert_eq!(
        old_row.properties.len(),
        1,
        "the pre-evolution row is untouched"
    );

    // Restart: the extended schema persists.
    let k2 = reopen(&engine).unwrap();
    assert_eq!(
        k2.catalog_get_type("Employee").unwrap(),
        Some(vec!["name".into(), "dept_id".into()])
    );
}

// --- ct003 — corruption fails the open closed ---------------------------------------

#[test]
fn ct003_corrupt_version_row_fails_open_closed() {
    let (k, engine) = mk_shared();
    // Corrupt the catalog: a version row whose `version` is not an Int.
    let mut props = PropertyMap::new();
    props.insert("kind".into(), Value::Text("version".into()));
    props.insert("name".into(), Value::Text("catalog".into()));
    props.insert("version".into(), Value::Text("garbage".into()));
    raw_catalog_row(&k, props);
    let err = match reopen(&engine) {
        Ok(_) => panic!("a corrupt catalog version must fail the open"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("catalog"),
        "the error names the catalog: {msg}"
    );
}

// --- ct004 — concurrent metadata changes serialize ----------------------------------

#[test]
fn ct004_concurrent_catalog_writes_serialize_without_loss() {
    let k = mk();
    let kref = &k;
    std::thread::scope(|s| {
        for i in 0..8 {
            s.spawn(move || {
                kref.catalog_create_type(&format!("T{i}"), &["p"]).unwrap();
            });
        }
    });
    let types = k.catalog_list_types().unwrap();
    assert_eq!(types.len(), 8, "no lost entries: {types:?}");
    for i in 0..8 {
        assert_eq!(
            k.catalog_get_type(&format!("T{i}")).unwrap(),
            Some(vec!["p".into()]),
            "no torn rows"
        );
    }
}

// --- ct005 — ensure deterministic + idempotent ---------------------------------------

#[test]
fn ct005_ensure_is_deterministic_and_idempotent() {
    let (k, engine) = mk_shared();
    let v1 = k
        .catalog_entry("version", "catalog")
        .unwrap()
        .expect("version row exists after init");
    let k2 = reopen(&engine).unwrap();
    assert_eq!(
        k2.catalog_entry("version", "catalog").unwrap(),
        Some(v1.clone()),
        "reopen leaves the version row byte-stable"
    );
    assert_eq!(k2.catalog_version().unwrap(), 1);
    // The fresh catalog row set is deterministic: exactly the version row.
    // Catalog rows are canonical-only (never in the derived type index), so
    // the observable is the journal — a second open must add no events.
    assert_eq!(
        k2.journal().unwrap().len(),
        1,
        "ensure adds nothing on a second run"
    );
}

// --- ct006 — version compatibility ----------------------------------------------------

#[test]
fn ct006_version_compatibility() {
    // (a) A pre-catalog database opens, re-initializes v1, and never
    // silently rewrites user data.
    {
        let (k, engine) = mk_shared();
        let mut req = RememberRequest::create(alice(), meta("Employee"));
        req.properties
            .insert("name".into(), Value::Text("A".into()));
        k.remember(req).unwrap();
        k.catalog_drop_entry("version", "catalog").unwrap(); // pre-M7 DB
        let k2 = reopen(&engine).unwrap();
        assert_eq!(k2.catalog_version().unwrap(), 1, "re-initialized at v1");
        let rows = k2.scan_by_type(&Subject::new("alice"), "Employee").unwrap();
        assert_eq!(rows.len(), 1, "user data untouched by the re-init");
    }

    // (b) A database stamped at an old catalog version migrates in one
    // open: the dispatch runs, the stamp updates the row in place — never
    // duplicates it.
    {
        let (k, engine) = mk_shared();
        k.catalog_drop_entry("version", "catalog").unwrap();
        let mut oldv = PropertyMap::new();
        oldv.insert("kind".into(), Value::Text("version".into()));
        oldv.insert("name".into(), Value::Text("catalog".into()));
        oldv.insert("version".into(), Value::Int(0));
        let v0_koid = raw_catalog_row(&k, oldv);
        let k2 = reopen(&engine).unwrap();
        assert_eq!(k2.catalog_version().unwrap(), 1, "migrated v0 → v1");
        // Catalog rows are canonical-only (never in the derived type index),
        // so "no duplicate row" is pinned on the journal: the migration's
        // only event is an UPDATE of the v0 row's own koid, not a create.
        let journal = k2.journal().unwrap();
        assert_eq!(journal.len(), 4, "one stamp event, nothing else");
        let stamp = journal.last().unwrap();
        assert_eq!(stamp.kind, EventKind::Updated, "stamp is an update");
        assert_eq!(stamp.koid, v0_koid, "the stamp updated in place");
    }

    // (c) A catalog version above what this build supports fails closed.
    {
        let (k, engine) = mk_shared();
        let mut props = PropertyMap::new();
        props.insert("kind".into(), Value::Text("version".into()));
        props.insert("name".into(), Value::Text("catalog".into()));
        props.insert("version".into(), Value::Int(999));
        raw_catalog_row(&k, props);
        let err = match reopen(&engine) {
            Ok(_) => panic!("an unsupported catalog version must fail the open"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(msg.contains("999"), "the error names the version: {msg}");
    }
}
