//! v0.3 K1b — Evidence + authority/scope wired into production write paths.
//! Acceptance targets: K1 exit criteria — authority/scope stamped on writes,
//! evidence survives ingestion→commit→storage→query, provenance immutable,
//! lifecycle transitions create evidence, no silent epistemic metadata drops.
//!
//! Harness mirrors epistemic.rs: ManualClock + fixed IdGen salt.

use aikoql_kernel::*;
use std::sync::Arc;

fn mk() -> Kernel {
    let clock = Arc::new(ManualClock::new(10_000));
    Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xE915).unwrap()
}

fn mk_with_store() -> (Kernel, Arc<MemoryEngine>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock, 0xE915).unwrap();
    (k, store)
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

fn root() -> Subject {
    Subject::with_roles("root", &["admin"])
}

fn ev(artifact: &str, method: EvidenceMethod) -> Evidence {
    Evidence::new(artifact, method).with_confidence(0.9)
}

fn create_fact(k: &Kernel, s: &Subject, t: &str) -> KOID {
    k.remember(RememberRequest::create(s, meta(t)))
        .unwrap()
        .koid
}

// ---- canonical evidence encoding ------------------------------------------

#[test]
fn evidence_canonical_round_trip_and_dedup() {
    let e1 = ev("src/main.rs", EvidenceMethod::AstExtraction)
        .with_location("lines 42-58")
        .with_revision("abc123");
    let e2 = ev("docs/guide.md", EvidenceMethod::DocExtraction);

    let mut ko = KnowledgeObject::new(
        IdGen::new(7).next(0),
        meta("x"),
        SecurityDescriptor {
            owner: "a".into(),
            acl: vec![],
            classification: None,
        },
    );
    assert!(ko.evidence().is_empty(), "no extension → empty trail");

    ko.set_evidence(vec![e1.clone()]);
    assert_eq!(ko.evidence(), vec![e1.clone()]);

    ko.add_evidence(e2.clone());
    assert_eq!(ko.evidence(), vec![e1, e2]);

    // Exact duplicates are skipped, and the stored value is a deterministic list.
    ko.add_evidence(
        ev("src/main.rs", EvidenceMethod::AstExtraction)
            .with_location("lines 42-58")
            .with_revision("abc123"),
    );
    match ko.extensions.get(KnowledgeObject::EXT_EVIDENCE) {
        Some(Value::List(l)) => assert_eq!(l.len(), 2),
        other => panic!("expected evidence list, got {:?}", other),
    }
}

#[test]
fn malformed_evidence_entries_are_skipped() {
    let koid = IdGen::new(7).next(0);
    let mut ko = KnowledgeObject::new(
        koid,
        meta("x"),
        SecurityDescriptor {
            owner: "a".into(),
            acl: vec![],
            classification: None,
        },
    );
    let e1 = ev("src/main.rs", EvidenceMethod::AstExtraction);
    let encoded = KnowledgeObject::evidence_value(&[e1]);
    let Value::List(items) = &encoded else {
        panic!("expected list");
    };
    // A map missing required fields + a valid entry: valid survives.
    let mut bogus = std::collections::BTreeMap::new();
    bogus.insert("confidence".into(), Value::Float(0.5));
    let mixed = Value::List(vec![
        Value::Map(bogus),
        items[0].clone(),
        Value::Text("junk".into()),
    ]);
    ko.extensions
        .insert(KnowledgeObject::EXT_EVIDENCE.into(), mixed);
    let decoded = ko.evidence();
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].source_artifact, "src/main.rs");
}

// ---- write-path stamping ---------------------------------------------------

#[test]
fn create_stamps_authority_and_scope_by_origin() {
    let k = mk();
    let cases: &[(Origin, &str, &str)] = &[
        (Origin::Human, "human_approved", "user"),
        (Origin::System, "organization_policy", "global"),
        (Origin::Reason, "agent_derived", "global"),
        (Origin::Agent("bob".into()), "agent_derived", "session"),
        (Origin::SemanticEnrichment, "agent_derived", "global"),
    ];
    for (origin, want_auth, want_scope) in cases {
        let mut req = RememberRequest::create(&alice(), meta("fact"));
        req.origin = origin.clone();
        let id = k.remember(req).unwrap().koid;
        let ko = k.get(&alice(), &id).unwrap();
        assert_eq!(
            ko.extensions.get("authority"),
            Some(&Value::Text((*want_auth).into())),
            "authority for {:?}",
            origin
        );
        assert_eq!(
            ko.extensions.get("scope"),
            Some(&Value::Text((*want_scope).into())),
            "scope for {:?}",
            origin
        );
    }

    // Explicit extensions always win over the stamp.
    let mut req = RememberRequest::create(&alice(), meta("fact"));
    req.extensions
        .insert("authority".into(), Value::Text("llm_inferred".into()));
    req.extensions
        .insert("scope".into(), Value::Text("task".into()));
    req.extensions.insert(
        KnowledgeObject::EXT_EPISTEMIC_STATUS.into(),
        Value::Text("verified".into()),
    );
    let id = k.remember(req).unwrap().koid;
    let ko = k.get(&alice(), &id).unwrap();
    assert_eq!(
        ko.extensions.get("authority"),
        Some(&Value::Text("llm_inferred".into()))
    );
    assert_eq!(
        ko.extensions.get("scope"),
        Some(&Value::Text("task".into()))
    );
    assert_eq!(ko.epistemic_status(), EpistemicStatus::Verified);
}

#[test]
fn evidence_survives_ingestion_commit_storage_query() {
    let (k, store) = mk_with_store();
    let e1 = ev("src/lib.rs", EvidenceMethod::AstExtraction)
        .with_location("page 3, bbox \"fn main\"")
        .with_confidence(0.95);
    let mut req = RememberRequest::create(&alice(), meta("fact"));
    req.extensions.insert(
        KnowledgeObject::EXT_EVIDENCE.into(),
        KnowledgeObject::evidence_value(&[e1.clone()]),
    );
    let id = k.remember(req).unwrap().koid;

    // Query boundary: the full trail decodes back, confidence intact.
    let ko = k.get(&alice(), &id).unwrap();
    let decoded = ko.evidence();
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0], e1);

    // Storage boundary: survives reopen on the same store.
    drop(k);
    let clock = Arc::new(ManualClock::new(50_000));
    let k2 = Kernel::open(store, clock, 0xE915).unwrap();
    let ko = k2.get(&alice(), &id).unwrap();
    assert_eq!(ko.evidence(), vec![e1]);
}

// ---- provenance enforcement ------------------------------------------------

#[test]
fn evidence_is_append_only_on_update() {
    let k = mk();
    let e1 = ev("a.md", EvidenceMethod::DocExtraction);
    let e2 = ev("b.md", EvidenceMethod::DocExtraction);
    let mut req = RememberRequest::create(&alice(), meta("fact"));
    req.extensions.insert(
        KnowledgeObject::EXT_EVIDENCE.into(),
        KnowledgeObject::evidence_value(&[e1.clone()]),
    );
    let id = k.remember(req).unwrap().koid;

    // Append: legal, trail grows.
    let mut up = RememberRequest::update(&alice(), id, meta("fact"));
    up.extensions.insert(
        KnowledgeObject::EXT_EVIDENCE.into(),
        KnowledgeObject::evidence_value(&[e1.clone(), e2.clone()]),
    );
    k.remember(up).unwrap();
    let ko = k.get(&alice(), &id).unwrap();
    assert_eq!(ko.evidence().len(), 2);
    assert_eq!(ko.version, 2);

    // Reorder (not a prefix of the head): rejected.
    let mut bad = RememberRequest::update(&alice(), id, meta("fact"));
    bad.extensions.insert(
        KnowledgeObject::EXT_EVIDENCE.into(),
        KnowledgeObject::evidence_value(&[e2.clone(), e1.clone()]),
    );
    let err = k.remember(bad).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)), "got {:?}", err);

    // Truncate: rejected.
    let mut bad = RememberRequest::update(&alice(), id, meta("fact"));
    bad.extensions.insert(
        KnowledgeObject::EXT_EVIDENCE.into(),
        KnowledgeObject::evidence_value(&[e1.clone()]),
    );
    assert!(matches!(k.remember(bad), Err(KError::InvalidObject(_))));

    // Mutate an existing entry: rejected.
    let mut bad = RememberRequest::update(&alice(), id, meta("fact"));
    let mut altered = e1.clone();
    altered.confidence = 0.1;
    bad.extensions.insert(
        KnowledgeObject::EXT_EVIDENCE.into(),
        KnowledgeObject::evidence_value(&[altered, e2.clone()]),
    );
    assert!(matches!(k.remember(bad), Err(KError::InvalidObject(_))));

    // Omit the key entirely: carried forward, legal, trail intact.
    let ok = RememberRequest::update(&alice(), id, meta("fact"));
    k.remember(ok).unwrap();
    let ko = k.get(&alice(), &id).unwrap();
    assert_eq!(ko.evidence().len(), 2);
    assert_eq!(ko.version, 3);
}

#[test]
fn source_artifact_and_revision_stay_strictly_immutable() {
    let k = mk();
    let mut req = RememberRequest::create(&alice(), meta("fact"));
    req.extensions
        .insert("source_artifact".into(), Value::Text("src/main.rs".into()));
    req.extensions
        .insert("revision".into(), Value::Text("rev1".into()));
    let id = k.remember(req).unwrap().koid;

    let mut bad = RememberRequest::update(&alice(), id, meta("fact"));
    bad.extensions
        .insert("revision".into(), Value::Text("rev2".into()));
    let err = k.remember(bad).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)), "got {:?}", err);
}

#[test]
fn authority_is_monotonic_up_without_admin() {
    let k = mk();
    let mut req = RememberRequest::create(&alice(), meta("fact"));
    req.extensions
        .insert("authority".into(), Value::Text("source_code".into()));
    let id = k.remember(req).unwrap().koid;

    // Upgrade by a plain user: fine.
    let mut up = RememberRequest::update(&alice(), id, meta("fact"));
    up.extensions
        .insert("authority".into(), Value::Text("human_approved".into()));
    k.remember(up).unwrap();
    assert_eq!(k.get(&alice(), &id).unwrap().version, 2);

    // Downgrade by a plain user: rejected.
    let mut down = RememberRequest::update(&alice(), id, meta("fact"));
    down.extensions
        .insert("authority".into(), Value::Text("documentation".into()));
    let err = k.remember(down).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)), "got {:?}", err);

    // Downgrade by an admin: explicit escalation path, allowed.
    let mut down = RememberRequest::update(&root(), id, meta("fact"));
    down.extensions
        .insert("authority".into(), Value::Text("documentation".into()));
    k.remember(down).unwrap();
    assert_eq!(k.get(&alice(), &id).unwrap().version, 3);
}

// ---- no silent drops -------------------------------------------------------

#[test]
fn plain_update_carries_epistemic_metadata_forward() {
    let k = mk();
    let mut req = RememberRequest::create(&alice(), meta("fact"));
    req.extensions
        .insert("authority".into(), Value::Text("test_verified".into()));
    let id = k.remember(req).unwrap().koid;
    k.transition_epistemic(
        &alice(),
        &id,
        EpistemicStatus::Verified,
        Origin::System,
        None,
        None,
        None,
    )
    .unwrap();
    let before = k.get(&alice(), &id).unwrap();

    // Update with NO extensions restated — the epistemic block must survive.
    let mut up = RememberRequest::update(&alice(), id, meta("fact"));
    up.properties.insert("x".into(), Value::Int(1));
    k.remember(up).unwrap();
    let after = k.get(&alice(), &id).unwrap();
    assert_eq!(
        after.extensions.get(KnowledgeObject::EXT_EPISTEMIC_STATUS),
        before.extensions.get(KnowledgeObject::EXT_EPISTEMIC_STATUS)
    );
    assert_eq!(
        after.extensions.get(KnowledgeObject::EXT_EPISTEMIC_HISTORY),
        before
            .extensions
            .get(KnowledgeObject::EXT_EPISTEMIC_HISTORY)
    );
    assert_eq!(
        after.extensions.get("authority"),
        Some(&Value::Text("test_verified".into()))
    );
    assert_eq!(after.epistemic_status(), EpistemicStatus::Verified);
}

// ---- lifecycle transitions create evidence ---------------------------------

#[test]
fn evolve_appends_lifecycle_history_and_survives_reopen() {
    let (k, store) = mk_with_store();
    let id = create_fact(&k, &alice(), "fact");
    k.evolve(
        &alice(),
        &id,
        LifecycleState::Active,
        Origin::System,
        None,
        None,
    )
    .unwrap();
    k.evolve(
        &alice(),
        &id,
        LifecycleState::Verified,
        Origin::System,
        None,
        Some("qa sign-off".into()),
    )
    .unwrap();
    drop(k);

    let clock = Arc::new(ManualClock::new(50_000));
    let k2 = Kernel::open(store, clock, 0xE915).unwrap();
    let ko = k2.get(&alice(), &id).unwrap();
    let history = match ko.extensions.get(KnowledgeObject::EXT_LIFECYCLE_HISTORY) {
        Some(Value::List(l)) => l.clone(),
        other => panic!("expected lifecycle history, got {:?}", other),
    };
    assert_eq!(history.len(), 2);
    let entry = |i: usize| -> Vec<(&str, Value)> {
        match &history[i] {
            Value::Map(m) => m.iter().map(|(k, v)| (k.as_str(), v.clone())).collect(),
            other => panic!("expected map, got {:?}", other),
        }
    };
    let first = entry(0);
    assert!(first.contains(&("from", Value::Text("draft".into()))));
    assert!(first.contains(&("to", Value::Text("active".into()))));
    assert!(first.contains(&("by", Value::Text("alice".into()))));
    let second = entry(1);
    assert!(second.contains(&("from", Value::Text("active".into()))));
    assert!(second.contains(&("to", Value::Text("verified".into()))));
    assert!(second.contains(&("reason", Value::Text("qa sign-off".into()))));
    assert!(second.contains(&("by", Value::Text("alice".into()))));
    // Both entries carry the wall-clock via the injected clock (evolve ran
    // before the reopen, so both land at 10_000).
    assert!(matches!(
        second.iter().find(|(k, _)| *k == "at"),
        Some((_, Value::Int(10_000)))
    ));
}

// ---- query filter ----------------------------------------------------------

#[test]
fn scan_by_type_filtered_selects_by_epistemic_status() {
    let k = mk();
    let asserted = create_fact(&k, &alice(), "fact"); // Human → Asserted
    let mut req = RememberRequest::create(&alice(), meta("fact"));
    req.origin = Origin::System; // → Observed
    let observed = k.remember(req).unwrap().koid;

    let all = k.scan_by_type(&alice(), "fact").unwrap();
    assert_eq!(all.len(), 2);

    let only_asserted = k
        .scan_by_type_filtered(&alice(), "fact", Some(EpistemicStatus::Asserted))
        .unwrap();
    assert_eq!(only_asserted.len(), 1);
    assert_eq!(only_asserted[0].koid, asserted);

    let only_observed = k
        .scan_by_type_filtered(&alice(), "fact", Some(EpistemicStatus::Observed))
        .unwrap();
    assert_eq!(only_observed.len(), 1);
    assert_eq!(only_observed[0].koid, observed);

    let none = k
        .scan_by_type_filtered(&alice(), "fact", Some(EpistemicStatus::Superseded))
        .unwrap();
    assert!(none.is_empty());
}
