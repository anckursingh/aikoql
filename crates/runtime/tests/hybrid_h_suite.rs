//! P5-M13 (ND-13) — AI-native hybrid query certification.
//!
//! Canonical workloads H1–H6: each pins the modality combination against a
//! hand-computed oracle — the scoring math is `kernel/src/knowledge/scoring.rs`
//! (cosine with the equal-length guard, Jaccard over tokenize) and
//! `runtime::fuse_scored` (RRF k0=60, 1-indexed ranks, score > 0 only) — plus
//! an env-gated latency cell (`AIKOQL_H_LATENCY=1`, reported, not asserted).
//! h7 writes the machine-readable benchmark report (`AIKOQL_H_BENCH=1` →
//! `target/hybrid_bench/result.json`).
//!
//! Seed (all rows owned by alice, remembered at clock 10_000):
//!
//! ```text
//! notes:  cats  {topic: pet,  body: "cats", emb: [1.0, 0.0]}
//!         dogs  {topic: pet,  body: "dogs", emb: [0.7071, 0.7071]}
//!         fish  {topic: pet,  body: "fish", emb: [0.0, 1.0]}
//!         bird  {topic: wild, body: "birds", emb: [1.0, 0.0]}
//! events: e1..e4
//! edges:  cats -mentions-> e1, e1 -mentions-> e2, dogs -mentions-> e3,
//!         fish -mentions-> e4
//!         dogs -derived_from-> cats, e1 -derived_from-> cats  (evidence)
//! ```
//!
//! Hand-computed oracles for query text "cats" (OneHotEmbeddings → [1.0, 0.0]):
//! cosine: cats 1.0, dogs 1/√2 ≈ 0.7071, fish 0.0, bird 0.0 (all unit length);
//! Jaccard: cats 1.0, dogs/fish/bird 0.0;
//! RRF k0=60: cats 2/62 ≈ 0.0322580645, dogs 1/63 ≈ 0.0158730159, fish/bird
//! absent (score > 0 rule).
//!
//! Dialect note: the shipped temporal token is `AS_OF <millis>` — the ND-13
//! example's `AS OF '...'` spelling is illustrative, not the dialect. As-of
//! compares full HLC instants, so a version written at millis M (counter > 0)
//! is visible from `AS_OF M+1` — the +1 convention pinned workspace-wide
//! (kse2_key_semantics, kse7_temporal_locality, mcp_stdio).

use std::collections::HashSet;
use std::sync::Arc;

use aikoql_compiler::parser;
use aikoql_graph::{GraphEngineApi, RelateRequest};
use aikoql_kernel::transaction::kernel::{KnowledgeContext, Subject};
use aikoql_kernel::{
    EmbeddingProvider, KResult, Kernel, ManualClock, MemoryEngine, Metadata, RememberRequest,
    SemanticBlock, Value, KOID,
};
use aikoql_runtime::{Interpreter, RowSet};

const H1: &str = r#"MATCH note WHERE topic == "pet" SIMILAR TO "cats" USING EMBEDDING RETURN *"#;
const H2: &str = r#"MATCH note SIMILAR TO "cats" TRAVERSE mentions DEPTH 2 RETURN *"#;
const H3A: &str = r#"MATCH note AS_OF 9999 TRAVERSE mentions DEPTH 1 RETURN *"#;
const H3B: &str = r#"MATCH note AS_OF 10001 TRAVERSE mentions DEPTH 1 RETURN *"#;
const H4: &str = r#"MATCH note AS_OF 10001 WHERE topic == "pet" RETURN *"#;
const H5: &str = r#"MATCH note WHERE topic == "pet" SIMILAR TO "cats" USING EMBEDDING TRAVERSE mentions DEPTH 2 RETURN *"#;
const H6: &str = H5; // same query, alice vs bob (restricted subject)

/// Deterministic text→vector table: one-hot 2-d unit vectors, so cosine is a
/// plain dot product and hand-computable (the kernel's cosine guard requires
/// equal-length vectors).
struct OneHotEmbeddings;

impl EmbeddingProvider for OneHotEmbeddings {
    fn embed(&self, text: &str, _model: Option<&str>) -> KResult<Vec<f32>> {
        Ok(match text {
            "cats" => vec![1.0, 0.0],
            "dogs" => vec![0.0, 1.0],
            _ => vec![0.70710677, 0.70710677], // unknown text: unit vector, never queried
        })
    }
}

struct Seeded {
    k: Kernel,
    cats: KOID,
    dogs: KOID,
    fish: KOID,
    e1: KOID,
    e2: KOID,
    e3: KOID,
    e4: KOID,
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn seeded() -> Seeded {
    let k = Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
    .with_embedding_provider(Arc::new(OneHotEmbeddings));
    let alice = Subject::new("alice");
    let ctx = || KnowledgeContext::new(alice.clone());

    let note = |topic: &str, body: &str, emb: Vec<f32>| -> KOID {
        let mut req = RememberRequest::create(ctx(), meta("note"));
        req.properties
            .insert("topic".into(), Value::Text(topic.into()));
        req.properties
            .insert("body".into(), Value::Text(body.into()));
        req.semantic = Some(SemanticBlock {
            embedding: Some(emb),
            embedding_model: None,
            summary: None,
            confidence: None,
            source: None,
        });
        k.remember(req).unwrap().koid
    };
    let event = |label: &str| -> KOID {
        let mut req = RememberRequest::create(ctx(), meta("event"));
        req.properties
            .insert("label".into(), Value::Text(label.into()));
        k.remember(req).unwrap().koid
    };
    let edge = |src: KOID, tgt: KOID, rel: &str| {
        k.relate(RelateRequest::new(ctx(), src, tgt, rel)).unwrap();
    };

    let cats = note("pet", "cats", vec![1.0, 0.0]);
    let dogs = note("pet", "dogs", vec![0.70710677, 0.70710677]);
    let fish = note("pet", "fish", vec![0.0, 1.0]);
    let _bird = note("wild", "birds", vec![1.0, 0.0]);
    let e1 = event("e1");
    let e2 = event("e2");
    let e3 = event("e3");
    let e4 = event("e4");
    edge(cats, e1, "mentions");
    edge(e1, e2, "mentions");
    edge(dogs, e3, "mentions");
    edge(fish, e4, "mentions");
    edge(dogs, cats, "derived_from");
    edge(e1, cats, "derived_from");
    Seeded {
        k,
        cats,
        dogs,
        fish,
        e1,
        e2,
        e3,
        e4,
    }
}

fn run(k: &Kernel, q: &str, subject: &str) -> RowSet {
    let plan = parser::compile_with_subject(q, subject).unwrap();
    Interpreter::execute(k, &plan).unwrap()
}

/// (koid, depth) pairs of a Traversal result — BFS order is pinned by the
/// executor, but the set is what the modality semantics define.
fn traversal_set(rows: RowSet) -> HashSet<(KOID, usize)> {
    match rows {
        RowSet::Traversal(t) => t.into_iter().map(|(koid, _, d)| (koid, d)).collect(),
        other => panic!("expected Traversal, got {:?}", other),
    }
}

fn median_ms(n: usize, mut f: impl FnMut()) -> f64 {
    let mut durs: Vec<f64> = (0..n)
        .map(|_| {
            let t = std::time::Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    durs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    durs[durs.len() / 2]
}

/// ND-13 acceptance "one logical query supports multiple modalities".
#[test]
fn h1_structured_to_vector() {
    let s = seeded();
    let rows = run(&s.k, H1, "alice");
    let scored = match rows {
        RowSet::Scored(scored) => scored,
        other => panic!("expected Scored, got {:?}", other),
    };
    // Filter dropped bird. Fuse over the ANN leg (cats 1.0, dogs 0.7071,
    // fish 0.0) and the text leg (cats 1.0, rest 0.0): cats ranks 1/1 →
    // 2/62, dogs rank 2 (ANN only, text score 0 excluded) → 1/63, fish
    // absent (score > 0 rule). The fused scores prove both legs fired.
    assert_eq!(scored.len(), 2, "fish/bird must be fused out: {scored:?}");
    assert_eq!(scored[0].0, s.cats, "cats must rank first: {scored:?}");
    assert!(
        (scored[0].1 - 2.0 / 62.0).abs() < 1e-6,
        "cats RRF: {scored:?}"
    );
    assert_eq!(scored[1].0, s.dogs, "dogs second: {scored:?}");
    assert!(
        (scored[1].1 - 1.0 / 63.0).abs() < 1e-6,
        "dogs RRF: {scored:?}"
    );
    if std::env::var("AIKOQL_H_LATENCY").as_deref() == Ok("1") {
        eprintln!(
            "h1_latency_median_ms={:.3}",
            median_ms(20, || {
                run(&s.k, H1, "alice");
            })
        );
    }
}

/// H2 vector → graph: the similarity result set seeds a graph walk.
#[test]
fn h2_vector_to_graph() {
    let s = seeded();
    let rows = run(&s.k, H2, "alice");
    let set = traversal_set(rows);
    // TextSearch starts = all four notes (Jaccard 0.0 entries stay in the
    // scored list — ranking is Fuse's job, not Traverse's). Closure DEPTH 2:
    // cats→e1→e2, dogs→e3, fish→e4; bird has no edges.
    let expected: HashSet<(KOID, usize)> = [(s.e1, 1), (s.e2, 2), (s.e3, 1), (s.e4, 1)]
        .into_iter()
        .collect();
    assert_eq!(set, expected, "similarity hits must seed the traversal");
    if std::env::var("AIKOQL_H_LATENCY").as_deref() == Ok("1") {
        eprintln!(
            "h2_latency_median_ms={:.3}",
            median_ms(20, || {
                run(&s.k, H2, "alice");
            })
        );
    }
}

/// H3 graph → temporal: the walk runs over an as-of snapshot.
#[test]
fn h3_graph_to_temporal() {
    let s = seeded();
    // As-of before the notes existed: reconstruction drops every row, the
    // walk starts from nothing.
    let set = traversal_set(run(&s.k, H3A, "alice"));
    assert!(set.is_empty(), "pre-existence as-of must be empty: {set:?}");
    // As-of just past the commit millis (HLC +1): all four notes, DEPTH 1 =
    // direct neighbors only.
    let set = traversal_set(run(&s.k, H3B, "alice"));
    let expected: HashSet<(KOID, usize)> = [(s.e1, 1), (s.e3, 1), (s.e4, 1)].into_iter().collect();
    assert_eq!(set, expected, "as-of 10001 must see cats/dogs/fish edges");
    if std::env::var("AIKOQL_H_LATENCY").as_deref() == Ok("1") {
        eprintln!(
            "h3_latency_median_ms={:.3}",
            median_ms(20, || {
                run(&s.k, H3B, "alice");
            })
        );
    }
}

/// H4 temporal → provenance: as-of rows answer `explain` with deterministic
/// evidence (relationships).
#[test]
fn h4_temporal_to_provenance() {
    let s = seeded();
    let rows = run(&s.k, H4, "alice");
    let koids: HashSet<KOID> = match rows {
        RowSet::Objects(objs) => objs.into_iter().map(|ko| ko.koid).collect(),
        other => panic!("expected Objects, got {}", other.shape()),
    };
    assert_eq!(koids, [s.cats, s.dogs, s.fish].into_iter().collect());
    let alice = KnowledgeContext::new(Subject::new("alice"));
    // dogs' evidence = all its relationships (mentions + provenance), in
    // insertion order; evidence is deterministic run-to-run.
    let a = s.k.explain(alice.clone(), &s.dogs, None).unwrap();
    assert_eq!(
        a.evidence,
        vec![
            ("mentions".to_string(), s.e3),
            ("derived_from".to_string(), s.cats),
        ]
    );
    let b = s.k.explain(alice.clone(), &s.dogs, None).unwrap();
    assert_eq!(a.evidence, b.evidence, "evidence must be deterministic");
    // e2 is a leaf: no outbound edges, so no evidence records.
    let c = s.k.explain(alice, &s.e2, None).unwrap();
    assert!(c.evidence.is_empty(), "leaf objects have no evidence");
    if std::env::var("AIKOQL_H_LATENCY").as_deref() == Ok("1") {
        eprintln!(
            "h4_latency_median_ms={:.3}",
            median_ms(20, || {
                run(&s.k, H4, "alice");
            })
        );
    }
}

/// H5 structured + vector + graph in ONE query: filter → ANN+text → RRF →
/// traverse. fish is fused out (score 0), so its edge e4 must not appear.
#[test]
fn h5_structured_vector_graph() {
    let s = seeded();
    let set = traversal_set(run(&s.k, H5, "alice"));
    let expected: HashSet<(KOID, usize)> = [(s.e1, 1), (s.e2, 2), (s.e3, 1)].into_iter().collect();
    assert_eq!(
        set, expected,
        "fuse must exclude fish's e4 edge from the walk"
    );

    // ND-13 acceptance "EXPLAIN shows every modality": the cost-explain
    // surface renders one line per operator — every modality of this plan.
    let lines = aikoql_runtime::cbo::explain_cost(&s.k, H5).unwrap();
    let joined = lines.join("\n");
    for modality in [
        "Scan",
        "Filter",
        "AnnSearch",
        "TextSearch",
        "Fuse",
        "Traverse",
    ] {
        assert!(
            joined.contains(modality),
            "explain must show {modality}: {joined}"
        );
    }
    if std::env::var("AIKOQL_H_LATENCY").as_deref() == Ok("1") {
        eprintln!(
            "h5_latency_median_ms={:.3}",
            median_ms(20, || {
                run(&s.k, H5, "alice");
            })
        );
    }
}

/// H6 full stack + ACL: the hybrid result set under the owner, the empty set
/// under a restricted subject, evidence deterministic and ACL-gated.
#[test]
fn h6_full_stack_with_acl() {
    let s = seeded();
    // Owner: the full H5 result.
    let set = traversal_set(run(&s.k, H6, "alice"));
    let expected: HashSet<(KOID, usize)> = [(s.e1, 1), (s.e2, 2), (s.e3, 1)].into_iter().collect();
    assert_eq!(set, expected, "owner sees the full hybrid closure");

    // Restricted subject: ACL filters the scan, and the empty set propagates
    // through the vector, fusion and graph legs — authorization applies
    // throughout the plan.
    let bob = run(&s.k, H6, "bob");
    let set = traversal_set(bob);
    assert!(set.is_empty(), "non-owner must see nothing: {set:?}");

    // Evidence: deterministic for the owner, denied for the restricted subject.
    let alice = KnowledgeContext::new(Subject::new("alice"));
    let bobctx = KnowledgeContext::new(Subject::new("bob"));
    let a = s.k.explain(alice.clone(), &s.e1, None).unwrap();
    assert_eq!(
        a.evidence,
        vec![
            ("mentions".to_string(), s.e2),
            ("derived_from".to_string(), s.cats),
        ]
    );
    let b = s.k.explain(alice, &s.e1, None).unwrap();
    assert_eq!(a.evidence, b.evidence, "evidence must be deterministic");
    assert!(
        s.k.explain(bobctx, &s.e1, None).is_err(),
        "ACL must gate explain"
    );
    if std::env::var("AIKOQL_H_LATENCY").as_deref() == Ok("1") {
        eprintln!(
            "h6_latency_median_ms={:.3}",
            median_ms(20, || {
                run(&s.k, H6, "alice");
            })
        );
    }
}

/// Machine-readable benchmark report (ND-13 acceptance "benchmark suite
/// exists"): `AIKOQL_H_BENCH=1` re-runs every canonical workload and writes
/// result.json. Off by default — CI runs only the correctness pins.
#[test]
fn h7_benchmark_report_machine_readable() {
    if std::env::var("AIKOQL_H_BENCH").as_deref() != Ok("1") {
        return;
    }
    let s = seeded();
    let workloads = [
        ("h1", H1, "alice"),
        ("h2", H2, "alice"),
        ("h3a", H3A, "alice"),
        ("h3b", H3B, "alice"),
        ("h4", H4, "alice"),
        ("h5", H5, "alice"),
        ("h6a", H6, "alice"),
        ("h6b", H6, "bob"),
    ];
    let rows_of = |rows: &RowSet| -> usize {
        match rows {
            RowSet::Objects(o) => o.len(),
            RowSet::Scored(o) => o.len(),
            RowSet::Traversal(o) => o.len(),
            _ => 0,
        }
    };
    let report = workloads
        .iter()
        .map(|(name, q, subj)| {
            let median = median_ms(20, || {
                let _ = run(&s.k, q, subj);
            });
            let rows = rows_of(&run(&s.k, q, subj));
            serde_json::json!({
                "name": name,
                "query": q,
                "rows": rows,
                "median_ms": median,
            })
        })
        .collect::<Vec<_>>();
    let out = serde_json::json!({
        "suite": "hybrid_h_suite",
        "n": 20,
        "generated_at_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "workloads": report,
    });
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/hybrid_bench");
    std::fs::create_dir_all(dir).unwrap();
    let path = format!("{dir}/result.json");
    std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap()).unwrap();
    // Round-trip: the report is machine-readable JSON, not a string blob.
    let parsed: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["workloads"].as_array().unwrap().len(), 8);
}
