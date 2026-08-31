//! MVP-QA-002 Suite E — schema evolution (QA2-SCHEMA-002, -006, -007).
//!
//! SCHEMA-001/003/004/005 are already covered (t06zt/t06zw/t06zx-t06zzf:
//! v1→v2 readability, rollback, old/new reader semantics, idempotency).
//! These close the three remaining rows:
//! - SCHEMA-002: the full v2 → v3 chain — additive and incompatible
//!   changes separately, per the documented migration rules.
//! - SCHEMA-006: a migration must commit the schema row and every data row
//!   in ONE engine batch — a kill mid-migration leaves valid pre OR post
//!   state, never a hybrid (new schema row + old data).
//! - SCHEMA-007: resume/restart — after reopen the persisted registry is
//!   reloaded and a re-applied migration is a no-op (or resumes cleanly).

use aikoql_kernel::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

fn mk(engine: Arc<MemoryEngine>) -> Kernel {
    Kernel::open(engine, Arc::new(ManualClock::new(10_000)), 0xC0FFEE).unwrap()
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn alice() -> Subject {
    Subject::new("alice")
}

fn item(k: &Kernel, name: &str) -> KOID {
    let mut r = RememberRequest::create(alice(), meta("Item"));
    r.properties.insert("name".into(), Value::Text(name.into()));
    k.remember(r).unwrap().koid
}

/// Wraps MemoryEngine and counts committed batches — the observable
/// boundary for SCHEMA-006: the schema row and the data rows must become
/// durable through a single atomic write.
struct CountingEngine {
    inner: MemoryEngine,
    batches: Arc<AtomicU64>,
}

impl CountingEngine {
    fn new() -> (Self, Arc<AtomicU64>) {
        let batches = Arc::new(AtomicU64::new(0));
        (
            CountingEngine {
                inner: MemoryEngine::new(),
                batches: batches.clone(),
            },
            batches,
        )
    }
}

impl StorageEngine for CountingEngine {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>> {
        self.inner.get(key)
    }
    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.scan(prefix)
    }
    fn write_batch(&self, batch: &WriteBatch) -> KResult<()> {
        self.inner.write_batch(batch)?;
        self.batches.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// QA2-SCHEMA-002 — v2 → v3 chain: additive and incompatible changes
// ---------------------------------------------------------------------------

#[test]
fn w2_schema_002_v2_to_v3_chain_additive_then_incompatible() {
    let engine = Arc::new(MemoryEngine::new());
    let k = mk(engine);
    k.register_schema(Schema::new("Item", 1).required_property("name", "Text"))
        .unwrap();
    let id1 = item(&k, "sword");
    let id2 = item(&k, "shield");

    // v1 → v2: ADDITIVE — a new optional property filled by default.
    let mig_v2 = SchemaMigration {
        schema: Schema::new("Item", 2)
            .required_property("name", "Text")
            .required_property("grade", "Text"),
        transforms: vec![PropertyTransform::SetDefault {
            property: "grade".into(),
            value: Value::Text("standard".into()),
        }],
    };
    let report = k.apply_schema_migration(&alice(), &mig_v2).unwrap();
    assert_eq!((report.scanned, report.migrated), (2, 2));
    for id in [id1, id2] {
        let ko = k.get(alice(), &id).unwrap();
        assert_eq!(ko.metadata.schema_version, 2);
        assert_eq!(
            ko.properties.get("grade"),
            Some(&Value::Text("standard".into())),
            "additive change fills the default on every existing KO"
        );
    }

    // v2 → v3: INCOMPATIBLE — a property rename (data must be transformed,
    // never silently dropped or misread).
    let mig_v3 = SchemaMigration {
        schema: Schema::new("Item", 3)
            .required_property("label", "Text")
            .required_property("grade", "Text"),
        transforms: vec![PropertyTransform::Rename {
            from: "name".into(),
            to: "label".into(),
        }],
    };
    let report = k.apply_schema_migration(&alice(), &mig_v3).unwrap();
    assert_eq!((report.scanned, report.migrated), (2, 2));
    for id in [id1, id2] {
        let ko = k.get(alice(), &id).unwrap();
        assert_eq!(ko.metadata.schema_version, 3);
        assert!(
            ko.properties.contains_key("label"),
            "incompatible change applied via transform, old property gone"
        );
        assert!(!ko.properties.contains_key("name"));
    }
    let ko1 = k.get(alice(), &id1).unwrap();
    assert_eq!(
        ko1.properties.get("label"),
        Some(&Value::Text("sword".into())),
        "v3 data still reads the v1 value through the chain of transforms"
    );

    // The whole chain is one append-only lineage per KO: v1, v2, v3.
    let lineage = k.trace(alice(), &id1).unwrap();
    assert_eq!(lineage.versions.len(), 3);
    assert!(k.prove(alice(), &id1).unwrap().chain_valid);

    // An incompatible change WITHOUT a transform must fail cleanly — the
    // documented rule is fail-closed, never silent reinterpretation.
    let bad = SchemaMigration {
        schema: Schema::new("Item", 4).required_property("title", "Text"),
        transforms: vec![],
    };
    assert!(
        k.apply_schema_migration(&alice(), &bad).is_err(),
        "incompatible change without a transform must be rejected"
    );
    let ko = k.get(alice(), &id1).unwrap();
    assert_eq!(
        ko.metadata.schema_version, 3,
        "a rejected migration leaves every stamp untouched"
    );
}

// ---------------------------------------------------------------------------
// QA2-SCHEMA-006 — interrupted migration: one atomic batch, never a hybrid
// ---------------------------------------------------------------------------

#[test]
fn w2_schema_006_migration_commits_schema_and_data_in_one_atomic_batch() {
    let (engine, batches) = CountingEngine::new();
    let k = Kernel::open(
        Arc::new(engine),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap();
    k.register_schema(Schema::new("Item", 1).required_property("name", "Text"))
        .unwrap();
    let id1 = item(&k, "sword");
    let id2 = item(&k, "shield");
    let before = batches.load(Ordering::SeqCst);

    let mig = SchemaMigration {
        schema: Schema::new("Item", 2)
            .required_property("name", "Text")
            .property("grade", "Text"),
        transforms: vec![PropertyTransform::SetDefault {
            property: "grade".into(),
            value: Value::Text("standard".into()),
        }],
    };
    let report = k.apply_schema_migration(&alice(), &mig).unwrap();
    assert_eq!((report.scanned, report.migrated), (2, 2));

    assert_eq!(
        batches.load(Ordering::SeqCst) - before,
        1,
        "SCHEMA-006: the schema row and every data row must commit in ONE \
         engine batch — two batches leave a kill window where the new \
         schema row coexists with old-version data (hybrid state)"
    );
    for id in [id1, id2] {
        let ko = k.get(alice(), &id).unwrap();
        assert_eq!(ko.metadata.schema_version, 2);
    }
}

#[test]
fn w2_schema_006_failed_migration_commits_nothing() {
    let (engine, batches) = CountingEngine::new();
    let k = Kernel::open(
        Arc::new(engine),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap();
    k.register_schema(Schema::new("Item", 1).required_property("name", "Text"))
        .unwrap();
    let id1 = item(&k, "sword");
    let before = batches.load(Ordering::SeqCst);

    // v2 requires a property no transform can provide → pre-validation
    // rejects the migration before anything is written.
    let bad = SchemaMigration {
        schema: Schema::new("Item", 2).required_property("label", "Text"),
        transforms: vec![],
    };
    assert!(k.apply_schema_migration(&alice(), &bad).is_err());

    assert_eq!(
        batches.load(Ordering::SeqCst),
        before,
        "a failed migration must commit zero batches — no schema row, no \
         data row, no rollback writes"
    );

    // Pre-migration state fully intact: data stamps unchanged, and the
    // registry still enforces the OLD schema (a KO missing the new
    // required property is still accepted).
    let ko = k.get(alice(), &id1).unwrap();
    assert_eq!(ko.metadata.schema_version, 1);
    let id3 = item(&k, "axe");
    assert_eq!(k.get(alice(), &id3).unwrap().metadata.schema_version, 1);

    // The corrected migration (with a transform) then applies cleanly.
    let good = SchemaMigration {
        schema: Schema::new("Item", 2).required_property("label", "Text"),
        transforms: vec![PropertyTransform::Rename {
            from: "name".into(),
            to: "label".into(),
        }],
    };
    let report = k.apply_schema_migration(&alice(), &good).unwrap();
    assert_eq!(report.migrated, 2, "id3 pre-dates the good migration too");
}

// ---------------------------------------------------------------------------
// QA2-SCHEMA-007 — migration recovery: resume / restart semantics
// ---------------------------------------------------------------------------

#[test]
fn w2_schema_007_restart_reloads_registry_and_reapply_is_noop() {
    let engine = Arc::new(MemoryEngine::new());
    let k = mk(engine.clone());
    k.register_schema(Schema::new("Item", 1).required_property("name", "Text"))
        .unwrap();
    let id1 = item(&k, "sword");
    let id2 = item(&k, "shield");

    let mig_v2 = SchemaMigration {
        schema: Schema::new("Item", 2)
            .required_property("name", "Text")
            .required_property("grade", "Text"),
        transforms: vec![PropertyTransform::SetDefault {
            property: "grade".into(),
            value: Value::Text("standard".into()),
        }],
    };
    k.apply_schema_migration(&alice(), &mig_v2).unwrap();
    let v_after = k.get(alice(), &id1).unwrap().version;

    // "Process restart": reopen on the same store. The registry must be
    // reloaded from the persisted schema rows (REC-002).
    drop(k);
    let k2 = mk(engine.clone());

    // Resume: re-applying the identical migration is a pure no-op — no
    // version churn, no data reinterpretation.
    let report = k2.apply_schema_migration(&alice(), &mig_v2).unwrap();
    assert_eq!(
        (report.scanned, report.migrated, report.already_at_target),
        (2, 0, 2)
    );
    let ko = k2.get(alice(), &id1).unwrap();
    assert_eq!(ko.version, v_after);
    assert_eq!(ko.metadata.schema_version, 2);

    // And the chain continues from the reloaded state: v2 → v3 after restart.
    let mig_v3 = SchemaMigration {
        schema: Schema::new("Item", 3)
            .required_property("label", "Text")
            .required_property("grade", "Text"),
        transforms: vec![PropertyTransform::Rename {
            from: "name".into(),
            to: "label".into(),
        }],
    };
    let report = k2.apply_schema_migration(&alice(), &mig_v3).unwrap();
    assert_eq!(report.migrated, 2);
    let ko = k2.get(alice(), &id2).unwrap();
    assert_eq!(ko.metadata.schema_version, 3);
    assert_eq!(
        ko.properties.get("label"),
        Some(&Value::Text("shield".into()))
    );

    // A failed attempt before restart also leaves a resumable pre-state.
    let bad = SchemaMigration {
        schema: Schema::new("Item", 4).required_property("title", "Text"),
        transforms: vec![],
    };
    assert!(k2.apply_schema_migration(&alice(), &bad).is_err());
    drop(k2);
    let k3 = mk(engine.clone());
    let mig_v4 = SchemaMigration {
        schema: Schema::new("Item", 4)
            .required_property("title", "Text")
            .required_property("grade", "Text"),
        transforms: vec![PropertyTransform::Rename {
            from: "label".into(),
            to: "title".into(),
        }],
    };
    let report = k3.apply_schema_migration(&alice(), &mig_v4).unwrap();
    assert_eq!(
        report.migrated, 2,
        "restart after a failed attempt resumes at v3"
    );
    assert_eq!(k3.get(alice(), &id1).unwrap().metadata.schema_version, 4);
}
