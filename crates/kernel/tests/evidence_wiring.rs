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
        let mut req = RememberRequest::create(alice(), meta("fact"));
        req.origin = origin.clone();
        let id = k.remember(req).unwrap().koid;
        let ko = k.get(alice(), &id).unwrap();
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

    // Review P0-1 (Test 1): caller-supplied kernel-managed keys are REJECTED
    // on the public boundary — epistemic status/authority/scope are stamped
    // by the kernel or set via the semantic ops, never smuggled through
    // remember().
    for (key, value) in [
        ("authority", Value::Text("llm_inferred".into())),
        ("scope", Value::Text("task".into())),
        (
            KnowledgeObject::EXT_EPISTEMIC_STATUS,
            Value::Text("verified".into()),
        ),
    ] {
        let mut req = RememberRequest::create(alice(), meta("fact"));
        req.extensions.insert(key.into(), value);
        let err = k.remember(req).unwrap_err();
        assert!(
            matches!(err, KError::InvalidObject(_)),
            "key {key} must be rejected, got {err:?}"
        );
    }
    // valid_from is deliberately caller-settable — it is the caller's own
    // temporal claim, not kernel epistemic state.
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.extensions
        .insert(KnowledgeObject::EXT_VALID_FROM.into(), Value::Int(0));
    assert!(k.remember(req).is_ok());
}

#[test]
fn evidence_survives_ingestion_commit_storage_query() {
    let (k, store) = mk_with_store();
    let e1 = ev("src/lib.rs", EvidenceMethod::AstExtraction)
        .with_location("page 3, bbox \"fn main\"")
        .with_confidence(0.95);
    // Review P0-1: evidence is kernel-managed — it enters via the semantic
    // ops (here: observe), not remember().
    let mut req = ObservationRequest::new(alice(), "fact");
    req.evidence = vec![e1.clone()];
    let id = k.observe(req).unwrap().koid;

    // Query boundary: the full trail decodes back, confidence intact.
    let ko = k.get(alice(), &id).unwrap();
    let decoded = ko.evidence();
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0], e1);

    // Storage boundary: survives reopen on the same store.
    drop(k);
    let clock = Arc::new(ManualClock::new(50_000));
    let k2 = Kernel::open(store, clock, 0xE915).unwrap();
    let ko = k2.get(alice(), &id).unwrap();
    assert_eq!(ko.evidence(), vec![e1]);
}

// ---- provenance enforcement ------------------------------------------------

#[test]
fn evidence_is_append_only_on_update() {
    let k = mk();
    let e1 = ev("a.md", EvidenceMethod::DocExtraction);
    let e2 = ev("b.md", EvidenceMethod::DocExtraction);
    // Review P0-1: evidence enters via the semantic ops (observe), and the
    // public remember() boundary rejects any attempt to smuggle it through.
    let mut obs = ObservationRequest::new(alice(), "fact");
    obs.evidence = vec![e1.clone()];
    let id = k.observe(obs).unwrap().koid;
    assert_eq!(k.get(alice(), &id).unwrap().evidence(), vec![e1.clone()]);

    // Append via verify (the semantic evidence path): trail grows.
    let mut v = VerificationRequest::new(alice(), id);
    v.evidence = vec![e2.clone()];
    k.verify_knowledge(v).unwrap();
    let ko = k.get(alice(), &id).unwrap();
    assert_eq!(ko.evidence(), vec![e1.clone(), e2.clone()]);

    // Review P2-3: re-verifying the same evidence is idempotent — exact
    // duplicates are dropped and the confirmation is not double-counted.
    let mut v = VerificationRequest::new(alice(), id);
    v.evidence = vec![e2.clone()];
    k.verify_knowledge(v).unwrap();
    let ko = k.get(alice(), &id).unwrap();
    assert_eq!(ko.evidence(), vec![e1.clone(), e2.clone()]);
    let conf = ko.confidence_context().unwrap();
    assert_eq!(
        conf.confirmations, 1,
        "same verifier + same evidence must not re-confirm"
    );
    assert_eq!(conf.verification_keys.len(), 1);

    // Review P0-1: a plain update carrying the managed evidence key is
    // rejected outright — there is no reorder/truncate/mutate path left
    // through the public boundary.
    let mut bad = RememberRequest::update(alice(), id, meta("fact"));
    bad.extensions.insert(
        KnowledgeObject::EXT_EVIDENCE.into(),
        KnowledgeObject::evidence_value(&[e2.clone(), e1.clone()]),
    );
    let err = k.remember(bad).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)), "got {:?}", err);

    // Omit the key entirely: carried forward, legal, trail intact.
    let version_before = k.get(alice(), &id).unwrap().version;
    let ok = RememberRequest::update(alice(), id, meta("fact"));
    k.remember(ok).unwrap();
    let ko = k.get(alice(), &id).unwrap();
    assert_eq!(ko.evidence(), vec![e1, e2]);
    assert_eq!(ko.version, version_before + 1);
}

#[test]
fn source_artifact_and_revision_stay_strictly_immutable() {
    let k = mk();
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.extensions
        .insert("source_artifact".into(), Value::Text("src/main.rs".into()));
    req.extensions
        .insert("revision".into(), Value::Text("rev1".into()));
    let id = k.remember(req).unwrap().koid;

    let mut bad = RememberRequest::update(alice(), id, meta("fact"));
    bad.extensions
        .insert("revision".into(), Value::Text("rev2".into()));
    let err = k.remember(bad).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)), "got {:?}", err);
}

#[test]
fn authority_is_monotonic_up_without_admin() {
    let k = mk();
    // Review P0-1: authority is kernel-managed. The public remember()
    // boundary rejects caller-supplied authority at create time...
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.extensions
        .insert("authority".into(), Value::Text("source_code".into()));
    let err = k.remember(req).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)), "got {:?}", err);

    // ...and at update time — no escalation/downgrade smuggling via updates.
    let id = create_fact(&k, &alice(), "fact");
    let mut up = RememberRequest::update(alice(), id, meta("fact"));
    up.extensions
        .insert("authority".into(), Value::Text("human_approved".into()));
    let err = k.remember(up).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)), "got {:?}", err);
    // The failed update left the object untouched.
    let ko = k.get(alice(), &id).unwrap();
    assert_eq!(ko.version, 1);

    // The explicit path is the semantic assert op, which stamps the
    // requested authority level (and rejects invalid levels).
    let mut bad = AssertionRequest::new(alice(), "fact");
    bad.authority = Some("not-a-level".into());
    bad.evidence = vec![Evidence::new("x", EvidenceMethod::HumanProvided)];
    assert!(matches!(
        k.assert_knowledge(bad),
        Err(KError::InvalidObject(_))
    ));
    let mut a = AssertionRequest::new(alice(), "fact");
    a.authority = Some("human_approved".into());
    a.evidence = vec![Evidence::new("x", EvidenceMethod::HumanProvided)];
    let asserted = k.assert_knowledge(a).unwrap().koid;
    assert_eq!(
        k.get(alice(), &asserted)
            .unwrap()
            .extensions
            .get("authority"),
        Some(&Value::Text("human_approved".into()))
    );
}

// ---- no silent drops -------------------------------------------------------

#[test]
fn plain_update_carries_epistemic_metadata_forward() {
    let k = mk();
    // Review P0-1: authority is stamped by the semantic assert op, and the
    // privileged transition is explicitly named admin_transition_epistemic
    // (review P0-2).
    let mut a = AssertionRequest::new(alice(), "fact");
    a.authority = Some("test_verified".into());
    a.evidence = vec![Evidence::new("x", EvidenceMethod::HumanProvided)];
    let id = k.assert_knowledge(a).unwrap().koid;
    k.admin_transition_epistemic(
        alice(),
        &id,
        EpistemicStatus::Verified,
        Origin::System,
        None,
        None,
        None,
    )
    .unwrap();
    let before = k.get(alice(), &id).unwrap();

    // Update with NO extensions restated — the epistemic block must survive.
    let mut up = RememberRequest::update(alice(), id, meta("fact"));
    up.properties.insert("x".into(), Value::Int(1));
    k.remember(up).unwrap();
    let after = k.get(alice(), &id).unwrap();
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
        alice(),
        &id,
        LifecycleState::Active,
        Origin::System,
        None,
        None,
    )
    .unwrap();
    k.evolve(
        alice(),
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
    let ko = k2.get(alice(), &id).unwrap();
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
    let mut req = RememberRequest::create(alice(), meta("fact"));
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

// ---- trusted first-party ingestion (ingest-dir) ----------------------------

#[test]
fn ingest_observation_derives_kernel_state_from_evidence() {
    let k = mk();

    // AstExtraction → extracted + source_code authority + trusted + repo scope.
    let mut req = IngestRequest::new(alice(), "Struct");
    req.evidence = vec![ev("src/lib.rs", EvidenceMethod::AstExtraction)];
    let id = k.ingest_observation(req).unwrap().koid;
    let ko = k.get(alice(), &id).unwrap();
    assert_eq!(ko.epistemic_status(), EpistemicStatus::Extracted);
    assert_eq!(ko.content_trust(), ContentTrust::Trusted);
    assert!(matches!(ko.extensions.get("authority"), Some(Value::Text(s)) if s == "source_code"));
    assert!(matches!(ko.extensions.get("scope"), Some(Value::Text(s)) if s == "repository"));
    assert_eq!(
        ko.evidence(),
        vec![ev("src/lib.rs", EvidenceMethod::AstExtraction)]
    );

    // DocExtraction → documentation authority; non-extraction methods stay
    // observed and keep their own authority rank.
    let mut doc = IngestRequest::new(alice(), "Fact");
    doc.evidence = vec![ev("docs/guide.md", EvidenceMethod::DocExtraction)];
    let doc_id = k.ingest_observation(doc).unwrap().koid;
    let doc_ko = k.get(alice(), &doc_id).unwrap();
    assert_eq!(doc_ko.epistemic_status(), EpistemicStatus::Extracted);
    assert!(
        matches!(doc_ko.extensions.get("authority"), Some(Value::Text(s)) if s == "documentation")
    );

    let mut obs = IngestRequest::new(alice(), "Fact");
    obs.evidence = vec![ev("prod", EvidenceMethod::RuntimeObservation)];
    let obs_id = k.ingest_observation(obs).unwrap().koid;
    let obs_ko = k.get(alice(), &obs_id).unwrap();
    assert_eq!(obs_ko.epistemic_status(), EpistemicStatus::Observed);
    assert!(
        matches!(obs_ko.extensions.get("authority"), Some(Value::Text(s)) if s == "deployment_observed")
    );

    // Evidence is mandatory — an unbacked ingestion is rejected.
    let bare = IngestRequest::new(alice(), "Fact");
    assert!(k.ingest_observation(bare).is_err());

    // Exact-once replay: the idempotency key resolves to the original KO.
    let mut replay = IngestRequest::new(alice(), "Struct");
    replay.evidence = vec![ev("src/lib.rs", EvidenceMethod::AstExtraction)];
    replay.idempotency_key = Some("ingest-entity:src/lib.rs:Struct".into());
    let first = k.ingest_observation(replay).unwrap().koid;
    let mut again = IngestRequest::new(alice(), "Struct");
    again.evidence = vec![ev("src/lib.rs", EvidenceMethod::AstExtraction)];
    again.idempotency_key = Some("ingest-entity:src/lib.rs:Struct".into());
    let second = k.ingest_observation(again).unwrap().koid;
    assert_eq!(first, second);
}
