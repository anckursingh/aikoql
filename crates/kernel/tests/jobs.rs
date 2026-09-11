//! P3-M7 — Class-B async jobs (MRFC-0011 §6.10–6.13, §7 Determinism Law,
//! §8 JOB_REJECTED, §10.2 audit-KE-per-admission). The M7 REDs cb001–005.
//!
//! cb004 uses the crash_kill child-kill pattern: a `job_crasher` child
//! submits a parked job and idles; the parent hard-kills it mid-job; the
//! reopened store must show the job Failed — never silently dropped.

use aikoql_kernel::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn mk() -> (Kernel, Arc<MemoryEngine>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xBEEF).unwrap();
    (k, store, clock)
}

fn sensor_props(zone: &str) -> PropertyMap {
    [("zone".to_string(), Value::Text(zone.into()))]
        .into_iter()
        .collect()
}

fn seed(k: &Kernel, type_name: &str, props: PropertyMap) -> KOID {
    k.remember(RememberRequest {
        metadata: Metadata {
            type_name: type_name.into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
        properties: props,
        origin: Origin::Human,
        ..RememberRequest::create(
            Subject::with_roles("seeder", &["admin"]),
            Metadata {
                type_name: type_name.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
        )
    })
    .unwrap()
    .koid
}

/// cb001: submit returns a handle, status progresses to Completed, and the
/// claims are retrievable — while staying OUT of the Class-A store.
#[test]
fn cb001_reason_returns_handle_status_progresses_result_retrievable() {
    let (k, _store, _clock) = mk();
    seed(&k, "sensor", sensor_props("a"));
    let job = k.reason("sensor", sensor_props("a")).unwrap();
    // Admission is durable and idempotency-hash-bearing from the start.
    let status = wait_status(
        &k,
        job.job_id,
        JobStatus::Completed,
        Duration::from_secs(10),
    );
    assert_eq!(
        status,
        JobStatus::Completed,
        "status must progress to Completed"
    );
    let claims = k.job_result(job.job_id).unwrap();
    assert_eq!(claims.len(), 1, "the matching sensor yields one claim");
    // The claim is Class B — invisible to Class-A reads until approved.
    let s = Subject::with_roles("tester", &["admin"]);
    assert!(
        k.scan_by_type(&s, "sensor-claim").unwrap().is_empty(),
        "Class-B output must not appear in the Class-A store pre-approval"
    );
}

/// cb002: over the admission limit, submit is JOB_REJECTED (MRFC-0011 §8).
#[test]
fn cb002_over_admission_limit_is_job_rejected() {
    let (k, _store, _clock) = mk();
    seed(&k, "sensor", sensor_props("a"));
    k.set_max_running_jobs(1);
    k.set_job_park_ms(500);
    let j1 = k.reason("sensor", sensor_props("a")).unwrap();
    // A DIFFERENT input — a same-input resubmit would legitimately dedup
    // (cb005) and never reach admission.
    assert!(
        matches!(
            k.reason("sensor", sensor_props("b")),
            Err(KError::JobRejected(_))
        ),
        "the second concurrent job must be rejected at admission"
    );
    // The admitted job still runs to completion; admission frees up after.
    let st = wait_status(&k, j1.job_id, JobStatus::Completed, Duration::from_secs(10));
    assert_eq!(st, JobStatus::Completed);
    assert!(
        k.reason("sensor", sensor_props("c")).is_ok(),
        "admission must free up once the running job completes"
    );
}

/// cb003: approval commits the Class-B claim with the correct epistemic
/// transition — origin=Reason, epistemic=Inferred — and only via approve.
#[test]
fn cb003_approval_commits_class_b_claim_with_epistemic_transition() {
    let (k, _store, _clock) = mk();
    seed(&k, "sensor", sensor_props("a"));
    let job = k.reason("sensor", sensor_props("a")).unwrap();
    // §10.2: the admission itself is an audit KE — the journal advanced by
    // exactly the admission event (seed create = seq 1, admission = seq 2).
    let (seq, _) = k.journal_head().unwrap();
    assert_eq!(seq, 2, "job admission must emit an audit KE in the journal");

    let st = wait_status(
        &k,
        job.job_id,
        JobStatus::Completed,
        Duration::from_secs(10),
    );
    assert_eq!(st, JobStatus::Completed);
    let s = Subject::with_roles("tester", &["admin"]);
    assert!(k.scan_by_type(&s, "sensor-claim").unwrap().is_empty());

    let remembered = k.approve_job(job.job_id).unwrap();
    assert_eq!(remembered.len(), 1);
    let claims = k.scan_by_type(&s, "sensor-claim").unwrap();
    assert_eq!(claims.len(), 1, "approval is the only bridge into Class A");
    let c = &claims[0];
    assert_eq!(c.lifecycle.origin, Origin::Reason);
    assert_eq!(
        c.epistemic_status(),
        EpistemicStatus::Inferred,
        "Origin::Reason commits as Inferred (the kernel-stamped transition)"
    );
    assert!(
        c.properties.contains_key("reasoned_from"),
        "claim carries its provenance"
    );
    assert!(
        k.prove(&s, &c.koid).unwrap().chain_valid,
        "the approved claim is in the hash-chained audit stream"
    );
    // Approving again is exact-once (idempotency key per claim).
    let again = k.approve_job(job.job_id).unwrap();
    assert_eq!(
        again[0].koid, remembered[0].koid,
        "re-approval must not duplicate claims"
    );
}

/// cb004: child killed between accept and complete → the job is marked
/// Failed on reopen, never silently dropped.
#[test]
fn cb004_child_kill_between_accept_and_complete_marks_failed_on_reopen() {
    let path = tmp_db("jobkill");
    let progress = path.with_extension("progress");

    let mut child = std::process::Command::new(crasher_exe())
        .arg(&path)
        .arg(&progress)
        .spawn()
        .expect("spawn job_crasher");

    // The child reports only after the Running record is durable — kill it
    // mid-job the moment that happens.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if std::fs::read_to_string(&progress).is_ok() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.wait();
            panic!("crasher never reported a running job");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    hard_kill(&child);
    let killed = child.wait().expect("reap child");
    assert!(
        !killed.success(),
        "child should have been killed, not exited"
    );

    // Windows may release the file handle a beat after taskkill returns.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut k = None;
    while k.is_none() && Instant::now() <= deadline {
        if let Ok(engine) = RedbEngine::open(&path) {
            k = Kernel::open(Arc::new(engine), Arc::new(SystemClock), 7).ok();
        }
        if k.is_none() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let k = k.expect("store must reopen after a hard kill");
    let jobs = k.jobs().unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "the accepted job must not be silently dropped"
    );
    assert_eq!(
        jobs[0].status,
        JobStatus::Failed,
        "a job killed mid-run reopens as Failed"
    );
    assert!(
        jobs[0]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("interrupted"),
        "the failure must say why: {:?}",
        jobs[0].error
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&progress);
}

/// cb005: idempotent submit — the same input hash returns the same job.
#[test]
fn cb005_same_input_hash_returns_same_job() {
    let (k, _store, _clock) = mk();
    seed(&k, "sensor", sensor_props("a"));
    let j1 = k.reason("sensor", sensor_props("a")).unwrap();
    let j2 = k.reason("sensor", sensor_props("a")).unwrap();
    assert_eq!(j1.job_id, j2.job_id, "identical inputs are one job");
    assert_eq!(j1.input_hash, j2.input_hash);
    let j3 = k.reason("sensor", sensor_props("b")).unwrap();
    assert_ne!(j3.job_id, j1.job_id, "different inputs are different jobs");
    let st = wait_status(&k, j1.job_id, JobStatus::Completed, Duration::from_secs(10));
    assert_eq!(st, JobStatus::Completed);
    assert_eq!(
        k.job_result(j1.job_id).unwrap().len(),
        1,
        "the deduped submit must not double-execute"
    );
}

/// M7b: infer and predict run on the same job machinery (no-op AiProvider —
/// the similarity executor — is still legal).
#[test]
fn m7b_infer_and_predict_run_on_the_job_machinery() {
    let (k, _store, _clock) = mk();
    seed(
        &k,
        "doc",
        [("text".to_string(), Value::Text("hello world".into()))]
            .into_iter()
            .collect(),
    );
    let s = Subject::with_roles("tester", &["admin"]);
    let ij = k.infer(&s, "doc", "hello").unwrap();
    let st = wait_status(&k, ij.job_id, JobStatus::Completed, Duration::from_secs(10));
    assert_eq!(st, JobStatus::Completed);
    assert!(
        !k.infer_job_result(ij.job_id).unwrap().is_empty(),
        "infer results are retrievable through the job"
    );

    let pj = k
        .predict(
            &s,
            "doc",
            &[("text".into(), Value::Text("hello".into()))]
                .into_iter()
                .collect(),
            3,
        )
        .unwrap();
    let st = wait_status(&k, pj.job_id, JobStatus::Completed, Duration::from_secs(10));
    assert_eq!(st, JobStatus::Completed);
    let merged = k.predict_job_result(pj.job_id).unwrap();
    assert!(
        merged.contains_key("predicted_from_count"),
        "predict results are retrievable through the job"
    );
}

// ---------------------------------------------------------------------------
// Harness helpers (the crash_kill pattern)
// ---------------------------------------------------------------------------

fn wait_status(k: &Kernel, job_id: u64, want: JobStatus, deadline: Duration) -> JobStatus {
    let start = Instant::now();
    loop {
        let s = k.job_status(job_id).unwrap();
        if s == want || Instant::now() - start > deadline {
            return s;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn tmp_db(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "aikoql_jobkill_{}_{}_{}.redb",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn crasher_exe() -> PathBuf {
    let mut exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    exe.push("../../target/debug/examples/job_crasher");
    #[cfg(windows)]
    exe.set_extension("exe");
    assert!(
        exe.exists(),
        "job_crasher example not built at {:?}; run `cargo build --examples` first",
        exe
    );
    exe
}

fn hard_kill(child: &std::process::Child) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &child.id().to_string()])
            .status();
    }
}
