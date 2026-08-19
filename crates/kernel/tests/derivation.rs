//! v0.3 K3 — First-class Derivation structure + confidence context model.
//! Acceptance targets: K3 exit criteria ("every derived KO answers WHY /
//! FROM WHAT / DERIVED HOW / BY WHOM / WHEN / WITH WHICH EVIDENCE — a bare
//! source pointer is insufficient", reviewer H4) + anti-CRUD-cosplay (H6):
//! derive() validates premises and wires real edges, not property strings.

use aikoql_kernel::*;
use std::sync::Arc;

fn mk_kernel() -> (Kernel, Arc<ManualClock>, Arc<MemoryEngine>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xE915).unwrap();
    (k, clock, store)
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn fact(k: &Kernel, who: &str, prop: &str, v: i64) -> KOID {
    let mut req = RememberRequest::create(Subject::new(who), meta("fact"));
    req.properties.insert(prop.into(), Value::Int(v));
    k.remember(req).unwrap().koid
}

#[test]
fn derive_stamps_derivation_record_and_wires_edges() {
    let (k, clock, _store) = mk_kernel();
    let a = fact(&k, "alice", "env", 1);
    let b = fact(&k, "alice", "env", 2);
    clock.tick(5);

    let mut req = DeriveRequest::new(Subject::new("alice"), "conclusion");
    req.properties.insert("answer".into(), Value::Int(3));
    req.sources = vec![a, b];
    req.operation = "inference".into();
    req.actor = "agent-7".into();
    req.model = Some("claude-sonnet-5".into());
    req.reason = Some("both premises observed the same environment".into());
    let r = k.derive(req).unwrap();

    let ko = k.get(&Subject::new("alice"), &r.koid).unwrap();
    // Origin::Reason => Inferred epistemic baseline (not a human assertion).
    assert_eq!(ko.epistemic_status(), EpistemicStatus::Inferred);

    // The first-class derivation record answers all six questions.
    let d = ko.derivation().expect("derivation record must be stamped");
    assert_eq!(d.operation, "inference"); // DERIVED HOW
    assert_eq!(d.actor, "agent-7"); // BY WHOM
    assert_eq!(d.model.as_deref(), Some("claude-sonnet-5")); // WITH WHICH MODEL
    assert_eq!(d.timestamp, 10_005); // WHEN
    assert_eq!(d.sources, vec![a, b]); // FROM WHAT
    assert_eq!(
        d.reason.as_deref(),
        Some("both premises observed the same environment") // WHY
    );

    // DERIVED_FROM edges: dependents are discoverable from every source
    // (inbound refs on the derived KO — this is K4 invalidation input).
    assert_eq!(
        k.outbound_edges(&a, Some(DERIVED_FROM)).unwrap(),
        vec![("derived_from".to_string(), r.koid)]
    );
    assert_eq!(
        k.outbound_edges(&b, Some(DERIVED_FROM)).unwrap(),
        vec![("derived_from".to_string(), r.koid)]
    );
    // The derived KO itself has no outbound derivation edges.
    assert!(k
        .outbound_edges(&r.koid, Some(DERIVED_FROM))
        .unwrap()
        .is_empty());
}

#[test]
fn derive_rejects_missing_source() {
    let (k, _clock, _store) = mk_kernel();
    let ghost = IdGen::new(99).next(0);
    let mut req = DeriveRequest::new(Subject::new("alice"), "conclusion");
    req.sources = vec![ghost];
    let err = k.derive(req).unwrap_err();
    assert!(matches!(err, KError::NotFound(_)), "got {:?}", err);
}

#[test]
fn derive_requires_read_access_on_every_source() {
    let (k, _clock, _store) = mk_kernel();
    // Empty ACL = owner-only Read (alice).
    let a = fact(&k, "alice", "env", 1);
    let mut req = DeriveRequest::new(Subject::new("bob"), "conclusion");
    req.sources = vec![a];
    assert!(k.derive(req).is_err(), "bob cannot read alice's premise");
}

#[test]
fn derivation_and_confidence_survive_reopen() {
    let (k, _clock, store) = mk_kernel();
    let a = fact(&k, "alice", "env", 1);
    let mut req = DeriveRequest::new(Subject::new("alice"), "conclusion");
    req.sources = vec![a];
    req.operation = "merge".into();
    req.reason = Some("merged".into());
    req.confidence = Some(ConfidenceContext {
        score: 0.75,
        confirmations: 3,
        last_verified: Some(9_999),
    });
    let r = k.derive(req).unwrap();
    drop(k);

    let clock = Arc::new(ManualClock::new(50_000));
    let k2 = Kernel::open(store, clock, 0xE915).unwrap();
    let ko = k2.get(&Subject::new("alice"), &r.koid).unwrap();
    let d = ko.derivation().expect("derivation survives reopen");
    assert_eq!(d.operation, "merge");
    assert_eq!(d.sources, vec![a]);
    assert_eq!(
        ko.confidence_context(),
        Some(ConfidenceContext {
            score: 0.75,
            confirmations: 3,
            last_verified: Some(9_999)
        })
    );
    assert_eq!(
        k2.outbound_edges(&a, Some(DERIVED_FROM)).unwrap(),
        vec![("derived_from".to_string(), r.koid)]
    );
}

#[test]
fn confidence_baseline_comes_from_sources_never_silently_full() {
    let (k, _clock, _store) = mk_kernel();
    let a = fact(&k, "alice", "env", 1);
    let b = fact(&k, "alice", "env", 2);

    // Sources with explicit confidence contexts: baseline = mean score,
    // confirmations = number of sources carrying a context.
    let set = |id: &KOID, score: f32| {
        let mut ko = k.get(&Subject::new("alice"), id).unwrap();
        ko.set_confidence_context(&ConfidenceContext {
            score,
            confirmations: 1,
            last_verified: None,
        });
        let mut upd = RememberRequest::update(Subject::new("alice"), *id, meta("fact"));
        upd.extensions = ko.extensions.clone();
        k.remember(upd).unwrap();
    };
    set(&a, 0.6);
    set(&b, 0.8);

    let mut req = DeriveRequest::new(Subject::new("alice"), "conclusion");
    req.sources = vec![a, b];
    let r = k.derive(req).unwrap();
    let c = k
        .get(&Subject::new("alice"), &r.koid)
        .unwrap()
        .confidence_context()
        .expect("baseline confidence must be stamped");
    assert!((c.score - 0.7).abs() < 0.001);
    assert_eq!(c.confirmations, 2);
    assert_eq!(c.last_verified, None);

    // No source context => explicit low-confidence baseline, NOT full trust.
    let c1 = fact(&k, "alice", "env", 3);
    let mut req = DeriveRequest::new(Subject::new("alice"), "conclusion2");
    req.sources = vec![c1];
    let r2 = k.derive(req).unwrap();
    let c2 = k
        .get(&Subject::new("alice"), &r2.koid)
        .unwrap()
        .confidence_context()
        .expect("baseline confidence must be stamped");
    assert_eq!(c2.score, 0.0);
    assert_eq!(c2.confirmations, 0);
}

#[test]
fn update_carries_derivation_and_confidence_forward() {
    let (k, _clock, _store) = mk_kernel();
    let a = fact(&k, "alice", "env", 1);
    let mut req = DeriveRequest::new(Subject::new("alice"), "conclusion");
    req.sources = vec![a];
    req.operation = "inference".into();
    let r = k.derive(req).unwrap();

    // Updating the derived KO's properties must not silently drop lineage.
    let mut upd = RememberRequest::update(Subject::new("alice"), r.koid, meta("conclusion"));
    upd.properties.insert("answer".into(), Value::Int(42));
    k.remember(upd).unwrap();

    let ko = k.get(&Subject::new("alice"), &r.koid).unwrap();
    let d = ko.derivation().expect("derivation must survive update");
    assert_eq!(d.operation, "inference");
    assert_eq!(d.sources, vec![a]);
    assert_eq!(ko.confidence_context().unwrap().score, 0.0);
    assert_eq!(
        k.outbound_edges(&a, Some(DERIVED_FROM)).unwrap(),
        vec![("derived_from".to_string(), r.koid)]
    );
}

#[test]
fn derive_with_evidence_stamps_canonical_trail() {
    let (k, _clock, _store) = mk_kernel();
    let a = fact(&k, "alice", "env", 1);
    let mut req = DeriveRequest::new(Subject::new("alice"), "conclusion");
    req.sources = vec![a];
    req.operation = "extraction".into();
    req.evidence = vec![Evidence::new("src/main.rs", EvidenceMethod::AstExtraction)
        .with_location("lines 42-58")
        .with_confidence(0.95)];
    let r = k.derive(req).unwrap();
    let ko = k.get(&Subject::new("alice"), &r.koid).unwrap();
    let ev = ko.evidence();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].source_artifact, "src/main.rs");
    assert_eq!(ev[0].location.as_deref(), Some("lines 42-58"));
    assert_eq!(ev[0].method, EvidenceMethod::AstExtraction);
    assert!((ev[0].confidence - 0.95).abs() < 0.001);
}

#[test]
fn confidence_context_round_trips_through_extensions() {
    let mut ko = KnowledgeObject::new(
        IdGen::new(9).next(0),
        meta("fact"),
        SecurityDescriptor {
            owner: "a".into(),
            acl: vec![],
            classification: None,
        },
    );
    assert_eq!(ko.derivation(), None);
    assert_eq!(ko.confidence_context(), None);

    ko.set_confidence_context(&ConfidenceContext {
        score: 0.5,
        confirmations: 2,
        last_verified: Some(123),
    });
    assert_eq!(
        ko.confidence_context(),
        Some(ConfidenceContext {
            score: 0.5,
            confirmations: 2,
            last_verified: Some(123)
        })
    );
}
