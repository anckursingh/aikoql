//! v0.3 K1a — Epistemic status: constrained transition table, append-only
//! history, audit-trailed transitions, legacy fallback. Acceptance targets:
//! review H5 + K1 exit criteria ("epistemic state on every production KO,
//! transitions create evidence, historical status retained").
//!
//! Harness mirrors conformance.rs: ManualClock + fixed IdGen salt =>
//! deterministic journals.

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

fn create_fact(k: &Kernel, s: &Subject, t: &str) -> KOID {
    k.remember(RememberRequest::create(s, meta(t)))
        .unwrap()
        .koid
}

use EpistemicStatus::*;

// ---- transition table ------------------------------------------------------

#[test]
fn transition_table_allows_every_documented_move() {
    let allowed: &[(EpistemicStatus, EpistemicStatus)] = &[
        (Observed, Extracted),
        (Observed, Asserted),
        (Observed, Verified),
        (Observed, Contradicted),
        (Observed, Superseded),
        (Extracted, Asserted),
        (Extracted, Verified),
        (Extracted, Contradicted),
        (Extracted, Superseded),
        (Asserted, Verified),
        (Asserted, Contradicted),
        (Asserted, Superseded),
        (Inferred, Verified),
        (Inferred, Contradicted),
        (Inferred, Superseded),
        (Verified, Contradicted),
        (Verified, Superseded),
        (Contradicted, Asserted),
        (Contradicted, Superseded),
    ];
    for (from, to) in allowed {
        assert!(
            from.can_transition(*to),
            "{:?} -> {:?} must be allowed",
            from,
            to
        );
    }
}

#[test]
fn transition_table_rejects_illegal_and_noop_moves() {
    let illegal: &[(EpistemicStatus, EpistemicStatus)] = &[
        // same-state no-ops — every recorded transition must change state
        (Observed, Observed),
        (Extracted, Extracted),
        (Asserted, Asserted),
        (Inferred, Inferred),
        (Verified, Verified),
        (Contradicted, Contradicted),
        (Superseded, Superseded),
        // terminal
        (Superseded, Observed),
        (Superseded, Extracted),
        (Superseded, Asserted),
        (Superseded, Inferred),
        (Superseded, Verified),
        (Superseded, Contradicted),
        // downgrades / reversals
        (Verified, Asserted),
        (Verified, Observed),
        (Extracted, Observed),
        (Asserted, Observed),
        (Contradicted, Verified),
        (Contradicted, Observed),
        (Contradicted, Inferred),
        // status is not a derivation axis
        (Extracted, Inferred),
        (Asserted, Inferred),
    ];
    for (from, to) in illegal {
        assert!(
            !from.can_transition(*to),
            "{:?} -> {:?} must be rejected",
            from,
            to
        );
    }
}

#[test]
fn status_round_trips_through_str() {
    for s in [
        Observed,
        Extracted,
        Asserted,
        Inferred,
        Verified,
        Contradicted,
        Superseded,
    ] {
        assert_eq!(EpistemicStatus::from_str(s.as_str()), Some(s));
    }
    assert_eq!(EpistemicStatus::from_str("not-a-status"), None);
}

#[test]
fn origin_maps_to_initial_status() {
    assert_eq!(EpistemicStatus::for_origin(&Origin::Reason), Inferred);
    assert_eq!(EpistemicStatus::for_origin(&Origin::Human), Asserted);
    assert_eq!(
        EpistemicStatus::for_origin(&Origin::Agent("a".into())),
        Asserted
    );
    assert_eq!(EpistemicStatus::for_origin(&Origin::System), Observed);
    assert_eq!(
        EpistemicStatus::for_origin(&Origin::SemanticEnrichment),
        Observed
    );
}

// ---- kernel behavior -------------------------------------------------------

#[test]
fn fresh_ko_is_stamped_by_origin() {
    let k = mk();
    let id = create_fact(&k, &alice(), "fact");
    let ko = k.get(alice(), &id).unwrap();
    // remember() stamps the epistemic baseline: Human writes are Asserted.
    assert_eq!(ko.epistemic_status(), Asserted);
    assert_eq!(
        ko.extensions.get("epistemic_status"),
        Some(&Value::Text("asserted".into()))
    );

    // System writes are Observed; an explicit extension always wins.
    let mut req = RememberRequest::create(alice(), meta("sys"));
    req.origin = Origin::System;
    let id2 = k.remember(req).unwrap().koid;
    assert_eq!(k.get(alice(), &id2).unwrap().epistemic_status(), Observed);
}

#[test]
fn legacy_lifecycle_verified_maps_to_verified() {
    // Legacy KOs (persisted before v0.3 K1) carry no extension — the
    // fallback derives status from the lifecycle state.
    let koid = IdGen::new(42).next(0);
    let mut ko = KnowledgeObject::new(
        koid,
        meta("legacy"),
        SecurityDescriptor {
            owner: "alice".into(),
            acl: vec![],
            classification: None,
        },
    );
    assert_eq!(ko.epistemic_status(), Observed); // Draft → Observed
    ko.lifecycle.state = LifecycleState::Extracted;
    assert_eq!(ko.epistemic_status(), Extracted);
    ko.lifecycle.state = LifecycleState::Verified;
    assert_eq!(ko.epistemic_status(), Verified);
    assert!(ko.extensions.is_empty(), "fallback, not stamped");
}

#[test]
fn happy_path_records_status_history_and_audit_event() {
    let k = mk();
    // System origin → Observed head, so Observed -> Asserted is a real move.
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.origin = Origin::System;
    let id = k.remember(req).unwrap().koid;

    let r = k
        .admin_transition_epistemic(
            alice(),
            &id,
            Asserted,
            Origin::Human,
            None,
            None,
            Some("manual review".into()),
        )
        .unwrap();
    assert_eq!((r.from, r.to), (Observed, Asserted));
    assert_eq!(r.version, 2);

    let ko = k.get(alice(), &id).unwrap();
    assert_eq!(ko.epistemic_status(), Asserted);
    assert_eq!(
        ko.extensions.get("epistemic_status"),
        Some(&Value::Text("asserted".into()))
    );

    // History: one append-only entry with from/to/at/by/reason.
    let history = match ko.extensions.get("epistemic_history") {
        Some(Value::List(l)) => l.clone(),
        other => panic!("expected history list, got {:?}", other),
    };
    assert_eq!(history.len(), 1);
    let entry = match &history[0] {
        Value::Map(m) => m,
        other => panic!("expected map entry, got {:?}", other),
    };
    assert_eq!(entry.get("from"), Some(&Value::Text("observed".into())));
    assert_eq!(entry.get("to"), Some(&Value::Text("asserted".into())));
    assert_eq!(entry.get("by"), Some(&Value::Text("alice".into())));
    assert_eq!(
        entry.get("reason"),
        Some(&Value::Text("manual review".into()))
    );
    assert!(matches!(entry.get("at"), Some(Value::Int(_))));

    // Audit trail carries the EpistemicChanged event with actor.
    let journal = k.journal().unwrap();
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[1].kind, EventKind::EpistemicChanged);
    assert_eq!(journal[1].actor, "alice");
    assert_eq!(journal[1].note.as_deref(), Some("manual review"));
}

#[test]
fn illegal_transition_is_rejected_unchanged() {
    let k = mk();
    let id = create_fact(&k, &alice(), "fact");
    k.admin_transition_epistemic(alice(), &id, Verified, Origin::System, None, None, None)
        .unwrap(); // Observed -> Verified is legal

    // Verified -> Asserted is a downgrade: rejected.
    let err = k
        .admin_transition_epistemic(alice(), &id, Asserted, Origin::System, None, None, None)
        .unwrap_err();
    assert!(matches!(
        err,
        KError::InvalidEpistemic {
            from: Verified,
            to: Asserted
        }
    ));

    // Terminal state: nothing after Superseded.
    k.admin_transition_epistemic(alice(), &id, Superseded, Origin::System, None, None, None)
        .unwrap();
    for target in [
        Observed,
        Extracted,
        Asserted,
        Inferred,
        Verified,
        Contradicted,
    ] {
        let err = k
            .admin_transition_epistemic(alice(), &id, target, Origin::System, None, None, None)
            .unwrap_err();
        assert!(matches!(err, KError::InvalidEpistemic { .. }));
    }

    // Rejected transitions do not bump the version or add history entries.
    let ko = k.get(alice(), &id).unwrap();
    assert_eq!(ko.version, 3); // create + 2 successful transitions
    match ko.extensions.get("epistemic_history") {
        Some(Value::List(l)) => assert_eq!(l.len(), 2),
        other => panic!("expected history list, got {:?}", other),
    }
}

#[test]
fn contradicted_can_be_reasserted_with_stronger_evidence() {
    let k = mk();
    let id = create_fact(&k, &alice(), "fact");
    k.admin_transition_epistemic(alice(), &id, Contradicted, Origin::System, None, None, None)
        .unwrap();
    let r = k
        .admin_transition_epistemic(
            alice(),
            &id,
            Asserted,
            Origin::Human,
            None,
            None,
            Some("re-asserted on stronger evidence".into()),
        )
        .unwrap();
    assert_eq!((r.from, r.to), (Contradicted, Asserted));
}

#[test]
fn history_survives_reopen_and_accumulates() {
    let (k, store) = mk_with_store();
    let id = create_fact(&k, &alice(), "fact"); // Human → Asserted head
    k.admin_transition_epistemic(alice(), &id, Verified, Origin::System, None, None, None)
        .unwrap();
    drop(k);

    // Reopen on the same store — status + history must survive.
    let clock = Arc::new(ManualClock::new(50_000));
    let k2 = Kernel::open(store, clock, 0xE915).unwrap();
    let ko = k2.get(alice(), &id).unwrap();
    assert_eq!(ko.epistemic_status(), Verified);
    match ko.extensions.get("epistemic_history") {
        Some(Value::List(l)) => {
            assert_eq!(l.len(), 1);
            if let Value::Map(first) = &l[0] {
                assert_eq!(first.get("from"), Some(&Value::Text("asserted".into())));
                assert_eq!(first.get("to"), Some(&Value::Text("verified".into())));
            } else {
                panic!("expected map entry");
            }
        }
        other => panic!("expected history list, got {:?}", other),
    }

    // And the chain continues from the persisted state.
    k2.admin_transition_epistemic(alice(), &id, Superseded, Origin::System, None, None, None)
        .unwrap();
    let ko = k2.get(alice(), &id).unwrap();
    assert_eq!(ko.epistemic_status(), Superseded);
    match ko.extensions.get("epistemic_history") {
        Some(Value::List(l)) => assert_eq!(l.len(), 2),
        other => panic!("expected history list, got {:?}", other),
    }
}

#[test]
fn occ_guard_applies_to_epistemic_transitions() {
    let k = mk();
    let id = create_fact(&k, &alice(), "fact"); // Asserted head
    let err = k
        .admin_transition_epistemic(alice(), &id, Verified, Origin::System, None, Some(99), None)
        .unwrap_err();
    assert!(matches!(
        err,
        KError::VersionConflict {
            expected: 99,
            found: 1,
            ..
        }
    ));
}
