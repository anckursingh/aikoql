//! MVP-QA-002 Suite A — concurrency invariants (QA2-CONC-002..006 kernel legs).
//!
//! The spec (W2-09): concurrent mutation must preserve all invariants —
//! no duplicate KO explosion, no impossible hybrid update/delete state,
//! no orphan relationship endpoints, one current truth per instant, and
//! authorization that never leaks during grant/revoke racing mutation.
//!
//! QA2-CONC-001 (four reader shapes incl. context compilation) lives in
//! `crates/ingestion/tests/qa2_concurrency.rs` — the compiler is an
//! ingestion-crate component and the test drives it against a live kernel.

use aikoql_kernel::*;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Barrier, Mutex};

fn mk() -> (Kernel, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0xC0FFEE).unwrap();
    (k, clock)
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

fn create(k: &Kernel, name: &str) -> KOID {
    let mut req = RememberRequest::create(alice(), meta("fact"));
    req.properties
        .insert("name".into(), Value::Text(name.into()));
    k.remember(req).unwrap().koid
}

// ---------------------------------------------------------------------------
// QA2-CONC-002 — concurrent ingestion commits exactly once per key
// ---------------------------------------------------------------------------

#[test]
fn w2_conc_002a_concurrent_same_key_ingest_commits_exactly_once() {
    let (k, _) = mk();
    let k = Arc::new(k);
    const SHARED: usize = 4; // all share one idempotency key
    const DISTINCT: usize = 4; // control leg: one key each
    let mut handles = Vec::new();
    for t in 0..SHARED + DISTINCT {
        let k = k.clone();
        handles.push(std::thread::spawn(move || {
            let mut props = PropertyMap::new();
            props.insert("n".into(), Value::Int(42));
            let key = if t < SHARED {
                "qa2-conc-002a-shared".to_string()
            } else {
                format!("qa2-conc-002a-{t}")
            };
            k.ingest_observation(IngestRequest {
                context: alice().into(),
                type_name: "fact".into(),
                properties: props,
                evidence: vec![Evidence::new(
                    "qa2-conc-002a.md",
                    EvidenceMethod::DocExtraction,
                )],
                idempotency_key: Some(key),
                tags: vec![],
                valid_from: None,
                security: None,
                note: None,
            })
        }));
    }
    let results: Vec<KOID> = handles
        .into_iter()
        .map(|h| h.join().unwrap().unwrap().koid)
        .collect();

    // The shared key commits exactly once; every thread saw the same KO.
    let (shared, distinct): (Vec<_>, Vec<_>) =
        results.iter().enumerate().partition(|(i, _)| *i < SHARED);
    assert!(
        shared.windows(2).all(|w| w[0].1 == w[1].1),
        "same idempotency key must resolve to one KO: {results:?}"
    );
    assert!(
        distinct
            .iter()
            .all(|(i, id)| id != &shared[0].1 || *i >= SHARED),
        "distinct keys must not collide with the shared KO"
    );
    // 1 shared + DISTINCT control entries — no duplicate KO explosion.
    let journal = k.journal().unwrap();
    assert_eq!(
        journal.len(),
        1 + DISTINCT,
        "journal must hold exactly one entry per logical ingest (got {})",
        journal.len()
    );
    let ko = k.get(alice(), shared[0].1).unwrap();
    assert_eq!(ko.version, 1, "shared KO must be committed once");
}

// ---------------------------------------------------------------------------
// QA2-CONC-003 — concurrent update + delete never produce a hybrid state
// ---------------------------------------------------------------------------

#[test]
fn w2_conc_003_concurrent_update_and_delete_never_hybrid() {
    for round in 0..30 {
        let (k, _) = mk();
        let k = Arc::new(k);
        let mut seed = RememberRequest::create(alice(), meta("fact"));
        seed.properties.insert("n".into(), Value::Int(0));
        let id = k.remember(seed).unwrap().koid;

        // Both ops target version 1 — OCC guarantees exactly one commits.
        let updater = {
            let k = k.clone();
            std::thread::spawn(move || {
                let mut req = RememberRequest::update(alice(), id, meta("fact"));
                req.expected_version = Some(1);
                req.properties.insert("n".into(), Value::Int(99));
                k.remember(req)
            })
        };
        let deleter = {
            let k = k.clone();
            std::thread::spawn(move || k.forget(alice(), &id, ForgetMode::Tombstone, Some(1), None))
        };
        let upd = updater.join().unwrap();
        let del = deleter.join().unwrap();
        assert!(
            upd.is_ok() != del.is_ok(),
            "round {round}: exactly one of update/delete must win — upd={upd:?} del={del:?}"
        );

        let ko = k.get(alice(), &id).unwrap();
        match ko.lifecycle.state {
            LifecycleState::Deleted => assert_eq!(
                ko.properties.get("n"),
                Some(&Value::Int(0)),
                "round {round}: delete won — the update must not have landed (hybrid)"
            ),
            _ => assert_eq!(
                ko.properties.get("n"),
                Some(&Value::Int(99)),
                "round {round}: update won — content must be the new value"
            ),
        }
        assert_eq!(
            k.journal().unwrap().len(),
            2,
            "round {round}: seed + exactly one committed op"
        );
        assert!(
            k.prove(alice(), &id).unwrap().chain_valid,
            "round {round}: audit chain must stay valid"
        );
    }
}

// ---------------------------------------------------------------------------
// QA2-CONC-004 — concurrent relationship mutation leaves no orphan endpoints
// ---------------------------------------------------------------------------

#[test]
fn w2_conc_004_concurrent_relationship_mutation_leaves_no_orphans() {
    let (k, _) = mk();
    let k = Arc::new(k);
    let a = create(&k, "A");
    let b = create(&k, "B");
    let c = create(&k, "C");
    let tracked = vec![a, b, c];

    let flipper = {
        let k = k.clone();
        std::thread::spawn(move || {
            for i in 0..40u32 {
                // Retry on OCC conflict — the flip must eventually land.
                loop {
                    let head = k.get(alice(), &a).unwrap();
                    let mut req = RememberRequest::update(alice(), a, meta("fact"));
                    req.expected_version = Some(head.version);
                    let target = if i % 2 == 0 { b } else { c };
                    req.relationships.push(RelationshipRef {
                        rel_type: "owns".into(),
                        target,
                        direction: Direction::Outbound,
                    });
                    match k.remember(req) {
                        Ok(_) => break,
                        Err(KError::VersionConflict { .. }) => continue,
                        Err(e) => panic!("flip {i}: {e}"),
                    }
                }
            }
        })
    };
    // Deletes B once, without retry — whether it wins or loses is the point.
    let deleter = {
        let k = k.clone();
        std::thread::spawn(move || {
            let head = k.get(alice(), &b).unwrap();
            k.forget(alice(), &b, ForgetMode::Tombstone, Some(head.version), None)
        })
    };
    let creator = {
        let k = k.clone();
        std::thread::spawn(move || {
            for i in 0..10u32 {
                let d = create(&k, &format!("D{i}"));
                loop {
                    let head = k.get(alice(), &a).unwrap();
                    let mut req = RememberRequest::update(alice(), a, meta("fact"));
                    req.expected_version = Some(head.version);
                    // preserve the flipper's relation, append ours
                    req.relationships = head.relationships.clone();
                    req.relationships.push(RelationshipRef {
                        rel_type: "owns".into(),
                        target: d,
                        direction: Direction::Outbound,
                    });
                    match k.remember(req) {
                        Ok(_) => break,
                        Err(KError::VersionConflict { .. }) => continue,
                        Err(e) => panic!("creator {i}: {e}"),
                    }
                }
            }
        })
    };
    flipper.join().unwrap();
    let del = deleter.join().unwrap();
    creator.join().unwrap();

    // INV-002: every active relationship endpoint resolves to a valid
    // (existing, non-Deleted) entity. Walk the tracked KOs' final heads.
    let b_alive = del.is_err();
    for id in &tracked {
        let ko = k.get(alice(), id).unwrap();
        for rel in &ko.relationships {
            let target = k.get(alice(), &rel.target).unwrap_or_else(|e| {
                panic!("orphan: {} references missing {}: {e}", id, rel.target)
            });
            assert_ne!(
                target.lifecycle.state,
                LifecycleState::Deleted,
                "orphan: {} references Deleted {}",
                id,
                rel.target
            );
        }
        if *id == a && !b_alive {
            assert!(
                !ko.relationships.iter().any(|r| r.target == b),
                "A must not reference the deleted B"
            );
        }
    }
    assert!(k.prove(alice(), &a).unwrap().chain_valid);
}

// ---------------------------------------------------------------------------
// QA2-CONC-005 — concurrent overlapping temporal versions keep one head
// ---------------------------------------------------------------------------

#[test]
fn w2_conc_005_concurrent_overlapping_temporal_updates_keep_one_head() {
    let (k, _) = mk();
    let k = Arc::new(k);
    let id = create(&k, "temporal");

    const PER_THREAD: u64 = 25;
    let mut handles = Vec::new();
    for t in 0..2u64 {
        let k = k.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..PER_THREAD {
                let mut req = RememberRequest::update(alice(), id, meta("fact"));
                // overlapping windows: base + t*10 + i
                req.properties
                    .insert("n".into(), Value::Int((t * 1000 + i) as i64));
                req.extensions.insert(
                    KnowledgeObject::EXT_VALID_FROM.into(),
                    Value::Int((t * 10 + i) as i64),
                );
                k.remember(req).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Every update committed exactly once, in a gapless chain — and there is
    // exactly ONE head: two versions can never both be current.
    let journal = k.journal().unwrap();
    assert_eq!(journal.len(), 1 + (2 * PER_THREAD) as usize);
    for (i, ke) in journal.iter().enumerate() {
        assert_eq!(ke.seq, (i + 1) as u64, "gapless journal");
    }
    let head = k.get(alice(), &id).unwrap();
    assert_eq!(head.version, 1 + 2 * PER_THREAD);
    // The surviving head's stamp is one of the committed windows.
    match head.extensions.get(KnowledgeObject::EXT_VALID_FROM) {
        Some(Value::Int(v)) => {
            assert!(
                (0..2 * PER_THREAD as i64).contains(v),
                "head stamp {v} not in the written set"
            )
        }
        other => panic!("head lost its valid_from: {other:?}"),
    }
    // History: one committed version per update, strictly ordered.
    let lin = k.trace(alice(), &id).unwrap();
    assert_eq!(lin.versions.len(), 1 + (2 * PER_THREAD) as usize);
    for w in lin.versions.windows(2) {
        assert!(
            w[1].commit_ts > w[0].commit_ts,
            "commit order must be monotone"
        );
    }
}

// ---------------------------------------------------------------------------
// QA2-CONC-006 — grant/revoke racing mutation never leaks
// ---------------------------------------------------------------------------

fn sec_bob(effect: Effect) -> SecurityDescriptor {
    SecurityDescriptor {
        owner: "alice".into(),
        acl: vec![AclEntry {
            principal: "bob".into(),
            action: Action::Read,
            effect,
        }],
        classification: None,
    }
}

#[test]
fn w2_conc_006_grant_revoke_during_mutation_never_leaks() {
    let (k, _) = mk();
    let k = Arc::new(k);
    let bob = Subject::new("bob");

    let mut seed = RememberRequest::create(alice(), meta("fact"));
    seed.properties.insert("n".into(), Value::Int(0));
    seed.security = Some(sec_bob(Effect::Deny));
    let id = k.remember(seed).unwrap().koid;

    // Phase bookkeeping: readers check every Ok read against the commit
    // version of the phase's ACL flip. A read of version >= deny-v during
    // the deny phase is a leak; a read below allow-v during the allow
    // phase is stale-by-invalid-ACL (also a leak of inconsistency).
    const ROUNDS: usize = 6;
    const READERS: usize = 4;
    let phase = Arc::new(AtomicU8::new(0)); // 0 = deny phase, 1 = allow phase
    let flips = Arc::new(Mutex::new(vec![(0u64, 0u64); ROUNDS])); // (deny_v, allow_v)
    let barrier = Arc::new(Barrier::new(READERS + 1));
    let mut reader_handles = Vec::new();
    for r in 0..READERS {
        let k = k.clone();
        let phase = phase.clone();
        let flips = flips.clone();
        let barrier = barrier.clone();
        let bob = bob.clone();
        reader_handles.push(std::thread::spawn(move || {
            let mut last_ok_version = 0u64;
            for round in 0..ROUNDS {
                barrier.wait(); // deny phase committed
                let (deny_v, _) = flips.lock().unwrap()[round];
                for _ in 0..25 {
                    match k.get(&bob, &id) {
                        Ok(ko) => {
                            assert!(
                                phase.load(Ordering::SeqCst) == 0 && ko.version < deny_v,
                                "reader {r} round {round}: leak — Ok read at version {} \
                                 during deny phase (deny committed at {deny_v})",
                                ko.version
                            );
                            assert!(
                                ko.version >= last_ok_version,
                                "reader {r}: version regressed"
                            );
                            last_ok_version = ko.version;
                            match ko.properties.get("n") {
                                Some(Value::Int(n)) => {
                                    assert!((0..=ROUNDS as i64).contains(n), "torn n={n}")
                                }
                                other => panic!("reader {r}: content lost: {other:?}"),
                            }
                        }
                        Err(KError::AccessDenied { .. }) => {}
                        Err(e) => panic!("reader {r}: unexpected error {e}"),
                    }
                }
                barrier.wait(); // deny-phase reads done

                barrier.wait(); // allow phase committed
                let (_, allow_v) = flips.lock().unwrap()[round];
                for _ in 0..25 {
                    match k.get(&bob, &id) {
                        Ok(ko) => {
                            assert!(
                                phase.load(Ordering::SeqCst) == 1 && ko.version >= allow_v,
                                "reader {r} round {round}: inconsistency — Ok read at \
                                 version {} below the allow commit {allow_v}",
                                ko.version
                            );
                            assert!(
                                ko.version >= last_ok_version,
                                "reader {r}: version regressed"
                            );
                            last_ok_version = ko.version;
                        }
                        Err(KError::AccessDenied { .. }) => {}
                        Err(e) => panic!("reader {r}: unexpected error {e}"),
                    }
                }
                barrier.wait(); // allow-phase reads done
            }
        }));
    }

    let mut n = 1i64;
    for round in 0..ROUNDS {
        // deny flip; writer then proves the post-deny state is live by
        // committing one more update on top of it
        let head = k.get(alice(), &id).unwrap();
        let mut deny = RememberRequest::update(alice(), id, meta("fact"));
        deny.expected_version = Some(head.version);
        deny.security = Some(sec_bob(Effect::Deny));
        let deny_v = k.remember(deny).unwrap().version;
        let mut post = RememberRequest::update(alice(), id, meta("fact"));
        post.expected_version = Some(deny_v);
        post.properties.insert("n".into(), Value::Int(n));
        k.remember(post).unwrap();
        n += 1;
        flips.lock().unwrap()[round].0 = deny_v;
        phase.store(0, Ordering::SeqCst);
        barrier.wait();
        barrier.wait(); // deny reads done

        let head = k.get(alice(), &id).unwrap();
        let mut allow = RememberRequest::update(alice(), id, meta("fact"));
        allow.expected_version = Some(head.version);
        allow.security = Some(sec_bob(Effect::Allow));
        let allow_v = k.remember(allow).unwrap().version;
        let mut post = RememberRequest::update(alice(), id, meta("fact"));
        post.expected_version = Some(allow_v);
        post.properties.insert("n".into(), Value::Int(n));
        k.remember(post).unwrap();
        n += 1;
        flips.lock().unwrap()[round].1 = allow_v;
        phase.store(1, Ordering::SeqCst);
        barrier.wait();
        barrier.wait(); // allow reads done
    }
    for h in reader_handles {
        h.join().unwrap();
    }

    // Sequential control: the last committed flip was allow — bob's final
    // read succeeds at the post-allow version.
    let final_ko = k.get(&bob, &id).expect("final allow flip must grant bob");
    let (_, last_allow_v) = flips.lock().unwrap()[ROUNDS - 1];
    assert!(
        final_ko.version >= last_allow_v,
        "final read must see the post-allow head"
    );
}
