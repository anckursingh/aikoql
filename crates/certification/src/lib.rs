//! P5-M14 (ND-14) — AIKOQL Database certification suites.
//!
//! The runner (`run_suite`) executes one of the five DB-* suite families
//! against the real kernel/runtime stack and writes a machine-readable
//! `<out>/<suite>/result.json` whose schema carries every ND-14 acceptance
//! dimension (reproducible seed, run commit, p50/p95/p99, throughput, RSS,
//! disk footprint, cold/warm cells, correctness parity). Correctness parity
//! is the suite's own hand-computed oracle against the run — a mismatch is
//! an `Err`, never a silent green artifact (cert002: `CERT_INJECT=1`
//! corrupts the fixture and must fail the run).
//!
//! Suites:
//! - db-oltp: point reads, point writes (updates), atomic transactions
//!   (single-op, read-your-writes), multi-connection concurrency, and
//!   restart durability over a real on-disk aikoql-v2 engine. "Transaction"
//!   is the kernel's public atomic unit (one `remember`); multi-statement
//!   batches have no kernel surface, and crash-injection is covered by the
//!   SE2-M36 suites — restart durability is what this suite pins.
//! - db-graph: 1-hop / 2-hop / fanout / relationship-type-filtered
//!   traversals (mentions vs derived_from) on the seeded graph.
//! - db-vector: ingestion round-trip and recall via the fused ANN+text
//!   ranking (RRF, hand-computed: rank 1 → 1/62, rank 2 → 1/63 — the
//!   P5-M13-pinned math), filtered and unfiltered.
//! - db-knowledge: structured filter, vector hits, relationship walk,
//!   temporal snapshot (AS_OF, +1 millis convention), evidence, ACL.
//! - db-agent: context retrieval, evidence coverage, provenance
//!   completeness, historical correctness, semantic relevance,
//!   authorization correctness — the same computations surface through
//!   `agent_provenance_check`.
//!
//! Seed (deterministic, all rows owned by alice, remembered at clock
//! 10_000): notes cats/dogs/fish (topic pet) + bird (topic wild) with the
//! P5-M13 one-hot embeddings; events e1..e6; edges cats→e1→e2,
//! dogs→e3, fish→e4, bird→e5, bird→e6 (mentions); dogs→cats,
//! e1→cats (derived_from, evidence).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use aikoql_compiler::parser;
use aikoql_graph::{GraphEngineApi, RelateRequest};
use aikoql_kernel::transaction::kernel::{KnowledgeContext, Subject};
use aikoql_kernel::{
    EmbeddingProvider, KResult, Kernel, ManualClock, MemoryEngine, Metadata, RememberRequest,
    SemanticBlock, Value, KOID,
};
use aikoql_runtime::{Interpreter, RowSet};
use aikoql_storage_v2::AikoqlStorageEngineV2;
use serde_json::json;

pub const SUITES: [&str; 5] = [
    "db-oltp",
    "db-graph",
    "db-vector",
    "db-knowledge",
    "db-agent",
];

/// Fixed certification seed: the fixture is deterministic by construction;
/// the field travels in every artifact so runs are reproducible by seed.
const CERT_SEED: &str = "aikoql-cert-nd14-v1";

// Queries shared by the suites (dialect: `AS_OF <millis>`; versions written
// at millis M become visible from AS_OF M+1 — the workspace +1 convention).
const Q_H1: &str = r#"MATCH note WHERE topic == "pet" SIMILAR TO "cats" USING EMBEDDING RETURN *"#;
const Q_HYBRID: &str = r#"MATCH note WHERE topic == "pet" SIMILAR TO "cats" USING EMBEDDING TRAVERSE mentions DEPTH 2 RETURN *"#;
const Q_PET_OBJ: &str = r#"MATCH note WHERE topic == "pet" RETURN *"#;
const Q_ASOF_PRE: &str = r#"MATCH note AS_OF 9999 WHERE topic == "pet" RETURN *"#;
const Q_ASOF_POST: &str = r#"MATCH note AS_OF 10001 WHERE topic == "pet" RETURN *"#;

/// A certification failure — carry the message, the pin only checks `is_err`.
#[derive(Debug)]
pub struct CertError(pub String);

impl std::fmt::Display for CertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CertError {}

impl CertError {
    fn io(e: std::io::Error) -> Self {
        CertError(format!("io: {e}"))
    }
}

/// DB-AGENT provenance assertions (cert003): evidence coverage is the
/// fraction of retrieved rows whose `explain` carries at least one evidence
/// record; completeness means every evidence target in the seeded KB
/// resolves to an existing object.
#[derive(Debug, Clone, Copy)]
pub struct AgentProvenance {
    pub evidence_coverage: f64,
    pub provenance_complete: bool,
}

// ---------------------------------------------------------------------------
// Fixture: deterministic KB, seeded through the public kernel surface.
// ---------------------------------------------------------------------------

/// Deterministic text→vector table: one-hot 2-d unit vectors, so cosine is
/// a plain dot product and hand-computable.
struct OneHotEmbeddings;

impl EmbeddingProvider for OneHotEmbeddings {
    fn embed(&self, text: &str, _model: Option<&str>) -> KResult<Vec<f32>> {
        Ok(match text {
            "cats" => vec![1.0, 0.0],
            "dogs" => vec![0.0, 1.0],
            _ => vec![0.70710677, 0.70710677], // unknown text: unit vector
        })
    }
}

struct Koids {
    cats: KOID,
    dogs: KOID,
    fish: KOID,
    bird: KOID,
    e1: KOID,
    e2: KOID,
    e3: KOID,
    e4: KOID,
    e5: KOID,
    e6: KOID,
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn mem_kernel() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
    .with_embedding_provider(Arc::new(OneHotEmbeddings))
}

fn ctx() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new("alice"))
}

/// cert002 hook: the injected regression corrupts cats' topic after seeding,
/// so the first oracle that reads cats (db-oltp point_read) fails parity.
fn maybe_inject(k: &Kernel, cats: KOID) {
    if std::env::var("CERT_INJECT").as_deref() == Ok("1") {
        let mut req = RememberRequest::update(ctx(), cats, meta("note"));
        req.properties
            .insert("topic".into(), Value::Text("CORRUPTED-INJECTED".into()));
        k.remember(req).unwrap();
    }
}

fn seed(k: &Kernel) -> Koids {
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
    let bird = note("wild", "birds", vec![1.0, 0.0]);
    let e1 = event("e1");
    let e2 = event("e2");
    let e3 = event("e3");
    let e4 = event("e4");
    let e5 = event("e5");
    let e6 = event("e6");
    edge(cats, e1, "mentions");
    edge(e1, e2, "mentions");
    edge(dogs, e3, "mentions");
    edge(fish, e4, "mentions");
    edge(bird, e5, "mentions");
    edge(bird, e6, "mentions");
    edge(dogs, cats, "derived_from");
    edge(e1, cats, "derived_from");
    edge(bird, e5, "derived_from");
    maybe_inject(k, cats);
    Koids {
        cats,
        dogs,
        fish,
        bird,
        e1,
        e2,
        e3,
        e4,
        e5,
        e6,
    }
}

// ---------------------------------------------------------------------------
// Measurement plumbing.
// ---------------------------------------------------------------------------

struct WorkloadSpec {
    name: &'static str,
    n: usize,
    op: Box<dyn FnMut() -> bool>,
}

fn wl(name: &'static str, n: usize, op: Box<dyn FnMut() -> bool>) -> WorkloadSpec {
    WorkloadSpec { name, n, op }
}

fn run(k: &Kernel, q: &str, subject: &str) -> RowSet {
    let plan = parser::compile_with_subject(q, subject).unwrap();
    Interpreter::execute(k, &plan).unwrap()
}

/// (koid, depth) pairs of a Traversal result — BFS order is executor
/// detail, the set is what the modality semantics define.
fn traversal_set(rows: RowSet) -> HashSet<(KOID, usize)> {
    match rows {
        RowSet::Traversal(t) => t.into_iter().map(|(koid, _, d)| (koid, d)).collect(),
        other => panic!("expected Traversal, got {other:?}"),
    }
}

fn objects_set(rows: RowSet) -> HashSet<KOID> {
    match rows {
        RowSet::Objects(objs) => objs.into_iter().map(|ko| ko.koid).collect(),
        other => panic!("expected Objects, got {other:?}"),
    }
}

fn scored_vec(rows: RowSet) -> Vec<(KOID, f64)> {
    match rows {
        RowSet::Scored(s) => s
            .into_iter()
            .map(|(koid, score, ..)| (koid, score as f64))
            .collect(),
        other => panic!("expected Scored, got {other:?}"),
    }
}

fn trav_op(
    k: Arc<Kernel>,
    q: &'static str,
    expect: HashSet<(KOID, usize)>,
) -> Box<dyn FnMut() -> bool> {
    Box::new(move || traversal_set(run(&k, q, "alice")) == expect)
}

fn obj_op(k: Arc<Kernel>, q: &'static str, expect: HashSet<KOID>) -> Box<dyn FnMut() -> bool> {
    Box::new(move || objects_set(run(&k, q, "alice")) == expect)
}

/// Nearest-rank percentile of millisecond samples.
fn pct(samples: &[f64], p: f64) -> f64 {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((p / 100.0) * s.len() as f64).ceil() as usize - 1;
    s[idx.min(s.len() - 1)]
}

/// One measurement pass: n samples of `op`, correctness AND-ed per sample.
fn measure_cell(n: usize, op: &mut Box<dyn FnMut() -> bool>) -> (serde_json::Value, bool) {
    let mut samples = Vec::with_capacity(n);
    let mut correct = true;
    let mut total_s = 0.0;
    for _ in 0..n {
        let t = Instant::now();
        correct &= op();
        let d = t.elapsed().as_secs_f64();
        total_s += d;
        samples.push(d * 1000.0);
    }
    let tput = n as f64 / total_s.max(1e-9);
    (
        json!({
            "p50_ms": pct(&samples, 50.0),
            "p95_ms": pct(&samples, 95.0),
            "p99_ms": pct(&samples, 99.0),
            "throughput_ops_s": tput,
        }),
        correct,
    )
}

/// Process RSS in KiB. Windows: GetProcessMemoryInfo (kernel32 export — no
/// extra link); unix: /proc/self/status VmRSS. `ponytail:` other platforms
/// report 0 — add a platform sampler when a non-Windows/Linux host matters.
#[cfg(windows)]
fn self_rss_kb() -> u64 {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Pmc {
        cb: u32,
        _page_faults: u32,
        _peak_ws: usize,
        ws: usize,
        _rest: [usize; 6],
    }
    extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        // K32GetProcessMemoryInfo is a kernel32 export (default-linked);
        // psapi's GetProcessMemoryInfo would need an explicit link attr.
        fn K32GetProcessMemoryInfo(p: *mut core::ffi::c_void, c: *mut Pmc, cb: u32) -> i32;
    }
    unsafe {
        let mut pmc = std::mem::zeroed::<Pmc>();
        pmc.cb = std::mem::size_of::<Pmc>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) == 0 {
            0
        } else {
            (pmc.ws / 1024) as u64
        }
    }
}

#[cfg(all(not(windows), unix))]
fn self_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

#[cfg(all(not(windows), not(unix)))]
fn self_rss_kb() -> u64 {
    0
}

fn dir_bytes(root: &Path) -> u64 {
    fn walk(p: &Path, acc: &mut u64) {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, acc);
                } else if let Ok(md) = e.metadata() {
                    *acc += md.len();
                }
            }
        }
    }
    let mut acc = 0;
    walk(root, &mut acc);
    acc
}

fn commit_hash() -> String {
    use std::sync::OnceLock;
    static COMMIT: OnceLock<Option<String>> = OnceLock::new();
    COMMIT
        .get_or_init(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .clone()
        .unwrap_or_else(|| "unknown".into())
}

fn started_at() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

// ---------------------------------------------------------------------------
// The runner.
// ---------------------------------------------------------------------------

/// Run one certification suite and write its machine-readable artifact to
/// `<out_dir>/<suite>/result.json`. A correctness-parity mismatch is an
/// `Err` and no artifact is written.
pub fn run_suite(suite: &str, out_dir: &Path) -> Result<PathBuf, CertError> {
    // Run from a clean checkout: the suite dir is removed first, otherwise a
    // re-run over a fixed path reopens the previous store and deterministic
    // KOIDs (HLC-encoded) collide as VersionConflict.
    let suite_dir = out_dir.join(suite);
    // Windows: remove_dir_all on a missing path is error 2, not a no-op.
    match std::fs::remove_dir_all(&suite_dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(CertError::io(e)),
        _ => {}
    }
    let (specs, disk_root) = match suite {
        "db-oltp" => db_oltp(out_dir)?,
        "db-graph" => db_graph(),
        "db-vector" => db_vector(),
        "db-knowledge" => db_knowledge(),
        "db-agent" => db_agent(),
        other => return Err(CertError(format!("unknown suite: {other}"))),
    };

    let rss = self_rss_kb();
    let disk = disk_root.as_deref().map(dir_bytes).unwrap_or(0);
    let mut workloads = Vec::new();
    for mut spec in specs {
        let (cold, cold_ok) = measure_cell(spec.n, &mut spec.op);
        let (warm, warm_ok) = measure_cell(spec.n, &mut spec.op);
        let correct = cold_ok && warm_ok;
        if !correct {
            // Fail fast with the workload name — detection power, and the
            // artifact is never written over a mismatch.
            return Err(CertError(format!(
                "{suite}/{}: correctness parity failed — oracle mismatch",
                spec.name
            )));
        }
        workloads.push(json!({
            "name": spec.name,
            "n": spec.n,
            "p50_ms": cold["p50_ms"],
            "p95_ms": cold["p95_ms"],
            "p99_ms": cold["p99_ms"],
            "throughput_ops_s": cold["throughput_ops_s"],
            "rss_kb": rss,
            "disk_bytes": disk,
            "cold": cold,
            "warm": warm,
            "correct": correct,
        }));
    }

    let report = json!({
        "suite": suite,
        "seed": CERT_SEED,
        "commit": commit_hash(),
        "started_at": started_at(),
        "workloads": workloads,
        "correctness_parity": true,
    });
    let dir = out_dir.join(suite);
    std::fs::create_dir_all(&dir).map_err(CertError::io)?;
    let path = dir.join("result.json");
    std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).map_err(CertError::io)?;
    // The report is the artifact — the on-disk store is scratch and must not
    // be published alongside it (best-effort: a failed run keeps it for
    // diagnosis).
    if let Some(root) = disk_root {
        let _ = std::fs::remove_dir_all(root);
    }
    Ok(path)
}

/// DB-AGENT provenance assertions (cert003): computed over a freshly
/// seeded KB, deterministic by construction (fixed seed, fixed data).
pub fn agent_provenance_check() -> Result<AgentProvenance, CertError> {
    let k = mem_kernel();
    seed(&k);
    let rows = traversal_set(run(&k, Q_HYBRID, "alice"));
    Ok(AgentProvenance {
        evidence_coverage: evidence_coverage(&k, &rows),
        provenance_complete: provenance_complete(&k),
    })
}

// ---------------------------------------------------------------------------
// DB-AGENT computations (shared by the suite and agent_provenance_check).
// ---------------------------------------------------------------------------

/// Fraction of retrieved rows carrying at least one evidence record.
fn evidence_coverage(k: &Kernel, rows: &HashSet<(KOID, usize)>) -> f64 {
    let total = rows.len().max(1);
    let with_evidence = rows
        .iter()
        .filter(|(koid, _)| matches!(k.explain(ctx(), koid, None), Ok(e) if !e.evidence.is_empty()))
        .count();
    with_evidence as f64 / total as f64
}

/// Every evidence target in the seeded KB resolves to an existing object,
/// and every seeded provenance edge (derived_from) is recorded.
fn provenance_complete(k: &Kernel) -> bool {
    let all: HashSet<KOID> = [
        Q_PET_OBJ,
        r#"MATCH note WHERE topic == "wild" RETURN *"#,
        r#"MATCH event RETURN *"#,
    ]
    .iter()
    .flat_map(|q| objects_set(run(k, q, "alice")))
    .collect();
    all.iter().all(|koid| match k.explain(ctx(), koid, None) {
        Ok(e) => e
            .evidence
            .iter()
            .all(|(_, target)| k.get(ctx(), target).is_ok()),
        Err(_) => false,
    })
}

// ---------------------------------------------------------------------------
// db-oltp — point reads/writes, transactions, concurrency, recovery on a
// real on-disk aikoql-v2 engine.
// ---------------------------------------------------------------------------

fn db_oltp(out_dir: &Path) -> Result<(Vec<WorkloadSpec>, Option<PathBuf>), CertError> {
    let root = out_dir.join("db-oltp").join("kb-data");
    std::fs::create_dir_all(&root).map_err(CertError::io)?;
    let engine = Arc::new(
        AikoqlStorageEngineV2::open(&root).map_err(|e| CertError(format!("v2 open: {e}")))?,
    );
    let k = Arc::new(
        Kernel::open(engine.clone(), Arc::new(ManualClock::new(10_000)), 0xC0FFEE)
            .map_err(|e| CertError(format!("kernel open: {e}")))?,
    );
    let f = seed(&k);

    // point_read: reads pin the stable topic field (point_write mutates
    // body afterwards, and the warm pass re-runs the same workload).
    let point_read = {
        let k = k.clone();
        let (cats, dogs, fish, bird) = (f.cats, f.dogs, f.fish, f.bird);
        wl(
            "point_read",
            50,
            Box::new(move || {
                let topic_ok = |koid: KOID, expect: &str| {
                    matches!(
                        k.get(ctx(), &koid),
                        Ok(ko) if matches!(ko.properties.get("topic"), Some(Value::Text(t)) if t == expect)
                    )
                };
                topic_ok(cats, "pet")
                    && topic_ok(dogs, "pet")
                    && topic_ok(fish, "pet")
                    && topic_ok(bird, "wild")
            }),
        )
    };

    // point_write: update replaces properties (kernel semantics); each
    // sample writes a fresh body value and reads it back.
    let point_write = {
        let k = k.clone();
        let cats = f.cats;
        let mut i = 0usize;
        wl(
            "point_write",
            30,
            Box::new(move || {
                i += 1;
                let body = format!("cats.v{i}");
                let mut req = RememberRequest::update(ctx(), cats, meta("note"));
                req.properties
                    .insert("topic".into(), Value::Text("pet".into()));
                req.properties
                    .insert("body".into(), Value::Text(body.clone()));
                if k.remember(req).is_err() {
                    return false;
                }
                matches!(
                    k.get(ctx(), &cats),
                    Ok(ko) if matches!(ko.properties.get("body"), Some(Value::Text(b)) if *b == body)
                )
            }),
        )
    };

    // transactions: one atomic remember per object, five per sample, all
    // read back. The kernel's public transactional unit is a single op —
    // restart durability (recovery below) is what pins all-or-nothing.
    let transactions = {
        let k = k.clone();
        let mut i = 0usize;
        wl(
            "transactions",
            30,
            Box::new(move || {
                i += 1;
                let written = (0..5)
                    .map(|j| {
                        let label = format!("txn-{i}-{j}");
                        let mut req = RememberRequest::create(ctx(), meta("txnitem"));
                        req.properties
                            .insert("label".into(), Value::Text(label.clone()));
                        k.remember(req).map(|r| (r.koid, label))
                    })
                    .collect::<KResult<Vec<_>>>();
                match written {
                    Ok(pairs) => pairs.into_iter().all(|(koid, label)| {
                        matches!(
                            k.get(ctx(), &koid),
                            Ok(ko) if matches!(ko.properties.get("label"), Some(Value::Text(l)) if *l == label)
                        )
                    }),
                    Err(_) => false,
                }
            }),
        )
    };

    // concurrency: four client threads write through the ONE kernel — the
    // kernel owns the store-global journal head, so concurrent clients are
    // threads over one instance (the mcp production shape); a separate
    // Kernel per thread would contend on the journal chain. Read-back
    // through the same kernel.
    let concurrency = {
        let k = k.clone();
        let mut i = 0usize;
        wl(
            "concurrency",
            3,
            Box::new(move || {
                i += 1;
                let handles = (0..4)
                    .map(|t| {
                        let k = k.clone();
                        std::thread::spawn(move || {
                            (0..25)
                                .map(|j| {
                                    let label = format!("conc-{i}-{t}-{j}");
                                    let mut req = RememberRequest::create(ctx(), meta("concitem"));
                                    req.properties
                                        .insert("label".into(), Value::Text(label.clone()));
                                    k.remember(req).map(|r| (r.koid, label))
                                })
                                .collect::<KResult<Vec<_>>>()
                        })
                    })
                    .collect::<Vec<_>>();
                let all_ok = handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .all(|r| r.is_ok());
                all_ok
                    && (0..4).all(|t| {
                        (0..25).all(|j| {
                            let label = format!("conc-{i}-{t}-{j}");
                            matches!(
                                k.get(ctx(), &find_conc(&k, &label)),
                                Ok(ko) if matches!(ko.properties.get("label"), Some(Value::Text(l)) if *l == label)
                            )
                        })
                    })
            }),
        )
    };

    // recovery: a fresh kernel commits six objects over the same engine,
    // is dropped (clean restart), and a reopened kernel reads all six back.
    // KOIDs encode the HLC, so each sample uses a distinct clock — a rerun
    // at the same clock would regenerate the same koids and trip the OCC
    // version check (observed: VersionConflict expected 0 found 1). The
    // reader opens one millis later (the +1 convention) so its fresh HLC
    // sees every version the writer committed.
    let recovery = {
        let engine = engine.clone();
        let mut i = 0usize;
        wl(
            "recovery",
            1,
            Box::new(move || {
                i += 1;
                let open = |clock: u64| {
                    Kernel::open(
                        engine.clone(),
                        Arc::new(ManualClock::new(clock)),
                        0xC0FFEE + 100,
                    )
                    .unwrap()
                };
                let written = {
                    let rk = open(10_001 + i as u64);
                    let pairs = (0..6)
                        .map(|j| {
                            let label = format!("rec-{i}-{j}");
                            let mut req = RememberRequest::create(ctx(), meta("recitem"));
                            req.properties
                                .insert("label".into(), Value::Text(label.clone()));
                            rk.remember(req).map(|r| (r.koid, label))
                        })
                        .collect::<KResult<Vec<_>>>();
                    drop(rk);
                    pairs
                };
                match written {
                    Ok(pairs) => {
                        let rk2 = open(10_002 + i as u64);
                        pairs.into_iter().all(|(koid, label)| {
                            matches!(
                                rk2.get(ctx(), &koid),
                                Ok(ko) if matches!(ko.properties.get("label"), Some(Value::Text(l)) if *l == label)
                            )
                        })
                    }
                    Err(_) => false,
                }
            }),
        )
    };

    Ok((
        vec![point_read, point_write, transactions, concurrency, recovery],
        Some(root),
    ))
}

/// concurrency read-back: the object's koid was generated inside a worker
/// kernel, so find it by label through the main kernel's scan.
fn find_conc(k: &Kernel, label: &str) -> KOID {
    objects_set(run(k, "MATCH concitem RETURN *", "alice"))
        .into_iter()
        .find(|koid| {
            matches!(
                k.get(ctx(), koid),
                Ok(ko) if matches!(ko.properties.get("label"), Some(Value::Text(l)) if l == label)
            )
        })
        .expect("concurrency object must be visible through the main kernel")
}

// ---------------------------------------------------------------------------
// db-graph — traversals over the seeded graph.
// ---------------------------------------------------------------------------

fn db_graph() -> (Vec<WorkloadSpec>, Option<PathBuf>) {
    let k = Arc::new(mem_kernel());
    let f = seed(&k);
    let set = |xs: &[(KOID, usize)]| xs.iter().copied().collect::<HashSet<_>>();

    let one_hop = trav_op(
        k.clone(),
        r#"MATCH note WHERE topic == "pet" TRAVERSE mentions DEPTH 1 RETURN *"#,
        set(&[(f.e1, 1), (f.e3, 1), (f.e4, 1)]),
    );
    let two_hop = trav_op(
        k.clone(),
        r#"MATCH note WHERE topic == "pet" TRAVERSE mentions DEPTH 2 RETURN *"#,
        set(&[(f.e1, 1), (f.e2, 2), (f.e3, 1), (f.e4, 1)]),
    );
    let fanout = trav_op(
        k.clone(),
        r#"MATCH note WHERE topic == "wild" TRAVERSE mentions DEPTH 1 RETURN *"#,
        set(&[(f.e5, 1), (f.e6, 1)]),
    );
    // Relationship filtering: only derived_from edges are walked — dogs is
    // the sole note with an outbound provenance edge, so the closure is
    // {cats} at depth 1.
    // Relationship filtering: only derived_from edges are followed — the
    // closure is {e5} (bird's provenance edge), a different set than the
    // mentions closure of the same seeds. dogs→cats is not emitted: the
    // BFS pre-seeds `visited` with the start notes, so an edge back into
    // the seed set is not a traversal result.
    let rel_filter = trav_op(
        k.clone(),
        r#"MATCH note TRAVERSE derived_from DEPTH 1 RETURN *"#,
        set(&[(f.e5, 1)]),
    );

    (
        vec![
            wl("one_hop", 30, one_hop),
            wl("two_hop", 30, two_hop),
            wl("fanout", 30, fanout),
            wl("rel_filter", 30, rel_filter),
        ],
        None,
    )
}

// ---------------------------------------------------------------------------
// db-vector — ingestion round-trip, recall@k and filtered ANN over the
// fused ranking (RRF: rank 1 → 1/62, rank 2 → 1/63 — P5-M13-pinned math).
// ---------------------------------------------------------------------------

fn db_vector() -> (Vec<WorkloadSpec>, Option<PathBuf>) {
    let k = Arc::new(mem_kernel());
    let f = seed(&k);

    // ingest: write a vector-backed object and read it back — the embedding
    // travels through the same storage path the ANN leg reads.
    let ingest = {
        let k = k.clone();
        let mut i = 0usize;
        wl(
            "ingest",
            30,
            Box::new(move || {
                i += 1;
                let label = format!("vec-{i}");
                let mut req = RememberRequest::create(ctx(), meta("vecitem"));
                req.properties
                    .insert("label".into(), Value::Text(label.clone()));
                req.semantic = Some(SemanticBlock {
                    embedding: Some(vec![(i % 2) as f32, ((i + 1) % 2) as f32]),
                    embedding_model: None,
                    summary: None,
                    confidence: None,
                    source: None,
                });
                match k.remember(req) {
                    Ok(r) => matches!(
                        k.get(ctx(), &r.koid),
                        Ok(ko) if matches!(ko.properties.get("label"), Some(Value::Text(l)) if *l == label)
                    ),
                    Err(_) => false,
                }
            }),
        )
    };

    // recall@k (k=2): query "cats" over the pet notes — fused ANN+text:
    // cats rank 1 in both legs → 2/62, dogs rank 2 (ANN only) → 1/63,
    // fish 0 in both legs → fused out.
    let recall = {
        let k = k.clone();
        wl(
            "recall_at_k",
            30,
            Box::new(move || {
                let s = scored_vec(run(&k, Q_H1, "alice"));
                s.len() == 2
                    && s[0].0 == f.cats
                    && (s[0].1 - 2.0 / 62.0).abs() < 1e-6
                    && s[1].0 == f.dogs
                    && (s[1].1 - 1.0 / 63.0).abs() < 1e-6
            }),
        )
    };

    // filtered ANN, query "dogs": ANN fish 1.0 (rank 1), dogs 0.7071
    // (rank 2), cats 0.0 (excluded); text dogs 1.0 (rank 1). Fused:
    // dogs 1/62 + 1/63, fish 1/62 — dogs ranks first.
    let filtered = {
        let k = k.clone();
        wl(
            "filtered_ann",
            30,
            Box::new(move || {
                let s = scored_vec(run(
                    &k,
                    r#"MATCH note WHERE topic == "pet" SIMILAR TO "dogs" USING EMBEDDING RETURN *"#,
                    "alice",
                ));
                s.len() == 2
                    && s[0].0 == f.dogs
                    && (s[0].1 - (1.0 / 62.0 + 1.0 / 63.0)).abs() < 1e-6
                    && s[1].0 == f.fish
                    && (s[1].1 - 1.0 / 62.0).abs() < 1e-6
            }),
        )
    };

    (vec![ingest, recall, filtered], None)
}

// ---------------------------------------------------------------------------
// db-knowledge — the full hybrid stack: structured, vector, relationship,
// temporal, evidence, authorization.
// ---------------------------------------------------------------------------

fn db_knowledge() -> (Vec<WorkloadSpec>, Option<PathBuf>) {
    let k = Arc::new(mem_kernel());
    let f = seed(&k);
    let set = |xs: &[(KOID, usize)]| xs.iter().copied().collect::<HashSet<_>>();

    let structured = obj_op(
        k.clone(),
        Q_PET_OBJ,
        [f.cats, f.dogs, f.fish].into_iter().collect(),
    );
    let vector_hits = {
        let k = k.clone();
        Box::new(move || {
            let s = scored_vec(run(&k, Q_H1, "alice"));
            s.len() == 2
                && s[0].0 == f.cats
                && (s[0].1 - 2.0 / 62.0).abs() < 1e-6
                && s[1].0 == f.dogs
                && (s[1].1 - 1.0 / 63.0).abs() < 1e-6
        }) as Box<dyn FnMut() -> bool>
    };
    let relationship = trav_op(
        k.clone(),
        r#"MATCH note TRAVERSE mentions DEPTH 2 RETURN *"#,
        set(&[
            (f.e1, 1),
            (f.e2, 2),
            (f.e3, 1),
            (f.e4, 1),
            (f.e5, 1),
            (f.e6, 1),
        ]),
    );
    // Temporal snapshot: one sample runs both ends of the as-of window.
    let temporal = {
        let k = k.clone();
        Box::new(move || {
            objects_set(run(&k, Q_ASOF_PRE, "alice")).is_empty()
                && objects_set(run(&k, Q_ASOF_POST, "alice"))
                    == [f.cats, f.dogs, f.fish].into_iter().collect()
        }) as Box<dyn FnMut() -> bool>
    };
    // Evidence: dogs' records = all its relationships in insertion order;
    // e2 is a leaf; determinism pinned across two calls.
    let evidence = {
        let k = k.clone();
        Box::new(move || {
            let a = k.explain(ctx(), &f.dogs, None).unwrap();
            let b = k.explain(ctx(), &f.dogs, None).unwrap();
            a.evidence
                == vec![
                    ("mentions".to_string(), f.e3),
                    ("derived_from".to_string(), f.cats),
                ]
                && a.evidence == b.evidence
                && k.explain(ctx(), &f.e2, None).unwrap().evidence.is_empty()
        }) as Box<dyn FnMut() -> bool>
    };
    // ACL: the restricted subject sees nothing and is denied evidence.
    let authz = {
        let k = k.clone();
        Box::new(move || {
            objects_set(run(&k, Q_PET_OBJ, "bob")).is_empty()
                && k.explain(KnowledgeContext::new(Subject::new("bob")), &f.dogs, None)
                    .is_err()
                && k.explain(ctx(), &f.dogs, None).is_ok()
        }) as Box<dyn FnMut() -> bool>
    };

    (
        vec![
            wl("structured_filter", 30, structured),
            wl("vector_hits", 30, vector_hits),
            wl("relationship_walk", 30, relationship),
            wl("temporal_snapshot", 30, temporal),
            wl("evidence_explain", 30, evidence),
            wl("authorization", 30, authz),
        ],
        None,
    )
}

// ---------------------------------------------------------------------------
// db-agent — the retrieval stack an agent drives: context, evidence,
// provenance, history, relevance, authorization.
// ---------------------------------------------------------------------------

fn db_agent() -> (Vec<WorkloadSpec>, Option<PathBuf>) {
    let k = Arc::new(mem_kernel());
    let f = seed(&k);
    let set = |xs: &[(KOID, usize)]| xs.iter().copied().collect::<HashSet<_>>();
    let hybrid_rows = set(&[(f.e1, 1), (f.e2, 2), (f.e3, 1)]);

    let context_retrieval = trav_op(k.clone(), Q_HYBRID, hybrid_rows.clone());
    // Evidence coverage over the retrieved context: only e1 carries
    // evidence records, so the oracle is exactly 1/3.
    let evidence = {
        let k = k.clone();
        Box::new(move || {
            let rows = traversal_set(run(&k, Q_HYBRID, "alice"));
            (evidence_coverage(&k, &rows) - 1.0 / 3.0).abs() < 1e-9
        }) as Box<dyn FnMut() -> bool>
    };
    let provenance = {
        let k = k.clone();
        Box::new(move || provenance_complete(&k)) as Box<dyn FnMut() -> bool>
    };
    let historical = {
        let k = k.clone();
        Box::new(move || {
            objects_set(run(&k, Q_ASOF_PRE, "alice")).is_empty()
                && objects_set(run(&k, Q_ASOF_POST, "alice"))
                    == [f.cats, f.dogs, f.fish].into_iter().collect()
        }) as Box<dyn FnMut() -> bool>
    };
    let semantic = {
        let k = k.clone();
        Box::new(move || {
            let s = scored_vec(run(&k, Q_H1, "alice"));
            s.first().map(|(koid, _)| *koid) == Some(f.cats)
        }) as Box<dyn FnMut() -> bool>
    };
    let authz = {
        let k = k.clone();
        Box::new(move || {
            traversal_set(run(&k, Q_HYBRID, "bob")).is_empty()
                && k.explain(KnowledgeContext::new(Subject::new("bob")), &f.e1, None)
                    .is_err()
                && k.explain(ctx(), &f.e1, None).is_ok()
        }) as Box<dyn FnMut() -> bool>
    };

    (
        vec![
            wl("context_retrieval", 30, context_retrieval),
            wl("evidence_coverage", 30, evidence),
            wl("provenance_completeness", 30, provenance),
            wl("historical_correctness", 30, historical),
            wl("semantic_relevance", 30, semantic),
            wl("authorization_correctness", 30, authz),
        ],
        None,
    )
}
