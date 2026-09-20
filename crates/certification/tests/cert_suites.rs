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
use std::sync::Mutex;

use aikoql_certification::{agent_provenance_check, run_suite, with_inject, SUITES};

/// cert002's CERT_INJECT is a process-wide env var — parallel tests would
/// observe the injection mid-run. Serialize the tests that run db-oltp /
/// touch the injection knob.
static INJECT_LOCK: Mutex<()> = Mutex::new(());

// Temp suite dirs written by THIS test thread, swept when the thread exits
// (the main thread's destructor runs at process exit — statics are NOT
// dropped on Windows MSVC, TLS is).
thread_local! {
    static TEMP_PATHS: std::cell::RefCell<TempSweeper> =
        const { std::cell::RefCell::new(TempSweeper { paths: Vec::new() }) };
}

struct TempSweeper {
    paths: Vec<PathBuf>,
}
impl Drop for TempSweeper {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

fn out_dir(name: &str) -> PathBuf {
    // Killed runs never reach the TLS sweep — purge their corpses at the
    // next startup (only entries older than a day, so a concurrent live
    // run's fresh dirs are untouched).
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("aikoql_cert_") {
                continue;
            }
            let stale = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .is_some_and(|t| t < cutoff);
            if stale {
                let _ = std::fs::remove_file(e.path());
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    });
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
    TEMP_PATHS.with(|t| t.borrow_mut().paths.push(dir.clone()));
    dir
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    INJECT_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The five roadmap suite families (ND-14), in order.
#[test]
fn cert000_suite_names() {
    assert_eq!(
        SUITES,
        [
            "db-oltp",
            "db-graph",
            "db-vector",
            "db-knowledge",
            "db-agent"
        ]
    );
}

#[test]
fn cert001_every_suite_writes_a_machine_readable_artifact() {
    let _guard = lock();
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
                assert!(
                    w[key].as_f64().is_some(),
                    "{suite} {key} must be numeric: {w}"
                );
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
    let _guard = lock();
    let out_a = out_dir("cert001b_a");
    let out_b = out_dir("cert001b_b");
    let a = std::fs::read_to_string(run_suite("db-oltp", &out_a).unwrap()).unwrap();
    // A re-run into the SAME dir must succeed — the runner clears the suite
    // dir (the deterministic seed regenerates identical KOIDs, which OCC
    // rejects against a stale store).
    run_suite("db-oltp", &out_a).expect("re-run over a fixed path must run clean");
    let b = std::fs::read_to_string(run_suite("db-oltp", &out_b).unwrap()).unwrap();
    let (va, vb): (serde_json::Value, serde_json::Value) = (
        serde_json::from_str(&a).unwrap(),
        serde_json::from_str(&b).unwrap(),
    );
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
    let _guard = lock();
    let out = out_dir("cert002");
    // Detection power: the certification data path must not write a green
    // artifact over an injected regression. with_inject scopes the hook to
    // this thread — a process-global flag leaked into sibling tests running
    // in parallel threads (cert003's determinism assertion flaked exactly
    // that way).
    let result = with_inject(|| run_suite("db-oltp", &out));
    assert!(result.is_err(), "injected regression must fail the suite");
}

#[test]
fn cert002b_injection_is_thread_scoped() {
    // cert002's hook must be scoped to the arming thread. A process-global
    // flag (env var) leaks into sibling tests running in parallel threads —
    // cert003's determinism assertion flaked exactly this way (coverage
    // 0.333 mid-window vs 0.0 after the window closed). The baseline below
    // is the clean check; the armed thread holds the flag open while this
    // thread runs the same check — it must see the same clean value.
    let clean = agent_provenance_check()
        .expect("clean check must run")
        .evidence_coverage;
    let (tx_open, rx_open) = std::sync::mpsc::channel::<()>();
    let (tx_done, rx_done) = std::sync::mpsc::channel::<()>();
    let t = std::thread::spawn(move || {
        with_inject(|| {
            let _ = tx_open.send(());
            std::thread::sleep(std::time::Duration::from_secs(2));
        });
        drop(tx_done); // the receiver's Err is the window-closed signal
    });
    rx_open.recv().unwrap(); // the window is open
    let during = agent_provenance_check()
        .expect("check during the window must run")
        .evidence_coverage;
    let _ = rx_done.recv(); // Err = thread finished; join below reaps it
    t.join().unwrap();
    assert_eq!(
        during, clean,
        "a sibling thread's injection must not leak into this check"
    );
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
