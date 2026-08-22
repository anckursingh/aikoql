//! v0.3 K5 — Agent Experience: record_experience / match_experiences as
//! first-class kernel ops — evidence-mandated capture, agent_derived
//! authority, TTL-bounded validity, reuse-condition gating,
//! confidence-weighted ranking, and ACL-scoped cross-agent reuse.
//! Acceptance targets: K5 exit criteria + cross-agent reuse proof.

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

fn record(
    k: &Kernel,
    who: &str,
    goal: &str,
    reuse_conditions: &[&str],
    ttl_seconds: Option<u64>,
    confidence: Option<f32>,
    shared_with: &[&str],
) -> KOID {
    let mut req = ExperienceRequest::new(Subject::new(who), goal, "ran the plan", "goal met");
    req.lesson = Some("keep going".into());
    req.reuse_conditions = reuse_conditions.iter().map(|s| s.to_string()).collect();
    req.evidence = vec![ev("run-log")];
    req.ttl_seconds = ttl_seconds;
    req.confidence = confidence;
    req.shared_with = shared_with.iter().map(|s| s.to_string()).collect();
    k.record_experience(req).unwrap().koid
}

// ---- capture ---------------------------------------------------------------

#[test]
fn record_experience_requires_evidence_and_complete_shape() {
    let (k, _clock, _store) = mk_kernel();
    // No evidence — an outcome nobody observed is not knowledge.
    let req = ExperienceRequest::new(Subject::new("alice"), "parse the file", "parsed", "ok");
    assert!(matches!(
        k.record_experience(req).unwrap_err(),
        KError::InvalidObject(_)
    ));
    // Empty goal is not an experience either.
    let mut req = ExperienceRequest::new(Subject::new("alice"), "parse the file", "parsed", "ok");
    req.evidence = vec![ev("run-log")];
    req.goal = String::new();
    assert!(matches!(
        k.record_experience(req).unwrap_err(),
        KError::InvalidObject(_)
    ));
}

#[test]
fn record_experience_rejects_ttl_overflow() {
    let (k, _clock, _store) = mk_kernel();
    // Review P1-6 (Test 7): u64::MAX seconds overflows the millis
    // conversion — rejected, never wrapped into an unbounded future.
    let mut req = ExperienceRequest::new(Subject::new("alice"), "parse the file", "parsed", "ok");
    req.evidence = vec![ev("run-log")];
    req.ttl_seconds = Some(u64::MAX);
    assert!(matches!(
        k.record_experience(req).unwrap_err(),
        KError::InvalidObject(_)
    ));
}

#[test]
fn record_experience_stamps_agent_derived_provenance_and_ttl() {
    let (k, _clock, _store) = mk_kernel();
    let mut req = ExperienceRequest::new(
        Subject::new("alice"),
        "refactor the parser",
        "refactored",
        "tests green",
    );
    req.lesson = Some("split the lexer first".into());
    req.preconditions = vec!["ci green".into()];
    req.causal_explanation = Some("smaller functions".into());
    req.reuse_conditions = vec!["refactor".into(), "parser".into()];
    req.evidence = vec![ev("run-log")];
    req.ttl_seconds = Some(30 * 24 * 3600);
    let r = k.record_experience(req).unwrap();
    let ko = k.get(Subject::new("alice"), &r.koid).unwrap();
    assert_eq!(ko.metadata.type_name, "aikoql:experience");
    assert_eq!(ko.epistemic_status(), EpistemicStatus::Asserted);
    assert_eq!(
        ko.extensions.get("authority"),
        Some(&Value::Text("agent_derived".into()))
    );
    assert_eq!(ko.valid_from(), Some(10_000));
    assert_eq!(ko.valid_to(), Some(10_000 + 30 * 24 * 3600 * 1000));
    // A fresh capture is a hypothesis about the world: 0.5, no confirmations.
    let cc = ko.confidence_context().unwrap();
    assert_eq!(cc.score, 0.5);
    assert_eq!(cc.confirmations, 0);
    assert_eq!(ko.evidence().len(), 1);
    assert_eq!(
        ko.properties.get("actor"),
        Some(&Value::Text("alice".into()))
    );
    assert_eq!(
        ko.properties.get("goal"),
        Some(&Value::Text("refactor the parser".into()))
    );
    assert_eq!(
        ko.properties.get("lesson"),
        Some(&Value::Text("split the lexer first".into()))
    );
    assert!(matches!(
        ko.properties.get("reuse_conditions"),
        Some(Value::List(_))
    ));
}

// ---- reuse matching ---------------------------------------------------------

#[test]
fn match_experiences_gates_on_all_reuse_condition_tokens() {
    let (k, _clock, _store) = mk_kernel();
    let kid = record(
        &k,
        "alice",
        "refactor the rust parser",
        &["rust parser"],
        None,
        None,
        &[],
    );
    // Every condition token present → eligible.
    let m = k
        .match_experiences(
            Subject::new("alice"),
            "please refactor the rust parser again",
            10,
        )
        .unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].0.koid, kid);
    // Partial coverage → gated out.
    let m = k
        .match_experiences(Subject::new("alice"), "refactor something else", 10)
        .unwrap();
    assert!(m.is_empty());
}

#[test]
fn match_experiences_requires_goal_overlap_without_conditions() {
    let (k, _clock, _store) = mk_kernel();
    record(
        &k,
        "alice",
        "optimize the query planner",
        &[],
        None,
        None,
        &[],
    );
    let m = k
        .match_experiences(Subject::new("alice"), "the query planner is slow", 10)
        .unwrap();
    assert_eq!(m.len(), 1);
    let m = k
        .match_experiences(Subject::new("alice"), "paint the bikeshed", 10)
        .unwrap();
    assert!(m.is_empty());
}

#[test]
fn match_experiences_ranks_by_confidence() {
    let (k, _clock, _store) = mk_kernel();
    let low = record(
        &k,
        "alice",
        "debug the flaky test",
        &[],
        None,
        Some(0.3),
        &[],
    );
    let high = record(
        &k,
        "alice",
        "debug the flaky test",
        &[],
        None,
        Some(0.9),
        &[],
    );
    let m = k
        .match_experiences(Subject::new("alice"), "debug the flaky test again", 10)
        .unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].0.koid, high);
    assert_eq!(m[1].0.koid, low);
    assert!(m[0].1 > m[1].1);
}

#[test]
fn match_experiences_filters_expired() {
    let (k, clock, _store) = mk_kernel();
    record(&k, "alice", "fix the build", &[], Some(10), None, &[]);
    // Within the TTL.
    clock.tick(9_999);
    assert_eq!(
        k.match_experiences(Subject::new("alice"), "fix the build", 10)
            .unwrap()
            .len(),
        1
    );
    // valid_to is half-open — at exactly now+ttl the experience is gone.
    clock.tick(1);
    assert!(k
        .match_experiences(Subject::new("alice"), "fix the build", 10)
        .unwrap()
        .is_empty());
}

// ---- cross-agent reuse ------------------------------------------------------

#[test]
fn match_experiences_respects_shared_with_acl() {
    let (k, _clock, _store) = mk_kernel();
    let shared = record(
        &k,
        "alice",
        "secure the auth flow",
        &[],
        None,
        None,
        &["bob"],
    );
    let private = record(&k, "alice", "secure the auth flow", &[], None, None, &[]);
    // The actor always matches her own.
    assert_eq!(
        k.match_experiences(Subject::new("alice"), "secure the auth flow", 10)
            .unwrap()
            .len(),
        2
    );
    // Bob only sees the one shared with him.
    let m = k
        .match_experiences(Subject::new("bob"), "secure the auth flow", 10)
        .unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].0.koid, shared);
    assert_ne!(m[0].0.koid, private);
}

#[test]
fn revoked_experience_sharing_stops_matching() {
    // The P1-9 invariant: share -> revoke ACL -> find_experiences() must not
    // return the experience. Eligibility (ACL) is enforced before ranking.
    let (k, _clock, _store) = mk_kernel();
    let kid = record(
        &k,
        "alice",
        "secure the auth flow",
        &[],
        None,
        None,
        &["bob"],
    );
    assert_eq!(
        k.match_experiences(Subject::new("bob"), "secure the auth flow", 10)
            .unwrap()
            .len(),
        1
    );
    // Revoke: remember with an explicit security descriptor REPLACES the ACL
    // (no bob entry — and no silent fallback to the previous descriptor).
    // Kernel-managed keys are stripped before re-submitting (review P0-1):
    // remember() rejects them, and they are carried forward automatically.
    let ko = k.get(Subject::new("alice"), &kid).unwrap();
    let mut rr = RememberRequest::update(Subject::new("alice"), kid, ko.metadata.clone());
    rr.properties = ko.properties.clone();
    rr.extensions = ko
        .extensions
        .clone()
        .into_iter()
        .filter(|(key, _)| !Kernel::KERNEL_MANAGED_EXTENSIONS.contains(&key.as_str()))
        .collect();
    rr.security = Some(SecurityDescriptor {
        owner: "alice".into(),
        acl: vec![],
        classification: None,
    });
    k.remember(rr).unwrap();

    assert!(k
        .match_experiences(Subject::new("bob"), "secure the auth flow", 10)
        .unwrap()
        .is_empty());
    // The owner still matches her own experience.
    assert_eq!(
        k.match_experiences(Subject::new("alice"), "secure the auth flow", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn invalidated_experiences_are_not_matched() {
    let (k, _clock, _store) = mk_kernel();
    let kid = record(&k, "alice", "migrate the schema", &[], None, None, &[]);
    assert_eq!(
        k.match_experiences(Subject::new("alice"), "migrate the schema", 10)
            .unwrap()
            .len(),
        1
    );
    let mut req = InvalidationRequest::new(Subject::new("alice"), kid);
    req.evidence = vec![ev("postmortem")];
    req.reason = Some("the migration corrupted data".into());
    k.invalidate(req).unwrap();
    assert!(k
        .match_experiences(Subject::new("alice"), "migrate the schema", 10)
        .unwrap()
        .is_empty());
}

// ---- persistence ------------------------------------------------------------

#[test]
fn experiences_survive_reopen() {
    let path = std::env::temp_dir().join(format!("aikoql-exp-{}-reopen.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let kid = {
        let clock = Arc::new(ManualClock::new(10_000));
        let store = Arc::new(RedbEngine::open(&path).expect("open"));
        let k = Kernel::open(store, clock, 0xE915).unwrap();
        let kid = record(
            &k,
            "alice",
            "cache the hot path",
            &["cache"],
            None,
            None,
            &[],
        );
        assert_eq!(
            k.match_experiences(Subject::new("alice"), "cache the hot path", 10)
                .unwrap()
                .len(),
            1
        );
        kid
    }; // kernel dropped → file unlocked
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(RedbEngine::open(&path).expect("reopen"));
    let k = Kernel::open(store, clock, 0xE915).unwrap();
    let m = k
        .match_experiences(Subject::new("alice"), "cache the hot path", 10)
        .unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].0.koid, kid);
    let ko = k.get(Subject::new("alice"), &kid).unwrap();
    assert_eq!(ko.epistemic_status(), EpistemicStatus::Asserted);
    assert_eq!(ko.valid_to(), Some(10_000 + 30 * 24 * 3600 * 1000));
    let _ = std::fs::remove_file(&path);
}
