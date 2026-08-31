//! v0.3 K2 — Valid-time model on the KO (extension-backed, half-open
//! [valid_from, valid_to) interval). Acceptance targets: K2 exit criteria +
//! the temporal-ambiguity adversarial test — valid time is distinct from
//! commit_ts (transaction time) and observed_at.

use aikoql_kernel::*;

#[test]
fn valid_time_round_trips_through_extensions() {
    let mut ko = KnowledgeObject::new(
        IdGen::new(9).next(0),
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
        SecurityDescriptor {
            owner: "a".into(),
            acl: vec![],
            classification: None,
        },
    );
    assert_eq!(ko.valid_from(), None);
    assert_eq!(ko.valid_to(), None);

    ko.set_valid_time(Some(1_000), Some(2_000)).unwrap();
    assert_eq!(ko.valid_from(), Some(1_000));
    assert_eq!(ko.valid_to(), Some(2_000));
    assert_eq!(
        ko.extensions.get(KnowledgeObject::EXT_VALID_FROM),
        Some(&Value::Int(1_000))
    );
    assert_eq!(
        ko.extensions.get(KnowledgeObject::EXT_VALID_TO),
        Some(&Value::Int(2_000))
    );

    // Clearing a bound removes the extension entirely.
    ko.set_valid_time(None, Some(3_000)).unwrap();
    assert_eq!(ko.valid_from(), None);
    assert_eq!(ko.valid_to(), Some(3_000));
    ko.set_valid_time(None, None).unwrap();
    assert_eq!(ko.valid_from(), None);
    assert_eq!(ko.valid_to(), None);
}

#[test]
fn inverted_interval_is_rejected_zero_duration_is_legal() {
    let mut ko = KnowledgeObject::new(
        IdGen::new(9).next(0),
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
        SecurityDescriptor {
            owner: "a".into(),
            acl: vec![],
            classification: None,
        },
    );
    // Inversion (from > to): rejected (review P1-1, Test 2).
    assert!(matches!(
        ko.set_valid_time(Some(9_000), Some(5_000)),
        Err(KError::InvalidObject(_))
    ));
    // Zero-duration (from == to): legal — a claim closed at its own
    // assertion instant is valid nowhere, which is the intended policy.
    ko.set_valid_time(Some(5_000), Some(5_000)).unwrap();
    assert_eq!(ko.valid_from(), Some(5_000));
    assert_eq!(ko.valid_to(), Some(5_000));
    assert!(!ko.valid_at(5_000), "[5_000, 5_000) contains no instant");
}

#[test]
fn valid_at_uses_half_open_interval() {
    let mut ko = KnowledgeObject::new(
        IdGen::new(9).next(0),
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
        SecurityDescriptor {
            owner: "a".into(),
            acl: vec![],
            classification: None,
        },
    );
    // [100, 200): 100 inside, 200 outside, 199 inside, 99 outside.
    ko.set_valid_time(Some(100), Some(200)).unwrap();
    assert!(ko.valid_at(100));
    assert!(ko.valid_at(199));
    assert!(!ko.valid_at(200));
    assert!(!ko.valid_at(99));

    // Unbounded on one side.
    ko.set_valid_time(Some(100), None).unwrap();
    assert!(!ko.valid_at(99));
    assert!(ko.valid_at(100));
    assert!(ko.valid_at(u64::MAX));
    ko.set_valid_time(None, Some(200)).unwrap();
    assert!(ko.valid_at(0));
    assert!(ko.valid_at(199));
    assert!(!ko.valid_at(200));
}

#[test]
fn valid_time_survives_commit_storage_reopen() {
    let clock = std::sync::Arc::new(ManualClock::new(5_000));
    let store = std::sync::Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xE915).unwrap();
    // Review P0-1: the caller claims valid_from (their own temporal claim);
    // valid_to is kernel-managed and is closed by the semantic supersession.
    let mut req = RememberRequest::create(
        Subject::new("alice"),
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.extensions
        .insert(KnowledgeObject::EXT_VALID_FROM.into(), Value::Int(5_000));
    let id = k.remember(req).unwrap().koid;

    clock.set(9_000);
    let successor = fact(&k, "alice", "msg", 2);
    k.admin_transition_epistemic(
        Subject::new("alice"),
        &id,
        EpistemicStatus::Superseded,
        Origin::System,
        Some(successor),
        None,
        Some("superseded by successor".into()),
    )
    .unwrap();

    // Transaction time is the HLC-packed commit timestamp (high bits =
    // wall-clock instant of the superseding commit) — distinct from the
    // validity interval, which is closed at the supersession instant.
    let ko = k.get(Subject::new("alice"), &id).unwrap();
    assert_eq!(ko.commit_ts >> 16, 9_000);
    assert_eq!(ko.valid_from(), Some(5_000));
    assert_eq!(ko.valid_to(), Some(9_000));

    drop(k);
    let clock = std::sync::Arc::new(ManualClock::new(50_000));
    let k2 = Kernel::open(store, clock, 0xE915).unwrap();
    let ko = k2.get(Subject::new("alice"), &id).unwrap();
    assert_eq!(ko.valid_from(), Some(5_000));
    assert_eq!(ko.valid_to(), Some(9_000));
}

// ---- K2 kernel wiring: supersession + transaction-time reads ----------------

fn mk_kernel() -> (
    Kernel,
    std::sync::Arc<ManualClock>,
    std::sync::Arc<MemoryEngine>,
) {
    let clock = std::sync::Arc::new(ManualClock::new(10_000));
    let store = std::sync::Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xE915).unwrap();
    (k, clock, store)
}

fn fact(k: &Kernel, who: &str, prop: &str, v: i64) -> KOID {
    let mut req = RememberRequest::create(
        Subject::new(who),
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties.insert(prop.into(), Value::Int(v));
    k.remember(req).unwrap().koid
}

#[test]
fn supersede_stamps_valid_to_and_wires_supersedes_edge() {
    let (k, clock, _store) = mk_kernel();
    let old = fact(&k, "alice", "msg", 1); // asserted at 10_000
    clock.set(20_000);
    let new = fact(&k, "alice", "msg", 2); // successor asserted at 20_000
    clock.tick(1);

    k.admin_transition_epistemic(
        Subject::new("alice"),
        &old,
        EpistemicStatus::Superseded,
        Origin::System,
        Some(new),
        None,
        Some("replaced by successor".into()),
    )
    .unwrap();

    let ko = k.get(Subject::new("alice"), &old).unwrap();
    assert_eq!(ko.epistemic_status(), EpistemicStatus::Superseded);
    // Validity ends at the supersession instant; from was never set.
    assert_eq!(ko.valid_from(), None);
    assert_eq!(ko.valid_to(), Some(20_001));
    // The edge points old → new, indexed for traversal.
    let edges = k.outbound_edges(&old, Some(SUPERSEDES)).unwrap();
    assert_eq!(edges, vec![("supersedes".to_string(), new)]);
    assert!(k.outbound_edges(&new, Some(SUPERSEDES)).unwrap().is_empty());
}

#[test]
fn supersede_without_successor_still_ends_validity() {
    let (k, _clock, _store) = mk_kernel();
    let id = fact(&k, "alice", "msg", 1);
    k.admin_transition_epistemic(
        Subject::new("alice"),
        &id,
        EpistemicStatus::Superseded,
        Origin::System,
        None,
        None,
        None,
    )
    .unwrap();
    let ko = k.get(Subject::new("alice"), &id).unwrap();
    assert_eq!(ko.valid_to(), Some(10_000));
    assert!(k.outbound_edges(&id, Some(SUPERSEDES)).unwrap().is_empty());
}

#[test]
fn superseded_by_requires_a_superseded_transition() {
    let (k, _clock, _store) = mk_kernel();
    let a = fact(&k, "alice", "msg", 1);
    let b = fact(&k, "alice", "msg", 2);
    let r = k.admin_transition_epistemic(
        Subject::new("alice"),
        &a,
        EpistemicStatus::Verified,
        Origin::System,
        Some(b),
        None,
        None,
    );
    assert!(r.is_err());
    // The KO is untouched.
    assert_eq!(
        k.get(Subject::new("alice"), &a).unwrap().epistemic_status(),
        EpistemicStatus::Asserted
    );
}

#[test]
fn superseded_by_target_must_exist() {
    let (k, _clock, _store) = mk_kernel();
    let a = fact(&k, "alice", "msg", 1);
    let ghost = IdGen::new(99).next(0);
    let r = k.admin_transition_epistemic(
        Subject::new("alice"),
        &a,
        EpistemicStatus::Superseded,
        Origin::System,
        Some(ghost),
        None,
        None,
    );
    assert!(r.is_err());
    assert_eq!(
        k.get(Subject::new("alice"), &a).unwrap().epistemic_status(),
        EpistemicStatus::Asserted
    );
}

#[test]
fn as_of_reads_the_version_committed_at_that_instant() {
    let (k, clock, _store) = mk_kernel();
    let id = fact(&k, "alice", "a", 1); // v1 committed at 10_000
    clock.set(20_000);
    let mut req = RememberRequest::update(
        Subject::new("alice"),
        id,
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties.insert("a".into(), Value::Int(2));
    k.remember(req).unwrap(); // v2 committed at 20_000

    // Between the two commits the system held v1 — reconstruction, not head.
    let ko = k
        .get_as_of(Subject::new("alice"), &id, 15_000)
        .unwrap()
        .unwrap();
    assert_eq!(ko.version, 1);
    assert_eq!(ko.properties.get("a"), Some(&Value::Int(1)));
    // After the second commit, the head version is returned.
    let ko = k
        .get_as_of(Subject::new("alice"), &id, 25_000)
        .unwrap()
        .unwrap();
    assert_eq!(ko.version, 2);
    assert_eq!(ko.properties.get("a"), Some(&Value::Int(2)));
    // Before creation there was nothing to read.
    assert!(k
        .get_as_of(Subject::new("alice"), &id, 5_000)
        .unwrap()
        .is_none());
}

#[test]
fn history_enumerates_all_versions_in_commit_order() {
    let (k, clock, _store) = mk_kernel();
    let id = fact(&k, "alice", "a", 1);
    clock.set(20_000);
    let mut req = RememberRequest::update(
        Subject::new("alice"),
        id,
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties.insert("a".into(), Value::Int(2));
    k.remember(req).unwrap();

    let hist = k.history(Subject::new("alice"), &id).unwrap();
    assert_eq!(hist.len(), 2);
    assert!(hist[0].0 < hist[1].0, "versions must ascend by commit_ts");
    assert_eq!(hist[0].1.version, 1);
    assert_eq!(hist[1].1.version, 2);
    assert_eq!(k.clock_now(), 20_000);
}

#[test]
fn update_carries_valid_time_forward() {
    let (k, clock, _store) = mk_kernel();
    clock.set(5_000);
    let mut req = RememberRequest::create(
        Subject::new("alice"),
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.extensions
        .insert(KnowledgeObject::EXT_VALID_FROM.into(), Value::Int(5_000));
    let id = k.remember(req).unwrap().koid;
    // valid_to is kernel-managed (review P0-1): closed by supersession.
    clock.set(9_000);
    let successor = fact(&k, "alice", "msg", 2);
    k.admin_transition_epistemic(
        Subject::new("alice"),
        &id,
        EpistemicStatus::Superseded,
        Origin::System,
        Some(successor),
        None,
        Some("superseded by successor".into()),
    )
    .unwrap();

    // A plain update must carry the closed interval forward, not reopen it.
    let mut upd = RememberRequest::update(
        Subject::new("alice"),
        id,
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    upd.properties.insert("a".into(), Value::Int(1));
    k.remember(upd).unwrap();

    let ko = k.get(Subject::new("alice"), &id).unwrap();
    assert_eq!(ko.valid_from(), Some(5_000));
    assert_eq!(ko.valid_to(), Some(9_000));
}

#[test]
fn invalidating_a_future_fact_collapses_it_to_never_valid() {
    let (k, _clock, _store) = mk_kernel(); // clock at 10_000
                                           // Caller's own temporal claim: this fact only becomes valid at 50_000.
    let mut req = RememberRequest::create(
        Subject::new("alice"),
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.extensions
        .insert(KnowledgeObject::EXT_VALID_FROM.into(), Value::Int(50_000));
    let id = k.remember(req).unwrap().koid;
    let ko = k.get(Subject::new("alice"), &id).unwrap();
    assert_eq!(ko.valid_from(), Some(50_000));
    assert!(!ko.valid_at(10_000));
    assert!(ko.valid_at(50_000));

    // Invalidate while still future (review P1-1, Test 3): the interval
    // collapses to [50_000, 50_000) — the fact is never valid anywhere.
    let mut ir = InvalidationRequest::new(Subject::new("alice"), id);
    ir.evidence = vec![Evidence::new(
        "refuting-observation",
        EvidenceMethod::HumanProvided,
    )];
    k.invalidate(ir).unwrap();
    let ko = k.get(Subject::new("alice"), &id).unwrap();
    assert_eq!(ko.valid_to(), Some(50_000));
    assert!(ko.invalidation().is_some());
    assert!(!ko.valid_at(50_000));
}
