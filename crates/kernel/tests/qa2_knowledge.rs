//! MVP-QA-002 Suite B — knowledge consistency (QA2-KNOW-002, -006, -007, -008).
//!
//! The pins that close Suite B:
//! - KNOW-002: source authority — the higher-authority claim is selected as
//!   current truth, the loser stays traceable (Conflict KO + CONTRADICTS edge).
//! - KNOW-006: entity split — a merged entity splits into two, each side
//!   keeping its own facts, unrelated relationships and provenance.
//! - KNOW-007: relationship conflict — both sides survive untouched, and the
//!   kernel never resolves without explicit policy.
//! - KNOW-008: evidence ≠ fact — a failed extraction leaves the source
//!   evidence retrievable and commits no partial fact.

use aikoql_graph::{GraphEngineApi, RelateRequest};
use aikoql_kernel::*;
use std::sync::Arc;

fn mk() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
}

fn alice() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new("alice"))
}

fn ev(src: &str) -> Evidence {
    Evidence::new(src, EvidenceMethod::DocExtraction)
}

/// Assert `properties` on explicit `authority` and return the claim KOID.
fn assert_claim(
    k: &Kernel,
    type_name: &str,
    properties: PropertyMap,
    authority: &str,
    src: &str,
) -> KOID {
    let mut req = AssertionRequest::new(alice(), type_name);
    req.properties = properties;
    req.authority = Some(authority.into());
    req.evidence = vec![ev(src)];
    k.assert_knowledge(req).unwrap().koid
}

// ---------------------------------------------------------------------------
// QA2-KNOW-002 — source authority: higher-authority claim is current truth,
// conflicting evidence remains traceable
// ---------------------------------------------------------------------------

#[test]
fn w2_know_002_source_authority_selects_current_truth() {
    let k = mk();

    // "PostgreSQL > MongoDB" for customer.status, mapped onto the authority
    // levels (per-source-type priority config does not exist — the authority
    // level IS the configured policy, see TESTING-PLAN §10.1).
    let mut pg_props = PropertyMap::new();
    pg_props.insert("subject".into(), Value::Text("customer.status".into()));
    pg_props.insert("value".into(), Value::Text("ACTIVE".into()));
    let pg = assert_claim(&k, "Claim", pg_props, "source_code", "postgres://billing");

    let mut mongo_props = PropertyMap::new();
    mongo_props.insert("subject".into(), Value::Text("customer.status".into()));
    mongo_props.insert("value".into(), Value::Text("SUSPENDED".into()));
    let mut contra = ContradictionRequest::new(alice(), pg);
    contra.counter_props = mongo_props;
    contra.authority = Some("documentation".into());
    contra.evidence = vec![ev("mongodb://crm")];
    let res = k.contradict(contra).unwrap();

    // Both claims are current until resolution — the conflict is symmetric
    // and neither side was silently collapsed.
    let conflict = k.get(alice(), &res.conflict).unwrap();
    assert_eq!(conflict.metadata.type_name, "aikoql:conflict");
    assert_eq!(
        conflict.extensions.get("resolution"),
        Some(&Value::Text("unresolved".into()))
    );
    assert!(k.get(alice(), &pg).unwrap().invalidation().is_none());
    assert!(k
        .get(alice(), &res.counter)
        .unwrap()
        .invalidation()
        .is_none());

    // Authority-ranked resolution: postgres (source_code=7) beats
    // mongodb (documentation=3) — selected as current truth...
    let outcome = k
        .resolve_conflict_by_authority(alice(), res.conflict, "billing is authoritative".into())
        .unwrap();
    assert_eq!(outcome.decision, ConflictResolution::ResolvedAPreferred);

    // ...the winning claim stays current with its value...
    let winner = k.get(alice(), &pg).unwrap();
    assert!(winner.invalidation().is_none());
    assert_eq!(
        winner.properties.get("value"),
        Some(&Value::Text("ACTIVE".into()))
    );

    // ...and the loser is Contradicted — but fully traceable: the KO, its
    // evidence, the CONTRADICTS edge and the conflict record all survive.
    let loser = k.get(alice(), &res.counter).unwrap();
    assert_eq!(
        loser.epistemic_status(),
        EpistemicStatus::Contradicted,
        "the lower-authority claim must be marked contradicted, not deleted"
    );
    assert_eq!(loser.evidence().len(), 1);
    assert!(loser
        .relationships
        .iter()
        .any(|r| r.rel_type == CONTRADICTS && r.target == pg));
    let resolved = k.get(alice(), &res.conflict).unwrap();
    assert_eq!(
        resolved.extensions.get("resolution"),
        Some(&Value::Text("resolved_a_preferred".into()))
    );
    // History of the loser preserves every step (asserted → contradicted).
    let lin = k.trace(alice(), &res.counter).unwrap();
    assert!(lin.versions.len() >= 2);
}

// ---------------------------------------------------------------------------
// QA2-KNOW-007 — relationship conflict: preserved, resolved only by explicit
// policy
// ---------------------------------------------------------------------------

#[test]
fn w2_know_007_relationship_conflict_preserved_resolved_by_policy_only() {
    let k = mk();

    // Entity B (the claimed endpoint must resolve to a real KO).
    let mut b_req = RememberRequest::create(
        alice().subject.clone(),
        Metadata {
            type_name: "entity".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    b_req
        .properties
        .insert("name".into(), Value::Text("B".into()));
    let b = k.remember(b_req).unwrap().koid;

    // Source A: A -> OWNS -> B (as a relation-bearing claim).
    let mut owns_props = PropertyMap::new();
    owns_props.insert("predicate".into(), Value::Text("owns".into()));
    owns_props.insert("target".into(), Value::Text(b.to_hex()));
    let owns = assert_claim(&k, "Claim", owns_props, "source_code", "graphdb://A");
    // Attach the relationship itself to the claim.
    let mut rel_req = RememberRequest::update(
        alice().subject.clone(),
        owns,
        Metadata {
            type_name: "Claim".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    rel_req.relationships.push(RelationshipRef {
        rel_type: "owns".into(),
        target: b,
        direction: Direction::Outbound,
    });
    k.remember(rel_req).unwrap();

    // Source B: A -> DOES_NOT_OWN -> B.
    let mut not_props = PropertyMap::new();
    not_props.insert("predicate".into(), Value::Text("does_not_own".into()));
    not_props.insert("target".into(), Value::Text(b.to_hex()));
    let mut contra = ContradictionRequest::new(alice(), owns);
    contra.counter_props = not_props;
    contra.authority = Some("source_code".into()); // same authority → tie
    contra.evidence = vec![ev("graphdb://B")];
    let res = k.contradict(contra).unwrap();

    // The conflict is preserved: BOTH relation claims remain current, each
    // carrying its own claim data, and the conflict KO records both sides.
    let owns_ko = k.get(alice(), &owns).unwrap();
    assert!(owns_ko.invalidation().is_none());
    assert!(owns_ko
        .relationships
        .iter()
        .any(|r| r.rel_type == "owns" && r.target == b));
    let counter_ko = k.get(alice(), &res.counter).unwrap();
    assert!(counter_ko.invalidation().is_none());
    assert_eq!(
        counter_ko.properties.get("predicate"),
        Some(&Value::Text("does_not_own".into()))
    );
    assert!(counter_ko
        .relationships
        .iter()
        .any(|r| r.rel_type == CONTRADICTS && r.target == owns));
    let conflict = k.get(alice(), &res.conflict).unwrap();
    assert_eq!(
        conflict.extensions.get("resolution"),
        Some(&Value::Text("unresolved".into())),
        "no implicit resolution — the kernel must not silently pick a side"
    );

    // Policy-only resolution: an equal-authority tie is REFUSED (the kernel
    // never auto-picks)...
    let tie = k.resolve_conflict_by_authority(alice(), res.conflict, "tie must be rejected".into());
    assert!(
        matches!(tie, Err(KError::InvalidObject(_))),
        "authority tie must be an explicit-decision error, got {tie:?}"
    );
    // ...and the conflict remains unresolved after the refusal.
    let conflict = k.get(alice(), &res.conflict).unwrap();
    assert_eq!(
        conflict.extensions.get("resolution"),
        Some(&Value::Text("unresolved".into()))
    );

    // An explicit human decision resolves it — and only then does the loser
    // flip to Contradicted (A preferred: the OWNS claim survives).
    let outcome = k
        .resolve_conflict(ConflictResolutionRequest {
            context: alice(),
            conflict: res.conflict,
            decision: ConflictResolution::ResolvedAPreferred,
            rationale: "deed registry confirms ownership".into(),
            replacement: None,
            split_at: None,
        })
        .unwrap();
    assert_eq!(outcome.decision, ConflictResolution::ResolvedAPreferred);
    assert!(k.get(alice(), &owns).unwrap().invalidation().is_none());
    assert_eq!(
        k.get(alice(), &res.counter).unwrap().epistemic_status(),
        EpistemicStatus::Contradicted
    );
}

// ---------------------------------------------------------------------------
// QA2-KNOW-008 — evidence ≠ fact: a failed extraction leaves the source
// evidence retrievable and commits no partial fact
// ---------------------------------------------------------------------------

#[test]
fn w2_know_008_failed_extraction_keeps_evidence_retrievable() {
    let k = mk();

    // The source statement enters as an observed document KO — the evidence
    // carrier — independent of whether any fact is extracted from it.
    let mut obs = ObservationRequest::new(alice().subject.clone(), "document");
    obs.evidence = vec![ev("corpus/customer.md")
        .with_location("L42")
        .with_confidence(0.99)];
    let doc = k.observe(obs).unwrap().koid;

    // Extraction of a fact FAILS (the extraction layer produced nothing —
    // modeled as the kernel's mandatory-evidence rejection: an unbacked
    // fact op is refused, never silently committed).
    let fact_fail = k.ingest_observation(IngestRequest {
        context: alice().subject.clone().into(),
        type_name: "fact".into(),
        properties: PropertyMap::new(),
        evidence: vec![], // extraction produced no evidence record
        idempotency_key: None,
        tags: vec![],
        valid_from: None,
        security: None,
        note: None,
    });
    assert!(
        matches!(fact_fail, Err(KError::InvalidObject(_))),
        "an unbacked fact must be rejected, got {fact_fail:?}"
    );

    // INV-004: missing extraction does not imply missing source evidence.
    // The document and its full evidence trail are still retrievable.
    let doc_ko = k.get(alice(), &doc).unwrap();
    assert_eq!(doc_ko.evidence().len(), 1);
    assert_eq!(doc_ko.evidence()[0].location, Some("L42".into()));
    assert_eq!(doc_ko.evidence()[0].confidence, 0.99);

    // No partial fact was committed — the journal holds exactly the document.
    assert_eq!(k.journal().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// QA2-KNOW-006 — entity split: a merged entity is determined to represent two
// distinct entities; the split preserves unrelated relationships and
// provenance on both sides
// ---------------------------------------------------------------------------

#[test]
fn w2_know_006_entity_split_preserves_unrelated_relationships_and_provenance() {
    let k = mk();

    // Two distinct entities that share a name (the classic over-merge).
    let mut bank = IngestRequest::new(alice(), "entity");
    bank.properties
        .insert("name".into(), Value::Text("Apple".into()));
    bank.properties
        .insert("sector".into(), Value::Text("banking".into()));
    bank.evidence = vec![ev("doc/apple-bank.md")];
    let bank_ko = k.ingest_observation(bank).unwrap().koid;

    let mut fruit = IngestRequest::new(alice(), "entity");
    fruit
        .properties
        .insert("name".into(), Value::Text("Apple".into()));
    fruit
        .properties
        .insert("family".into(), Value::Text("Rosaceae".into()));
    fruit.evidence = vec![ev("doc/apple-fruit.md")];
    let fruit_ko = k.ingest_observation(fruit).unwrap().koid;

    // The previously merged entity — a first-class kernel merge.
    let mut merged_props = PropertyMap::new();
    merged_props.insert("name".into(), Value::Text("Apple".into()));
    merged_props.insert("sector".into(), Value::Text("banking".into()));
    merged_props.insert("family".into(), Value::Text("Rosaceae".into()));
    let mut merge_req = MergeRequest::new(alice(), "entity", vec![bank_ko, fruit_ko]);
    merge_req.strategy = MergeStrategy::Manual;
    merge_req.properties = Some(merged_props);
    merge_req.reason = Some("name-based identity merge".into());
    let merged = k.merge(merge_req).unwrap();
    let m = merged.koid;

    // Unrelated relationships accrued on the merged entity: the bank owns the
    // subsidiary; the fruit supplies it.
    let mut c_req = IngestRequest::new(alice(), "entity");
    c_req
        .properties
        .insert("name".into(), Value::Text("Subsidiary".into()));
    c_req.evidence = vec![ev("doc/subsidiary.md")];
    let c = k.ingest_observation(c_req).unwrap().koid;
    k.relate(RelateRequest::new(alice(), m, c, "owns")).unwrap();
    k.relate(RelateRequest::new(alice(), m, c, "supplies"))
        .unwrap();

    let head = k.get(alice(), &m).unwrap();
    let head_version = head.version;
    assert!(head
        .relationships
        .iter()
        .any(|r| r.rel_type == DERIVED_FROM && r.target == bank_ko));

    // SPLIT: the fruit side leaves the merged entity, taking its fact and its
    // relationship with it.
    let mut b_props = PropertyMap::new();
    b_props.insert("name".into(), Value::Text("Apple".into()));
    b_props.insert("family".into(), Value::Text("Rosaceae".into()));
    let split = k
        .split(SplitRequest {
            context: alice(),
            subject: m,
            expected_version: Some(head_version),
            b_properties: b_props,
            b_relationships: vec![RelationshipRef {
                rel_type: "supplies".into(),
                target: c,
                direction: Direction::Outbound,
            }],
            reason: "the bank and the fruit are distinct entities".into(),
            idempotency_key: None,
        })
        .unwrap();
    assert_eq!(split.original, (m, head_version + 1));
    let b_new = split.new_entity.0;

    // Side B: its own facts, its moved relationship, lineage to the merged
    // entity, and the full evidence trail — provenance preserved.
    let b_ko = k.get(alice(), &b_new).unwrap();
    assert_eq!(b_ko.metadata.type_name, "entity");
    assert_eq!(
        b_ko.properties.get("family"),
        Some(&Value::Text("Rosaceae".into()))
    );
    assert!(!b_ko.properties.contains_key("sector"));
    assert!(b_ko
        .relationships
        .iter()
        .any(|r| r.rel_type == "supplies" && r.target == c));
    assert!(b_ko
        .relationships
        .iter()
        .any(|r| r.rel_type == DERIVED_FROM && r.target == m));
    let derivation = b_ko.derivation().expect("split must record a derivation");
    assert_eq!(derivation.operation, "split");
    assert_eq!(derivation.sources, vec![m]);
    assert_eq!(
        derivation.reason.as_deref(),
        Some("the bank and the fruit are distinct entities")
    );
    assert_eq!(b_ko.evidence().len(), 2, "full trail inherited");
    assert_eq!(
        b_ko.confidence_context().map(|c| c.score),
        head.confidence_context().map(|c| c.score)
    );

    // Side A (the original): keeps its KOID and lineage, loses only the moved
    // fact and the moved relationship. Unrelated relationships survive.
    let a_ko = k.get(alice(), &m).unwrap();
    assert_eq!(a_ko.version, head_version + 1);
    assert_eq!(
        a_ko.properties.get("sector"),
        Some(&Value::Text("banking".into()))
    );
    assert!(!a_ko.properties.contains_key("family"));
    assert!(a_ko
        .relationships
        .iter()
        .any(|r| r.rel_type == "owns" && r.target == c));
    assert!(!a_ko
        .relationships
        .iter()
        .any(|r| r.rel_type == "supplies" && r.target == c));
    // Merge-lineage edges survive the split untouched (kernel-managed carry).
    assert!(a_ko
        .relationships
        .iter()
        .any(|r| r.rel_type == DERIVED_FROM && r.target == bank_ko));
    assert!(a_ko
        .relationships
        .iter()
        .any(|r| r.rel_type == DERIVED_FROM && r.target == fruit_ko));
    assert_eq!(a_ko.evidence().len(), 2, "evidence trail intact");

    // The relationship index followed: the moved edge is no longer reachable
    // from the original, and is reachable from the new side.
    assert!(k.outbound_edges(&m, Some("supplies")).unwrap().is_empty());
    assert_eq!(k.outbound_edges(&b_new, Some("supplies")).unwrap().len(), 1);

    // OCC + validation: a stale expected_version is a conflict, and moving
    // keys that are no longer on the subject is rejected.
    let mut again = SplitRequest {
        context: alice(),
        subject: m,
        expected_version: Some(head_version),
        b_properties: {
            let mut p = PropertyMap::new();
            p.insert("family".into(), Value::Text("Rosaceae".into()));
            p
        },
        b_relationships: vec![],
        reason: "stale retry".into(),
        idempotency_key: None,
    };
    assert!(matches!(
        k.split(again.clone()),
        Err(KError::VersionConflict { .. })
    ));
    again.expected_version = Some(head_version + 1);
    assert!(matches!(k.split(again), Err(KError::InvalidObject(_))));

    // Lineage continuity: superseding the merged entity sweeps the split-off
    // side as a dependent — the split child stays wired into derivation BFS.
    let mut sup = SupersedeRequest::new(alice(), m, "entity");
    sup.properties = a_ko.properties.clone();
    sup.evidence = vec![ev("doc/apple-superseded.md")];
    sup.reason = Some("merged record retired after the split".into());
    k.supersede(sup).unwrap();
    assert!(
        k.get(alice(), &b_new).unwrap().invalidation().is_some(),
        "split-off side must be invalidated with its premise"
    );
}
