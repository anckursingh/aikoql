//! P5-M14 (ND-14) — AIKOQL Database certification REDs.
//!
//! cert001: every DB-* suite runs and writes a machine-readable artifact —
//! the acceptance list (deterministic datasets, reproducible seeds,
//! machine-readable results, cold/warm, p50/p95/p99, throughput, RSS, disk,
//! correctness parity) is the artifact SCHEMA, pinned field-by-field, plus
//! determinism (same seed → same workload names / n / correctness).
//! cert002: detection power — an injected regression (CERT_INJECT=1) must
//! fail the run, never silently write a green artifact.
//! cert003: DB-AGENT provenance assertions — evidence coverage and
//! provenance completeness over the seeded knowledge base.

use std::path::{Path, PathBuf};

use aikoql_certification::{agent_provenance_check, run_suite, SUITES};

fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aikoql_cert_{}_{}_{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The five roadmap suite families (ND-14), in order.
#[test]
fn cert000_suite_names() {
    assert_eq!(
        SUITES,
        ["db-oltp", "db-graph", "db-vector", "db-knowledge", "db-agent"]
    );
}

#[test]
fn cert001_every_suite_writes_a_machine_readable_artifact() {
    for suite in SUITES {
        let out = out_dir("cert001");
        let path = run_suite(suite, &out).expect("suite must run from a clean checkout");
        assert_eq!(
            path,
            out.join(suite).join("result.json"),
            "artifact path contract: <out>/<suite>/result.json"
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();

        // Reproducibility fields.
        assert_eq!(v["suite"], suite);
        assert!(v["seed"].is_string(), "reproducible seed: {v}");
        assert!(v["commit"].is_string(), "run commit hash: {v}");
        assert!(v["started_at"].is_string(), "run date: {v}");

        // Machine-readable measurements: every workload cell carries the
        // ND-14 acceptance dimensions.
        let workloads = v["workloads"].as_array().expect("workloads array");
        assert!(!workloads.is_empty(), "{suite} has no workloads");
        for w in workloads {
            assert!(w["name"].is_string(), "workload name: {w}");
            assert!(w["n"].as_u64().unwrap_or(0) > 0, "sample count: {w}");
            for key in ["p50_ms", "p95_ms", "p99_ms", "throughput_ops_s"] {
                assert!(w[key].as_f64().is_some(), "{suite} {key} must be numeric: {w}");
            }
            assert!(w["rss_kb"].as_u64().is_some(), "{suite} RSS: {w}");
            assert!(w["disk_bytes"].as_u64().is_some(), "{suite} disk: {w}");
            // Cold/warm: every workload reports both regimes.
            for regime in ["cold", "warm"] {
                assert!(w[regime].is_object(), "{suite} {regime} cell: {w}");
            }
            assert!(w["correct"].is_boolean(), "{suite} correctness parity: {w}");
        }

        // Correctness parity: the suite's own oracle must agree with the run.
        assert_eq!(
            v["correctness_parity"], true,
            "{suite} correctness parity must hold"
        );
    }
}

#[test]
fn cert001b_same_seed_is_reproducible() {
    let out_a = out_dir("cert001b_a");
    let out_b = out_dir("cert001b_b");
    let a = std::fs::read_to_string(run_suite("db-oltp", &out_a).unwrap()).unwrap();
    let b = std::fs::read_to_string(run_suite("db-oltp", &out_b).unwrap()).unwrap();
    let (va, vb): (serde_json::Value, serde_json::Value) =
        (serde_json::from_str(&a).unwrap(), serde_json::from_str(&b).unwrap());
    // Timings are noise; the deterministic surface is the seed, the workload
    // list and the correctness oracle.
    assert_eq!(va["seed"], vb["seed"]);
    let shape = |v: &serde_json::Value| -> Vec<(String, u64, bool)> {
        v["workloads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                (
                    w["name"].as_str().unwrap().to_string(),
                    w["n"].as_u64().unwrap(),
                    w["correct"].as_bool().unwrap(),
                )
            })
            .collect()
    };
    assert_eq!(shape(&va), shape(&vb), "same seed, same workload shape");
}

#[test]
fn cert002_injected_regression_fails_the_run() {
    let out = out_dir("cert002");
    // Detection power: the certification data path must not write a green
    // artifact over an injected regression.
    std::env::set_var("CERT_INJECT", "1");
    let result = run_suite("db-oltp", &out);
    std::env::remove_var("CERT_INJECT");
    assert!(result.is_err(), "injected regression must fail the suite");
}

#[test]
fn cert003_db_agent_provenance_assertions() {
    // Evidence coverage and provenance completeness over the seeded KB —
    // the DB-AGENT acceptance dimensions, computed, not hard-coded.
    let a = agent_provenance_check().expect("agent provenance check must run");
    assert!(
        (0.0..=1.0).contains(&a.evidence_coverage),
        "coverage is a fraction: {a:?}"
    );
    assert!(a.provenance_complete, "seeded KB provenance must close");
    let b = agent_provenance_check().expect("check must be deterministic");
    assert_eq!(
        a.evidence_coverage, b.evidence_coverage,
        "coverage must be deterministic"
    );
}

/// Sanity guard for the GREEN author: the runner API is exercised only
/// through the pinned surface above — nothing else in this crate may
/// silently grow an unpinned knob.
#[allow(dead_code)]
fn _pin_surface(_p: &Path) {}
