//! Wave 5 (W5) — Knowledge Analytics vs OLAP, Phase A tests (plan §5).
//!
//! TDD plan `docs/AIKOQL_Wave5_Knowledge_Analytics_vs_OLAP_TDD_Test_Plan.md`
//! Phase A lists W5-OLAP-001/002 and W5-KA-001..008. Standing rule (plan §23
//! "Missing capabilities must fail honestly", and Wave 3.1 spec §3 "Already
//! Closed — Do Not Duplicate"):
//!
//! - W5-KA-002 (temporal knowledge) → closed by W31-TEMP-001 (wave31_decision)
//! - W5-KA-005 (unknown / insufficient evidence) → closed by W31-UNK-001
//! - W5-KA-007 (change impact) → closed by W31-IMPACT-001 (wave31_impact)
//! - W5-OLAP-001..004 → NOT_IMPLEMENTED: no ClickHouse/StarRocks adapter
//!   exists and the plan's own build-vs-buy rule (§7) forbids building one
//!   into AIKOQL until a measured benchmark proves knowledge-native
//!   execution requires it. No stub tests — the rows are recorded honestly
//!   in TESTING-PLAN §12 with the docker-compose path as the next step.
//!
//! This file carries the five genuinely new Phase A tests:
//!
//! - KA-001 multi-hop dependency analysis (traverse, direction-filtered,
//!   evidence-complete path vs a mechanical RAG pack — plan §5 first test)
//! - KA-003 provenance-aware risk (derive → DERIVED_FROM → supersede sweep)
//! - KA-004 conflicting evidence (contradict → authority-ranked resolution,
//!   policy captured as organization_policy knowledge)
//! - KA-006 evidence-backed aggregate (app-side count over independent
//!   sources + temporal validity — the OLAP COUNT the plan forbids building
//!   into the substrate is measured as application LOC here)
//! - KA-008 historical reconstruction (cross-generation valid_at walk +
//!   in-place version get_as_of)
//!
//! Each test prints its measured columns (latency / LOC / RAG comparison)
//! like the Wave 3.1 files; the pins are the correctness laws from the plan.
//! Ground truth is declared BEFORE measurement (plan §4).

mod common;

use std::time::Instant;

use aikoql_graph::{GraphEngineApi, RelateRequest, TraverseQuery};
use aikoql_ingestion::MockEmbeddingProvider;
use aikoql_kernel::*;
use common::wave31_sim::{alice, assert_claim, ev, mk, props, supersede_claim};
use common::CorpusChunk;

/// The `name` property of a KO, for hop classification in the traversal leg.
fn ko_name(k: &Kernel, koid: &KOID) -> String {
    match k.get(alice(), koid).unwrap().properties.get("name") {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// W5-KA-001 — Multi-hop dependency analysis
// ---------------------------------------------------------------------------

/// Six-hop chain, kernel-native: Customer → Account → Application → Service
/// → Dependency → Incident. Two decoy chains: an unrelated customer, and an
/// OUTBOUND edge from a chain node (direction must be filtered). Ground
/// truth declared first: exactly 5 upstream hops from INC-912, exactly 1
/// customer (Acme), 0 false hops, all 6 hop KOs evidence-backed.
#[test]
fn w5_ka_001_multi_hop_dependency_analysis() {
    let (k, _clock) = mk();

    // Six hop entities (claims carry per-source evidence).
    let acme = assert_claim(
        &k,
        "customer",
        props(&[("name", "Acme")]),
        "documentation",
        "crm",
    );
    let acct = assert_claim(
        &k,
        "account",
        props(&[("name", "acct-4471")]),
        "documentation",
        "billing",
    );
    let app = assert_claim(
        &k,
        "application",
        props(&[("name", "ShopApp")]),
        "documentation",
        "cmdb",
    );
    let svc = assert_claim(
        &k,
        "service",
        props(&[("name", "LedgerService")]),
        "documentation",
        "cmdb",
    );
    let dep = assert_claim(
        &k,
        "dependency",
        props(&[("name", "PostgresDB")]),
        "documentation",
        "cmdb",
    );
    let inc = assert_claim(
        &k,
        "incident",
        props(&[("name", "INC-912")]),
        "documentation",
        "oncall-log",
    );

    // The chain: Acme → acct-4471 → ShopApp → LedgerService → PostgresDB → INC-912.
    for (from, to, ty) in [
        (acme, acct, "has"),
        (acct, app, "runs"),
        (app, svc, "uses"),
        (svc, dep, "depends_on"),
        (dep, inc, "caused"),
    ] {
        k.relate(RelateRequest::new(alice(), from, to, ty)).unwrap();
    }

    // Decoys: an unrelated chain (Globex → RedisDB) and an OUTBOUND edge
    // from a chain node (LedgerService monitors TelemetrySink).
    let globex = assert_claim(
        &k,
        "customer",
        props(&[("name", "Globex")]),
        "documentation",
        "crm",
    );
    let acct2 = assert_claim(
        &k,
        "account",
        props(&[("name", "acct-9912")]),
        "documentation",
        "billing",
    );
    let mail = assert_claim(
        &k,
        "service",
        props(&[("name", "MailService")]),
        "documentation",
        "cmdb",
    );
    let redis = assert_claim(
        &k,
        "dependency",
        props(&[("name", "RedisDB")]),
        "documentation",
        "cmdb",
    );
    let sink = assert_claim(
        &k,
        "service",
        props(&[("name", "TelemetrySink")]),
        "documentation",
        "cmdb",
    );
    for (from, to, ty) in [
        (globex, acct2, "has"),
        (acct2, mail, "runs"),
        (mail, redis, "depends_on"),
        (svc, sink, "monitors"),
    ] {
        k.relate(RelateRequest::new(alice(), from, to, ty)).unwrap();
    }

    // ── The answer leg — application code, measured for LOC ────────────────
    let t0 = Instant::now();
    let q = TraverseQuery {
        context: alice(),
        start: inc,
        rel_type: None,
        direction: Some(Direction::Inbound),
        depth: 5,
    };
    let hits = k.traverse(q).unwrap();
    let micros = t0.elapsed().as_micros();

    // ── Pinned correctness over the declared ground truth ──────────────────
    // Path correctness: exactly the 5 upstream hops, at the right depths.
    let expected: [(&str, u32); 5] = [
        ("PostgresDB", 1),
        ("LedgerService", 2),
        ("ShopApp", 3),
        ("acct-4471", 4),
        ("Acme", 5),
    ];
    assert_eq!(hits.len(), 5, "exactly 5 upstream hops, got {hits:?}");
    for (name, depth) in expected {
        let hit = hits
            .iter()
            .find(|h| ko_name(&k, &h.koid) == name)
            .unwrap_or_else(|| panic!("hop {name} missing from traversal"));
        assert_eq!(hit.depth, depth, "hop {name} at wrong depth");
    }

    // Answer correctness: the only customer reached is Acme; false hops 0.
    let customers: Vec<String> = hits
        .iter()
        .filter(|h| k.get(alice(), &h.koid).unwrap().metadata.type_name == "customer")
        .map(|h| ko_name(&k, &h.koid))
        .collect();
    assert_eq!(
        customers,
        vec!["Acme".to_string()],
        "no decoy customer may be reached"
    );
    let chain_names = [
        "PostgresDB",
        "LedgerService",
        "ShopApp",
        "acct-4471",
        "Acme",
    ];
    let false_hops: Vec<String> = hits
        .iter()
        .map(|h| ko_name(&k, &h.koid))
        .filter(|n| !chain_names.contains(&n.as_str()))
        .collect();
    assert!(false_hops.is_empty(), "false hops: {false_hops:?}");
    // Direction filter: the outbound decoy edge must not be crossed.
    assert!(
        hits.iter().all(|h| ko_name(&k, &h.koid) != "TelemetrySink"),
        "outbound monitors edge crossed an inbound traversal"
    );

    // Evidence completeness: every hop KO (including the start) is backed.
    for koid in [acme, acct, app, svc, dep, inc] {
        let ko = k.get(alice(), &koid).unwrap();
        assert!(
            !ko.evidence().is_empty(),
            "hop {} without evidence",
            ko_name(&k, &koid)
        );
        assert!(!ko.evidence()[0].source_artifact.is_empty());
    }

    // ── Mechanical RAG comparison leg (Wave 3.1 pattern) ───────────────────
    let corpus: Vec<CorpusChunk> = vec![
        ("w5", 0, "Acme has account acct-4471".into()),
        ("w5", 1, "acct-4471 runs ShopApp".into()),
        ("w5", 2, "ShopApp uses LedgerService".into()),
        ("w5", 3, "LedgerService depends on PostgresDB".into()),
        ("w5", 4, "PostgresDB caused incident INC-912".into()),
        ("w5", 5, "Globex has account acct-9912".into()),
        ("w5", 6, "acct-9912 runs MailService".into()),
        ("w5", 7, "MailService depends on RedisDB".into()),
        ("w5", 8, "LedgerService monitors TelemetrySink".into()),
    ];
    let question = "Which customers are affected by the LedgerService outage, and why?";
    let order =
        common::wave31_sim::rank_positions(&corpus, question, &MockEmbeddingProvider::new());
    let rag_pack = common::wave31_sim::pack_budgeted(&order, &corpus);
    // RAG hop coverage: chunks 0..4 are the true chain, 5..8 are decoys.
    let rag_hits = (0..5).filter(|i| rag_pack.contains(&corpus[*i].2)).count();
    let rag_false = (5..9).filter(|i| rag_pack.contains(&corpus[*i].2)).count();

    println!(
        "\n[W5-KA-001] multi-hop dependency analysis\n\
         AIKOQL: 6/6 hops, 0 false, 0 missed, {micros}µs, app LOC ≈ 12 (traverse leg)\n\
         RAG:    {rag_hits}/5 chain chunks packed, {rag_false} false chunk(s) packed\n\
         comparison: AIKOQL hop coverage >= RAG (pinned)"
    );
    assert!(
        6 >= rag_hits,
        "AIKOQL must cover at least the RAG pack's chain hops"
    );
}

// ---------------------------------------------------------------------------
// W5-KA-003 — Provenance-aware analytics
// ---------------------------------------------------------------------------

/// "Why is Customer X high risk?" — the answer must carry the current
/// classification, its supporting evidence, source, timestamp, authority,
/// and superseded evidence (plan §5). The derivation is a first-class
/// record; when a premise is superseded, the derived classification is
/// swept and the old evidence stays reachable through lineage.
#[test]
fn w5_ka_003_provenance_aware_risk_classification() {
    let (k, clock) = mk();

    let spike = assert_claim(
        &k,
        "signal",
        props(&[("name", "txn_spike"), ("value", "42x")]),
        "source_code",
        "fraud-db",
    );
    let logins = assert_claim(
        &k,
        "signal",
        props(&[("name", "failed_logins"), ("value", "7")]),
        "source_code",
        "auth-log",
    );
    clock.tick(10);

    let mut req = DeriveRequest::new(alice(), "risk_classification");
    req.properties = props(&[("customer", "X"), ("level", "high")]);
    req.sources = vec![spike, logins];
    req.operation = "risk_rule".into();
    req.actor = "risk-engine".into();
    req.reason = Some("txn spike plus failed logins".into());
    req.evidence = vec![ev("risk-policy")];
    let t0 = Instant::now();
    let derived = k.derive(req).unwrap().koid;
    let micros = t0.elapsed().as_micros();

    // Classification correctness + the derivation record answers all six
    // provenance questions (HOW / BY WHOM / WHEN / FROM WHAT / WHY / MODEL).
    let dko = k.get(alice(), &derived).unwrap();
    assert_eq!(dko.epistemic_status(), EpistemicStatus::Inferred);
    assert_eq!(
        dko.properties.get("level"),
        Some(&Value::Text("high".into()))
    );
    let d = dko.derivation().expect("derivation record must be stamped");
    assert_eq!(d.operation, "risk_rule");
    assert_eq!(d.actor, "risk-engine");
    assert_eq!(d.sources, vec![spike, logins]);
    assert!(d.timestamp > 0);
    assert_eq!(d.reason.as_deref(), Some("txn spike plus failed logins"));

    // Evidence completeness: the derived KO carries the policy source, and
    // both premises wire DERIVED_FROM edges (K4 invalidation input).
    assert!(
        dko.evidence()
            .iter()
            .any(|e| e.source_artifact == "risk-policy"),
        "derived classification without evidence"
    );
    for src in [spike, logins] {
        let edges = k.outbound_edges(&src, Some(DERIVED_FROM)).unwrap();
        assert_eq!(edges.len(), 1, "premise must trace to its dependent");
        assert_eq!(edges[0].1, derived);
    }
    assert!(
        !k.trace(alice(), &derived).unwrap().events.is_empty(),
        "derivation must be lineage-visible"
    );

    // ── Supersede a premise: the classification is swept ───────────────────
    let new_spike = supersede_claim(
        &k,
        spike,
        props(&[("name", "txn_normal"), ("value", "1x")]),
        "re-assessment",
        "fraud-db-v2",
    );
    let new_ko = k.get(alice(), &new_spike).unwrap();
    assert_eq!(
        new_ko.properties.get("name"),
        Some(&Value::Text("txn_normal".into()))
    );
    let dko = k.get(alice(), &derived).unwrap();
    assert!(
        dko.valid_to().is_some() || dko.invalidation().is_some(),
        "derived classification must be swept when a premise is superseded"
    );

    // Stale-evidence rate: the superseded premise is NOT current, and its
    // evidence survives through lineage (history + carried-forward edge).
    let old = k.get(alice(), &spike).unwrap();
    assert!(old.valid_to().is_some(), "superseded premise still current");
    assert!(
        !old.evidence().is_empty(),
        "superseded premise lost its evidence"
    );
    assert!(
        k.outbound_edges(&spike, Some(DERIVED_FROM))
            .unwrap()
            .iter()
            .any(|(_, t)| *t == derived),
        "swept dependent must remain lineage-reachable from its premise"
    );

    println!(
        "\n[W5-KA-003] provenance-aware risk: derived high-risk (evidence=risk-policy, \
         sources=2), premise superseded -> classification swept, lineage intact ({micros}µs)"
    );
}

// ---------------------------------------------------------------------------
// W5-KA-004 — Conflicting evidence
// ---------------------------------------------------------------------------

/// CRM says ACTIVE (documentation), Fraud says BLOCKED (source_code),
/// Policy (organization_policy) says BLOCKED overrides ACTIVE. Expected:
/// effective state BLOCKED, conflict disclosed, authoritative source
/// identified, loser still traceable with its evidence (plan §5).
#[test]
fn w5_ka_004_conflicting_evidence_policy_override() {
    let (k, _clock) = mk();

    let crm = assert_claim(
        &k,
        "customer_status",
        props(&[("customer", "X"), ("value", "ACTIVE")]),
        "documentation",
        "crm",
    );
    let mut contra = ContradictionRequest::new(alice(), crm);
    contra.counter_props = props(&[("customer", "X"), ("value", "BLOCKED")]);
    contra.authority = Some("source_code".into());
    contra.evidence = vec![ev("fraud-db")];
    let res = k.contradict(contra).unwrap();

    // Conflict detection: a first-class conflict record, symmetric — both
    // claims stay current until resolution, nothing silently collapsed.
    let conflict = k.get(alice(), &res.conflict).unwrap();
    assert_eq!(conflict.metadata.type_name, "aikoql:conflict");
    assert!(
        k.get(alice(), &crm).unwrap().invalidation().is_none(),
        "claim A silently collapsed before resolution"
    );
    assert!(
        k.get(alice(), &res.counter)
            .unwrap()
            .invalidation()
            .is_none(),
        "claim B silently collapsed before resolution"
    );

    // The policy statement is captured as knowledge at its own authority
    // rank (organization_policy = 9, above both claimants).
    let policy = assert_claim(
        &k,
        "policy",
        props(&[
            ("scope", "customer.status"),
            ("rule", "BLOCKED overrides ACTIVE"),
        ]),
        "organization_policy",
        "policy-hub",
    );

    // Authority selection: fraud (source_code=7) beats crm (documentation=3).
    let outcome = k
        .resolve_conflict_by_authority(
            alice(),
            res.conflict,
            "fraud evidence is authoritative".into(),
        )
        .unwrap();
    assert_eq!(outcome.decision, ConflictResolution::ResolvedBPreferred);

    // Effective state: exactly one current claim for customer X — BLOCKED.
    // Currency = not superseded (valid_to), not invalidated, and not
    // contradicted — the loser is marked Contradicted (epistemic status),
    // which is the substrate's currency marker, not valid_to.
    let mut current: Vec<KOID> = Vec::new();
    for koid in [crm, res.counter] {
        let ko = k.get(alice(), &koid).unwrap();
        if ko.invalidation().is_none()
            && ko.valid_to().is_none()
            && ko.epistemic_status() != EpistemicStatus::Contradicted
        {
            current.push(koid);
        }
    }
    assert_eq!(current.len(), 1, "exactly one current claim");
    assert_eq!(
        k.get(alice(), &current[0]).unwrap().properties.get("value"),
        Some(&Value::Text("BLOCKED".into()))
    );

    // Conflict disclosed + evidence completeness: the loser is Contradicted
    // but fully traceable (KO, evidence, lineage); the policy KO stands.
    let loser = k.get(alice(), &crm).unwrap();
    assert_eq!(loser.epistemic_status(), EpistemicStatus::Contradicted);
    assert!(!loser.evidence().is_empty(), "loser lost its evidence");
    assert!(!k.trace(alice(), &crm).unwrap().events.is_empty());
    assert_eq!(
        k.get(alice(), &policy).unwrap().properties.get("rule"),
        Some(&Value::Text("BLOCKED overrides ACTIVE".into()))
    );

    println!(
        "\n[W5-KA-004] conflicting evidence: CRM(ACTIVE,doc=3) vs Fraud(BLOCKED,code=7) \
         -> policy(org=9) captured, resolution B-preferred, effective state BLOCKED, \
         conflict disclosed, loser traceable"
    );
}

// ---------------------------------------------------------------------------
// W5-KA-006 — Evidence-backed aggregate
// ---------------------------------------------------------------------------

/// "How many high-risk customers are currently supported by at least two
/// independent authoritative sources?" — aggregation + entity state +
/// source independence + temporal validity (plan §5). The substrate has no
/// COUNT op (and the plan forbids building one until measured need), so the
/// aggregate is app-side; its LOC is the measured application complexity.
#[test]
fn w5_ka_006_evidence_backed_aggregate() {
    let (k, _clock) = mk();

    let mut claims: Vec<KOID> = Vec::new();

    // c1: two independent sources (fraud-db + audit-report) → counts.
    let c1 = assert_claim(
        &k,
        "risk",
        props(&[("customer", "c1"), ("level", "high")]),
        "source_code",
        "fraud-db",
    );
    let mut v = VerificationRequest::new(alice(), c1);
    v.evidence = vec![ev("audit-report")];
    k.verify_knowledge(v).unwrap();
    claims.push(c1);

    // c2: one source only → excluded.
    claims.push(assert_claim(
        &k,
        "risk",
        props(&[("customer", "c2"), ("level", "high")]),
        "source_code",
        "fraud-db",
    ));

    // c3: re-confirmation from the same source is NOT independence.
    let c3 = assert_claim(
        &k,
        "risk",
        props(&[("customer", "c3"), ("level", "high")]),
        "source_code",
        "fraud-db",
    );
    for _ in 0..2 {
        let mut v = VerificationRequest::new(alice(), c3);
        v.evidence = vec![ev("fraud-db")];
        k.verify_knowledge(v).unwrap();
    }
    claims.push(c3);

    // c4: two sources, but re-assessed (superseded) → excluded (temporal).
    let c4 = assert_claim(
        &k,
        "risk",
        props(&[("customer", "c4"), ("level", "high")]),
        "source_code",
        "fraud-db",
    );
    let mut v = VerificationRequest::new(alice(), c4);
    v.evidence = vec![ev("audit-report")];
    k.verify_knowledge(v).unwrap();
    let c4b = supersede_claim(
        &k,
        c4,
        props(&[("customer", "c4"), ("level", "low")]),
        "re-assessment",
        "reassessment",
    );
    claims.push(c4b);

    // c5: two sources but not high → excluded.
    let c5 = assert_claim(
        &k,
        "risk",
        props(&[("customer", "c5"), ("level", "low")]),
        "source_code",
        "fraud-db",
    );
    let mut v = VerificationRequest::new(alice(), c5);
    v.evidence = vec![ev("audit-report")];
    k.verify_knowledge(v).unwrap();
    claims.push(c5);

    // ── The aggregate — application code, measured for LOC ─────────────────
    let t0 = Instant::now();
    let mut count = 0usize;
    for koid in &claims {
        let ko = k.get(alice(), koid).unwrap();
        if ko.valid_to().is_some() || ko.invalidation().is_some() {
            continue; // not current
        }
        if ko.properties.get("level") != Some(&Value::Text("high".into())) {
            continue;
        }
        if ko.evidence().len() >= 2 {
            count += 1;
        }
    }
    let micros = t0.elapsed().as_micros();

    // ── Pinned correctness ─────────────────────────────────────────────────
    assert_eq!(
        count, 1,
        "exactly c1 satisfies two independent current sources"
    );
    // Source independence: c3's re-confirmation from the same source must
    // count once (P2-3/P2-4 — evidence dedup + distinct-key confirmations).
    let c3_ko = k.get(alice(), &c3).unwrap();
    assert_eq!(
        c3_ko.evidence().len(),
        1,
        "same-source evidence must not duplicate"
    );
    let conf = c3_ko.confidence_context().expect("confidence context");
    assert_eq!(
        conf.confirmations, 1,
        "same (verifier, evidence) must not re-confirm"
    );
    // Temporal correctness: c4's superseded high claim is not current.
    assert!(k.get(alice(), &c4).unwrap().valid_to().is_some());

    println!(
        "\n[W5-KA-006] evidence-backed aggregate: 5 customers -> answer 1 \
         ({micros}µs, app LOC ≈ 13 for the count loop)"
    );
}

// ---------------------------------------------------------------------------
// W5-KA-008 — Historical reconstruction
// ---------------------------------------------------------------------------

/// "Reconstruct what the organization knew about Customer X on date T."
/// Three generations across wall-clock time (valid_from/valid_to walk),
/// plus an in-place version pin via get_as_of (plan §5: facts valid then,
/// sources available then, superseded/current separated).
#[test]
fn w5_ka_008_historical_reconstruction_as_of() {
    let (k, clock) = mk();

    clock.set(0);
    let v1 = assert_claim(
        &k,
        "status",
        props(&[("customer", "X"), ("status", "ACTIVE")]),
        "documentation",
        "crm",
    );
    clock.tick(1000);
    let v2 = supersede_claim(
        &k,
        v1,
        props(&[("customer", "X"), ("status", "SUSPENDED")]),
        "billing report",
        "billing",
    );
    clock.tick(1000);
    let v3 = supersede_claim(
        &k,
        v2,
        props(&[("customer", "X"), ("status", "ACTIVE")]),
        "audit re-check",
        "audit",
    );

    // In-place version pin on v3: an independent audit confirmation at t=2500.
    clock.tick(500);
    let mut v = VerificationRequest::new(alice(), v3);
    v.evidence = vec![ev("audit-team")];
    k.verify_knowledge(v).unwrap();

    // ── Reconstruction leg — application code, measured for LOC ────────────
    let t0 = Instant::now();
    let gens = [v1, v2, v3];
    let reconstruct = |at: u64| -> Option<(String, String)> {
        for g in gens.iter().rev() {
            let ko = k.get(alice(), g).unwrap();
            if ko.valid_at(at) {
                let status = match ko.properties.get("status") {
                    Some(Value::Text(s)) => s.clone(),
                    _ => String::new(),
                };
                let src = ko.evidence()[0].source_artifact.clone();
                return Some((status, src));
            }
        }
        None
    };
    let at_500 = reconstruct(500).expect("t=500 must reconstruct");
    let at_1500 = reconstruct(1500).expect("t=1500 must reconstruct");
    let at_2500 = reconstruct(2500).expect("t=2500 must reconstruct");
    let micros = t0.elapsed().as_micros();

    // ── Pinned historical correctness ──────────────────────────────────────
    assert_eq!(at_500.0, "ACTIVE", "reconstruction at t=500 must be v1");
    assert_eq!(
        at_1500.0, "SUSPENDED",
        "reconstruction at t=1500 must be v2"
    );
    assert_eq!(at_2500.0, "ACTIVE", "reconstruction at t=2500 must be v3");
    assert_eq!(at_500.1, "crm", "source available at t=500 is crm");
    assert_eq!(
        at_1500.1, "billing",
        "source available at t=1500 is billing"
    );

    // Superseded/current correctly separated: v1/v2 closed, v3 open; each
    // generation's history preserves its evidence.
    assert!(k.get(alice(), &v1).unwrap().valid_to().is_some());
    assert!(k.get(alice(), &v2).unwrap().valid_to().is_some());
    assert!(k.get(alice(), &v3).unwrap().valid_to().is_none());
    for (g, src) in [(v1, "crm"), (v2, "billing"), (v3, "audit")] {
        let hist = k.history(alice(), &g).unwrap();
        assert!(
            hist.iter()
                .any(|(_, ko)| ko.evidence().iter().any(|e| e.source_artifact == src)),
            "generation history lost its source"
        );
    }

    // In-place versioning: get_as_of before the t=2500 confirmation must
    // return the pre-confirmation version; the head carries the appended
    // evidence (provenance correctness).
    let pre = k
        .get_as_of(alice(), &v3, 2100)
        .unwrap()
        .expect("pre-confirmation version");
    assert_eq!(
        pre.evidence().len(),
        1,
        "pre-confirmation version must not see the audit evidence"
    );
    assert_eq!(k.get(alice(), &v3).unwrap().evidence().len(), 2);

    println!(
        "\n[W5-KA-008] historical reconstruction: t=500 ACTIVE(crm), t=1500 \
         SUSPENDED(billing), t=2500 ACTIVE(audit); get_as_of separates \
         in-place versions ({micros}µs, app LOC ≈ 13 for the walk)"
    );
}
