//! v0.3 AG-41 — Coordination primitives: claim/release/handoff on KOs.
//! Claims ride the kernel CAS (expected_version under the pipe lock) so a
//! task is claimed exactly once; leases are wall-clock so an agent's death
//! frees its tasks. Gate: N agents, M tasks — each task claimed exactly
//! once, claims expire on agent death. Handoffs transfer the claim and
//! leave an `aikoql:handoff` record with provenance.

use aikoql_kernel::*;
use std::sync::Arc;

fn mk_kernel() -> (Kernel, Arc<ManualClock>, Arc<MemoryEngine>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xE915).unwrap();
    (k, clock, store)
}

/// A task KO owned by the dispatcher, readable + writable by the pool.
fn mk_task(k: &Kernel, name: &str, agents: &[&str]) -> KOID {
    let mut rr = RememberRequest::create(
        Subject::new("dispatcher"),
        Metadata {
            type_name: "aikoql:task".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    rr.properties.insert("task".into(), Value::Text(name.into()));
    rr.security = Some(SecurityDescriptor {
        owner: "dispatcher".into(),
        acl: agents
            .iter()
            .flat_map(|a| {
                [
                    AclEntry {
                        principal: a.to_string(),
                        action: Action::Read,
                        effect: Effect::Allow,
                    },
                    AclEntry {
                        principal: a.to_string(),
                        action: Action::Write,
                        effect: Effect::Allow,
                    },
                ]
            })
            .collect(),
        classification: None,
    });
    k.remember(rr).unwrap().koid
}

// ---- the gate: N agents, M tasks, exactly-once ---------------------------------

#[test]
fn ag41_n_agents_claim_m_tasks_exactly_once() {
    let (k, _clock, _store) = mk_kernel();
    let agents: Vec<String> = (0..4).map(|i| format!("agent-{i}")).collect();
    let agent_refs: Vec<&str> = agents.iter().map(|s| s.as_str()).collect();
    let tasks: Vec<KOID> = (0..6)
        .map(|i| mk_task(&k, &format!("task-{i}"), &agent_refs))
        .collect();
    let k = Arc::new(k);
    let mut handles = Vec::new();
    for agent in &agents {
        let k = k.clone();
        let tasks = tasks.clone();
        let agent = agent.clone();
        handles.push(std::thread::spawn(move || {
            let mut won = Vec::new();
            for t in tasks {
                let req = ClaimRequest::new(Subject::new(agent.as_str()), t, 60);
                match k.claim_task(req) {
                    Ok(r) => {
                        assert!(!r.renewed, "a fresh claim must not be a renew");
                        won.push((t, agent.clone()));
                    }
                    Err(KError::ClaimConflict { .. }) => {}
                    Err(e) => panic!("{agent} on {t}: unexpected error {e:?}"),
                }
            }
            won
        }));
    }
    // Every task claimed, exactly once, by exactly one agent.
    let mut winners: Vec<(KOID, String)> = Vec::new();
    for h in handles {
        winners.extend(h.join().unwrap());
    }
    let mut seen: Vec<(KOID, String)> = Vec::new();
    for (t, who) in winners {
        assert!(
            !seen.iter().any(|(ot, _)| *ot == t),
            "task {t} claimed by more than one agent"
        );
        seen.push((t, who));
    }
    assert_eq!(seen.len(), tasks.len(), "every task must be claimed exactly once");
}

#[test]
fn ag41_claim_expires_when_the_agent_dies() {
    let (k, clock, _store) = mk_kernel();
    let task = mk_task(&k, "expiring", &["alice", "bob"]);
    let r = k
        .claim_task(ClaimRequest::new(Subject::new("alice"), task, 5))
        .unwrap();
    assert_eq!(r.claim_until, 10_000 + 5_000);
    // Alice died (no renewals). While the lease is live, bob must fail…
    assert!(matches!(
        k.claim_task(ClaimRequest::new(Subject::new("bob"), task, 5)),
        Err(KError::ClaimConflict { .. })
    ));
    // …and succeed once it expires (agent death frees the task).
    clock.tick(5_001);
    let r = k
        .claim_task(ClaimRequest::new(Subject::new("bob"), task, 5))
        .unwrap();
    assert!(!r.renewed);
    // A lease that overflows wall-clock millis is rejected, never wrapped.
    assert!(matches!(
        k.claim_task(ClaimRequest::new(Subject::new("bob"), task, u64::MAX)),
        Err(KError::InvalidObject(_))
    ));
}

#[test]
fn ag41_renew_and_release() {
    let (k, clock, _store) = mk_kernel();
    let task = mk_task(&k, "lease", &["alice", "bob"]);
    let r1 = k
        .claim_task(ClaimRequest::new(Subject::new("alice"), task, 5))
        .unwrap();
    clock.tick(2_000);
    // The holder re-claiming renews the lease from now.
    let r2 = k
        .claim_task(ClaimRequest::new(Subject::new("alice"), task, 5))
        .unwrap();
    assert!(r2.renewed);
    assert_eq!(r2.claim_until, k.clock_now() + 5_000);
    assert!(r2.claim_until > r1.claim_until);
    // Only the holder releases a live claim.
    assert!(matches!(
        k.release_task(ReleaseRequest::new(Subject::new("bob"), task)),
        Err(KError::ClaimConflict { .. })
    ));
    let rel = k
        .release_task(ReleaseRequest::new(Subject::new("alice"), task))
        .unwrap();
    assert_eq!(rel.koid, task);
    let ko = k.get(Subject::new("alice"), &task).unwrap();
    assert!(!ko.extensions.contains_key("claim_holder"));
    // Released → claimable by anyone in the pool; unclaimed → release errors.
    assert!(k
        .claim_task(ClaimRequest::new(Subject::new("bob"), task, 5))
        .is_ok());
    assert!(k
        .release_task(ReleaseRequest::new(Subject::new("bob"), task))
        .is_ok());
    assert!(matches!(
        k.release_task(ReleaseRequest::new(Subject::new("bob"), task)),
        Err(KError::InvalidObject(_))
    ));
}

#[test]
fn ag41_claim_requires_write_access_to_the_task() {
    let (k, _clock, _store) = mk_kernel();
    let task = mk_task(&k, "guarded", &["alice"]);
    assert!(matches!(
        k.claim_task(ClaimRequest::new(Subject::new("mallory"), task, 5)),
        Err(KError::AccessDenied { .. })
    ));
}

// ---- handoff ------------------------------------------------------------------

#[test]
fn ag41_handoff_transfers_the_claim_and_records_provenance() {
    let (k, _clock, _store) = mk_kernel();
    let task = mk_task(&k, "handoff", &["alice", "bob"]);
    let before = k
        .claim_task(ClaimRequest::new(Subject::new("alice"), task, 60))
        .unwrap();
    let mut req = HandoffRequest::new(Subject::new("alice"), task, "bob".into());
    req.evidence = vec![Evidence::new(
        "handoff message",
        EvidenceMethod::AgentAnalysis,
    )];
    req.note = Some("bob takes over the migration".into());
    let h = k.handoff_task(req).unwrap();
    // The claim moved to bob with the same lease end…
    let ko = k.get(Subject::new("bob"), &task).unwrap();
    assert_eq!(
        ko.extensions.get("claim_holder"),
        Some(&Value::Text("bob".into()))
    );
    assert_eq!(
        ko.extensions.get("claim_until"),
        Some(&Value::Int(before.claim_until as i64))
    );
    // …and the handoff KO records who/what/why with provenance.
    let hko = k.get(Subject::new("bob"), &h.koid).unwrap();
    assert_eq!(hko.metadata.type_name, "aikoql:handoff");
    assert_eq!(
        hko.properties.get("from"),
        Some(&Value::Text("alice".into()))
    );
    assert_eq!(hko.properties.get("to"), Some(&Value::Text("bob".into())));
    assert_eq!(
        hko.properties.get("task"),
        Some(&Value::Text(task.to_hex()))
    );
    assert_eq!(hko.epistemic_status(), EpistemicStatus::Asserted);
    assert!(!hko.evidence().is_empty(), "the handoff carries its evidence");
    // A non-holder cannot hand the task off (evidence present so the
    // failure is the claim, not the evidence gate).
    let mut rogue = HandoffRequest::new(Subject::new("alice"), task, "alice".into());
    rogue.evidence = vec![Evidence::new("rogue handoff", EvidenceMethod::AgentAnalysis)];
    assert!(matches!(k.handoff_task(rogue), Err(KError::ClaimConflict { .. })));
    // An unclaimed task cannot be handed off either.
    let free = mk_task(&k, "free", &["alice", "bob"]);
    let mut orphan = HandoffRequest::new(Subject::new("alice"), free, "bob".into());
    orphan.evidence = vec![Evidence::new("orphan handoff", EvidenceMethod::AgentAnalysis)];
    assert!(matches!(k.handoff_task(orphan), Err(KError::InvalidObject(_))));
}
