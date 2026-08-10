//! KS-ABI Conformance & Acceptance Suite (MRFC-0011 §11, MRFC-0001 §14).
//!
//! Every backend and every future adapter MUST pass this suite unchanged.
//! Deterministic by construction: ManualClock + fixed IdGen salt => identical
//! journals across runs (Determinism Law, MRFC-0011 §7).

use mnemosyne_kernel::codec;
use mnemosyne_kernel::*;
use std::collections::BTreeSet;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn mk() -> (Kernel, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0xC0FFEE).unwrap();
    (k, clock)
}

fn mk_with_store() -> (Kernel, Arc<MemoryEngine>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xC0FFEE).unwrap();
    (k, store, clock)
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

fn ke_store_key(seq: u64) -> Vec<u8> {
    let mut v = b"ke/".to_vec();
    v.extend_from_slice(&seq.to_be_bytes());
    v
}

fn obj_store_key(koid: &KOID, ts: u64) -> Vec<u8> {
    let mut v = b"ko/".to_vec();
    v.extend_from_slice(koid.as_bytes());
    v.extend_from_slice(&ts.to_be_bytes());
    v
}

/// Drive a fresh KO to `target` along the legal chain.
fn drive_to(k: &Kernel, s: &Subject, koid: &KOID, target: LifecycleState) {
    use LifecycleState::*;
    let path: &[LifecycleState] = match target {
        Draft => &[],
        Active => &[Active],
        Verified => &[Active, Verified],
        Archived => &[Active, Verified, Archived],
        Deleted => &[Active, Verified, Archived, Deleted],
        // MRFC-0070 states: not reachable from Draft via legacy path.
        // KOs in these states are created directly at that state.
        Discovered | Extracted | Proposed | Validated | Accepted | Updated | Superseded => &[],
    };
    for t in path {
        k.evolve(s, koid, *t, Origin::System, None, None).unwrap();
    }
}

fn create_fact(k: &Kernel, s: &Subject, t: &str) -> KOID {
    k.remember(RememberRequest::create(s.clone(), meta(t)))
        .unwrap()
        .koid
}

// ---------------------------------------------------------------------------
// remember / OCC / idempotency
// ---------------------------------------------------------------------------

#[test]
fn t01_create_persists_all_blocks_and_emits_created() {
    let (k, _c) = mk();
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.properties
        .insert("revenue".into(), Value::Int(1_000_000));
    req.semantic = Some(SemanticBlock {
        embedding_model: Some("bge-m3".into()),
        embedding: Some(vec![0.1, 0.2]),
        confidence: Some(0.99),
        source: Some("sec-filing".into()),
        summary: None,
    });
    req.extensions
        .insert("x-future".into(), Value::Text("preserved".into()));
    let r = k.remember(req).unwrap();
    assert_eq!(r.version, 1);

    let ko = k.get(&alice(), &r.koid).unwrap();
    assert_eq!(ko.version, 1);
    assert_eq!(ko.properties.get("revenue"), Some(&Value::Int(1_000_000)));
    assert_eq!(
        ko.semantic.as_ref().unwrap().source.as_deref(),
        Some("sec-filing")
    );
    assert_eq!(
        ko.extensions.get("x-future"),
        Some(&Value::Text("preserved".into())),
        "unknown extension fields must survive (MRFC-0001 req 9)"
    );
    assert_eq!(ko.lifecycle.state, LifecycleState::Draft);
    assert_eq!(ko.security.owner, "alice");
    assert_eq!(ko.event_refs.len(), 1);

    let journal = k.journal().unwrap();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].kind, EventKind::Created);
    assert_eq!(journal[0].actor, "alice");
}

#[test]
fn t02_update_with_correct_occ_version_commits() {
    let (k, _c) = mk();
    let id = create_fact(&k, &alice(), "fact");
    let mut req = RememberRequest::update(alice(), id, meta("fact"));
    req.expected_version = Some(1);
    let r = k.remember(req).unwrap();
    assert_eq!(r.version, 2);
}

#[test]
fn t03_stale_occ_version_conflicts_deterministically() {
    let (k, _c) = mk();
    let id = create_fact(&k, &alice(), "fact");
    k.remember(RememberRequest::update(alice(), id, meta("fact")))
        .unwrap(); // -> v2
    let mut req = RememberRequest::update(alice(), id, meta("fact"));
    req.expected_version = Some(1);
    let err = k.remember(req).unwrap_err();
    assert!(matches!(
        err,
        KError::VersionConflict {
            expected: 1,
            found: 2,
            ..
        }
    ));
}

#[test]
fn t04_create_over_existing_koid_conflicts() {
    let (k, _c) = mk();
    let id = create_fact(&k, &alice(), "fact");
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.koid = Some(id);
    assert!(matches!(
        k.remember(req).unwrap_err(),
        KError::VersionConflict {
            expected: 0,
            found: 1,
            ..
        }
    ));
}

#[test]
fn t05_update_missing_koid_is_not_found() {
    let (k, _c) = mk();
    let ghost = k.new_koid();
    let req = RememberRequest::update(alice(), ghost, meta("fact"));
    assert!(matches!(k.remember(req).unwrap_err(), KError::NotFound(_)));
}

#[test]
fn t06_idempotent_retry_commits_exactly_once() {
    let (k, _c) = mk();
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.idempotency_key = Some("req-123".into());
    let r1 = k.remember(req.clone()).unwrap();
    let r2 = k.remember(req).unwrap(); // retry, e.g. after client timeout
    assert_eq!(r1, r2);
    assert_eq!(k.journal().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// referential integrity (MRFC-0001 §7)
// ---------------------------------------------------------------------------

#[test]
fn t06b_strict_referential_policy_rejects_dangling_ref() {
    let (k, _c) = mk();
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.referential_policy = ReferentialPolicy::Strict;
    req.relationships.push(RelationshipRef {
        rel_type: "cites".into(),
        target: k.new_koid(),
        direction: Direction::Outbound,
    });
    assert!(matches!(
        k.remember(req).unwrap_err(),
        KError::InvalidObject(_)
    ));
}

#[test]
fn t06c_strict_referential_policy_accepts_existing_target() {
    let (k, _c) = mk();
    let target = create_fact(&k, &alice(), "source");
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.referential_policy = ReferentialPolicy::Strict;
    req.relationships.push(RelationshipRef {
        rel_type: "cites".into(),
        target,
        direction: Direction::Outbound,
    });
    let r = k.remember(req).unwrap();
    assert_eq!(r.version, 1);
    let ko = k.get(&alice(), &r.koid).unwrap();
    assert_eq!(ko.relationships.len(), 1);
}

#[test]
fn t06d_permissive_policy_allows_dangling_ref() {
    let (k, _c) = mk();
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.referential_policy = ReferentialPolicy::Permissive;
    req.relationships.push(RelationshipRef {
        rel_type: "cites".into(),
        target: k.new_koid(),
        direction: Direction::Outbound,
    });
    let r = k.remember(req).unwrap();
    assert_eq!(r.version, 1);
}

// ---------------------------------------------------------------------------
// schema registry (MRFC-0001 §10 automatic schema validation)
// ---------------------------------------------------------------------------

#[test]
fn t06e_registered_schema_rejects_missing_required_property() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("fact", 1).require("title"));
    let req = RememberRequest::create(alice(), meta("fact"));
    assert!(matches!(
        k.remember(req).unwrap_err(),
        KError::InvalidSchema(_)
    ));
}

#[test]
fn t06f_registered_schema_rejects_version_mismatch() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("fact", 2));
    let req = RememberRequest::create(alice(), meta("fact"));
    assert!(matches!(
        k.remember(req).unwrap_err(),
        KError::InvalidSchema(_)
    ));
}

#[test]
fn t06g_registered_schema_accepts_conforming_object() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("fact", 1).require("title"));
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.properties
        .insert("title".into(), Value::Text("ok".into()));
    let r = k.remember(req).unwrap();
    assert_eq!(r.version, 1);
}

#[test]
fn t06h_unregistered_type_is_not_validated() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("claim", 1).require("title"));
    // "fact" is not registered, so remember succeeds without required props.
    let req = RememberRequest::create(alice(), meta("fact"));
    let r = k.remember(req).unwrap();
    assert_eq!(r.version, 1);
}

#[test]
fn t06i_registered_closed_schema_rejects_unknown_core_field() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("fact", 1).require("title").allow("title"));
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.properties
        .insert("title".into(), Value::Text("ok".into()));
    req.properties
        .insert("extra".into(), Value::Text("surprise".into()));
    assert!(matches!(
        k.remember(req).unwrap_err(),
        KError::InvalidSchema(_)
    ));

    // same KO without the extra field succeeds
    let mut req2 = RememberRequest::create(alice(), meta("fact"));
    req2.properties
        .insert("title".into(), Value::Text("ok".into()));
    assert_eq!(k.remember(req2).unwrap().version, 1);
}

// ---------------------------------------------------------------------------
// multi-object transactions (MRFC-0011 §6 atomic batch commit)
// ---------------------------------------------------------------------------

#[test]
fn t06j_transact_creates_multiple_objects_atomically() {
    let (k, _c) = mk();
    let r1 = RememberRequest::create(alice(), meta("fact"));
    let r2 = RememberRequest::create(alice(), meta("note"));
    let res = k
        .transact(vec![
            TransactionOp::new(alice(), r1),
            TransactionOp::new(alice(), r2),
        ])
        .unwrap();
    assert_eq!(res.len(), 2);
    assert_eq!(res[0].version, 1);
    assert_eq!(res[1].version, 1);
    assert_eq!(k.journal().unwrap().len(), 2);
    // both heads are readable
    assert!(k.get(&alice(), &res[0].koid).is_ok());
    assert!(k.get(&alice(), &res[1].koid).is_ok());
}

#[test]
fn t06k_transact_is_all_or_nothing() {
    let (k, _c) = mk();
    let id = create_fact(&k, &alice(), "fact");
    // first op will conflict (stale version); second op is valid.
    let mut stale = RememberRequest::update(alice(), id, meta("fact"));
    stale.expected_version = Some(0);
    let ok = RememberRequest::create(alice(), meta("note"));
    assert!(matches!(
        k.transact(vec![
            TransactionOp::new(alice(), stale),
            TransactionOp::new(alice(), ok)
        ])
        .unwrap_err(),
        KError::VersionConflict { .. }
    ));
    // journal remains untouched
    assert_eq!(k.journal().unwrap().len(), 1);
}

#[test]
fn t06l_transact_strict_referential_allows_intra_batch_targets() {
    let (k, _c) = mk();
    let parent_koid = KOID([0xab; KOID_LEN]);
    let mut parent = RememberRequest::create(alice(), meta("fact"));
    parent.koid = Some(parent_koid);
    parent.expected_version = Some(0);
    parent.referential_policy = ReferentialPolicy::Strict;

    let mut child = RememberRequest::create(alice(), meta("fact"));
    child.referential_policy = ReferentialPolicy::Strict;
    child.relationships.push(RelationshipRef {
        rel_type: "child-of".into(),
        target: parent_koid,
        direction: Direction::Outbound,
    });
    let res = k
        .transact(vec![
            TransactionOp::new(alice(), parent),
            TransactionOp::new(alice(), child),
        ])
        .unwrap();
    assert_eq!(res.len(), 2);
    // child exists and its relationship target exists
    assert!(k.get(&alice(), &res[1].koid).is_ok());
}

// ---------------------------------------------------------------------------
// evolve / lifecycle (MRFC-0001 §6 full matrix)
// ---------------------------------------------------------------------------

#[test]
fn t07_lifecycle_matrix_all_25_pairs() {
    let (k, _c) = mk();
    use LifecycleState::*;
    let states = [Draft, Active, Verified, Archived, Deleted];
    for from in states {
        for to in states {
            let id = create_fact(&k, &alice(), "fact");
            drive_to(&k, &alice(), &id, from);
            let res = k.evolve(&alice(), &id, to, Origin::System, None, None);
            if from.can_transition(to) {
                assert!(res.is_ok(), "{} -> {} must succeed", from, to);
            } else {
                assert!(
                    matches!(res.unwrap_err(), KError::InvalidState { .. }),
                    "{} -> {} must be INVALID_STATE",
                    from,
                    to
                );
            }
        }
    }
}

#[test]
fn t08_evolve_emits_lifecycle_changed_with_actor_and_note() {
    let (k, _c) = mk();
    let id = create_fact(&k, &alice(), "fact");
    let e = k
        .evolve(
            &alice(),
            &id,
            LifecycleState::Active,
            Origin::Human,
            None,
            Some("promote".into()),
        )
        .unwrap();
    assert_eq!(e.version, 2);
    let journal = k.journal().unwrap();
    let ke = &journal[1];
    assert_eq!(ke.kind, EventKind::LifecycleChanged);
    assert_eq!(ke.actor, "alice");
    assert_eq!(ke.note.as_deref(), Some("promote"));
}

// ---------------------------------------------------------------------------
// forget: tombstone & legal erasure
// ---------------------------------------------------------------------------

#[test]
fn t09_forget_tombstone_marks_deleted_but_retains_lineage() {
    let (k, _c) = mk();
    let id = create_fact(&k, &alice(), "fact");
    let f = k
        .forget(
            &alice(),
            &id,
            ForgetMode::Tombstone,
            None,
            Some("gdpr-req-1".into()),
        )
        .unwrap();
    assert_eq!(f.version, 2);
    let ko = k.get(&alice(), &id).unwrap();
    assert_eq!(ko.lifecycle.state, LifecycleState::Deleted);
    // lineage retained: both versions still traceable
    let lineage = k.trace(&alice(), &id).unwrap();
    assert_eq!(lineage.versions.len(), 2);
    assert_eq!(lineage.events.len(), 2);
    assert_eq!(lineage.events[1].kind, EventKind::Forgotten);
}

#[test]
fn t10_forget_erase_removes_versions_but_keeps_proof_possible() {
    let (k, _c) = mk();
    let victim = create_fact(&k, &alice(), "secret");
    let witness = create_fact(&k, &alice(), "fact");
    k.forget(&alice(), &victim, ForgetMode::Erase, None, None)
        .unwrap();

    assert!(matches!(k.get(&alice(), &victim), Err(KError::NotFound(_))));
    // journal retained (3 events: 2 creates + 1 forgotten)
    assert_eq!(k.journal().unwrap().len(), 3);
    // audit chain over the whole journal still verifies, via tombstone stub
    let proof = k.prove(&alice(), &witness).unwrap();
    assert!(
        proof.chain_valid,
        "chain must remain valid after legal erasure"
    );
}

// ---------------------------------------------------------------------------
// verify / ACL (kernel-boundary enforcement, MRFC-0001 §12)
// ---------------------------------------------------------------------------

#[test]
fn t11_default_deny_for_non_owner() {
    let (k, _c) = mk();
    let id = create_fact(&k, &alice(), "fact");
    let bob = Subject::new("bob");
    assert!(matches!(k.get(&bob, &id), Err(KError::AccessDenied { .. })));
    assert!(matches!(
        k.remember(RememberRequest::update(bob.clone(), id, meta("fact")))
            .unwrap_err(),
        KError::AccessDenied { .. }
    ));
    assert!(matches!(
        k.evolve(
            &bob,
            &id,
            LifecycleState::Active,
            Origin::System,
            None,
            None
        )
        .unwrap_err(),
        KError::AccessDenied { .. }
    ));
    assert!(matches!(
        k.forget(&bob, &id, ForgetMode::Tombstone, None, None)
            .unwrap_err(),
        KError::AccessDenied { .. }
    ));
}

#[test]
fn t12_acl_allow_deny_precedence_and_admin_role() {
    let (k, _c) = mk();
    let sec = SecurityDescriptor {
        owner: "alice".into(),
        acl: vec![
            AclEntry {
                principal: "bob".into(),
                action: Action::Read,
                effect: Effect::Allow,
            },
            AclEntry {
                principal: "bob".into(),
                action: Action::Write,
                effect: Effect::Deny,
            },
            AclEntry {
                principal: "editors".into(),
                action: Action::Write,
                effect: Effect::Allow,
            },
        ],
        classification: None,
    };
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.security = Some(sec);
    let id = k.remember(req).unwrap().koid;

    let bob = Subject::new("bob");
    assert!(k.get(&bob, &id).is_ok(), "explicit Allow read");
    assert!(
        matches!(
            k.remember(RememberRequest::update(bob, id, meta("fact")))
                .unwrap_err(),
            KError::AccessDenied { .. }
        ),
        "explicit Deny beats nothing"
    );

    let ed = Subject::with_roles("carol", &["editors"]);
    assert!(
        k.remember(RememberRequest::update(ed, id, meta("fact")))
            .is_ok(),
        "role grant works"
    );

    let root = Subject::with_roles("root", &["admin"]);
    assert!(k.get(&root, &id).is_ok(), "admin role bypasses");
    assert!(k.verify(&root, &id, Action::Delete).is_ok());
}

// ---------------------------------------------------------------------------
// MVCC snapshot isolation (MRFC-0001 §8)
// ---------------------------------------------------------------------------

#[test]
fn t13_snapshot_reads_are_stable_under_concurrent_commits() {
    let (k, clock) = mk();
    let id = create_fact(&k, &alice(), "fact");
    let snap = k.snapshot();
    clock.tick(10);
    let mut req = RememberRequest::update(alice(), id, meta("fact"));
    req.properties.insert("v".into(), Value::Int(2));
    k.remember(req).unwrap();

    let old = k.get_at(&alice(), &id, snap).unwrap();
    assert_eq!(old.version, 1);
    assert_eq!(old.properties.get("v"), None);
    let new = k.get(&alice(), &id).unwrap();
    assert_eq!(new.version, 2);
    assert_eq!(new.properties.get("v"), Some(&Value::Int(2)));
}

// ---------------------------------------------------------------------------
// trace / explain (provenance as queries — the moat, review §10.1)
// ---------------------------------------------------------------------------

#[test]
fn t14_trace_returns_full_lineage() {
    let (k, _c) = mk();
    let id = create_fact(&k, &alice(), "fact");
    k.evolve(
        &alice(),
        &id,
        LifecycleState::Active,
        Origin::Human,
        None,
        None,
    )
    .unwrap();
    k.remember(RememberRequest::update(alice(), id, meta("fact")))
        .unwrap();

    let lin = k.trace(&alice(), &id).unwrap();
    assert_eq!(lin.versions.len(), 3);
    assert_eq!(lin.events.len(), 3);
    assert_eq!(
        lin.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        vec![
            EventKind::Created,
            EventKind::LifecycleChanged,
            EventKind::Updated
        ]
    );
    assert!(lin.events.windows(2).all(|w| w[0].seq < w[1].seq));
}

#[test]
fn t15_explain_answers_why_believed() {
    let (k, _c) = mk();
    let evidence_id = create_fact(&k, &alice(), "evidence");
    let mut req = RememberRequest::create(alice(), meta("claim"));
    req.semantic = Some(SemanticBlock {
        embedding_model: None,
        embedding: None,
        confidence: Some(0.99),
        source: Some("sec-10k-filing".into()),
        summary: None,
    });
    req.relationships.push(RelationshipRef {
        rel_type: "supported-by".into(),
        target: evidence_id,
        direction: Direction::Outbound,
    });
    let claim = k.remember(req).unwrap().koid;
    drive_to(&k, &alice(), &claim, LifecycleState::Verified);

    let ex = k.explain(&alice(), &claim, None).unwrap();
    assert_eq!(ex.source.as_deref(), Some("sec-10k-filing"));
    assert_eq!(ex.confidence, Some(0.99));
    assert!(ex.verified);
    assert_eq!(ex.evidence, vec![("supported-by".to_string(), evidence_id)]);
    assert!(!ex.event_refs.is_empty());
}

// ---------------------------------------------------------------------------
// prove (hash-chained audit — tamper evidence)
// ---------------------------------------------------------------------------

#[test]
fn t16_prove_valid_chain() {
    let (k, _c) = mk();
    let a = create_fact(&k, &alice(), "fact");
    let b = create_fact(&k, &alice(), "fact");
    k.evolve(
        &alice(),
        &a,
        LifecycleState::Active,
        Origin::System,
        None,
        None,
    )
    .unwrap();
    k.remember(RememberRequest::update(alice(), b, meta("fact")))
        .unwrap();

    let proof = k.prove(&alice(), &a).unwrap();
    assert!(proof.chain_valid);
    assert_eq!(proof.events, 4);
    let (seq, audit) = k.journal_head().unwrap();
    assert_eq!(seq, 4);
    assert_eq!(audit, proof.head_audit_hash);
}

#[test]
fn t17_prove_detects_tampered_event() {
    let (k, store, _c) = mk_with_store();
    let id = create_fact(&k, &alice(), "fact");
    k.remember(RememberRequest::update(alice(), id, meta("fact")))
        .unwrap();

    // attacker rewrites the note of event #1 in storage
    let raw = store.get(&ke_store_key(1)).unwrap().unwrap();
    let mut ke = codec::decode_ke(&raw).unwrap();
    ke.note = Some("forged".into());
    let mut b = WriteBatch::new();
    b.put(ke_store_key(1), codec::encode_ke(&ke));
    store.write_batch(&b).unwrap();

    let proof = k.prove(&alice(), &id).unwrap();
    assert!(
        !proof.chain_valid,
        "tampered event must break the audit chain"
    );
}

#[test]
fn t18_prove_detects_tampered_object_payload() {
    let (k, store, _c) = mk_with_store();
    let id = create_fact(&k, &alice(), "fact");
    let ts = k.journal().unwrap()[0].commit_ts;

    let key = obj_store_key(&id, ts);
    let mut bytes = store.get(&key).unwrap().unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 0xFF; // flip a bit in the stored object
    let mut b = WriteBatch::new();
    b.put(key, bytes);
    store.write_batch(&b).unwrap();

    let proof = k.prove(&alice(), &id).unwrap();
    assert!(!proof.chain_valid, "tampered payload must be detected");
}

#[test]
fn t18b_prove_with_signing_key_verifies_signatures() {
    let clock = Arc::new(ManualClock::new(10_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xC0FFEE)
        .unwrap()
        .with_signing_key([0x11; 32]);
    let id = create_fact(&k, &alice(), "fact");
    k.remember(RememberRequest::update(alice(), id, meta("fact")))
        .unwrap();

    let proof = k.prove(&alice(), &id).unwrap();
    assert!(proof.chain_valid);
    assert!(proof.signatures_verified);
    // every event carries a signature
    for ke in k.journal().unwrap() {
        assert!(ke.signature.is_some(), "signed kernel must sign every KE");
    }
}

#[test]
fn t18c_prove_detects_tampered_signature() {
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock, 0xC0FFEE)
        .unwrap()
        .with_signing_key([0x22; 32]);
    let id = create_fact(&k, &alice(), "fact");

    let raw = store.get(&ke_store_key(1)).unwrap().unwrap();
    let mut ke = codec::decode_ke(&raw).unwrap();
    ke.signature = ke.signature.map(|mut s| {
        s[0] ^= 0xFF;
        s
    });
    let mut b = WriteBatch::new();
    b.put(ke_store_key(1), codec::encode_ke(&ke));
    store.write_batch(&b).unwrap();

    let proof = k.prove(&alice(), &id).unwrap();
    assert!(!proof.signatures_verified, "bad signature must be detected");
}

// ---------------------------------------------------------------------------
// find_similar (hybrid recall: vector + text + filters + ACL)
// ---------------------------------------------------------------------------

fn create_with_vec(k: &Kernel, s: &Subject, t: &str, body: &str, emb: Vec<f32>) -> KOID {
    let mut req = RememberRequest::create(s.clone(), meta(t));
    req.properties
        .insert("body".into(), Value::Text(body.into()));
    req.semantic = Some(SemanticBlock {
        embedding_model: Some("test-model".into()),
        embedding: Some(emb),
        confidence: None,
        source: None,
        summary: None,
    });
    k.remember(req).unwrap().koid
}

#[test]
fn t19_vector_recall_orders_by_cosine() {
    let (k, _c) = mk();
    let a = create_with_vec(&k, &alice(), "fact", "alpha", vec![1.0, 0.0]);
    let b = create_with_vec(&k, &alice(), "fact", "beta", vec![0.9, 0.1]);
    let _c2 = create_with_vec(&k, &alice(), "fact", "gamma", vec![0.0, 1.0]);

    let res = k
        .find_similar(SimilarityQuery {
            context: alice().into(),
            filter: None,
            text: None,
            vector: Some(vec![1.0, 0.0]),
            embedding_model: None,
            k: 2,
            fusion: Fusion::VectorOnly,
        })
        .unwrap();
    assert_eq!(res.len(), 2);
    assert_eq!(res[0].ko.koid, a);
    assert_eq!(res[1].ko.koid, b);
    assert!(res[0].score > res[1].score);
}

#[test]
fn t20_text_recall_and_type_filter() {
    let (k, _c) = mk();
    let _a = create_with_vec(&k, &alice(), "fact", "cats are great", vec![1.0, 0.0]);
    let b = create_with_vec(&k, &alice(), "note", "dogs are loyal", vec![0.0, 1.0]);

    // type filter restricts to "note" only
    let res = k
        .find_similar(SimilarityQuery {
            context: alice().into(),
            filter: Some(PropertyFilter {
                type_name: Some("note".into()),
                required: vec![],
            }),
            text: Some("dogs".into()),
            vector: None,
            embedding_model: None,
            k: 5,
            fusion: Fusion::TextOnly,
        })
        .unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].ko.koid, b);
    assert!(res[0].score > 0.0);
}

#[test]
fn t21_rrf_fuses_vector_and_text_rankings() {
    let (k, _c) = mk();
    // A: strong vector match AND strong text match (ranks in both lists).
    // C: strong text match, orthogonal vector (ranks only in text).
    let a = create_with_vec(&k, &alice(), "fact", "cats", vec![1.0, 0.0]);
    let c = create_with_vec(&k, &alice(), "fact", "cats and dogs", vec![0.0, 1.0]);

    let res = k
        .find_similar(SimilarityQuery {
            context: alice().into(),
            filter: None,
            text: Some("cats".into()),
            vector: Some(vec![1.0, 0.0]),
            embedding_model: None,
            k: 3,
            fusion: Fusion::Rrf { k0: 60 },
        })
        .unwrap();
    assert_eq!(res.len(), 2);
    // A wins: it ranks in BOTH lists; C ranks only in text
    assert_eq!(res[0].ko.koid, a);
    assert_eq!(res[1].ko.koid, c);
    assert!(res[0].score > res[1].score);
}

#[test]
fn t22_find_similar_respects_acl_silently() {
    let (k, _c) = mk();
    let secret = create_with_vec(&k, &alice(), "fact", "classified", vec![1.0, 0.0]);
    let bob = Subject::new("bob");
    let res = k
        .find_similar(SimilarityQuery {
            context: bob.into(),
            filter: None,
            text: None,
            vector: Some(vec![1.0, 0.0]),
            embedding_model: None,
            k: 10,
            fusion: Fusion::VectorOnly,
        })
        .unwrap();
    assert!(res.is_empty(), "no existence leak for {}", secret);
}

// ---------------------------------------------------------------------------
// notify (CDC stream)
// ---------------------------------------------------------------------------

#[test]
fn t23_notify_delivers_commits_in_order_with_filter() {
    let (k, _c) = mk();
    let a = create_fact(&k, &alice(), "fact");
    let b = create_fact(&k, &alice(), "fact");

    let rx_a = k.notify(EventFilter {
        koid: Some(a),
        kinds: None,
    });
    let rx_b_created = k.notify(EventFilter {
        koid: Some(b),
        kinds: Some(vec![EventKind::Created]),
    });

    k.evolve(
        &alice(),
        &a,
        LifecycleState::Active,
        Origin::System,
        None,
        None,
    )
    .unwrap();
    k.remember(RememberRequest::update(alice(), b, meta("fact")))
        .unwrap();

    let e1 = rx_a
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    assert_eq!(e1.kind, EventKind::LifecycleChanged);
    assert_eq!(e1.koid, a);

    // b's subscriber only wants Created, which happened BEFORE subscribe: empty
    assert!(rx_b_created.try_recv().is_err());
}

// ---------------------------------------------------------------------------
// determinism (MRFC-0011 §7 + §11: replay => identical journal)
// ---------------------------------------------------------------------------

#[test]
fn t24_deterministic_replay_produces_identical_journal() {
    fn script(k: &Kernel) {
        let a = create_fact(k, &alice(), "fact");
        k.evolve(
            &alice(),
            &a,
            LifecycleState::Active,
            Origin::Human,
            None,
            Some("go".into()),
        )
        .unwrap();
        let mut req = RememberRequest::update(alice(), a, meta("fact"));
        req.properties.insert("n".into(), Value::Int(7));
        k.remember(req).unwrap();
        let _b = create_fact(k, &alice(), "note");
        k.forget(&alice(), &a, ForgetMode::Tombstone, None, None)
            .unwrap();
    }
    let (k1, _c1) = mk();
    let (k2, _c2) = mk();
    script(&k1);
    script(&k2);
    let j1 = k1.journal().unwrap();
    let j2 = k2.journal().unwrap();
    assert_eq!(
        j1, j2,
        "identical script + clock schedule => identical journal"
    );
    assert_eq!(j1.len(), 5);
    // byte-identical encodings too
    let b1: Vec<u8> = j1.iter().flat_map(|e| codec::encode_ke(e)).collect();
    let b2: Vec<u8> = j2.iter().flat_map(|e| codec::encode_ke(e)).collect();
    assert_eq!(b1, b2);
}

// ---------------------------------------------------------------------------
// concurrency (single-writer pipeline correctness)
// ---------------------------------------------------------------------------

#[test]
fn t25_concurrent_creators_get_unique_koids_and_gapless_journal() {
    let clock = Arc::new(ManualClock::new(50_000));
    let k = Arc::new(Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xAB).unwrap());
    let mut handles = Vec::new();
    for t in 0..4 {
        let k = k.clone();
        handles.push(std::thread::spawn(move || {
            let mut ids = Vec::new();
            for i in 0..25 {
                let s = Subject::new(&format!("worker-{}-{}", t, i));
                ids.push(
                    k.remember(RememberRequest::create(s, meta("fact")))
                        .unwrap()
                        .koid,
                );
            }
            ids
        }));
    }
    let mut all: BTreeSet<KOID> = BTreeSet::new();
    for h in handles {
        for id in h.join().unwrap() {
            assert!(all.insert(id), "duplicate KOID across threads");
        }
    }
    assert_eq!(all.len(), 100);
    let journal = k.journal().unwrap();
    assert_eq!(journal.len(), 100);
    // gapless sequence despite concurrency
    for (i, ke) in journal.iter().enumerate() {
        assert_eq!(ke.seq, (i + 1) as u64);
    }
}

// ---------------------------------------------------------------------------
// kernel reopen (journal recovery)
// ---------------------------------------------------------------------------

#[test]
fn t26_reopen_recovers_journal_head_and_continues_chain() {
    let clock = Arc::new(ManualClock::new(70_000));
    let store = Arc::new(MemoryEngine::new());
    let id;
    let (seq0, audit0);
    {
        let k = Kernel::open(store.clone(), clock.clone(), 1).unwrap();
        id = create_fact(&k, &alice(), "fact");
        let head = k.journal_head().unwrap();
        seq0 = head.0;
        audit0 = head.1;
    }
    // "restart": new Kernel over the same store
    let k2 = Kernel::open(store.clone(), clock.clone(), 1).unwrap();
    let (seq, audit) = k2.journal_head().unwrap();
    assert_eq!((seq, audit), (seq0, audit0));
    // chain continues unbroken after reopen
    k2.remember(RememberRequest::update(alice(), id, meta("fact")))
        .unwrap();
    let proof = k2.prove(&alice(), &id).unwrap();
    assert!(proof.chain_valid);
    assert_eq!(proof.events, 2);
}

// ---------------------------------------------------------------------------
// MRFC-0060 Phase C2: uniqueness constraints
// ---------------------------------------------------------------------------

#[test]
fn t06m_unique_constraint_prevents_duplicate() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("User", 1).unique(&["email"], UniquenessScope::Type));
    let mut r1 = RememberRequest::create(alice(), meta("User"));
    r1.properties
        .insert("email".into(), Value::Text("dup@b.com".into()));
    assert!(k.remember(r1).is_ok());

    // Same email → rejected
    let mut r2 = RememberRequest::create(alice(), meta("User"));
    r2.properties
        .insert("email".into(), Value::Text("dup@b.com".into()));
    let err = k.remember(r2).unwrap_err();
    assert!(matches!(err, KError::InvalidSchema(_)));
}

#[test]
fn t06n_composite_unique_enforced() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("Org", 1).unique(&["tenant", "slug"], UniquenessScope::Type));
    let mut r1 = RememberRequest::create(alice(), meta("Org"));
    r1.properties
        .insert("tenant".into(), Value::Text("t1".into()));
    r1.properties
        .insert("slug".into(), Value::Text("acme".into()));
    assert!(k.remember(r1).is_ok());

    // Same tenant + same slug → rejected
    let mut r2 = RememberRequest::create(alice(), meta("Org"));
    r2.properties
        .insert("tenant".into(), Value::Text("t1".into()));
    r2.properties
        .insert("slug".into(), Value::Text("acme".into()));
    assert!(k.remember(r2).is_err());

    // Same tenant + different slug → ok
    let mut r3 = RememberRequest::create(alice(), meta("Org"));
    r3.properties
        .insert("tenant".into(), Value::Text("t1".into()));
    r3.properties
        .insert("slug".into(), Value::Text("beta".into()));
    assert!(k.remember(r3).is_ok());
}

#[test]
fn t06o_unique_update_same_value_does_not_conflict_with_self() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("User", 1).unique(&["name"], UniquenessScope::Type));
    let mut r1 = RememberRequest::create(alice(), meta("User"));
    r1.properties
        .insert("name".into(), Value::Text("Alice".into()));
    let res = k.remember(r1).unwrap();

    // Update same KO with same name → ok (not a conflict with itself)
    let mut r2 = RememberRequest::update(alice(), res.koid, meta("User"));
    r2.expected_version = Some(res.version);
    r2.properties
        .insert("name".into(), Value::Text("Alice".into()));
    assert!(k.remember(r2).is_ok());
}

// ---- MRFC-0060 Phase C3: cardinality + ontology relationship enforcement ----

#[test]
fn t06p_cardinality_1to1_prevents_duplicate_target() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    // Register ontology: Person worksAt Organization (1:1)
    let mut classes = BTreeMap::new();
    classes.insert(
        "Person".into(),
        ClassDef {
            name: "Person".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Organization".into(),
        ClassDef {
            name: "Organization".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "worksAt".into(),
        RelDef {
            name: "worksAt".into(),
            domain: Some("Person".into()),
            range: Some("Organization".into()),
            cardinality: Some(Cardinality::OneToOne),
            max_count: None,
        },
    );
    let ontology = OntologyDef {
        namespace: "test".into(),
        version: "1".into(),
        classes,
        relationships,
        property_defs: BTreeMap::new(),
        mappings: vec![],
    };
    k.register_ontology(OntologyRegistry::new(ontology).expect("valid ontology"));

    // Create target Organization
    let org = k
        .remember(RememberRequest::create(alice(), meta("Organization")))
        .unwrap();

    // First Person → worksAt → Org (ok)
    let mut r1 = RememberRequest::create(alice(), meta("Person"));
    r1.referential_policy = ReferentialPolicy::Enforced;
    r1.relationships.push(RelationshipRef {
        rel_type: "worksAt".into(),
        target: org.koid,
        direction: Direction::Outbound,
    });
    assert!(k.remember(r1).is_ok());

    // Second Person → worksAt → same Org (cardinality violation)
    let mut r2 = RememberRequest::create(alice(), meta("Person"));
    r2.referential_policy = ReferentialPolicy::Enforced;
    r2.relationships.push(RelationshipRef {
        rel_type: "worksAt".into(),
        target: org.koid,
        direction: Direction::Outbound,
    });
    let err = k.remember(r2).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));
}

#[test]
fn t06q_domain_mismatch_rejected_under_enforced() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    // Ontology: "worksAt" domain=Person
    let mut classes = BTreeMap::new();
    classes.insert(
        "Person".into(),
        ClassDef {
            name: "Person".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Document".into(),
        ClassDef {
            name: "Document".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "worksAt".into(),
        RelDef {
            name: "worksAt".into(),
            domain: Some("Person".into()),
            range: None,
            cardinality: None,
            max_count: None,
        },
    );
    let ontology = OntologyDef {
        namespace: "test".into(),
        version: "1".into(),
        classes,
        relationships,
        property_defs: BTreeMap::new(),
        mappings: vec![],
    };
    k.register_ontology(OntologyRegistry::new(ontology).expect("valid ontology"));

    // A Document cannot have a "worksAt" relationship (domain mismatch)
    let mut r = RememberRequest::create(alice(), meta("Document"));
    r.referential_policy = ReferentialPolicy::Enforced;
    r.relationships.push(RelationshipRef {
        rel_type: "worksAt".into(),
        target: KOID::ZERO,
        direction: Direction::Outbound,
    });
    let err = k.remember(r).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));
}

#[test]
fn t06r_range_mismatch_rejected_under_enforced() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    // Ontology: "worksAt" range=Organization
    let mut classes = BTreeMap::new();
    classes.insert(
        "Person".into(),
        ClassDef {
            name: "Person".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Organization".into(),
        ClassDef {
            name: "Organization".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Document".into(),
        ClassDef {
            name: "Document".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "worksAt".into(),
        RelDef {
            name: "worksAt".into(),
            domain: Some("Person".into()),
            range: Some("Organization".into()),
            cardinality: None,
            max_count: None,
        },
    );
    let ontology = OntologyDef {
        namespace: "test".into(),
        version: "1".into(),
        classes,
        relationships,
        property_defs: BTreeMap::new(),
        mappings: vec![],
    };
    k.register_ontology(OntologyRegistry::new(ontology).expect("valid ontology"));

    // Create a Document as target (wrong type for range=Organization)
    let doc = k
        .remember(RememberRequest::create(alice(), meta("Document")))
        .unwrap();

    // Person → worksAt → Document (range mismatch: Document ≠ Organization)
    let mut r = RememberRequest::create(alice(), meta("Person"));
    r.referential_policy = ReferentialPolicy::Enforced;
    r.relationships.push(RelationshipRef {
        rel_type: "worksAt".into(),
        target: doc.koid,
        direction: Direction::Outbound,
    });
    let err = k.remember(r).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));
}

#[test]
fn t06s_permissive_policy_skips_ontology_checks() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    // Same 1:1 ontology as t06p
    let mut classes = BTreeMap::new();
    classes.insert(
        "Person".into(),
        ClassDef {
            name: "Person".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Organization".into(),
        ClassDef {
            name: "Organization".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "worksAt".into(),
        RelDef {
            name: "worksAt".into(),
            domain: Some("Person".into()),
            range: Some("Organization".into()),
            cardinality: Some(Cardinality::OneToOne),
            max_count: None,
        },
    );
    let ontology = OntologyDef {
        namespace: "test".into(),
        version: "1".into(),
        classes,
        relationships,
        property_defs: BTreeMap::new(),
        mappings: vec![],
    };
    k.register_ontology(OntologyRegistry::new(ontology).expect("valid ontology"));

    let org = k
        .remember(RememberRequest::create(alice(), meta("Organization")))
        .unwrap();

    // Both use Permissive (default) — ontology checks are skipped
    let mut r1 = RememberRequest::create(alice(), meta("Person"));
    r1.relationships.push(RelationshipRef {
        rel_type: "worksAt".into(),
        target: org.koid,
        direction: Direction::Outbound,
    });
    assert!(k.remember(r1).is_ok());

    // Second Person → same Org also ok under Permissive
    let mut r2 = RememberRequest::create(alice(), meta("Person"));
    r2.relationships.push(RelationshipRef {
        rel_type: "worksAt".into(),
        target: org.koid,
        direction: Direction::Outbound,
    });
    assert!(k.remember(r2).is_ok());
}

// ---- MRFC-0060 Phase C3: enhanced cardinality (bidirectional, max_count, transact) ----

#[test]
fn t06t_one_to_many_inbound_exclusivity() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    let mut classes = BTreeMap::new();
    classes.insert(
        "Employee".into(),
        ClassDef {
            name: "Employee".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Department".into(),
        ClassDef {
            name: "Department".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "belongsTo".into(),
        RelDef {
            name: "belongsTo".into(),
            domain: Some("Employee".into()),
            range: Some("Department".into()),
            cardinality: Some(Cardinality::OneToMany),
            max_count: None,
        },
    );
    let ontology = OntologyDef {
        namespace: "test".into(),
        version: "1".into(),
        classes,
        relationships,
        property_defs: BTreeMap::new(),
        mappings: vec![],
    };
    k.register_ontology(OntologyRegistry::new(ontology).expect("valid ontology"));

    let dept = k
        .remember(RememberRequest::create(alice(), meta("Department")))
        .unwrap();

    // First Employee → belongsTo → Department (ok)
    let mut r1 = RememberRequest::create(alice(), meta("Employee"));
    r1.referential_policy = ReferentialPolicy::Enforced;
    r1.relationships.push(RelationshipRef {
        rel_type: "belongsTo".into(),
        target: dept.koid,
        direction: Direction::Outbound,
    });
    assert!(k.remember(r1).is_ok());

    // Second Employee → belongsTo → same Department (rejected — 1:N inbound exclusivity)
    let mut r2 = RememberRequest::create(alice(), meta("Employee"));
    r2.referential_policy = ReferentialPolicy::Enforced;
    r2.relationships.push(RelationshipRef {
        rel_type: "belongsTo".into(),
        target: dept.koid,
        direction: Direction::Outbound,
    });
    let err = k.remember(r2).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));
}

#[test]
fn t06u_max_count_enforced() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    let mut classes = BTreeMap::new();
    classes.insert(
        "Person".into(),
        ClassDef {
            name: "Person".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Project".into(),
        ClassDef {
            name: "Project".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "memberOf".into(),
        RelDef {
            name: "memberOf".into(),
            domain: Some("Person".into()),
            range: Some("Project".into()),
            cardinality: Some(Cardinality::ManyToMany),
            max_count: Some(2),
        },
    );
    let ontology = OntologyDef {
        namespace: "test".into(),
        version: "1".into(),
        classes,
        relationships,
        property_defs: BTreeMap::new(),
        mappings: vec![],
    };
    k.register_ontology(OntologyRegistry::new(ontology).expect("valid ontology"));

    let p1 = k
        .remember(RememberRequest::create(alice(), meta("Project")))
        .unwrap();
    let p2 = k
        .remember(RememberRequest::create(alice(), meta("Project")))
        .unwrap();
    let p3 = k
        .remember(RememberRequest::create(alice(), meta("Project")))
        .unwrap();

    // 3 memberOf relationships in one request → rejected (max 2)
    let mut r = RememberRequest::create(alice(), meta("Person"));
    r.referential_policy = ReferentialPolicy::Enforced;
    r.relationships.push(RelationshipRef {
        rel_type: "memberOf".into(),
        target: p1.koid,
        direction: Direction::Outbound,
    });
    r.relationships.push(RelationshipRef {
        rel_type: "memberOf".into(),
        target: p2.koid,
        direction: Direction::Outbound,
    });
    r.relationships.push(RelationshipRef {
        rel_type: "memberOf".into(),
        target: p3.koid,
        direction: Direction::Outbound,
    });
    let err = k.remember(r).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));
}

#[test]
fn t06v_max_count_within_limit_passes() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    let mut classes = BTreeMap::new();
    classes.insert(
        "Person".into(),
        ClassDef {
            name: "Person".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Project".into(),
        ClassDef {
            name: "Project".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "memberOf".into(),
        RelDef {
            name: "memberOf".into(),
            domain: Some("Person".into()),
            range: Some("Project".into()),
            cardinality: Some(Cardinality::ManyToMany),
            max_count: Some(2),
        },
    );
    let ontology = OntologyDef {
        namespace: "test".into(),
        version: "1".into(),
        classes,
        relationships,
        property_defs: BTreeMap::new(),
        mappings: vec![],
    };
    k.register_ontology(OntologyRegistry::new(ontology).expect("valid ontology"));

    let p1 = k
        .remember(RememberRequest::create(alice(), meta("Project")))
        .unwrap();
    let p2 = k
        .remember(RememberRequest::create(alice(), meta("Project")))
        .unwrap();

    // 2 memberOf relationships → ok (exactly at limit)
    let mut r = RememberRequest::create(alice(), meta("Person"));
    r.referential_policy = ReferentialPolicy::Enforced;
    r.relationships.push(RelationshipRef {
        rel_type: "memberOf".into(),
        target: p1.koid,
        direction: Direction::Outbound,
    });
    r.relationships.push(RelationshipRef {
        rel_type: "memberOf".into(),
        target: p2.koid,
        direction: Direction::Outbound,
    });
    assert!(k.remember(r).is_ok());
}

#[test]
fn t06w_one_to_one_outbound_source_side() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    let mut classes = BTreeMap::new();
    classes.insert(
        "Person".into(),
        ClassDef {
            name: "Person".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Organization".into(),
        ClassDef {
            name: "Organization".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "worksAt".into(),
        RelDef {
            name: "worksAt".into(),
            domain: Some("Person".into()),
            range: Some("Organization".into()),
            cardinality: Some(Cardinality::OneToOne),
            max_count: None,
        },
    );
    let ontology = OntologyDef {
        namespace: "test".into(),
        version: "1".into(),
        classes,
        relationships,
        property_defs: BTreeMap::new(),
        mappings: vec![],
    };
    k.register_ontology(OntologyRegistry::new(ontology).expect("valid ontology"));

    let org1 = k
        .remember(RememberRequest::create(alice(), meta("Organization")))
        .unwrap();
    let org2 = k
        .remember(RememberRequest::create(alice(), meta("Organization")))
        .unwrap();

    // One Person with two worksAt → rejected (1:1 outbound)
    let mut r = RememberRequest::create(alice(), meta("Person"));
    r.referential_policy = ReferentialPolicy::Enforced;
    r.relationships.push(RelationshipRef {
        rel_type: "worksAt".into(),
        target: org1.koid,
        direction: Direction::Outbound,
    });
    r.relationships.push(RelationshipRef {
        rel_type: "worksAt".into(),
        target: org2.koid,
        direction: Direction::Outbound,
    });
    let err = k.remember(r).unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));
}

#[test]
fn t06x_transact_cardinality_inbound_across_batch() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    let mut classes = BTreeMap::new();
    classes.insert(
        "Employee".into(),
        ClassDef {
            name: "Employee".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Department".into(),
        ClassDef {
            name: "Department".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "belongsTo".into(),
        RelDef {
            name: "belongsTo".into(),
            domain: Some("Employee".into()),
            range: Some("Department".into()),
            cardinality: Some(Cardinality::OneToMany),
            max_count: None,
        },
    );
    k.register_ontology(
        OntologyRegistry::new(OntologyDef {
            namespace: "test".into(),
            version: "1".into(),
            classes,
            relationships,
            property_defs: BTreeMap::new(),
            mappings: vec![],
        })
        .expect("valid ontology"),
    );

    // Create Department + two Employees in one transact
    let dept_koid = KOID::from_bytes([1u8; 16]);
    let emp1_koid = KOID::from_bytes([2u8; 16]);
    let emp2_koid = KOID::from_bytes([3u8; 16]);

    let mut r_dept = RememberRequest::create(alice(), meta("Department"));
    r_dept.koid = Some(dept_koid);
    r_dept.referential_policy = ReferentialPolicy::Enforced;

    let mut r1 = RememberRequest::create(alice(), meta("Employee"));
    r1.koid = Some(emp1_koid);
    r1.referential_policy = ReferentialPolicy::Enforced;
    r1.relationships.push(RelationshipRef {
        rel_type: "belongsTo".into(),
        target: dept_koid,
        direction: Direction::Outbound,
    });

    let mut r2 = RememberRequest::create(alice(), meta("Employee"));
    r2.koid = Some(emp2_koid);
    r2.referential_policy = ReferentialPolicy::Enforced;
    r2.relationships.push(RelationshipRef {
        rel_type: "belongsTo".into(),
        target: dept_koid,
        direction: Direction::Outbound,
    });

    // Both employees target same department → inbound cardinality violated intra-batch
    let err = k
        .transact(vec![
            TransactionOp::new(alice(), r_dept),
            TransactionOp::new(alice(), r1),
            TransactionOp::new(alice(), r2),
        ])
        .unwrap_err();
    assert!(matches!(err, KError::InvalidObject(_)));
}

#[test]
fn t06y_transact_range_check_within_batch() {
    use std::collections::BTreeMap;
    let (k, _c) = mk();

    let mut classes = BTreeMap::new();
    classes.insert(
        "Person".into(),
        ClassDef {
            name: "Person".into(),
            parent: None,
            description: None,
        },
    );
    classes.insert(
        "Organization".into(),
        ClassDef {
            name: "Organization".into(),
            parent: None,
            description: None,
        },
    );
    let mut relationships = BTreeMap::new();
    relationships.insert(
        "worksAt".into(),
        RelDef {
            name: "worksAt".into(),
            domain: Some("Person".into()),
            range: Some("Organization".into()),
            cardinality: None,
            max_count: None,
        },
    );
    k.register_ontology(
        OntologyRegistry::new(OntologyDef {
            namespace: "test".into(),
            version: "1".into(),
            classes,
            relationships,
            property_defs: BTreeMap::new(),
            mappings: vec![],
        })
        .expect("valid ontology"),
    );

    let org_koid = KOID::from_bytes([10u8; 16]);
    let person_koid = KOID::from_bytes([20u8; 16]);

    // Create both Organization and Person in one batch; Person targets Org
    let mut r_org = RememberRequest::create(alice(), meta("Organization"));
    r_org.koid = Some(org_koid);
    r_org.referential_policy = ReferentialPolicy::Enforced;

    let mut r_person = RememberRequest::create(alice(), meta("Person"));
    r_person.koid = Some(person_koid);
    r_person.referential_policy = ReferentialPolicy::Enforced;
    r_person.relationships.push(RelationshipRef {
        rel_type: "worksAt".into(),
        target: org_koid,
        direction: Direction::Outbound,
    });

    // Range check resolves target type from within-batch → should pass
    let results = k
        .transact(vec![
            TransactionOp::new(alice(), r_org),
            TransactionOp::new(alice(), r_person),
        ])
        .unwrap();
    assert_eq!(results.len(), 2);
}

// ---- MRFC-0060 Phase C5: transaction-aware constraints ----

#[test]
fn t06z_deferred_unique_intra_batch_conflict() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("User", 1).unique_deferred(&["email"], UniquenessScope::Type));

    let mut r1 = RememberRequest::create(alice(), meta("User"));
    r1.properties
        .insert("email".into(), Value::Text("dup@b.com".into()));
    let mut r2 = RememberRequest::create(alice(), meta("User"));
    r2.properties
        .insert("email".into(), Value::Text("dup@b.com".into()));

    // Both in one batch → deferred uniqueness catches intra-batch conflict
    let err = k
        .transact(vec![
            TransactionOp::new(alice(), r1),
            TransactionOp::new(alice(), r2),
        ])
        .unwrap_err();
    assert!(matches!(err, KError::InvalidSchema(_)));
    let msg = format!("{}", err);
    assert!(msg.contains("deferred") && msg.contains("within batch"));
}

#[test]
fn t06za_deferred_unique_storage_conflict() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("User", 1).unique_deferred(&["email"], UniquenessScope::Type));

    // Commit one first
    let mut r1 = RememberRequest::create(alice(), meta("User"));
    r1.properties
        .insert("email".into(), Value::Text("exists@b.com".into()));
    k.remember(r1).unwrap();

    // Then try another with same email in transact → deferred catches storage conflict
    let mut r2 = RememberRequest::create(alice(), meta("User"));
    r2.properties
        .insert("email".into(), Value::Text("exists@b.com".into()));
    let err = k
        .transact(vec![TransactionOp::new(alice(), r2)])
        .unwrap_err();
    assert!(matches!(err, KError::InvalidSchema(_)));
    assert!(format!("{}", err).contains("already exists"));
}

#[test]
fn t06zb_deferred_check_constraint_in_transact() {
    let (k, _c) = mk();
    k.register_schema(
        Schema::new("Event", 1)
            .property("start_date", "Text")
            .property("end_date", "Text")
            .check_deferred(
                "end_ge_start",
                CheckExpression::Compare {
                    op: CompareOp::Gte,
                    left: Box::new(CheckExpression::Property("end_date".into())),
                    right: Box::new(CheckExpression::Property("start_date".into())),
                },
            ),
    );

    let mut r1 = RememberRequest::create(alice(), meta("Event"));
    r1.properties
        .insert("start_date".into(), Value::Text("2024-06-01".into()));
    r1.properties
        .insert("end_date".into(), Value::Text("2024-01-01".into())); // end < start → violation

    let err = k
        .transact(vec![TransactionOp::new(alice(), r1)])
        .unwrap_err();
    assert!(matches!(err, KError::InvalidSchema(_)));
    assert!(format!("{}", err).contains("end_ge_start"));
}

#[test]
fn t06zc_deferred_unique_passes_when_no_conflict() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("User", 1).unique_deferred(&["email"], UniquenessScope::Type));

    let mut r1 = RememberRequest::create(alice(), meta("User"));
    r1.properties
        .insert("email".into(), Value::Text("a@b.com".into()));
    let mut r2 = RememberRequest::create(alice(), meta("User"));
    r2.properties
        .insert("email".into(), Value::Text("b@c.com".into()));

    // Different emails → no conflict
    let results = k
        .transact(vec![
            TransactionOp::new(alice(), r1),
            TransactionOp::new(alice(), r2),
        ])
        .unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn t06zd_immediate_unique_still_fails_fast_in_transact() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("User", 1).unique(&["email"], UniquenessScope::Type));

    // Commit one first
    let mut r1 = RememberRequest::create(alice(), meta("User"));
    r1.properties
        .insert("email".into(), Value::Text("immediate@b.com".into()));
    k.remember(r1).unwrap();

    // transact with same email → immediate constraint fails fast (not deferred)
    let mut r2 = RememberRequest::create(alice(), meta("User"));
    r2.properties
        .insert("email".into(), Value::Text("immediate@b.com".into()));
    let err = k
        .transact(vec![TransactionOp::new(alice(), r2)])
        .unwrap_err();
    assert!(matches!(err, KError::InvalidSchema(_)));
    // Immediate violation message does NOT mention "deferred"
    assert!(!format!("{}", err).contains("deferred"));
}

// --- MRFC-0060 Phase C6: Constraint Dependency Graph ---

#[test]
fn t06ze_write_set_filters_unaffected_check_constraints() {
    // A check constraint on "end_date >= start_date" references only those two
    // properties. Updating an unrelated property ("title") should skip the
    // constraint evaluation entirely via write-set filtering.
    let (k, _c) = mk();
    k.register_schema(
        Schema::new("Event", 1)
            .property("title", "Text")
            .property("start_date", "Text")
            .property("end_date", "Text")
            .check(
                "end_ge_start",
                CheckExpression::Compare {
                    op: CompareOp::Gte,
                    left: Box::new(CheckExpression::Property("end_date".into())),
                    right: Box::new(CheckExpression::Property("start_date".into())),
                },
            ),
    );

    let mut r1 = RememberRequest::create(alice(), meta("Event"));
    r1.properties
        .insert("title".into(), Value::Text("Conf".into()));
    r1.properties
        .insert("start_date".into(), Value::Text("2024-06-01".into()));
    r1.properties
        .insert("end_date".into(), Value::Text("2024-06-30".into()));
    let rem = k.remember(r1).unwrap();

    // Update only "title" — include all properties but only change title.
    // write-set = {"title"}, which doesn't intersect {"end_date", "start_date"}.
    let mut r2 = RememberRequest::update(alice(), rem.koid, meta("Event"));
    r2.expected_version = Some(rem.version);
    r2.properties
        .insert("title".into(), Value::Text("NewTitle".into()));
    r2.properties
        .insert("start_date".into(), Value::Text("2024-06-01".into()));
    r2.properties
        .insert("end_date".into(), Value::Text("2024-06-30".into()));
    let rem2 = k.remember(r2).unwrap();
    assert_eq!(rem2.version, rem.version + 1);
}

#[test]
fn t06zf_update_no_property_change_skim_passes() {
    // Skim optimization: empty write-set on update → zero constraint overhead.
    let (k, _c) = mk();
    k.register_schema(Schema::new("Item", 1).property("name", "Text").check(
        "name_not_empty",
        CheckExpression::Compare {
            op: CompareOp::Neq,
            left: Box::new(CheckExpression::Property("name".into())),
            right: Box::new(CheckExpression::Literal(Value::Text("".into()))),
        },
    ));

    let mut r1 = RememberRequest::create(alice(), meta("Item"));
    r1.properties
        .insert("name".into(), Value::Text("ValidName".into()));
    let rem = k.remember(r1).unwrap();

    // Update with identical properties — write-set is empty → skim.
    let mut r2 = RememberRequest::update(alice(), rem.koid, meta("Item"));
    r2.expected_version = Some(rem.version);
    r2.properties
        .insert("name".into(), Value::Text("ValidName".into()));
    let rem2 = k.remember(r2).unwrap();
    assert_eq!(rem2.version, rem.version + 1);
}

// --- MRFC-0060 Phase C7: Connector Pushdown conformance --------------------

/// Wraps MemoryEngine to override constraint capabilities.
struct CapableEngine {
    inner: MemoryEngine,
    caps: ConstraintCapabilities,
}
impl CapableEngine {
    fn new(caps: ConstraintCapabilities) -> Self {
        CapableEngine {
            inner: MemoryEngine::new(),
            caps,
        }
    }
}
impl StorageEngine for CapableEngine {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>> {
        self.inner.get(key)
    }
    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.scan(prefix)
    }
    fn write_batch(&self, batch: &WriteBatch) -> KResult<()> {
        self.inner.write_batch(batch)
    }
    fn constraint_capabilities(&self) -> ConstraintCapabilities {
        self.caps
    }
}

#[test]
fn t06zg_check_pushdown_skips_kernel_evaluation() {
    // Backend claims `check: true` — kernel skips check constraint evaluation.
    let caps = ConstraintCapabilities {
        check: true,
        unique: false,
        not_null: false,
    };
    let engine = Arc::new(CapableEngine::new(caps));
    let clock = Arc::new(ManualClock::new(1000));
    let k = Kernel::open(engine, clock.clone(), 42).unwrap();

    // Register a schema with a check constraint that would normally reject.
    k.register_schema(Schema::new("Item", 1).property("qty", "Int").check(
        "qty_positive",
        CheckExpression::Compare {
            op: CompareOp::Gt,
            left: Box::new(CheckExpression::Property("qty".into())),
            right: Box::new(CheckExpression::Literal(Value::Int(0))),
        },
    ));

    // qty=-5 violates qty > 0, but kernel trusts backend → write succeeds.
    let mut r = RememberRequest::create(alice(), meta("Item"));
    r.properties.insert("qty".into(), Value::Int(-5));
    let result = k.remember(r);
    assert!(
        result.is_ok(),
        "kernel should trust backend and skip check; got {:?}",
        result.err()
    );
}

#[test]
fn t06zh_default_capabilities_still_enforce() {
    // Default MemoryEngine (all-false) still enforces everything.
    let (k, _) = mk();
    k.register_schema(Schema::new("Item", 1).property("qty", "Int").check(
        "qty_positive",
        CheckExpression::Compare {
            op: CompareOp::Gt,
            left: Box::new(CheckExpression::Property("qty".into())),
            right: Box::new(CheckExpression::Literal(Value::Int(0))),
        },
    ));

    let mut r = RememberRequest::create(alice(), meta("Item"));
    r.properties.insert("qty".into(), Value::Int(-5));
    let result = k.remember(r);
    assert!(
        result.is_err(),
        "default engine should enforce check constraints"
    );
}

#[test]
fn t06zi_not_null_pushdown_skips_required_check() {
    // Backend claims `not_null: true` — kernel skips required property checks.
    let caps = ConstraintCapabilities {
        check: false,
        unique: false,
        not_null: true,
    };
    let engine = Arc::new(CapableEngine::new(caps));
    let clock = Arc::new(ManualClock::new(1000));
    let k = Kernel::open(engine, clock.clone(), 42).unwrap();

    k.register_schema(Schema::new("Item", 1).required_property("name", "Text"));

    // Missing required "name" — kernel trusts backend's NOT NULL enforcement.
    let r = RememberRequest::create(alice(), meta("Item"));
    let result = k.remember(r);
    assert!(
        result.is_ok(),
        "kernel should skip required check; got {:?}",
        result.err()
    );
}

#[test]
fn t06zj_unique_pushdown_skips_immediate_uniqueness_in_transact() {
    // Backend claims `unique: true` — kernel skips immediate uniqueness check in transact.
    let caps = ConstraintCapabilities {
        check: false,
        unique: true,
        not_null: false,
    };
    let engine = Arc::new(CapableEngine::new(caps));
    let clock = Arc::new(ManualClock::new(1000));
    let k = Kernel::open(engine, clock.clone(), 42).unwrap();

    k.register_schema(Schema::new("User", 1).unique(&["email"], UniquenessScope::Type));

    // First object with email X.
    let mut r1 = RememberRequest::create(alice(), meta("User"));
    r1.properties
        .insert("email".into(), Value::Text("dup@x.com".into()));
    let op1 = TransactionOp::new(alice(), r1);

    // Second object with same email X — would conflict but backend handles it.
    let mut r2 = RememberRequest::create(alice(), meta("User"));
    r2.properties
        .insert("email".into(), Value::Text("dup@x.com".into()));
    let op2 = TransactionOp::new(alice(), r2);

    let result = k.transact(vec![op1, op2]);
    assert!(
        result.is_ok(),
        "kernel should trust backend uniqueness; got {:?}",
        result.err()
    );
}

// --- C8: constraint inference ---

#[test]
fn t06zk_inference_discovers_uniqueness_candidate() {
    let (k, _clock) = mk();
    k.register_schema(Schema::new("User", 1).property("email", "Text"));

    // Create 3 users with distinct emails.
    let mut r1 = RememberRequest::create(alice(), meta("User"));
    r1.properties
        .insert("email".into(), Value::Text("a@b.com".into()));
    k.remember(r1).unwrap();

    let mut r2 = RememberRequest::create(alice(), meta("User"));
    r2.properties
        .insert("email".into(), Value::Text("c@d.com".into()));
    k.remember(r2).unwrap();

    let mut r3 = RememberRequest::create(alice(), meta("User"));
    r3.properties
        .insert("email".into(), Value::Text("e@f.com".into()));
    k.remember(r3).unwrap();

    let candidates = k.infer_constraints(&alice(), "User").unwrap();
    let unique = candidates
        .iter()
        .find(|c| c.constraint_desc.contains("UNIQUE"));
    assert!(unique.is_some(), "should infer UNIQUE(email)");
    let u = unique.unwrap();
    assert_eq!(u.type_name, "User");
    assert_eq!(u.confidence, 1.0);
    assert_eq!(u.violations, 0);
}

#[test]
fn t06zl_inference_never_auto_enforces() {
    // AC-18: inferred constraints must never auto-promote to ENFORCED.
    let (k, _clock) = mk();
    k.register_schema(Schema::new("Item", 1).property("value", "Int"));

    // Create items with duplicate values (would violate UNIQUE).
    let mut r1 = RememberRequest::create(alice(), meta("Item"));
    r1.properties.insert("value".into(), Value::Int(42));
    k.remember(r1).unwrap();

    let mut r2 = RememberRequest::create(alice(), meta("Item"));
    r2.properties.insert("value".into(), Value::Int(42));
    k.remember(r2).unwrap();

    // Inference shows duplicates...
    let candidates = k.infer_constraints(&alice(), "Item").unwrap();
    let unique = candidates
        .iter()
        .find(|c| c.constraint_desc.contains("UNIQUE"));
    assert!(unique.is_some());
    assert!(unique.unwrap().violations > 0);

    // ...but writes still succeed because nothing was auto-registered.
    let mut r3 = RememberRequest::create(alice(), meta("Item"));
    r3.properties.insert("value".into(), Value::Int(42));
    let result = k.remember(r3);
    assert!(
        result.is_ok(),
        "AC-18 violated: inference auto-enforced a constraint; got {:?}",
        result.err()
    );
}

// --- C9 Programmable Constraints ---

#[test]
fn t06zm_arithmetic_check_constraint_enforced() {
    // Register schema with arithmetic check: @total == @price * @quantity
    let (k, _clock) = mk();
    let schema = Schema::new("Order", 1)
        .property("total", "Int")
        .property("price", "Int")
        .property("quantity", "Int")
        .check(
            "total_eq_price_times_qty",
            CheckExpression::Compare {
                op: CompareOp::Eq,
                left: Box::new(CheckExpression::Property("total".into())),
                right: Box::new(CheckExpression::Arith(
                    Box::new(CheckExpression::Property("price".into())),
                    ArithOp::Mul,
                    Box::new(CheckExpression::Property("quantity".into())),
                )),
            },
        );
    k.register_schema(schema);

    // Valid: total == price * quantity
    let mut r1 = RememberRequest::create(alice(), meta("Order"));
    r1.properties.insert("total".into(), Value::Int(100));
    r1.properties.insert("price".into(), Value::Int(20));
    r1.properties.insert("quantity".into(), Value::Int(5));
    assert!(k.remember(r1).is_ok());

    // Invalid: total != price * quantity → constraint violation
    let mut r2 = RememberRequest::create(alice(), meta("Order"));
    r2.properties.insert("total".into(), Value::Int(999));
    r2.properties.insert("price".into(), Value::Int(20));
    r2.properties.insert("quantity".into(), Value::Int(5));
    let result = k.remember(r2);
    assert!(result.is_err());
    let err = format!("{:?}", result.unwrap_err());
    assert!(err.contains("total_eq_price_times_qty"));
}

#[test]
fn t06zn_conditional_if_constraint() {
    // IF @status == "active" THEN @email != NULL ELSE true
    let (k, _clock) = mk();
    let schema = Schema::new("User", 1)
        .property("status", "Text")
        .nullable_property("email", "Text")
        .check(
            "active_requires_email",
            CheckExpression::If(
                Box::new(CheckExpression::Compare {
                    op: CompareOp::Eq,
                    left: Box::new(CheckExpression::Property("status".into())),
                    right: Box::new(CheckExpression::Literal(Value::Text("active".into()))),
                }),
                Box::new(CheckExpression::Compare {
                    op: CompareOp::Neq,
                    left: Box::new(CheckExpression::Property("email".into())),
                    right: Box::new(CheckExpression::Literal(Value::Null)),
                }),
                Box::new(CheckExpression::Literal(Value::Bool(true))),
            ),
        );
    k.register_schema(schema);

    // Active with email → ok
    let mut r1 = RememberRequest::create(alice(), meta("User"));
    r1.properties
        .insert("status".into(), Value::Text("active".into()));
    r1.properties
        .insert("email".into(), Value::Text("u@x.com".into()));
    assert!(k.remember(r1).is_ok());

    // Active with null → violated
    let mut r2 = RememberRequest::create(alice(), meta("User"));
    r2.properties
        .insert("status".into(), Value::Text("active".into()));
    r2.properties.insert("email".into(), Value::Null);
    let result = k.remember(r2);
    assert!(result.is_err());

    // Inactive with null → ok (else branch)
    let mut r3 = RememberRequest::create(alice(), meta("User"));
    r3.properties
        .insert("status".into(), Value::Text("inactive".into()));
    r3.properties.insert("email".into(), Value::Null);
    assert!(k.remember(r3).is_ok());
}

// ── AC-05: UniquenessScope enforcement ──

fn meta_tenant(t: &str, tenant: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: Some(tenant.into()),
        schema_version: 1,
        tags: vec![],
    }
}

#[test]
fn t06zo_tenant_scoped_unique_allows_same_value_different_tenants() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("User", 1).unique(&["email"], UniquenessScope::Tenant));

    // t1 owns email
    let mut r1 = RememberRequest::create(alice(), meta_tenant("User", "t1"));
    r1.properties
        .insert("email".into(), Value::Text("x@y.com".into()));
    assert!(k.remember(r1).is_ok());

    // t2 can use the same email — different tenant
    let mut r2 = RememberRequest::create(alice(), meta_tenant("User", "t2"));
    r2.properties
        .insert("email".into(), Value::Text("x@y.com".into()));
    assert!(k.remember(r2).is_ok());
}

#[test]
fn t06zp_tenant_scoped_unique_rejects_same_tenant() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("User", 1).unique(&["email"], UniquenessScope::Tenant));

    let mut r1 = RememberRequest::create(alice(), meta_tenant("User", "t1"));
    r1.properties
        .insert("email".into(), Value::Text("x@y.com".into()));
    assert!(k.remember(r1).is_ok());

    // Same tenant + same email → rejected
    let mut r2 = RememberRequest::create(alice(), meta_tenant("User", "t1"));
    r2.properties
        .insert("email".into(), Value::Text("x@y.com".into()));
    let err = k.remember(r2).unwrap_err();
    assert!(matches!(err, KError::InvalidSchema(_)));
}

#[test]
fn t06zq_global_unique_rejects_across_types() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("Alpha", 1).unique(&["code"], UniquenessScope::Global));
    k.register_schema(Schema::new("Beta", 1).unique(&["code"], UniquenessScope::Global));

    // Alpha owns code
    let mut r1 = RememberRequest::create(alice(), meta("Alpha"));
    r1.properties
        .insert("code".into(), Value::Text("GLOBAL-1".into()));
    assert!(k.remember(r1).is_ok());

    // Beta with same code → rejected (global scope crosses types)
    let mut r2 = RememberRequest::create(alice(), meta("Beta"));
    r2.properties
        .insert("code".into(), Value::Text("GLOBAL-1".into()));
    let err = k.remember(r2).unwrap_err();
    assert!(matches!(err, KError::InvalidSchema(_)));
}

// ── AC-22: schema migration validation ──

#[test]
fn t06zr_migration_detects_violations() {
    let (k, _c) = mk();
    // Register relaxed schema v1: no constraints on price
    k.register_schema(Schema::new("Item", 1).property("price", "Int"));
    let mut r1 = RememberRequest::create(alice(), meta("Item"));
    r1.properties.insert("price".into(), Value::Int(-5));
    assert!(k.remember(r1).is_ok());

    // Propose stricter schema v2: price >= 0 via check constraint
    let v2 = Schema::new("Item", 2).property("price", "Int").check(
        "price_non_negative",
        CheckExpression::Compare {
            op: CompareOp::Gte,
            left: Box::new(CheckExpression::Property("price".into())),
            right: Box::new(CheckExpression::Literal(Value::Int(0))),
        },
    );
    let violations = k.validate_schema_migration(&alice(), &v2).unwrap();
    assert!(!violations.is_empty());
    // Violation should carry the KOID
    assert!(violations.iter().any(|v| v.koid.is_some()));
}

#[test]
fn t06zs_migration_passes_clean_data() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("Item", 1).property("price", "Int"));
    let mut r1 = RememberRequest::create(alice(), meta("Item"));
    r1.properties.insert("price".into(), Value::Int(42));
    assert!(k.remember(r1).is_ok());

    // v2 requires price >= 0 — existing data is fine
    let v2 = Schema::new("Item", 2).property("price", "Int").check(
        "price_non_negative",
        CheckExpression::Compare {
            op: CompareOp::Gte,
            left: Box::new(CheckExpression::Property("price".into())),
            right: Box::new(CheckExpression::Literal(Value::Int(0))),
        },
    );
    let violations = k.validate_schema_migration(&alice(), &v2).unwrap();
    assert!(violations.is_empty());
}

// ── AC-17: provenance-required properties ──

#[test]
fn t06zu_provenance_required_rejects_missing_source() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("Fact", 1).provenance_required_property("claim", "Text"));
    // No semantic source → rejected
    let mut r = RememberRequest::create(alice(), meta("Fact"));
    r.properties
        .insert("claim".into(), Value::Text("sky is blue".into()));
    let err = k.remember(r).unwrap_err();
    assert!(matches!(err, KError::InvalidSchema(_)));
}

#[test]
fn t06zv_provenance_required_accepts_sourced() {
    let (k, _c) = mk();
    k.register_schema(Schema::new("Fact", 1).provenance_required_property("claim", "Text"));
    let mut r = RememberRequest::create(alice(), meta("Fact"));
    r.properties
        .insert("claim".into(), Value::Text("sky is blue".into()));
    r.semantic = Some(SemanticBlock {
        embedding_model: None,
        embedding: None,
        confidence: None,
        source: Some("sec-filing".into()),
        summary: None,
    });
    assert!(k.remember(r).is_ok());
}

// ---------------------------------------------------------------------------
// Phase R8 — Security: authorisation hardening tests
// ---------------------------------------------------------------------------

#[test]
fn t27_non_owner_denied_without_acl_grant() {
    let k = mk().0;
    let alice = Subject::new("alice");
    let bob = Subject::new("bob");

    // Bob creates a document with no ACL — only the owner can access.
    let mut r = RememberRequest::create(bob, meta("Document"));
    r.properties
        .insert("title".into(), Value::Text("bob-private".into()));
    let created = k.remember(r).unwrap();

    // Alice (non-owner, no ACL grant) cannot read.
    assert!(
        k.get(&alice, &created.koid).is_err(),
        "non-owner without ACL grant must be denied"
    );
}

#[test]
fn t28_acl_read_allowed_delete_denied() {
    let k = mk().0;
    let alice = Subject::new("alice");
    let bob = Subject::new("bob");

    // Bob creates a document with an ACL granting Alice read-only.
    let mut r = RememberRequest::create(bob, meta("Document"));
    r.properties
        .insert("title".into(), Value::Text("bob-shared".into()));
    r.security = Some(SecurityDescriptor {
        owner: "bob".into(),
        acl: vec![AclEntry {
            principal: "alice".into(),
            action: Action::Read,
            effect: Effect::Allow,
        }],
        classification: None,
    });
    let created = k.remember(r).unwrap();

    // Alice can read (ACL allows).
    assert!(
        k.get(&alice, &created.koid).is_ok(),
        "ACL-granted read must work"
    );
    // But Alice cannot delete (no write/delete ACL entry).
    assert!(
        k.forget(&alice, &created.koid, ForgetMode::Tombstone, None, None)
            .is_err(),
        "delete without ACL grant must be denied"
    );
}

#[test]
fn t29_anonymous_and_unauthorised_access_denied() {
    let k = mk().0;
    let alice = Subject::new("alice");
    let mut r = RememberRequest::create(alice, meta("Document"));
    r.properties
        .insert("title".into(), Value::Text("needs-session".into()));
    let created = k.remember(r).unwrap();

    // Empty-name subject (unauthenticated) cannot read.
    assert!(
        k.get(&Subject::new(""), &created.koid).is_err(),
        "anonymous read must be denied"
    );

    // Different user with no grant cannot read.
    assert!(
        k.get(&Subject::new("eve"), &created.koid).is_err(),
        "unauthorised read must be denied"
    );
}
