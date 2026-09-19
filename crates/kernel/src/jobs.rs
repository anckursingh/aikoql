//! P3-M7 — Class-B job scheduler (MRFC-0011 §6.10–6.13, §7, §8, §10.2).
//!
//! `reason` / `infer` / `predict` submit Class-B jobs instead of computing
//! inline. A job is a persisted minimal record (status, input-hash, result
//! ref) in the raw store — `job/<id>` and `jobres/<id>`. Results re-enter
//! the Class-A store ONLY via `Kernel::approve_job` (the Determinism Law,
//! §7: Class-B outputs influence Class A only as committed Claims).
//!
//! cb004: the Running record is durable BEFORE the worker thread starts, and
//! `recover` marks any Running row Failed("interrupted") on open — a job
//! killed with its process is never silently dropped.
//!
//! Job idempotency (P5-M20): Completed jobs deduplicate — in memory via
//! by_hash and across restart via recover's rebuild; Failed jobs retry
//! fresh — finish evicts them from by_hash and recover skips them. The
//! persisted record carries the caller's subject/tenant/roles, and the
//! worker runs under that context (job004).

use crate::knowledge::codec::{decode_ko, encode_ko, Dec, Enc};
use crate::knowledge::kom::{KError, KResult, KnowledgeObject, Value};
use crate::storage::store::{StorageEngine, WriteBatch};
use crate::transaction::kernel::{Kernel, ScoredKO, Subject};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One job row: `job/<id(8BE)>` — the persisted minimal job table.
const JOB_PREFIX: &[u8] = b"job/";
/// The job's result blob: `jobres/<id(8BE)>` — the "result ref" (the id IS
/// the ref; nothing stores the blob twice).
const JOBRES_PREFIX: &[u8] = b"jobres/";
/// Admission bound for concurrent Class-B jobs (over it: JOB_REJECTED, §8).
pub const DEFAULT_MAX_RUNNING_JOBS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Completed,
    Failed,
}

impl JobStatus {
    pub fn tag(self) -> u8 {
        match self {
            JobStatus::Running => 0,
            JobStatus::Completed => 1,
            JobStatus::Failed => 2,
        }
    }
    pub fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(JobStatus::Running),
            1 => Some(JobStatus::Completed),
            2 => Some(JobStatus::Failed),
            _ => None,
        }
    }
}

/// What the job computes — persisted in the record so result decoders can
/// disagree loudly instead of decoding garbage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Reason,
    Infer,
    Predict,
}

impl JobKind {
    pub fn tag(self) -> u8 {
        match self {
            JobKind::Reason => 0,
            JobKind::Infer => 1,
            JobKind::Predict => 2,
        }
    }
    pub fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(JobKind::Reason),
            1 => Some(JobKind::Infer),
            2 => Some(JobKind::Predict),
            _ => None,
        }
    }
}

/// The handle `reason`/`infer`/`predict` return — submit never blocks on the
/// computation (MRFC-0011 §6.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobHandle {
    pub job_id: u64,
    pub input_hash: [u8; 32],
}

/// The persisted job-table row (MRFC-0011 §6.10: status, input-hash, result
/// ref — the ref is `jobres/<job_id>`, implied by the id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRecord {
    pub job_id: u64,
    pub status: JobStatus,
    pub kind: JobKind,
    pub input_hash: [u8; 32],
    pub error: Option<String>,
    /// P5-M20: the caller the job runs under — persisted so a job's context
    /// survives its process (Class-B workers are tenant-confined).
    pub subject_name: String,
    pub subject_roles: Vec<String>,
    pub subject_tenant: Option<String>,
}

fn encode_job(rec: &JobRecord) -> Vec<u8> {
    let mut e = Enc::new();
    e.u64(rec.job_id);
    e.u8(rec.status.tag());
    e.u8(rec.kind.tag());
    e.hash256(&rec.input_hash);
    e.opt_str(rec.error.as_deref());
    e.str(&rec.subject_name);
    e.u32(rec.subject_roles.len() as u32);
    for r in &rec.subject_roles {
        e.str(r);
    }
    e.opt_str(rec.subject_tenant.as_deref());
    e.buf
}

fn decode_job(buf: &[u8]) -> KResult<JobRecord> {
    let mut d = Dec::new(buf);
    let job_id = d.u64()?;
    let status = JobStatus::from_tag(d.u8()?)
        .ok_or_else(|| KError::Codec("invalid job status tag".into()))?;
    let kind =
        JobKind::from_tag(d.u8()?).ok_or_else(|| KError::Codec("invalid job kind tag".into()))?;
    let input_hash = d.hash256()?;
    let error = d.opt_str()?;
    // Pre-M20 rows end here — decode their caller context as the old
    // kernel-reason/admin defaults (exactly the context they ran under).
    let (subject_name, subject_roles, subject_tenant) = if d.remaining() > 0 {
        let name = d.str()?;
        let n = d.u32()? as usize;
        let mut roles = Vec::with_capacity(n);
        for _ in 0..n {
            roles.push(d.str()?);
        }
        let tenant = d.opt_str()?;
        (name, roles, tenant)
    } else {
        ("kernel-reason".into(), vec!["admin".into()], None)
    };
    d.finish()?;
    Ok(JobRecord {
        job_id,
        status,
        kind,
        input_hash,
        error,
        subject_name,
        subject_roles,
        subject_tenant,
    })
}

fn job_key(id: u64) -> Vec<u8> {
    let mut k = JOB_PREFIX.to_vec();
    k.extend_from_slice(&id.to_be_bytes());
    k
}

fn jobres_key(id: u64) -> Vec<u8> {
    let mut k = JOBRES_PREFIX.to_vec();
    k.extend_from_slice(&id.to_be_bytes());
    k
}

/// The job's input, moved into the worker thread.
pub(crate) enum JobWork {
    Reason {
        subject: Subject,
        rule_type: String,
        rule_props: BTreeMap<String, Value>,
    },
    Infer {
        subject: Subject,
        type_name: String,
        text: String,
    },
    Predict {
        subject: Subject,
        type_name: String,
        props: BTreeMap<String, Value>,
        k: usize,
    },
}

impl JobWork {
    /// P5-M20 (job004): the caller this job runs under — persisted into the
    /// record at submit and carried into the worker, so the Class-B scan is
    /// confined to the caller's tenant.
    pub(crate) fn subject(&self) -> &Subject {
        match self {
            JobWork::Reason { subject, .. } => subject,
            JobWork::Infer { subject, .. } => subject,
            JobWork::Predict { subject, .. } => subject,
        }
    }
}

/// Execute the job against the kernel and return the encoded result blob.
/// Reason claims stay Class B here — nothing touches the Class-A store.
pub(crate) fn run_work(kernel: &Kernel, work: &JobWork) -> KResult<Vec<u8>> {
    match work {
        JobWork::Reason {
            subject,
            rule_type,
            rule_props,
        } => {
            let claims = kernel.run_reason(subject, rule_type, rule_props)?;
            let mut e = Enc::new();
            e.u32(claims.len() as u32);
            for c in &claims {
                let b = encode_ko(c);
                e.u32(b.len() as u32);
                e.raw(&b);
            }
            Ok(e.buf)
        }
        JobWork::Infer {
            subject,
            type_name,
            text,
        } => {
            let rows = kernel.run_infer(subject, type_name, text)?;
            let mut e = Enc::new();
            e.u32(rows.len() as u32);
            for r in &rows {
                let b = encode_ko(&r.ko);
                e.u32(b.len() as u32);
                e.raw(&b);
                e.f32(r.score);
                e.u64(r.index_lag_ms);
            }
            Ok(e.buf)
        }
        JobWork::Predict {
            subject,
            type_name,
            props,
            k,
        } => {
            let merged = kernel.run_predict(subject, type_name, props, *k)?;
            let mut e = Enc::new();
            crate::knowledge::codec::enc_map(&mut e, &merged);
            Ok(e.buf)
        }
    }
}

pub(crate) fn decode_reason_result(buf: &[u8]) -> KResult<Vec<KnowledgeObject>> {
    let mut d = Dec::new(buf);
    let n = d.u32()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let len = d.u32()? as usize;
        out.push(decode_ko(d.raw(len)?)?);
    }
    d.finish()?;
    Ok(out)
}

pub(crate) fn decode_infer_result(buf: &[u8]) -> KResult<Vec<ScoredKO>> {
    let mut d = Dec::new(buf);
    let n = d.u32()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let len = d.u32()? as usize;
        let ko = decode_ko(d.raw(len)?)?;
        let score = d.f32()?;
        let index_lag_ms = d.u64()?;
        out.push(ScoredKO {
            ko,
            score,
            index_lag_ms,
        });
    }
    d.finish()?;
    Ok(out)
}

pub(crate) fn decode_predict_result(buf: &[u8]) -> KResult<BTreeMap<String, Value>> {
    let mut d = Dec::new(buf);
    let m = crate::knowledge::codec::dec_map(&mut d)?;
    d.finish()?;
    Ok(m)
}

/// The Class-B scheduler: admission control + the persisted job table.
pub struct JobScheduler {
    store: Arc<dyn StorageEngine>,
    next_id: AtomicU64,
    running: AtomicUsize,
    max_running: AtomicUsize,
    /// Test hook (cb002/cb004): the worker sleeps this many ms at job start,
    /// so tests can pin admission and kill windows. 0 = no park.
    park_ms: AtomicU64,
    /// cb005: input_hash → job_id. In-memory (see the ponytail note above).
    by_hash: Mutex<HashMap<[u8; 32], u64>>,
}

impl JobScheduler {
    /// Recover on open: any Running job died with its process (the store was
    /// live-locked to it) — mark it Failed("interrupted"). Never silently
    /// dropped (cb004).
    pub fn recover(store: Arc<dyn StorageEngine>, max_running: usize) -> KResult<Self> {
        let mut next_id = 0u64;
        let mut by_hash = HashMap::new();
        let mut interrupted = WriteBatch::new();
        for (k, v) in store.scan(JOB_PREFIX)? {
            let mut rec = decode_job(&v)?;
            if rec.status == JobStatus::Running {
                rec.status = JobStatus::Failed;
                rec.error = Some("interrupted by process exit".into());
                interrupted.put(k, encode_job(&rec));
            }
            next_id = next_id.max(rec.job_id);
            // P5-M20 (job003): only non-Failed rows rebuild the dedup table —
            // a failed input (including one just marked interrupted) must be
            // retriable fresh after a restart.
            if rec.status != JobStatus::Failed {
                by_hash.insert(rec.input_hash, rec.job_id);
            }
        }
        if !interrupted.is_empty() {
            store.write_batch(&interrupted)?;
        }
        Ok(JobScheduler {
            store,
            next_id: AtomicU64::new(next_id),
            running: AtomicUsize::new(0),
            max_running: AtomicUsize::new(max_running),
            park_ms: AtomicU64::new(0),
            by_hash: Mutex::new(by_hash),
        })
    }

    pub fn set_max_running(&self, n: usize) {
        self.max_running.store(n, Ordering::Relaxed);
    }

    pub fn set_park_ms(&self, ms: u64) {
        self.park_ms.store(ms, Ordering::Relaxed);
    }

    pub fn record(&self, job_id: u64) -> KResult<JobRecord> {
        match self.store.get(&job_key(job_id))? {
            Some(v) => decode_job(&v),
            None => Err(KError::NotFound(crate::knowledge::kom::KOID::ZERO)),
        }
    }

    pub fn list(&self) -> KResult<Vec<JobRecord>> {
        self.store
            .scan(JOB_PREFIX)?
            .into_iter()
            .map(|(_, v)| decode_job(&v))
            .collect()
    }

    pub fn result_blob(&self, job_id: u64) -> KResult<Vec<u8>> {
        let rec = self.record(job_id)?;
        match rec.status {
            JobStatus::Completed => self
                .store
                .get(&jobres_key(job_id))?
                .ok_or_else(|| KError::Codec(format!("job {job_id} completed without a result"))),
            JobStatus::Failed => Err(KError::UnsupportedOperation(format!(
                "job {job_id} failed: {}",
                rec.error.unwrap_or_default()
            ))),
            JobStatus::Running => Err(KError::UnsupportedOperation(format!(
                "job {job_id} is still running"
            ))),
        }
    }

    /// Admit (cb002), dedup (cb005), persist the Running record durably
    /// (cb004), emit the admission audit KE (§10.2), then run the worker.
    /// P5-M20 (job001/job002): the admission slot is claimed by CAS BEFORE
    /// the durable write — the Running row, the by_hash entry and the slot
    /// are all-or-nothing: every abort path releases them.
    pub(crate) fn submit(
        self: &Arc<Self>,
        kernel: &Kernel,
        kind: JobKind,
        input_hash: [u8; 32],
        work: JobWork,
    ) -> KResult<JobHandle> {
        if let Some(existing) = self.by_hash.lock().unwrap().get(&input_hash) {
            return Ok(JobHandle {
                job_id: *existing,
                input_hash,
            });
        }
        let max = self.max_running.load(Ordering::Relaxed);
        let mut cur = self.running.load(Ordering::Acquire);
        loop {
            if cur >= max {
                return Err(KError::JobRejected(format!(
                    "admission limit reached ({cur} >= {max})"
                )));
            }
            match self.running.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(c) => cur = c,
            }
        }
        let job_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let caller = work.subject().clone();
        let rec = JobRecord {
            job_id,
            status: JobStatus::Running,
            kind,
            input_hash,
            error: None,
            subject_name: caller.name,
            subject_roles: caller.roles,
            subject_tenant: caller.tenant,
        };
        let mut batch = WriteBatch::new();
        batch.put(job_key(job_id), encode_job(&rec));
        if let Err(e) = self.store.write_batch(&batch) {
            self.running.fetch_sub(1, Ordering::AcqRel);
            return Err(e);
        }
        self.by_hash.lock().unwrap().insert(input_hash, job_id);
        if let Err(e) =
            kernel.record_audit(&format!("class-b job {job_id} admitted (kind {kind:?})"))
        {
            // The Running row and by_hash entry are already durable — undo
            // them, or a restart would resurrect a job never admitted.
            self.abort_admission(job_id, input_hash);
            return Err(e);
        }
        let s2 = Arc::clone(self);
        let k2 = kernel.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("aikoql-job-{job_id}"))
            .spawn(move || {
                let park = s2.park_ms.load(Ordering::Relaxed);
                if park > 0 {
                    std::thread::sleep(Duration::from_millis(park));
                }
                let res = run_work(&k2, &work);
                s2.finish(job_id, res);
                s2.running.fetch_sub(1, Ordering::Relaxed);
            });
        if let Err(e) = spawned {
            self.abort_admission(job_id, input_hash);
            return Err(KError::Store(format!("failed to spawn job {job_id}: {e}")));
        }
        Ok(JobHandle { job_id, input_hash })
    }

    /// P5-M20 (job002): undo an admitted-but-never-run job — the durable
    /// Running row, the by_hash entry, and the CAS admission slot. Called
    /// from every abort path after the durable write.
    fn abort_admission(&self, job_id: u64, input_hash: [u8; 32]) {
        let mut undo = WriteBatch::new();
        undo.del(job_key(job_id));
        let _ = self.store.write_batch(&undo);
        self.by_hash.lock().unwrap().remove(&input_hash);
        self.running.fetch_sub(1, Ordering::AcqRel);
    }

    /// The worker's terminal write: result blob + terminal status in one
    /// batch, so a crash never leaves Completed-without-result (or vice
    /// versa).
    fn finish(&self, job_id: u64, res: KResult<Vec<u8>>) {
        let mut batch = WriteBatch::new();
        let mut rec = match self.record(job_id) {
            Ok(r) => r,
            Err(_) => return,
        };
        match res {
            Ok(blob) => {
                rec.status = JobStatus::Completed;
                batch.put(jobres_key(job_id), blob);
            }
            Err(e) => {
                rec.status = JobStatus::Failed;
                rec.error = Some(e.to_string());
            }
        }
        batch.put(job_key(job_id), encode_job(&rec));
        let _ = self.store.write_batch(&batch);
        // P5-M20 (job003): a failed job retries fresh — evict it from the
        // dedup table so the next identical submit is a new admission.
        if rec.status == JobStatus::Failed {
            self.by_hash.lock().unwrap().remove(&rec.input_hash);
        }
    }
}
