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

    ko.set_valid_time(Some(1_000), Some(2_000));
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
    ko.set_valid_time(None, Some(3_000));
    assert_eq!(ko.valid_from(), None);
    assert_eq!(ko.valid_to(), Some(3_000));
    ko.set_valid_time(None, None);
    assert_eq!(ko.valid_from(), None);
    assert_eq!(ko.valid_to(), None);
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
    ko.set_valid_time(Some(100), Some(200));
    assert!(ko.valid_at(100));
    assert!(ko.valid_at(199));
    assert!(!ko.valid_at(200));
    assert!(!ko.valid_at(99));

    // Unbounded on one side.
    ko.set_valid_time(Some(100), None);
    assert!(!ko.valid_at(99));
    assert!(ko.valid_at(100));
    assert!(ko.valid_at(u64::MAX));
    ko.set_valid_time(None, Some(200));
    assert!(ko.valid_at(0));
    assert!(ko.valid_at(199));
    assert!(!ko.valid_at(200));
}

#[test]
fn valid_time_survives_commit_storage_reopen() {
    let clock = std::sync::Arc::new(ManualClock::new(10_000));
    let store = std::sync::Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock, 0xE915).unwrap();
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
    req.extensions
        .insert(KnowledgeObject::EXT_VALID_TO.into(), Value::Int(9_000));
    let id = k.remember(req).unwrap().koid;

    // Transaction time is the HLC-packed commit timestamp (10_000 << 16,
    // first commit, counter 0) — distinct from the validity interval.
    let ko = k.get(&Subject::new("alice"), &id).unwrap();
    assert_eq!(ko.commit_ts, 10_000 << 16);
    assert_eq!(ko.valid_from(), Some(5_000));
    assert_eq!(ko.valid_to(), Some(9_000));

    drop(k);
    let clock = std::sync::Arc::new(ManualClock::new(50_000));
    let k2 = Kernel::open(store, clock, 0xE915).unwrap();
    let ko = k2.get(&Subject::new("alice"), &id).unwrap();
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

    k.transition_epistemic(
        &Subject::new("alice"),
        &old,
        EpistemicStatus::Superseded,
        Origin::System,
        Some(new),
        None,
        Some("replaced by successor".into()),
    )
    .unwrap();

    let ko = k.get(&Subject::new("alice"), &old).unwrap();
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
    k.transition_epistemic(
        &Subject::new("alice"),
        &id,
        EpistemicStatus::Superseded,
        Origin::System,
        None,
        None,
        None,
    )
    .unwrap();
    let ko = k.get(&Subject::new("alice"), &id).unwrap();
    assert_eq!(ko.valid_to(), Some(10_000));
    assert!(k.outbound_edges(&id, Some(SUPERSEDES)).unwrap().is_empty());
}

#[test]
fn superseded_by_requires_a_superseded_transition() {
    let (k, _clock, _store) = mk_kernel();
    let a = fact(&k, "alice", "msg", 1);
    let b = fact(&k, "alice", "msg", 2);
    let r = k.transition_epistemic(
        &Subject::new("alice"),
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
        k.get(&Subject::new("alice"), &a)
            .unwrap()
            .epistemic_status(),
        EpistemicStatus::Asserted
    );
}

#[test]
fn superseded_by_target_must_exist() {
    let (k, _clock, _store) = mk_kernel();
    let a = fact(&k, "alice", "msg", 1);
    let ghost = IdGen::new(99).next(0);
    let r = k.transition_epistemic(
        &Subject::new("alice"),
        &a,
        EpistemicStatus::Superseded,
        Origin::System,
        Some(ghost),
        None,
        None,
    );
    assert!(r.is_err());
    assert_eq!(
        k.get(&Subject::new("alice"), &a)
            .unwrap()
            .epistemic_status(),
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
        .get_as_of(&Subject::new("alice"), &id, 15_000)
        .unwrap()
        .unwrap();
    assert_eq!(ko.version, 1);
    assert_eq!(ko.properties.get("a"), Some(&Value::Int(1)));
    // After the second commit, the head version is returned.
    let ko = k
        .get_as_of(&Subject::new("alice"), &id, 25_000)
        .unwrap()
        .unwrap();
    assert_eq!(ko.version, 2);
    assert_eq!(ko.properties.get("a"), Some(&Value::Int(2)));
    // Before creation there was nothing to read.
    assert!(k
        .get_as_of(&Subject::new("alice"), &id, 5_000)
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

    let hist = k.history(&Subject::new("alice"), &id).unwrap();
    assert_eq!(hist.len(), 2);
    assert!(hist[0].0 < hist[1].0, "versions must ascend by commit_ts");
    assert_eq!(hist[0].1.version, 1);
    assert_eq!(hist[1].1.version, 2);
    assert_eq!(k.clock_now(), 20_000);
}

#[test]
fn update_carries_valid_time_forward() {
    let (k, _clock, _store) = mk_kernel();
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
    req.extensions
        .insert(KnowledgeObject::EXT_VALID_TO.into(), Value::Int(9_000));
    let id = k.remember(req).unwrap().koid;

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

    let ko = k.get(&Subject::new("alice"), &id).unwrap();
    assert_eq!(ko.valid_from(), Some(5_000));
    assert_eq!(ko.valid_to(), Some(9_000));
}
