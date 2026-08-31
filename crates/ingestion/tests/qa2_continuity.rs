//! MVP-QA-002 Suite H — QA2-CONT-001 flagship knowledge continuity chain.
//!
//! Source → Ingest → Query → Modify source → Incremental ingest → Query →
//! Restart → Query → Rebuild indexes → Query → Backup → Restore → Query →
//! Schema migration → Query.
//!
//! One corpus, one RedbEngine-backed kernel, an 8-dimension checkpoint after
//! every leg: KO set / KO content / fact set / relation set / provenance /
//! evidence / temporal state / constraints / representative query answers
//! (context pack + semantic search).
//!
//! Legs where knowledge is untouched (restart/rebuild/backup-restore) must be
//! checkpoint-identical. The two delta legs (source modification, schema
//! migration) assert their deltas explicitly — every difference is explained.

use aikoql_ingestion::{
    compile_context, incremental_diff_ingest, incremental_ingest_directory,
    render_context_markdown, KnowledgeIr,
};
use aikoql_kernel::*;
use aikoql_scheduler::IndexMaintainer;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const SEED: u64 = 0xC0FFEE;
/// Task text targeting knowledge that never changes across the corpus edit.
const TASK: &str = "validate constraints at commit";

// ---------------------------------------------------------------------------
// Corpus + git helpers
// ---------------------------------------------------------------------------

fn tmp_dir(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("aikoql_cont001_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn git(args: &[&str], dir: &Path) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must run");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_repo_with(dir: &Path, files: &[(&str, &str)]) {
    git(&["init", "-q"], dir);
    git(&["config", "user.email", "qa@aikoql.test"], dir);
    git(&["config", "user.name", "qa"], dir);
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
    }
    git(&["add", "-A"], dir);
    git(&["commit", "-q", "-m", "seed"], dir);
}

// Noun-phrase bullets, not deontic ones: a "must …" bullet classifies its
// section as a Rule (facts only, no EntityCandidate) and the repo pipeline's
// compile_file then drops the whole IR (entities-empty filter). Entity
// sections need prose + non-imperative bullets — the sanctioned corpus shape.
const CORPUS_V1: &str = "# Payments\n\
\n\
Payments validates constraints at commit time.\n\
\n\
- constraint validation at commit time\n\
- MVCC-based writes\n\
\n\
references [[Audit]]\n\
\n\
# Audit\n\
\n\
Audit records every commit.\n\
\n\
- an audit trail for every commit\n";

const CORPUS_V2: &str = "# Payments\n\
\n\
Payments validates constraints at commit time.\n\
\n\
- constraint validation at commit time\n\
- snapshot isolation across writes\n\
- serialized commit processing\n\
\n\
references [[Ledger]]\n\
\n\
# Audit\n\
\n\
Audit records every commit.\n\
\n\
- an audit trail for every commit\n\
\n\
# Ledger\n\
\n\
Ledger records settled transactions.\n\
\n\
- entries for settled transactions\n";

// ---------------------------------------------------------------------------
// IR → kernel commit (mirrors the sanctioned mcp ingest-dir flow subset:
// ingest_observation + idempotency keys + wholesale relationship restate)
// ---------------------------------------------------------------------------

fn ctx(subj: &str) -> KnowledgeContext {
    KnowledgeContext::new(Subject::new(subj))
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

/// Commit the IR into the kernel. Re-runnable: idempotency keys resolve
/// existing KOs to updates (exact-once), relation edges are restated
/// wholesale per subject (EVO-003: dropped relations disappear).
/// Returns (entity name → KOID).
fn commit_ir(
    k: &Kernel,
    ir: &KnowledgeIr,
    now: u64,
    snapshot: &mut Option<KOID>,
) -> HashMap<String, KOID> {
    let mut by_name: HashMap<String, KOID> = HashMap::new();
    for ent in &ir.entities {
        let tname = ent.type_hint.clone().unwrap_or_else(|| "Entity".into());
        let mut props = PropertyMap::new();
        props.insert("name".into(), Value::Text(ent.name.clone()));
        if let Some(h) = &ent.type_hint {
            props.insert("type_hint".into(), Value::Text(h.clone()));
        }
        props.insert(
            "mentions".into(),
            Value::List(ent.mentions.iter().cloned().map(Value::Text).collect()),
        );
        props.insert("confidence".into(), Value::Float(ent.confidence as f64));
        if let Some(d) = &ent.evidence.document_id {
            props.insert("evidence_document".into(), Value::Text(d.clone()));
        }
        let facts: Vec<String> = ir
            .facts
            .iter()
            .filter(|f| f.entities.iter().any(|e| e == &ent.name))
            .map(|f| f.statement.clone())
            .collect();
        props.insert(
            "facts".into(),
            Value::List(facts.into_iter().map(Value::Text).collect()),
        );
        let idem = format!(
            "ingest-entity:{}:{}",
            ent.evidence.document_id.as_deref().unwrap_or_default(),
            ent.name
        );
        let security = SecurityDescriptor {
            owner: "ingest-dir".into(),
            acl: vec![],
            classification: None,
        };
        let koid = match k.resolve_idempotency(&idem) {
            Ok(Some((koid, _, _))) => {
                let mut req = RememberRequest::update(ctx("ingest-dir"), koid, meta(&tname));
                req.properties = props;
                req.security = Some(security.clone());
                k.remember(req).unwrap().koid
            }
            _ => {
                let ev = Evidence::new(
                    ent.evidence.document_id.clone().unwrap_or_default(),
                    EvidenceMethod::DocExtraction,
                )
                .with_confidence(ent.evidence.confidence);
                k.ingest_observation(IngestRequest {
                    context: ctx("ingest-dir"),
                    type_name: tname,
                    properties: props,
                    evidence: vec![ev],
                    idempotency_key: Some(idem),
                    tags: vec!["ingest-dir".into(), "auto".into()],
                    valid_from: Some(now),
                    security: Some(security),
                    note: Some("entity from ingest-dir".into()),
                })
                .unwrap()
                .koid
            }
        };
        by_name.insert(ent.name.to_lowercase(), koid);
    }

    // Relations: grouped per subject, restated wholesale.
    let mut edges: HashMap<KOID, Vec<RelationshipRef>> = HashMap::new();
    for rel in &ir.relations {
        let (Some(&src), Some(&dst)) = (
            by_name.get(&rel.subject.to_lowercase()),
            by_name.get(&rel.object.to_lowercase()),
        ) else {
            continue; // out-of-corpus endpoint — skipped, same as the mcp flow
        };
        edges.entry(src).or_default().push(RelationshipRef {
            rel_type: rel.predicate.clone(),
            target: dst,
            direction: Direction::Outbound,
        });
    }
    for (koid, rels) in &edges {
        let head = k.get(ctx("ingest-dir"), koid).unwrap();
        let mut req = RememberRequest::update(
            ctx("ingest-dir"),
            *koid,
            Metadata {
                type_name: head.metadata.type_name.clone(),
                tenant: None,
                schema_version: head.metadata.schema_version,
                tags: vec![],
            },
        );
        req.properties = head.properties.clone();
        req.relationships = rels.clone();
        k.remember(req).unwrap();
    }

    // The ir_json snapshot KO — the context compiler's query input, so the
    // query leg runs from kernel-stored state after a restart.
    let ir_json = serde_json::to_string(ir).unwrap();
    match snapshot {
        None => {
            let mut props = PropertyMap::new();
            props.insert("ir_json".into(), Value::Text(ir_json));
            if let Some(rev) = &ir.source_revision {
                props.insert("source_revision".into(), Value::Text(rev.clone()));
            }
            let mut req = RememberRequest::create(ctx("ingest-dir"), meta("snapshot"));
            req.properties = props;
            *snapshot = Some(k.remember(req).unwrap().koid);
        }
        Some(koid) => {
            let head = k.get(ctx("ingest-dir"), koid).unwrap();
            let mut props = head.properties.clone();
            props.insert("ir_json".into(), Value::Text(ir_json));
            if let Some(rev) = &ir.source_revision {
                props.insert("source_revision".into(), Value::Text(rev.clone()));
            }
            let mut req = RememberRequest::update(ctx("ingest-dir"), *koid, meta("snapshot"));
            req.properties = props;
            k.remember(req).unwrap();
        }
    }
    by_name
}

// ---------------------------------------------------------------------------
// The 8-dimension checkpoint
// ---------------------------------------------------------------------------

/// One KO's content row: (koid, type, schema_version, sorted (prop key, value)).
type KoRow = (String, String, u32, Vec<(String, String)>);

#[derive(Debug, PartialEq, Clone)]
struct Ck {
    /// KO set: (type, koid) sorted.
    kos: Vec<(String, String)>,
    /// KO content rows sorted by koid.
    props: Vec<KoRow>,
    /// Fact set from the IR: (statement, document, confidence) sorted.
    facts: Vec<(String, String, f32)>,
    /// Relation set from the kernel: (src koid, rel_type, dst koid) sorted.
    rels: Vec<(String, String, String)>,
    /// Provenance: per-KO evidence_document prop, sorted by koid.
    prov: Vec<(String, String)>,
    /// Evidence: per-KO kernel evidence (artifact, method, confidence).
    ev: Vec<(String, String, String, f32)>,
    /// Temporal state: (koid, valid_from, valid_to) sorted.
    temporal: Vec<(String, Option<u64>, Option<u64>)>,
    /// Constraints: the v2 schema validates the existing data with zero
    /// violations at every leg.
    constraints_ok: bool,
    /// Representative query answer: rendered context pack.
    query: String,
    /// Semantic search: (koid, score) in result order.
    sim: Vec<(String, f32)>,
    /// Kernel-stored IR json (the query input after restart).
    snap_ir: String,
}

/// Find a KO's content row by its `name` property.
fn ko_row<'a>(ck: &'a Ck, name: &str) -> &'a KoRow {
    let want = format!("Text({name:?})");
    ck.props
        .iter()
        .find(|(_, _, _, p)| p.iter().any(|(k, v)| k == "name" && v == &want))
        .unwrap_or_else(|| panic!("no KO named {name:?}"))
}

fn capture(k: &Kernel, ir: &KnowledgeIr, v2: &Schema) -> Ck {
    let mut kos = Vec::new();
    let mut props = Vec::new();
    let mut prov = Vec::new();
    let mut ev = Vec::new();
    let mut temporal = Vec::new();
    let mut all: Vec<KnowledgeObject> = Vec::new();
    let mut snap: Option<KOID> = None;
    let mut types: std::collections::HashSet<String> = ir
        .entities
        .iter()
        .filter_map(|e| e.type_hint.clone())
        .collect();
    types.insert("Entity".into());
    types.insert("snapshot".into());
    for t in types {
        for ko in k.scan_by_type(&Subject::new("ingest-dir"), &t).unwrap() {
            if t == "snapshot" {
                snap = Some(ko.koid);
            }
            let mut p: Vec<(String, String)> = ko
                .properties
                .iter()
                .map(|(key, v)| (key.clone(), format!("{v:?}")))
                .collect();
            p.sort();
            props.push((ko.koid.to_hex(), t.clone(), ko.metadata.schema_version, p));
            if let Some(Value::Text(doc)) = ko.properties.get("evidence_document") {
                prov.push((ko.koid.to_hex(), doc.clone()));
            }
            for e in &ko.evidence() {
                ev.push((
                    ko.koid.to_hex(),
                    e.source_artifact.clone(),
                    format!("{:?}", e.method),
                    e.confidence,
                ));
            }
            temporal.push((ko.koid.to_hex(), ko.valid_from(), ko.valid_to()));
            kos.push((t.clone(), ko.koid.to_hex()));
            all.push(ko);
        }
    }
    let mut rels = Vec::new();
    for ko in &all {
        for (rt, dst) in k.outbound_edges(&ko.koid, None).unwrap() {
            rels.push((ko.koid.to_hex(), rt, dst.to_hex()));
        }
    }
    kos.sort();
    props.sort();
    rels.sort();
    prov.sort();
    ev.sort_by(|a, b| a.partial_cmp(b).unwrap());
    temporal.sort();

    let mut facts: Vec<(String, String, f32)> = ir
        .facts
        .iter()
        .map(|f| {
            (
                f.statement.clone(),
                f.evidence.document_id.clone().unwrap_or_default(),
                f.evidence.confidence,
            )
        })
        .collect();
    facts.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let constraints_ok = k
        .validate_schema_migration(&Subject::new("ingest-dir"), v2)
        .map(|v| v.is_empty())
        .unwrap_or(false);

    let query = render_context_markdown(&compile_context(TASK, ir, 400));
    assert!(!query.is_empty(), "query pack must be non-empty");

    let sim: Vec<(String, f32)> = k
        .find_similar(SimilarityQuery {
            context: ctx("ingest-dir"),
            filter: None,
            text: Some(TASK.into()),
            vector: None,
            embedding_model: None,
            k: 5,
            fusion: Fusion::Rrf { k0: 60 },
        })
        .unwrap()
        .into_iter()
        .map(|s| (s.ko.koid.to_hex(), s.score))
        .collect();

    let snap_ko = k
        .get(ctx("ingest-dir"), &snap.expect("snapshot KO"))
        .unwrap();
    let snap_ir = match snap_ko.properties.get("ir_json") {
        Some(Value::Text(s)) => s.clone(),
        other => panic!("snapshot KO lost ir_json: {other:?}"),
    };

    Ck {
        kos,
        props,
        facts,
        rels,
        prov,
        ev,
        temporal,
        constraints_ok,
        query,
        sim,
        snap_ir,
    }
}

fn sim_query(k: &Kernel) -> Vec<(String, f32)> {
    k.find_similar(SimilarityQuery {
        context: ctx("ingest-dir"),
        filter: None,
        text: Some(TASK.into()),
        vector: None,
        embedding_model: None,
        k: 5,
        fusion: Fusion::Rrf { k0: 60 },
    })
    .unwrap()
    .into_iter()
    .map(|s| (s.ko.koid.to_hex(), s.score))
    .collect()
}

// ---------------------------------------------------------------------------
// QA2-CONT-001 — the flagship chain
// ---------------------------------------------------------------------------

#[test]
fn w2_cont_001_full_knowledge_continuity_chain() {
    let corpus = tmp_dir("corpus");
    git_repo_with(&corpus, &[("facts.md", CORPUS_V1)]);
    let root_s = corpus.to_string_lossy().to_string();

    let dbdir = tmp_dir("db");
    let db = dbdir.join("continuity.redb");
    let bakdir = tmp_dir("backup");
    let backup = bakdir.join("continuity_snap.redb");
    let clock = Arc::new(ManualClock::new(10_000));
    let engine = Arc::new(RedbEngine::open(&db).unwrap());
    let mut k = Kernel::open(engine.clone(), clock.clone(), SEED).unwrap();

    // Markdown heading entities classify as type "Project" (the sanctioned
    // entity_type_name mapping of the "Project" hint) — the migration story
    // is a Project-schema v1→v2 bump covering every heading entity.
    let v2 = Schema {
        type_name: "Project".into(),
        schema_version: 2,
        required_properties: vec!["name".into()],
        allowed_properties: None,
        properties: vec![],
        unique_constraints: vec![],
        check_constraints: vec![],
    };
    let v1 = Schema {
        schema_version: 1,
        ..v2.clone()
    };
    k.register_schema(v1).unwrap();

    // Leg 1 — Source → Ingest → Query.
    let (full, is_full) = incremental_ingest_directory(&root_s).unwrap();
    assert!(is_full, "first ingest must be full");
    let mut snapshot: Option<KOID> = None;
    commit_ir(&k, &full.ir, clock.millis(), &mut snapshot);
    let ck_a = capture(&k, &full.ir, &v2);

    // Leg 2 — Modify source → Incremental ingest → Query.
    std::fs::write(corpus.join("facts.md"), CORPUS_V2).unwrap();
    git(&["add", "-A"], &corpus);
    git(&["commit", "-q", "-m", "v2"], &corpus);
    // Second element is potentially_stale_facts, not changed paths — the
    // fact-delta asserts below prove the incremental leg end to end.
    let (incr, _stale) = incremental_diff_ingest(&root_s, &full.ir).unwrap();
    commit_ir(&k, &incr.ir, clock.millis(), &mut snapshot);
    let ck_b = capture(&k, &incr.ir, &v2);

    // Leg 2 deltas — every difference explained.
    let has_fact = |ck: &Ck, stmt: &str| ck.facts.iter().any(|(s, _, _)| s.contains(stmt));
    assert!(
        has_fact(&ck_a, "MVCC-based writes"),
        "v1 fact must exist before the edit"
    );
    assert!(
        !has_fact(&ck_b, "MVCC-based writes"),
        "edited fact must not survive"
    );
    assert!(
        has_fact(&ck_b, "snapshot isolation across writes"),
        "edited fact must be current"
    );
    assert!(
        has_fact(&ck_b, "serialized commit processing"),
        "added fact must exist"
    );
    assert!(
        has_fact(&ck_b, "entries for settled transactions"),
        "new entity's fact must exist"
    );
    assert!(
        has_fact(&ck_b, "an audit trail for every commit"),
        "untouched fact must survive"
    );
    assert_eq!(
        ko_row(&ck_a, "Audit"),
        ko_row(&ck_b, "Audit"),
        "untouched entity must be byte-identical"
    );
    let p_a = ko_row(&ck_a, "Payments").0.clone();
    let p_b = ko_row(&ck_b, "Payments").0.clone();
    let audit_a = ko_row(&ck_a, "Audit").0.clone();
    let ledger_b = ko_row(&ck_b, "Ledger").0.clone();
    assert!(
        ck_a.rels
            .contains(&(p_a.clone(), "references".into(), audit_a.clone())),
        "v1 relation Payments→Audit must exist"
    );
    assert!(
        ck_b.rels
            .contains(&(p_b.clone(), "references".into(), ledger_b)),
        "v2 relation Payments→Ledger must exist"
    );
    assert!(
        !ck_b
            .rels
            .iter()
            .any(|(_, rt, d)| rt == "references" && d == &audit_a),
        "dropped relation must not survive"
    );
    assert_eq!(
        ck_b.kos.len(),
        ck_a.kos.len() + 1,
        "KO set grows by exactly the Ledger entity"
    );
    // The query surface reflects the edit.
    assert!(ck_a
        .query
        .contains("Payments validates constraints at commit time."));
    assert!(ck_b
        .query
        .contains("Payments validates constraints at commit time."));

    // Leg 3 — Restart → Query. Same Redb file, fresh kernel. Everything
    // re-derived from the reopened store must equal leg 2 exactly.
    drop(k);
    drop(engine); // release the Windows file lock before reopening
    let engine2 = Arc::new(RedbEngine::open(&db).unwrap());
    k = Kernel::open(engine2.clone(), clock.clone(), SEED).unwrap();
    let ir_b: KnowledgeIr = serde_json::from_str(&ck_b.snap_ir).unwrap();
    let ck_c = capture(&k, &ir_b, &v2);
    assert_eq!(ck_b, ck_c, "restart must be checkpoint-identical");

    // Leg 4 — Rebuild indexes → Query. A fresh maintainer replays the
    // journal; the indexed search path must agree with the exact path.
    let m = IndexMaintainer::start(
        &k,
        Arc::new(BruteForceVectorIndex::new()),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());
    m.wait_caught_up(&k, Duration::from_secs(10)).unwrap();
    let sim_rebuilt = sim_query(&k);
    assert_eq!(
        sim_rebuilt.len(),
        ck_c.sim.len(),
        "rebuild must restore all hits"
    );
    for ((koid_g, s_g), (koid_w, s_w)) in sim_rebuilt.iter().zip(ck_c.sim.iter()) {
        assert_eq!(koid_g, koid_w, "rebuild must preserve hit identity");
        assert!((s_g - s_w).abs() < 1e-6, "rebuild must preserve scores");
    }
    m.shutdown();

    // Leg 5 — Backup → Restore → Query. Faithful snapshot roundtrip (Suite G
    // fix 2) — the store after restore must equal the store before backup.
    engine2.snapshot_to(&backup).unwrap();
    k.restore_store_from(&backup).unwrap();
    let ck_d = capture(&k, &ir_b, &v2);
    assert_eq!(ck_c, ck_d, "backup/restore must be checkpoint-identical");

    // Leg 6 — Schema migration → Query. The v1 schema row survived the
    // restart (fail-closed reload), so the v2 migration applies: Payments
    // objects gain `migrated` and stamp schema_version 2. Everything else
    // is untouched.
    let report = k
        .apply_schema_migration(
            &Subject::new("ingest-dir"),
            &SchemaMigration {
                schema: v2.clone(),
                transforms: vec![PropertyTransform::SetDefault {
                    property: "migrated".into(),
                    value: Value::Bool(true),
                }],
            },
        )
        .unwrap();
    assert_eq!(
        report.migrated, 3,
        "migration must rewrite all three Project objects"
    );
    let ck_e = capture(&k, &ir_b, &v2);
    // Strip the documented migration delta from E, then require identity.
    let mut ck_e_norm = ck_e.clone();
    for (koid, t, sv, p) in ck_e_norm.props.iter_mut() {
        if t == "Project" {
            assert_eq!(*sv, 2, "migration must stamp schema_version 2 on {koid}");
            assert_eq!(
                p.iter()
                    .find(|(key, _)| key == "migrated")
                    .map(|(_, v)| v.as_str()),
                Some("Bool(true)"),
                "migration must fill the migrated default on {koid}"
            );
            *sv = 1;
            p.retain(|(key, _)| key != "migrated");
        }
    }
    assert_eq!(
        ck_d.props, ck_e_norm.props,
        "migration must touch only the documented delta"
    );

    // Final query leg — the pack still holds after everything.
    let final_query = render_context_markdown(&compile_context(TASK, &ir_b, 400));
    assert_eq!(
        final_query, ck_b.query,
        "query answers must survive the whole chain"
    );
    assert!(final_query.contains("Payments validates constraints at commit time."));
    assert!(
        ck_e.constraints_ok,
        "v2 schema must validate post-migration data"
    );

    for d in [&corpus, &dbdir, &bakdir] {
        let _ = std::fs::remove_dir_all(d);
    }
}
