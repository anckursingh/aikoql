//! v0.3 K4 — Knowledge Transactions: observe / assert / verify / contradict /
//! supersede / merge / invalidate / resolve_conflict as first-class kernel
//! ops with transaction semantics, authorization, provenance, and
//! temporal/dependency effects. Acceptance targets: K4 exit criteria +
//! anti-CRUD-cosplay (reviewer H6 — VERIFY must not reduce to a status flip;
//! CONTRADICT must not touch the original claim; invalidation propagates
//! through DERIVED_FROM but never rewrites a dependent's epistemic status).

use aikoql_kernel::*;
use std::sync::Arc;

fn mk_kernel() -> (Kernel, Arc<ManualClock>, Arc<MemoryEngine>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xE915).unwrap();
    (k, clock, store)
}

fn ev(src: &str) -> Evidence {
    Evidence::new(src, EvidenceMethod::TestObservation)
}

fn observe(k: &Kernel, who: &str, prop: &str, v: i64) -> KOID {
    let mut req = ObservationRequest::new(Subject::new(who), "fact");
    req.properties.insert(prop.into(), Value::Int(v));
    req.evidence = vec![ev("test-run")];
    k.observe(req).unwrap().koid
}

/// ACL shared by the test actors — multi-actor flows (contradict, resolve,
/// merge across owners) need cross-subject access beyond the owner default.
fn shared_acl(owner: &str) -> SecurityDescriptor {
    SecurityDescriptor {
        owner: owner.into(),
        acl: vec!["alice", "bob", "carol"]
            .into_iter()
            .flat_map(|p| {
                [
                    AclEntry {
                        principal: p.into(),
                        action: Action::Read,
                        effect: Effect::Allow,
                    },
                    AclEntry {
                        principal: p.into(),
                        action: Action::Write,
                        effect: Effect::Allow,
                    },
                ]
            })
            .collect(),
        classification: None,
    }
}

fn assert_k(k: &Kernel, who: &str, prop: &str, v: i64, authority: &str) -> KOID {
    let mut req = AssertionRequest::new(Subject::new(who), "fact");
    req.properties.insert(prop.into(), Value::Int(v));
    req.authority = Some(authority.into());
    req.evidence = vec![ev("test-run")];
    req.security = Some(shared_acl(who));
    k.assert_knowledge(req).unwrap().koid
}

fn derive_from(k: &Kernel, who: &str, src: KOID, prop: &str, v: i64) -> KOID {
    let mut req = DeriveRequest::new(Subject::new(who), "conclusion");
    req.properties.insert(prop.into(), Value::Int(v));
    req.sources = vec![src];
    req.operation = "inference".into();
    req.actor = "agent-7".into();
    k.derive(req).unwrap().koid
}

// ---- observe ---------------------------------------------------------------

#[test]
fn observe_requires_evidence_and_stamps_provenance() {
    let (k, clock, _store) = mk_kernel();
    let mut req = ObservationRequest::new(Subject::new("alice"), "sighting");
    req.properties.insert("temp".into(), Value::Int(21));
    // No evidence — an unbacked observation is rejected, not downgraded.
    assert!(matches!(
        k.observe(req).unwrap_err(),
        KError::InvalidObject(_)
    ));

    let mut req = ObservationRequest::new(Subject::new("alice"), "sighting");
    req.properties.insert("temp".into(), Value::Int(21));
    req.evidence = vec![ev("thermometer-1")];
    let r = k.observe(req).unwrap();
    clock.tick(1);
    let ko = k.get(&Subject::new("alice"), &r.koid).unwrap();
    // A direct observation is epistemic Observed — never a bare Asserted.
    assert_eq!(ko.epistemic_status(), EpistemicStatus::Observed);
    assert_eq!(ko.valid_from(), Some(10_000));
    assert_eq!(ko.evidence().len(), 1);
    assert_eq!(ko.evidence()[0].source_artifact, "thermometer-1");
}

// ---- assert ----------------------------------------------------------------

#[test]
fn assert_requires_evidence_and_valid_authority() {
    let (k, _clock, _store) = mk_kernel();
    let mut req = AssertionRequest::new(Subject::new("alice"), "claim");
    req.properties.insert("x".into(), Value::Int(1));
    req.authority = Some("source_code".into());
    assert!(matches!(
        k.assert_knowledge(req).unwrap_err(),
        KError::InvalidObject(_)
    ));

    let mut req = AssertionRequest::new(Subject::new("alice"), "claim");
    req.properties.insert("x".into(), Value::Int(1));
    req.evidence = vec![ev("src/main.rs")];
    assert!(matches!(
        k.assert_knowledge(req).unwrap_err(),
        KError::InvalidObject(_)
    ));

    let mut req = AssertionRequest::new(Subject::new("alice"), "claim");
    req.properties.insert("x".into(), Value::Int(1));
    req.authority = Some("not_an_authority".into());
    req.evidence = vec![ev("src/main.rs")];
    assert!(matches!(
        k.assert_knowledge(req).unwrap_err(),
        KError::InvalidObject(_)
    ));

    let mut req = AssertionRequest::new(Subject::new("alice"), "claim");
    req.properties.insert("x".into(), Value::Int(1));
    req.authority = Some("source_code".into());
    req.evidence = vec![ev("src/main.rs")];
    let r = k.assert_knowledge(req).unwrap();
    let ko = k.get(&Subject::new("alice"), &r.koid).unwrap();
    assert_eq!(ko.epistemic_status(), EpistemicStatus::Asserted);
    assert_eq!(ko.authority(), Some(Authority::SourceCode));
}

// ---- verify ----------------------------------------------------------------

#[test]
fn verify_is_not_a_status_flip() {
    let (k, clock, _store) = mk_kernel();
    let f = observe(&k, "alice", "env", 1);
    clock.tick(1);

    let req = VerificationRequest::new(Subject::new("alice"), f);
    // VERIFY without evidence is rejected — it cannot degrade to a flag set.
    assert!(matches!(
        k.verify_knowledge(req).unwrap_err(),
        KError::InvalidObject(_)
    ));

    let mut req = VerificationRequest::new(Subject::new("alice"), f);
    req.evidence = vec![ev("ci-run-1")];
    let res = k.verify_knowledge(req).unwrap();
    let ko = k.get(&Subject::new("alice"), &f).unwrap();
    assert_eq!(ko.epistemic_status(), EpistemicStatus::Verified);
    assert_eq!(res.status, EpistemicStatus::Verified);
    let conf = ko.confidence_context().expect("confidence context");
    assert_eq!(conf.confirmations, 1);
    assert_eq!(conf.last_verified, Some(10_001));
    // Evidence is appended, never replaced.
    assert_eq!(ko.evidence().len(), 2);
}

#[test]
fn verify_bumps_confirmations_and_never_lowers_score() {
    let (k, clock, _store) = mk_kernel();
    // Premise asserted with confidence 0.8 (0.8 score, 1 confirmation).
    let f = assert_k(&k, "alice", "env", 1, "source_code");
    let mut ko = k.get(&Subject::new("alice"), &f).unwrap();
    ko.set_confidence_context(&ConfidenceContext {
        score: 0.8,
        confirmations: 1,
        last_verified: None,
    });
    let mut rr = RememberRequest::update(&Subject::new("alice"), f, ko.metadata.clone());
    rr.properties = ko.properties.clone();
    rr.extensions = ko.extensions.clone();
    k.remember(rr).unwrap();
    clock.tick(1);

    let mut req = VerificationRequest::new(Subject::new("alice"), f);
    req.evidence = vec![ev("ci-run-1")];
    req.confidence = Some(0.5); // a weaker verification must not lower the score
    let res = k.verify_knowledge(req).unwrap();
    assert_eq!(res.confirmations, 2);
    let ko = k.get(&Subject::new("alice"), &f).unwrap();
    let conf = ko.confidence_context().unwrap();
    assert_eq!(conf.score, 0.8);
    assert_eq!(conf.confirmations, 2);
    assert_eq!(conf.last_verified, Some(10_001));

    // A second verify of an already-Verified KO is a confirmation bump.
    let mut req = VerificationRequest::new(Subject::new("alice"), f);
    req.evidence = vec![ev("ci-run-2")];
    k.verify_knowledge(req).unwrap();
    let conf = k
        .get(&Subject::new("alice"), &f)
        .unwrap()
        .confidence_context()
        .unwrap();
    assert_eq!(conf.confirmations, 3);
}

#[test]
fn verify_rejects_illegal_epistemic_moves() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "documentation");
    let mut cr = ContradictionRequest::new(Subject::new("bob"), a);
    cr.counter_props.insert("env".into(), Value::Int(2));
    cr.evidence = vec![ev("bob-observation")];
    let cc = k.contradict(cr).unwrap();
    // Resolve in favour of A — B becomes Contradicted.
    let out = k
        .resolve_conflict(ConflictResolutionRequest {
            context: Subject::new("bob").into(),
            conflict: cc.conflict,
            decision: ConflictResolution::ResolvedAPreferred,
            rationale: "a has stronger evidence".into(),
            replacement: None,
        })
        .unwrap();
    let (_, st) = out
        .effects
        .iter()
        .find(|(ko, _)| *ko == cc.counter)
        .unwrap();
    assert_eq!(*st, EpistemicStatus::Contradicted);

    // VERIFY of a Contradicted claim is an illegal epistemic move.
    let mut vr = VerificationRequest::new(Subject::new("bob"), cc.counter);
    vr.evidence = vec![ev("late-run")];
    assert!(matches!(
        k.verify_knowledge(vr).unwrap_err(),
        KError::InvalidEpistemic { .. }
    ));
}

// ---- contradict ------------------------------------------------------------

#[test]
fn contradict_persists_symmetric_conflict_without_touching_original() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let mut cr = ContradictionRequest::new(Subject::new("bob"), a);
    cr.counter_props.insert("env".into(), Value::Int(2));
    cr.evidence = vec![ev("bob-observation")];
    let res = k.contradict(cr).unwrap();

    // The original claim is UNTOUCHED — the conflict is symmetric.
    let orig = k.get(&Subject::new("alice"), &a).unwrap();
    assert_eq!(orig.epistemic_status(), EpistemicStatus::Asserted);
    assert!(orig.invalidation().is_none());
    assert!(orig.valid_to().is_none());

    // Counter-claim: asserted, evidenced, CONTRADICTS edge.
    let counter = k.get(&Subject::new("bob"), &res.counter).unwrap();
    assert_eq!(counter.epistemic_status(), EpistemicStatus::Asserted);
    assert_eq!(
        k.outbound_edges(&res.counter, Some(CONTRADICTS)).unwrap(),
        vec![("contradicts".to_string(), a)]
    );

    // Persisted Conflict KO: claims, description, resolution, snapshots.
    let conflict = k.get(&Subject::new("bob"), &res.conflict).unwrap();
    assert_eq!(conflict.metadata.type_name, "aikoql:conflict");
    assert_eq!(
        conflict.properties.get("claim_a"),
        Some(&Value::Text(a.to_hex()))
    );
    assert_eq!(
        conflict.properties.get("claim_b"),
        Some(&Value::Text(res.counter.to_hex()))
    );
    let ext = conflict.extensions.get("resolution");
    assert_eq!(ext, Some(&Value::Text("unresolved".into())));
    // Per-assertion snapshots carry authority + evidence + timestamp + scope.
    let assertions = match conflict.extensions.get("assertions") {
        Some(Value::Map(m)) => m,
        _ => panic!("assertions snapshot missing"),
    };
    let side_a = match assertions.get("a") {
        Some(Value::Map(m)) => m,
        _ => panic!("side a snapshot missing"),
    };
    assert_eq!(
        side_a.get("authority"),
        Some(&Value::Text("source_code".into()))
    );
    assert!(side_a.contains_key("evidence"));
    assert!(side_a.contains_key("timestamp"));
}

#[test]
fn contradict_rejects_identical_or_non_current_claims() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    // Identical properties — not a contradiction.
    let mut cr = ContradictionRequest::new(Subject::new("bob"), a);
    cr.counter_props.insert("env".into(), Value::Int(1));
    cr.evidence = vec![ev("bob-observation")];
    assert!(matches!(
        k.contradict(cr).unwrap_err(),
        KError::InvalidObject(_)
    ));
    // No evidence — rejected.
    let mut cr = ContradictionRequest::new(Subject::new("bob"), a);
    cr.counter_props.insert("env".into(), Value::Int(2));
    assert!(matches!(
        k.contradict(cr).unwrap_err(),
        KError::InvalidObject(_)
    ));
}

// ---- supersede -------------------------------------------------------------

#[test]
fn supersede_transitions_old_and_stamps_dependents() {
    let (k, clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let dep = derive_from(&k, "alice", a, "answer", 42);
    clock.tick(1);

    let mut sr = SupersedeRequest::new(Subject::new("alice"), a, "fact");
    sr.properties.insert("env".into(), Value::Int(2));
    sr.evidence = vec![ev("new-observation")];
    sr.reason = Some("the world changed".into());
    let res = k.supersede(sr).unwrap();

    // Old: Superseded + valid_to=now + SUPERSEDES edge — but fully preserved.
    let old = k.get(&Subject::new("alice"), &a).unwrap();
    assert_eq!(old.epistemic_status(), EpistemicStatus::Superseded);
    assert_eq!(old.valid_to(), Some(10_001));
    assert_eq!(old.properties.get("env"), Some(&Value::Int(1)));
    assert_eq!(
        k.outbound_edges(&a, Some(SUPERSEDES)).unwrap(),
        vec![("supersedes".to_string(), res.new)]
    );

    // New: current, evidenced, asserted.
    let new = k.get(&Subject::new("alice"), &res.new).unwrap();
    assert_eq!(new.epistemic_status(), EpistemicStatus::Asserted);
    assert_eq!(new.properties.get("env"), Some(&Value::Int(2)));
    assert_eq!(new.valid_to(), None);

    // The dependent was swept: stamped invalidated + valid_to, but its
    // epistemic status is untouched (nothing contradicted IT).
    assert_eq!(res.invalidated_dependents, vec![dep]);
    let dep_ko = k.get(&Subject::new("alice"), &dep).unwrap();
    assert_eq!(dep_ko.epistemic_status(), EpistemicStatus::Inferred);
    assert!(dep_ko.invalidation().is_some());
    assert_eq!(dep_ko.invalidation().unwrap().actor, "alice");
    assert_eq!(dep_ko.valid_to(), Some(10_001));
}

#[test]
fn supersede_rejects_already_superseded() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let mut sr = SupersedeRequest::new(Subject::new("alice"), a, "fact");
    sr.properties.insert("env".into(), Value::Int(2));
    sr.evidence = vec![ev("run-1")];
    k.supersede(sr).unwrap();
    let mut sr = SupersedeRequest::new(Subject::new("alice"), a, "fact");
    sr.properties.insert("env".into(), Value::Int(3));
    sr.evidence = vec![ev("run-2")];
    assert!(matches!(
        k.supersede(sr).unwrap_err(),
        KError::InvalidObject(_)
    ));
}

// ---- supersede onto an existing successor (superseded_by, review P0-1) ------

#[test]
fn supersede_with_superseded_by_links_existing_successor() {
    let (k, clock, _store) = mk_kernel();
    let old = assert_k(&k, "alice", "env", 1, "source_code");
    let successor = assert_k(&k, "alice", "env", 2, "source_code");
    let dep = derive_from(&k, "alice", old, "answer", 42);
    clock.tick(1);

    let mut sr = SupersedeRequest::new(Subject::new("alice"), old, "fact");
    sr.superseded_by = Some(successor);
    sr.evidence = vec![ev("migration-runbook")];
    sr.reason = Some("migrated".into());
    let res = k.supersede(sr).unwrap();

    // No new generation is minted — the named successor IS the result.
    assert_eq!(res.new, successor);
    let successor_ko = k.get(&Subject::new("alice"), &successor).unwrap();
    assert_eq!(successor_ko.version, 1);
    assert_eq!(successor_ko.epistemic_status(), EpistemicStatus::Asserted);
    assert_eq!(successor_ko.valid_to(), None);

    // Old: Superseded + valid_to stamped + SUPERSEDES edge to the successor.
    let old_ko = k.get(&Subject::new("alice"), &old).unwrap();
    assert_eq!(old_ko.epistemic_status(), EpistemicStatus::Superseded);
    assert_eq!(old_ko.valid_to(), Some(10_001));
    assert_eq!(
        k.outbound_edges(&old, Some(SUPERSEDES)).unwrap(),
        vec![("supersedes".to_string(), successor)]
    );
    // Supersession evidence is appended to the old claim, never dropped.
    assert_eq!(old_ko.evidence().len(), 2);
    assert!(old_ko
        .evidence()
        .iter()
        .any(|e| e.source_artifact == "migration-runbook"));

    // The dependent was swept.
    assert_eq!(res.invalidated_dependents, vec![dep]);
    assert!(k
        .get(&Subject::new("alice"), &dep)
        .unwrap()
        .invalidation()
        .is_some());
}

#[test]
fn supersede_with_superseded_by_rejects_dead_successor() {
    let (k, _clock, _store) = mk_kernel();
    let old = assert_k(&k, "alice", "env", 1, "source_code");
    let dead = assert_k(&k, "alice", "env", 2, "source_code");
    let mut ir = InvalidationRequest::new(Subject::new("alice"), dead);
    ir.evidence = vec![ev("refuting-observation")];
    k.invalidate(ir).unwrap();

    let mut sr = SupersedeRequest::new(Subject::new("alice"), old, "fact");
    sr.superseded_by = Some(dead);
    sr.evidence = vec![ev("migration-runbook")];
    assert!(matches!(
        k.supersede(sr).unwrap_err(),
        KError::InvalidObject(_)
    ));
    // The rejected supersession left the old claim untouched.
    let old_ko = k.get(&Subject::new("alice"), &old).unwrap();
    assert_eq!(old_ko.epistemic_status(), EpistemicStatus::Asserted);
    assert_eq!(old_ko.valid_to(), None);
}

// ---- merge -----------------------------------------------------------------

#[test]
fn merge_is_a_first_class_derivation_with_property_folding() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "a", 1, "source_code");
    let b = assert_k(&k, "bob", "b", 2, "documentation");
    let c = assert_k(&k, "carol", "c", 3, "documentation");

    // Fewer than two sources is not a merge.
    let mr = MergeRequest::new(Subject::new("alice"), "merged", vec![a]);
    assert!(matches!(k.merge(mr).unwrap_err(), KError::InvalidObject(_)));

    // Manual without caller properties is rejected.
    let mut mr = MergeRequest::new(Subject::new("alice"), "merged", vec![a, b]);
    mr.properties = None;
    assert!(matches!(k.merge(mr).unwrap_err(), KError::InvalidObject(_)));

    // NewestWins folds in commit order (later sources overwrite earlier).
    let mut mr = MergeRequest::new(Subject::new("alice"), "merged", vec![a, b, c]);
    mr.strategy = MergeStrategy::NewestWins;
    mr.evidence = vec![ev("merge-run")];
    let r = k.merge(mr).unwrap();
    let ko = k.get(&Subject::new("alice"), &r.koid).unwrap();
    assert_eq!(ko.properties.get("a"), Some(&Value::Int(1)));
    assert_eq!(ko.properties.get("b"), Some(&Value::Int(2)));
    assert_eq!(ko.properties.get("c"), Some(&Value::Int(3)));

    // A merge is a derivation: operation "merge", DERIVED_FROM every source.
    let d = ko.derivation().expect("derivation record");
    assert_eq!(d.operation, "merge");
    assert_eq!(d.sources, vec![a, b, c]);
    for src in [a, b, c] {
        assert_eq!(
            k.outbound_edges(&src, Some(DERIVED_FROM)).unwrap(),
            vec![("derived_from".to_string(), r.koid)]
        );
    }
}

// ---- invalidate ------------------------------------------------------------

#[test]
fn invalidate_contradicts_target_and_sweeps_derivation_chain() {
    let (k, clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let b = derive_from(&k, "alice", a, "mid", 10);
    let c = derive_from(&k, "alice", b, "final", 100);
    clock.tick(1);

    let mut ir = InvalidationRequest::new(Subject::new("alice"), a);
    ir.evidence = vec![ev("refuting-observation")];
    ir.reason = Some("premise refuted".into());
    let res = k.invalidate(ir).unwrap();

    // BFS order: target first, then dependents.
    assert_eq!(res.invalidated, vec![a, b, c]);

    // Target: Contradicted + stamp + valid_to.
    let a_ko = k.get(&Subject::new("alice"), &a).unwrap();
    assert_eq!(a_ko.epistemic_status(), EpistemicStatus::Contradicted);
    let inv = a_ko.invalidation().expect("invalidation stamp");
    assert_eq!(inv.actor, "alice");
    assert_eq!(inv.reason, "premise refuted");
    assert_eq!(a_ko.valid_to(), Some(10_001));

    // Dependents: stamped + valid_to, but epistemic status untouched.
    let b_ko = k.get(&Subject::new("alice"), &b).unwrap();
    assert_eq!(b_ko.epistemic_status(), EpistemicStatus::Inferred);
    assert!(b_ko.invalidation().is_some());
    assert_eq!(b_ko.valid_to(), Some(10_001));
    let c_ko = k.get(&Subject::new("alice"), &c).unwrap();
    assert_eq!(c_ko.epistemic_status(), EpistemicStatus::Inferred);
    assert!(c_ko.invalidation().is_some());
    assert_eq!(c_ko.valid_to(), Some(10_001));
}

#[test]
fn invalidate_requires_evidence_and_is_rejected_when_already_invalidated() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let ir = InvalidationRequest::new(Subject::new("alice"), a);
    assert!(matches!(
        k.invalidate(ir).unwrap_err(),
        KError::InvalidObject(_)
    ));
    let mut ir = InvalidationRequest::new(Subject::new("alice"), a);
    ir.evidence = vec![ev("refuting-observation")];
    k.invalidate(ir).unwrap();
    let mut ir = InvalidationRequest::new(Subject::new("alice"), a);
    ir.evidence = vec![ev("refuting-observation-2")];
    assert!(matches!(
        k.invalidate(ir).unwrap_err(),
        KError::InvalidObject(_)
    ));
}

// ---- sweep robustness (review P1-7) ----------------------------------------

#[test]
fn sweep_terminates_on_derived_from_cycles() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let b = derive_from(&k, "alice", a, "mid", 10);
    let c = derive_from(&k, "alice", b, "final", 100);
    // Close the cycle: wire C -> A as an inbound DERIVED_FROM edge on A.
    let a_ko = k.get(&Subject::new("alice"), &a).unwrap();
    let mut rr = RememberRequest::update(&Subject::new("alice"), a, a_ko.metadata.clone());
    rr.properties = a_ko.properties.clone();
    rr.extensions = a_ko.extensions.clone();
    rr.relationships = vec![RelationshipRef {
        rel_type: DERIVED_FROM.into(),
        target: c,
        direction: Direction::Inbound,
    }];
    k.remember(rr).unwrap();

    let mut ir = InvalidationRequest::new(Subject::new("alice"), a);
    ir.evidence = vec![ev("refuting-observation")];
    // Reaching here at all is the assertion — a cycle must not loop forever.
    let res = k.invalidate(ir).unwrap();
    let mut expected = vec![a, b, c];
    expected.sort();
    let mut got = res.invalidated;
    got.sort();
    assert_eq!(got, expected);
    for koid in [b, c] {
        let ko = k.get(&Subject::new("alice"), &koid).unwrap();
        assert!(ko.invalidation().is_some());
    }
}

#[test]
fn sweep_collapses_duplicate_edges_to_one_stamp() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    // Two identical premise edges on one derivation.
    let mut req = DeriveRequest::new(Subject::new("alice"), "conclusion");
    req.properties.insert("answer".into(), Value::Int(42));
    req.sources = vec![a, a];
    req.operation = "inference".into();
    req.actor = "agent-7".into();
    let b = k.derive(req).unwrap().koid;

    let mut ir = InvalidationRequest::new(Subject::new("alice"), a);
    ir.evidence = vec![ev("refuting-observation")];
    let res = k.invalidate(ir).unwrap();
    // B appears exactly once and is stamped exactly once (one version bump).
    assert_eq!(res.invalidated, vec![a, b]);
    let b_ko = k.get(&Subject::new("alice"), &b).unwrap();
    assert_eq!(b_ko.version, 2); // v1 created, v2 invalidation stamp
}

#[test]
fn repeated_sweep_is_idempotent_per_dependent() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let x = assert_k(&k, "alice", "env", 9, "documentation");
    // B derives from BOTH premises — two invalidation sweeps will reach it.
    let mut req = DeriveRequest::new(Subject::new("alice"), "conclusion");
    req.properties.insert("answer".into(), Value::Int(42));
    req.sources = vec![a, x];
    req.operation = "inference".into();
    req.actor = "agent-7".into();
    let b = k.derive(req).unwrap().koid;

    let mut ir = InvalidationRequest::new(Subject::new("alice"), a);
    ir.evidence = vec![ev("refuting-observation")];
    let res = k.invalidate(ir).unwrap();
    assert_eq!(res.invalidated, vec![a, b]);
    let b_ko = k.get(&Subject::new("alice"), &b).unwrap();
    assert_eq!(b_ko.version, 2);

    // The second premise sweep finds B already stamped and leaves it alone.
    let mut ir = InvalidationRequest::new(Subject::new("alice"), x);
    ir.evidence = vec![ev("refuting-observation-2")];
    let res = k.invalidate(ir).unwrap();
    assert_eq!(res.invalidated, vec![x]);
    let b_ko = k.get(&Subject::new("alice"), &b).unwrap();
    assert_eq!(b_ko.version, 2);
}

// ---- resolve_conflict ------------------------------------------------------

#[test]
fn resolve_conflict_applies_decision_and_records_rationale() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let mut cr = ContradictionRequest::new(Subject::new("bob"), a);
    cr.counter_props.insert("env".into(), Value::Int(2));
    cr.evidence = vec![ev("bob-observation")];
    let cc = k.contradict(cr).unwrap();

    let out = k
        .resolve_conflict(ConflictResolutionRequest {
            context: Subject::new("bob").into(),
            conflict: cc.conflict,
            decision: ConflictResolution::ResolvedAPreferred,
            rationale: "a's observation is stronger".into(),
            replacement: None,
        })
        .unwrap();

    // Decision effects: B contradicted, A untouched.
    assert_eq!(
        out.effects,
        vec![(cc.counter, EpistemicStatus::Contradicted)]
    );
    assert_eq!(
        k.get(&Subject::new("alice"), &a)
            .unwrap()
            .epistemic_status(),
        EpistemicStatus::Asserted
    );
    // The Conflict KO records the decision + rationale.
    let conflict = k.get(&Subject::new("bob"), &cc.conflict).unwrap();
    assert_eq!(
        conflict.extensions.get("resolution"),
        Some(&Value::Text("resolved_a_preferred".into()))
    );
    assert_eq!(
        conflict.extensions.get("resolution_rationale"),
        Some(&Value::Text("a's observation is stronger".into()))
    );
}

#[test]
fn resolve_conflict_validates_decision_rationale_and_state() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let mut cr = ContradictionRequest::new(Subject::new("bob"), a);
    cr.counter_props.insert("env".into(), Value::Int(2));
    cr.evidence = vec![ev("bob-observation")];
    let cc = k.contradict(cr).unwrap();

    // An unresolved decision is not a resolution.
    let err = k
        .resolve_conflict(ConflictResolutionRequest {
            context: Subject::new("bob").into(),
            conflict: cc.conflict,
            decision: ConflictResolution::Unresolved,
            rationale: "still thinking".into(),
            replacement: None,
        })
        .unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));

    // A rationale is mandatory.
    let err = k
        .resolve_conflict(ConflictResolutionRequest {
            context: Subject::new("bob").into(),
            conflict: cc.conflict,
            decision: ConflictResolution::ResolvedAPreferred,
            rationale: "  ".into(),
            replacement: None,
        })
        .unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));

    // Resolve it, then resolving again is rejected.
    k.resolve_conflict(ConflictResolutionRequest {
        context: Subject::new("bob").into(),
        conflict: cc.conflict,
        decision: ConflictResolution::ResolvedAPreferred,
        rationale: "a's observation is stronger".into(),
        replacement: None,
    })
    .unwrap();
    let err = k
        .resolve_conflict(ConflictResolutionRequest {
            context: Subject::new("bob").into(),
            conflict: cc.conflict,
            decision: ConflictResolution::ResolvedAPreferred,
            rationale: "again".into(),
            replacement: None,
        })
        .unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));
}

#[test]
fn resolve_conflict_replaced_requires_replacement_and_supersedes_both() {
    let (k, _clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let mut cr = ContradictionRequest::new(Subject::new("bob"), a);
    cr.counter_props.insert("env".into(), Value::Int(2));
    cr.evidence = vec![ev("bob-observation")];
    let cc = k.contradict(cr).unwrap();
    let replacement = assert_k(&k, "alice", "env", 3, "human_approved");

    // Missing replacement → rejected.
    let err = k
        .resolve_conflict(ConflictResolutionRequest {
            context: Subject::new("bob").into(),
            conflict: cc.conflict,
            decision: ConflictResolution::ResolvedReplaced,
            rationale: "both were wrong".into(),
            replacement: None,
        })
        .unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));

    let out = k
        .resolve_conflict(ConflictResolutionRequest {
            context: Subject::new("bob").into(),
            conflict: cc.conflict,
            decision: ConflictResolution::ResolvedReplaced,
            rationale: "both were wrong".into(),
            replacement: Some(replacement),
        })
        .unwrap();
    let mut superseded = out.effects.iter().map(|(ko, _)| *ko).collect::<Vec<_>>();
    superseded.sort();
    let mut expected = vec![a, cc.counter];
    expected.sort();
    assert_eq!(superseded, expected);
    assert!(out
        .effects
        .iter()
        .all(|(_, st)| *st == EpistemicStatus::Superseded));
    let conflict = k.get(&Subject::new("bob"), &cc.conflict).unwrap();
    assert_eq!(
        conflict.extensions.get("replacement"),
        Some(&Value::Text(replacement.to_hex()))
    );
}

#[test]
fn resolve_replaced_wires_supersedes_edges_and_sweeps_dependents() {
    let (k, clock, _store) = mk_kernel();
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let dep_a = derive_from(&k, "bob", a, "answer", 42);
    let mut cr = ContradictionRequest::new(Subject::new("bob"), a);
    cr.counter_props.insert("env".into(), Value::Int(2));
    cr.evidence = vec![ev("bob-observation")];
    let cc = k.contradict(cr).unwrap();
    let dep_b = derive_from(&k, "bob", cc.counter, "answer", 84);
    let replacement = assert_k(&k, "alice", "env", 3, "human_approved");
    clock.tick(1);

    let out = k
        .resolve_conflict(ConflictResolutionRequest {
            context: Subject::new("bob").into(),
            conflict: cc.conflict,
            decision: ConflictResolution::ResolvedReplaced,
            rationale: "both were wrong".into(),
            replacement: Some(replacement),
        })
        .unwrap();

    // Both claims superseded, valid_to stamped, SUPERSEDES edge to R each.
    for claim in [a, cc.counter] {
        let ko = k.get(&Subject::new("bob"), &claim).unwrap();
        assert_eq!(ko.epistemic_status(), EpistemicStatus::Superseded);
        assert_eq!(ko.valid_to(), Some(10_001));
        assert_eq!(
            k.outbound_edges(&claim, Some(SUPERSEDES)).unwrap(),
            vec![("supersedes".to_string(), replacement)]
        );
    }
    // The replacement stays current and untouched.
    let r_ko = k.get(&Subject::new("alice"), &replacement).unwrap();
    assert_eq!(r_ko.epistemic_status(), EpistemicStatus::Asserted);
    assert_eq!(r_ko.valid_to(), None);

    // Dependents of BOTH claims swept (staleness, not contradiction).
    let mut swept = out.invalidated_dependents;
    swept.sort();
    let mut expected = vec![dep_a, dep_b];
    expected.sort();
    assert_eq!(swept, expected);
    for dep in [dep_a, dep_b] {
        let ko = k.get(&Subject::new("bob"), &dep).unwrap();
        assert_eq!(ko.epistemic_status(), EpistemicStatus::Inferred);
        assert!(ko.invalidation().is_some());
        assert_eq!(ko.valid_to(), Some(10_001));
    }
}

// ---- Knowledge Continuity scenario (review #15) ------------------------------
// The 13-step flagship scenario, kernel-level: K1 epistemic states, K2
// temporal reconstruction, K3 derivation lineage, K4 invalidation sweep,
// K5 experience reuse — all in one story. The MCP/e2e layer covers the same
// story through tools/call (mcp_stdio k1/k2/k4 + scripts/e2e-dogfood.js
// Q1-Q10 + scripts/e2e-k2-temporal.js).

#[test]
fn knowledge_continuity_kafka_to_rabbitmq() {
    let (k, clock, _store) = mk_kernel();
    let agent = Subject::new("agent");
    let human = Subject::new("human");

    // 1+2. Agent observes: Kafka is used, with evidence attached.
    let mut obs = ObservationRequest::new(&agent, "claim");
    obs.properties
        .insert("bus".into(), Value::Text("kafka".into()));
    obs.evidence = vec![ev("migration-notes-2019")];
    let kafka = k.observe(obs).unwrap().koid;
    assert_eq!(
        k.get(&agent, &kafka).unwrap().epistemic_status(),
        EpistemicStatus::Observed
    );

    // 3. Agent derives: the system uses Kafka.
    let derived = derive_from(&k, "agent", kafka, "system", 1);

    // 4. Later: new evidence says RabbitMQ is used.
    let mut obs = ObservationRequest::new(&agent, "claim");
    obs.properties
        .insert("bus".into(), Value::Text("rabbitmq".into()));
    obs.evidence = vec![ev("migration-runbook-2025")];
    let rabbit = k.observe(obs).unwrap().koid;

    // 5. Agent asserts the contradiction (old claim untouched).
    let mut cr = ContradictionRequest::new(agent.clone(), kafka);
    cr.counter_props
        .insert("bus".into(), Value::Text("rabbitmq".into()));
    cr.evidence = vec![ev("migration-runbook-2025")];
    let cc = k.contradict(cr).unwrap();
    let counter = cc.counter;
    let kafka_ko = k.get(&agent, &kafka).unwrap();
    assert_eq!(kafka_ko.epistemic_status(), EpistemicStatus::Observed);

    // 6. Human verifies the new evidence. The agent first submits the
    // counter-claim for review (grants the human Write on it).
    let counter_ko = k.get(&agent, &counter).unwrap();
    let mut grant = RememberRequest::update(&agent, counter, counter_ko.metadata.clone());
    grant.properties = counter_ko.properties.clone();
    grant.extensions = counter_ko.extensions.clone();
    grant.security = Some(SecurityDescriptor {
        owner: "agent".into(),
        acl: vec![
            AclEntry {
                principal: "human".into(),
                action: Action::Read,
                effect: Effect::Allow,
            },
            AclEntry {
                principal: "human".into(),
                action: Action::Write,
                effect: Effect::Allow,
            },
        ],
        classification: None,
    });
    k.remember(grant).unwrap();
    let mut vr = VerificationRequest::new(human.clone(), counter);
    vr.evidence = vec![ev("human-review-notes")];
    vr.note = Some("human review".into());
    let vres = k.verify_knowledge(vr).unwrap();
    assert_eq!(vres.status, EpistemicStatus::Verified);
    assert_eq!(vres.confirmations, 1);
    let counter_ko = k.get(&agent, &counter).unwrap();
    assert_eq!(counter_ko.epistemic_status(), EpistemicStatus::Verified);

    // 7. System supersedes the old knowledge onto the verified claim.
    clock.tick(1);
    let mut sr = SupersedeRequest::new(agent.clone(), kafka, "claim");
    sr.superseded_by = Some(counter);
    sr.evidence = vec![ev("deployment-observed")];
    sr.reason = Some("migrated to rabbitmq".into());
    let sres = k.supersede(sr).unwrap();
    assert_eq!(sres.new, counter);

    // 8. Derived knowledge depending on Kafka is invalidated (swept).
    assert_eq!(sres.invalidated_dependents, vec![derived]);
    let derived_ko = k.get(&agent, &derived).unwrap();
    let inv = derived_ko.invalidation().expect("sweep stamp");
    assert_eq!(inv.actor, "agent");
    assert!(inv.reason.contains("superseded"));
    assert_eq!(derived_ko.epistemic_status(), EpistemicStatus::Inferred);

    // 9. What do we currently know? The successor is current and verified;
    // the Kafka claim is superseded with validity ended at the transition.
    let kafka_ko = k.get(&agent, &kafka).unwrap();
    assert_eq!(kafka_ko.epistemic_status(), EpistemicStatus::Superseded);
    assert_eq!(kafka_ko.valid_to(), Some(10_001));
    let counter_ko = k.get(&agent, &counter).unwrap();
    assert_eq!(counter_ko.epistemic_status(), EpistemicStatus::Verified);
    assert_eq!(counter_ko.valid_to(), None);
    assert_eq!(
        k.outbound_edges(&kafka, Some(SUPERSEDES)).unwrap(),
        vec![("supersedes".to_string(), counter)]
    );

    // 10. What did we know before? The Kafka claim's committed versions are
    // preserved: v1 observed (Kafka), then the supersession transitions.
    let lineage = k.trace(&agent, &kafka).unwrap();
    let versions: Vec<u64> = lineage.versions.iter().map(|v| v.version).collect();
    assert_eq!(versions, vec![1, 2, 3]);
    assert!(lineage.versions.iter().all(|v| v.commit_ts >= 10_000));
    assert_eq!(
        kafka_ko.properties.get("bus"),
        Some(&Value::Text("kafka".into()))
    );

    // 11. Why do we believe RabbitMQ? The verified claim carries its evidence
    // chain and the verification history.
    let counter_ko = k.get(&agent, &counter).unwrap();
    assert!(counter_ko
        .evidence()
        .iter()
        .any(|e| e.source_artifact == "migration-runbook-2025"));
    assert_eq!(counter_ko.confidence_context().unwrap().confirmations, 1);
    let t = k.trace(&agent, &counter).unwrap();
    assert!(t
        .events
        .iter()
        .any(|e| e.note.as_deref() == Some("human review")));

    // 12. What became stale? The derived KO is stamped with who/when/why.
    let inv = k.get(&agent, &derived).unwrap().invalidation().unwrap();
    assert_eq!(inv.actor, "agent");
    assert_eq!(inv.at, 10_001);

    // 13. Have we seen a similar migration before? Record and match.
    let mut er = ExperienceRequest::new(
        agent.clone(),
        "migrate the message bus from kafka to rabbitmq",
        "observed, contradicted, verified, superseded",
        "system now uses rabbitmq",
    );
    er.reuse_conditions = vec!["message bus".into(), "migrate".into()];
    er.evidence = vec![ev("this-test-run")];
    let exp = k.record_experience(er).unwrap().koid;
    let m = k
        .match_experiences(
            &agent,
            "we must migrate the message bus to another broker again",
            5,
        )
        .unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].0.koid, exp);
}

// ---- resolve_conflict_by_authority ------------------------------------------

#[test]
fn authority_resolution_ranks_snapshots_and_rejects_ties() {
    let (k, _clock, _store) = mk_kernel();
    // A carries higher authority than B — A wins by rank.
    let a = assert_k(&k, "alice", "env", 1, "source_code");
    let mut cr = ContradictionRequest::new(Subject::new("bob"), a);
    cr.counter_props.insert("env".into(), Value::Int(2));
    cr.evidence = vec![ev("bob-observation")];
    let cc = k.contradict(cr).unwrap();
    let out = k
        .resolve_conflict_by_authority(Subject::new("bob"), cc.conflict, "rank".into())
        .unwrap();
    assert_eq!(out.decision, ConflictResolution::ResolvedAPreferred);
    assert_eq!(
        out.effects,
        vec![(cc.counter, EpistemicStatus::Contradicted)]
    );

    // Equal authorities — a tie is an error, never a silent pick.
    let x = assert_k(&k, "alice", "env", 3, "documentation");
    let mut cr = ContradictionRequest::new(Subject::new("bob"), x);
    cr.counter_props.insert("env".into(), Value::Int(4));
    cr.authority = Some("documentation".into());
    cr.evidence = vec![ev("bob-observation-2")];
    let cc2 = k.contradict(cr).unwrap();
    let err = k
        .resolve_conflict_by_authority(Subject::new("bob"), cc2.conflict, "rank".into())
        .unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));
}
