//! TP-1 — Certification traceability registry (docs/TESTING-PLAN.md).
//!
//! The machine-readable matrix for the two QA certification suites:
//! - QA-AIKOQL-AGENT-MEMORY-001 — every P0/P1 test ID (the suites' release
//!   rule: P0 = 100% pass, P1 ≥ 98%). MEM/AGENT/IDX groups carry no priority
//!   in the suite and stay in TESTING-PLAN.md prose.
//! - QA-AIKOQL-CHATBOT-001 — the §53 release claims (priority `R`: release
//!   tier only, never the PR gate).
//!
//! Two gates:
//! - `certification_matrix_integrity` — always runs (PR gate). Fails on an
//!   unregistered/duplicate/unknown ID, an invalid status, a covered row
//!   whose test path does not exist, or a partial/gap row without a note.
//!   Must be green at all times.
//! - `certification_p0_closure` — `#[ignore]` (release tier; runs in the
//!   weekly `cargo test --workspace -- --ignored` sweep). Fails while any
//!   P0 row is not covered — the suites' hard rule: *a test for an
//!   architectural target is not evidence the feature exists*, so known
//!   gaps keep the certification red until the capability is delivered.

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Pri {
    P0,
    P1,
    R,
}

impl Pri {
    const fn parse(s: &str) -> Self {
        match s.as_bytes() {
            [b'P', b'0'] => Pri::P0,
            [b'P', b'1'] => Pri::P1,
            [b'R'] => Pri::R,
            _ => panic!("unknown priority — compile-time registry validation"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Status {
    Covered,
    Partial,
    Gap,
}

impl Status {
    const fn parse(s: &str) -> Self {
        match s.as_bytes() {
            b"covered" => Status::Covered,
            b"partial" => Status::Partial,
            b"gap" => Status::Gap,
            _ => panic!("unknown status — compile-time registry validation"),
        }
    }
}

/// One certification row: suite test ID → our test location + verdict.
struct Row {
    id: &'static str,
    group: &'static str,
    pri: Pri,
    status: Status,
    /// Repo-root-relative path of the test exercising the ID (covered rows).
    test: Option<&'static str>,
    /// Required for partial/gap: what is missing.
    note: Option<&'static str>,
}

const fn row(
    id: &'static str,
    group: &'static str,
    pri: &'static str,
    status: &'static str,
    test: Option<&'static str>,
    note: Option<&'static str>,
) -> Row {
    Row {
        id,
        group,
        pri: Pri::parse(pri),
        status: Status::parse(status),
        test,
        note,
    }
}

#[rustfmt::skip]
const MATRIX: &[Row] = &[
    // ── Agent suite P0/P1 ────────────────────────────────────────────────
    // KB — repository knowledge compiler
    row("KB-001", "KB", "P0", "covered", Some("crates/ingestion/tests/e2e_pipeline.rs"), None),
    row("KB-002", "KB", "P0", "covered", Some("crates/ingestion/tests/e2e_pipeline.rs"), None),
    row("KB-003", "KB", "P0", "covered", Some("crates/ingestion/tests/e2e_pipeline.rs"), None),
    row("KB-004", "KB", "P0", "covered", Some("crates/ingestion/tests/multi_source_ontology.rs"), None),
    row("KB-005", "KB", "P0", "covered", Some("crates/ingestion/tests/e2e_pipeline.rs"), None),
    row("KB-006", "KB", "P1", "covered", Some("crates/ingestion/tests/e2e_pipeline.rs"), None),
    row("KB-007", "KB", "P0", "covered", Some("crates/ingestion/tests/e2e_pipeline.rs"), None),
    row("KB-008", "KB", "P1", "covered", Some("crates/ingestion/src/ingest_dir.rs"), None),
    row("KB-009", "KB", "P1", "covered", Some("crates/ingestion/src/ingest_incremental.rs"), None),
    // INC — incremental knowledge
    row("INC-001", "INC", "P0", "covered", Some("crates/ingestion/src/ingest_incremental.rs"), None),
    row("INC-002", "INC", "P0", "covered", Some("crates/ingestion/src/ingest_incremental.rs"), None),
    row("INC-003", "INC", "P1", "covered", Some("crates/ingestion/src/ingest_incremental.rs"), None),
    row("INC-004", "INC", "P0", "covered", Some("crates/ingestion/src/ingest_incremental.rs"), None),
    row("INC-005", "INC", "P1", "covered", Some("crates/ingestion/src/ingest_incremental.rs"), None),
    // KO — knowledge object model
    row("KO-001", "KO", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("KO-002", "KO", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("KO-003", "KO", "P0", "covered", Some("crates/kernel/tests/proptest_kom.rs"), None),
    row("KO-004", "KO", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("KO-005", "KO", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("KO-006", "KO", "P1", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    // MM — multi-model knowledge
    row("MM-001", "MM", "P1", "partial", None, Some("fixtures exist; automated connector matrix absent (TP-5)")),
    row("MM-002", "MM", "P1", "partial", None, Some("fixtures exist; automated connector matrix absent (TP-5)")),
    row("MM-003", "MM", "P1", "partial", None, Some("fixtures exist; automated connector matrix absent (TP-5)")),
    row("MM-004", "MM", "P1", "partial", None, Some("fixtures exist; automated connector matrix absent (TP-5)")),
    row("MM-005", "MM", "P0", "covered", Some("crates/ingestion/tests/multi_source_ontology.rs"), None),
    // ONT — ontology
    row("ONT-001", "ONT", "P0", "covered", Some("crates/kernel/tests/ontology_integration.rs"), None),
    row("ONT-002", "ONT", "P1", "covered", Some("crates/kernel/src/lifecycle/constraint.rs"), None),
    row("ONT-003", "ONT", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("ONT-004", "ONT", "P1", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    // PROV — provenance and evidence
    row("PROV-001", "PROV", "P0", "covered", Some("crates/kernel/tests/evidence_wiring.rs"), None),
    row("PROV-002", "PROV", "P0", "covered", Some("crates/kernel/tests/durability.rs"), None),
    row("PROV-003", "PROV", "P0", "covered", Some("crates/kernel/tests/epistemic.rs"), None),
    row("PROV-004", "PROV", "P1", "covered", Some("crates/kernel/tests/transactions.rs"), None),
    // TEMP — temporal knowledge
    row("TEMP-001", "TEMP", "P0", "covered", Some("crates/kernel/tests/temporal.rs"), None),
    row("TEMP-002", "TEMP", "P0", "covered", Some("crates/kernel/tests/temporal.rs"), None),
    row("TEMP-003", "TEMP", "P1", "covered", Some("crates/kernel/tests/temporal.rs"), None),
    // CON — contradictions
    row("CON-001", "CON", "P0", "covered", Some("crates/kernel/tests/epistemic.rs"), None),
    row("CON-002", "CON", "P1", "covered", Some("crates/kernel/tests/evals.rs"), None),
    row("CON-003", "CON", "P1", "covered", Some("crates/kernel/tests/transactions.rs"), None),
    // CST — constraints
    row("CST-001", "CST", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("CST-002", "CST", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("CST-003", "CST", "P0", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("CST-004", "CST", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    // QL — parser
    row("QL-001", "QL", "P0", "covered", Some("crates/compiler/tests/golden_snapshots.rs"), None),
    row("QL-002", "QL", "P0", "covered", Some("crates/compiler/tests/golden_snapshots.rs"), None),
    row("QL-003", "QL", "P0", "covered", Some("crates/compiler/tests/golden_snapshots.rs"), None),
    row("QL-004", "QL", "P0", "covered", Some("crates/compiler/tests/golden_snapshots.rs"), None),
    row("QL-005", "QL", "P1", "covered", Some("crates/compiler/tests/golden_snapshots.rs"), None),
    row("QL-006", "QL", "P1", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("QL-007", "QL", "P1", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("QL-008", "QL", "P0", "covered", Some("crates/compiler/tests/grammar_coverage.rs"), None),
    row("QL-009", "QL", "P0", "covered", Some("crates/compiler/tests/fuzz_parser.rs"), None),
    // EXE — query execution
    row("EXE-001", "EXE", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("EXE-002", "EXE", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("EXE-003", "EXE", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("EXE-004", "EXE", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("EXE-005", "EXE", "P1", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("EXE-006", "EXE", "P1", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    // RET — retrieval
    row("RET-001", "RET", "P1", "covered", Some("crates/ingestion/tests/retrieval_quality.rs"), None),
    row("RET-002", "RET", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("RET-003", "RET", "P1", "covered", Some("crates/ingestion/tests/retrieval_quality.rs"), None),
    row("RET-004", "RET", "P1", "partial", None, Some("rank fusion only; no learned reranker (HLD §60: mock stays)")),
    row("RET-005", "RET", "P0", "covered", Some("crates/ingestion/tests/retrieval_quality.rs"), None),
    // DOC — documents/OCR
    row("DOC-001", "DOC", "P1", "covered", Some("crates/ingestion/tests/multimodal_golden.rs"), None),
    row("DOC-002", "DOC", "P1", "partial", None, Some("OCR feature-gated (vlm); never in default build")),
    row("DOC-003", "DOC", "P1", "covered", Some("crates/ingestion/tests/multimodal_golden.rs"), None),
    row("DOC-004", "DOC", "P1", "covered", Some("crates/ingestion/tests/multimodal_golden.rs"), None),
    row("DOC-005", "DOC", "P1", "covered", Some("crates/ingestion/tests/multimodal_golden.rs"), None),
    row("DOC-006", "DOC", "P1", "covered", Some("crates/ingestion/tests/multimodal_golden.rs"), None),
    row("DOC-007", "DOC", "P1", "covered", Some("crates/ingestion/src/ingest_incremental.rs"), None),
    // SEC — security and encryption
    row("SEC-001", "SEC", "P0", "covered", Some("crates/kernel/tests/encryption.rs"), None),
    row("SEC-002", "SEC", "P0", "covered", Some("crates/kernel/tests/encryption.rs"), None),
    row("SEC-003", "SEC", "P0", "covered", Some("crates/kernel/tests/encryption.rs"), None),
    row("SEC-004", "SEC", "P0", "covered", Some("crates/kernel/tests/encryption.rs"), None),
    row("SEC-005", "SEC", "P0", "covered", Some("crates/kernel/tests/encryption.rs"), None),
    row("SEC-006", "SEC", "P0", "covered", Some("crates/kernel/tests/encryption.rs"), None),
    row("SEC-007", "SEC", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    // DB — persistence and recovery
    row("DB-001", "DB", "P0", "covered", Some("crates/kernel/tests/durability.rs"), None),
    row("DB-002", "DB", "P0", "covered", Some("crates/kernel/tests/crash_kill.rs"), None),
    row("DB-003", "DB", "P0", "covered", Some("crates/kernel/tests/transactions.rs"), None),
    row("DB-004", "DB", "P1", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    // PRG — programs-as-KO
    row("PRG-001", "PRG", "P0", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("PRG-002", "PRG", "P0", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("PRG-003", "PRG", "P0", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("PRG-004", "PRG", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("PRG-005", "PRG", "P0", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("PRG-006", "PRG", "P0", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("PRG-007", "PRG", "P1", "covered", Some("crates/services/api/mcp/src/tests.rs"), None),
    row("PRG-008", "PRG", "P1", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    // EVO — knowledge evolution
    row("EVO-001", "EVO", "P1", "covered", Some("crates/kernel/tests/derivation.rs"), None),
    row("EVO-002", "EVO", "P0", "covered", Some("crates/ingestion/src/ingest_incremental.rs"), None),
    row("EVO-003", "EVO", "P0", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("EVO-004", "EVO", "P1", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("EVO-005", "EVO", "P1", "covered", Some("crates/kernel/tests/evidence_wiring.rs"), None),
    // IDX — derived index consistency (suite §23)
    row("IDX-001", "IDX", "P0", "covered", Some("crates/kernel/tests/indexes.rs"), None),
    row("IDX-002", "IDX", "P0", "covered", Some("crates/kernel/tests/indexes.rs"), None),
    row("IDX-003", "IDX", "P0", "covered", Some("crates/kernel/tests/indexes.rs"), None),

    // ── Chatbot suite §53 release claims (tier R — release only) ──────────
    // Chatbot Memory Ready
    row("CMEM-001", "CMEM", "R", "covered", Some("crates/services/api/mcp/tests/mcp_real_world.rs"), None),
    row("CMEM-002", "CMEM", "R", "covered", Some("scripts/e2e-restart.js"), None),
    row("CMEM-003", "CMEM", "R", "covered", Some("crates/services/api/mcp/tests/mcp_real_world.rs"), None),
    row("CMEM-004", "CMEM", "R", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("CMEM-005", "CMEM", "R", "covered", Some("crates/kernel/tests/epistemic.rs"), None),
    row("CMEM-006", "CMEM", "R", "covered", Some("crates/services/api/mcp/tests/mcp_real_world.rs"), None),
    row("CMEM-007", "CMEM", "R", "covered", Some("crates/services/api/mcp/tests/mcp_real_world.rs"), None),
    row("CMEM-008", "CMEM", "R", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("CMEM-009", "CMEM", "R", "covered", Some("crates/kernel/tests/evidence_wiring.rs"), None),
    // Knowledge-Grounded Chatbot Ready
    row("CKG-001", "CKG", "R", "covered", Some("crates/ingestion/tests/retrieval_quality.rs"), None),
    row("CKG-002", "CKG", "R", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("CKG-003", "CKG", "R", "covered", Some("crates/kernel/tests/temporal.rs"), None),
    row("CKG-004", "CKG", "R", "covered", Some("crates/kernel/tests/evals.rs"), None),
    row("CKG-005", "CKG", "R", "covered", Some("crates/kernel/tests/transactions.rs"), None),
    row("CKG-006", "CKG", "R", "covered", Some("crates/kernel/tests/epistemic.rs"), None),
    row("CKG-007", "CKG", "R", "covered", Some("crates/ingestion/tests/e2e_answer_quality.rs"), None),
    row("CKG-008", "CKG", "R", "covered", Some("scripts/e2e-k3-lineage.js"), None),
    // Agentic Chatbot Ready
    row("CAG-001", "CAG", "R", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("CAG-002", "CAG", "R", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("CAG-003", "CAG", "R", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("CAG-004", "CAG", "R", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("CAG-005", "CAG", "R", "covered", Some("crates/kernel/tests/conformance.rs"), None),
    row("CAG-006", "CAG", "R", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("CAG-007", "CAG", "R", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("CAG-008", "CAG", "R", "covered", Some("crates/kernel/tests/experiences.rs"), None),
    row("CAG-009", "CAG", "R", "covered", Some("crates/kernel/tests/derivation.rs"), None),
    // §51 critical e2e scenario
    row("C51-001", "C51", "R", "covered", Some("crates/services/api/mcp/tests/mcp_real_world.rs"), None),
];

/// The complete gate catalog from the suites, per group. Integrity fails on
/// any matrix ID not listed here (unregistered) or listed but missing from
/// the matrix (dropped).
#[rustfmt::skip]
const CATALOG: &[(&str, &[&str])] = &[
    ("KB",  &["KB-001","KB-002","KB-003","KB-004","KB-005","KB-006","KB-007","KB-008","KB-009"]),
    ("INC", &["INC-001","INC-002","INC-003","INC-004","INC-005"]),
    ("KO",  &["KO-001","KO-002","KO-003","KO-004","KO-005","KO-006"]),
    ("MM",  &["MM-001","MM-002","MM-003","MM-004","MM-005"]),
    ("ONT", &["ONT-001","ONT-002","ONT-003","ONT-004"]),
    ("PROV",&["PROV-001","PROV-002","PROV-003","PROV-004"]),
    ("TEMP",&["TEMP-001","TEMP-002","TEMP-003"]),
    ("CON", &["CON-001","CON-002","CON-003"]),
    ("CST", &["CST-001","CST-002","CST-003","CST-004"]),
    ("QL",  &["QL-001","QL-002","QL-003","QL-004","QL-005","QL-006","QL-007","QL-008","QL-009"]),
    ("EXE", &["EXE-001","EXE-002","EXE-003","EXE-004","EXE-005","EXE-006"]),
    ("RET", &["RET-001","RET-002","RET-003","RET-004","RET-005"]),
    ("DOC", &["DOC-001","DOC-002","DOC-003","DOC-004","DOC-005","DOC-006","DOC-007"]),
    ("SEC", &["SEC-001","SEC-002","SEC-003","SEC-004","SEC-005","SEC-006","SEC-007"]),
    ("DB",  &["DB-001","DB-002","DB-003","DB-004"]),
    ("PRG", &["PRG-001","PRG-002","PRG-003","PRG-004","PRG-005","PRG-006","PRG-007","PRG-008"]),
    ("EVO", &["EVO-001","EVO-002","EVO-003","EVO-004","EVO-005"]),
    ("IDX", &["IDX-001","IDX-002","IDX-003"]),
    ("CMEM",&["CMEM-001","CMEM-002","CMEM-003","CMEM-004","CMEM-005","CMEM-006","CMEM-007","CMEM-008","CMEM-009"]),
    ("CKG", &["CKG-001","CKG-002","CKG-003","CKG-004","CKG-005","CKG-006","CKG-007","CKG-008"]),
    ("CAG", &["CAG-001","CAG-002","CAG-003","CAG-004","CAG-005","CAG-006","CAG-007","CAG-008","CAG-009"]),
    ("C51", &["C51-001"]),
];

fn repo_root() -> std::path::PathBuf {
    // crates/kernel → repo root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root")
}

/// PR gate — the registry must be complete and well-formed at all times.
#[test]
fn certification_matrix_integrity() {
    let root = repo_root();

    // 1. No duplicate IDs.
    let mut ids = MATRIX.iter().map(|r| r.id).collect::<Vec<_>>();
    ids.sort_unstable();
    let mut deduped = ids.clone();
    deduped.dedup();
    assert_eq!(ids.len(), deduped.len(), "duplicate IDs in MATRIX");

    // 2. Catalog ↔ matrix agreement, per group (unregistered / dropped IDs).
    let mut problems = Vec::new();
    for (group, expected) in CATALOG {
        let mut got = MATRIX
            .iter()
            .filter(|r| r.group == *group)
            .map(|r| r.id)
            .collect::<Vec<_>>();
        let mut want = expected.to_vec();
        got.sort_unstable();
        want.sort_unstable();
        if got != want {
            problems.push(format!("group {group}: catalog {want:?} != matrix {got:?}"));
        }
    }
    let mut group_set = MATRIX.iter().map(|r| r.group).collect::<Vec<_>>();
    group_set.sort_unstable();
    group_set.dedup();
    for g in group_set {
        if !CATALOG.iter().any(|(name, _)| name == &g) {
            problems.push(format!("matrix group {g} has no catalog entry"));
        }
    }
    assert!(
        problems.is_empty(),
        "registry mismatch:\n{}",
        problems.join("\n")
    );

    // 3. Per-row shape: covered → existing test path; partial/gap → note.
    let mut problems = Vec::new();
    for r in MATRIX {
        match r.status {
            Status::Covered => {
                let Some(test) = r.test else {
                    problems.push(format!("{}: covered without test path", r.id));
                    continue;
                };
                if !root.join(test).exists() {
                    problems.push(format!("{}: test path missing: {test}", r.id));
                }
            }
            Status::Partial | Status::Gap => {
                if r.note.is_none() {
                    problems.push(format!("{}: {:?} without note", r.id, r.status));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "row problems:\n{}",
        problems.join("\n")
    );

    let total = MATRIX.len();
    let covered = MATRIX
        .iter()
        .filter(|r| r.status == Status::Covered)
        .count();
    eprintln!(
        "[CERT-MATRIX] {total} gate IDs registered: {covered} covered, \
         {} partial/gap",
        total - covered
    );
}

/// Release tier — P0 closure: every P0 ID must be covered. `#[ignore]`d so
/// known P0 gaps keep the certification red without breaking the PR gate;
/// runs in the weekly `cargo test --workspace -- --ignored` sweep.
#[test]
#[ignore = "release-tier P0 certification gate (weekly ignored sweep)"]
fn certification_p0_closure() {
    let open_p0: Vec<&Row> = MATRIX
        .iter()
        .filter(|r| r.pri == Pri::P0 && r.status != Status::Covered)
        .collect();
    let mut verdict = String::new();
    for r in &open_p0 {
        verdict.push_str(&format!(
            "P0 OPEN {} ({:?}): {}\n",
            r.id,
            r.status,
            r.note.unwrap_or("no note")
        ));
    }
    let p0_total = MATRIX.iter().filter(|r| r.pri == Pri::P0).count();
    let p1_partial = MATRIX
        .iter()
        .filter(|r| r.pri == Pri::P1 && r.status != Status::Covered)
        .count();
    eprintln!(
        "[CERT-P0] {} P0 open of {p0_total}; {} P1 not covered (informational)",
        open_p0.len(),
        p1_partial
    );
    assert!(
        open_p0.is_empty(),
        "P0 certification not closed (suites' release rule: P0 = 100%):\n{verdict}"
    );
}
