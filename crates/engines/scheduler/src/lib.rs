//! Aikoql Scheduler Engine — background job execution.
#![allow(clippy::len_without_is_empty)]
//!
//! Provides the `Scheduler` that manages pluggable background jobs, and
//! `IndexMaintainer` (KE-driven async index maintenance) as the first
//! built-in job type.
//!
//! MRFC-0005 §Knowledge Services: The Scheduler runs background jobs
//! (indexing, embedding generation, compaction) off the critical path.
//! It is a service *around* the kernel, never on the commit path.

pub mod compaction;
pub mod key_rotation;

pub use compaction::CompactionJob;
pub use key_rotation::KeyRotationJob;

use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::knowledge::scoring::{ko_text, tokenize};
use aikoql_kernel::transaction::kernel::Kernel;
use aikoql_kernel::{
    EventFilter, IndexMaintainerApi, IndexStatus, IndexStatusKind, TextIndex, VectorIndex,
};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

// ---------------------------------------------------------------------------
// SchedulerJob trait — pluggable background work
// ---------------------------------------------------------------------------

/// A unit of background work driven by the Knowledge Event stream.
/// Implementations register with the `Scheduler` and receive every
/// committed event. Each job manages its own water mark, catch-up,
/// and checkpoint lifecycle.
pub trait SchedulerJob: Send + Sync {
    /// Human-readable name for logging and debugging.
    fn name(&self) -> &str;

    /// Replay the journal (catch-up), then subscribe to live events.
    /// Called once by the scheduler after all jobs are registered.
    fn start(&self, kernel: &Kernel) -> KResult<()>;

    /// Stop the background thread and join it.
    fn shutdown(&self);

    /// Persist job state to a checkpoint directory.
    fn checkpoint(&self, dir: &std::path::Path) -> KResult<()>;

    /// Current high-water mark (last applied event seq).
    fn water(&self) -> u64;

    /// Events committed but not yet applied.
    fn lag(&self, kernel: &Kernel) -> KResult<u64> {
        let (head, _) = kernel.journal_head()?;
        Ok(head.saturating_sub(self.water()))
    }
}

// ---------------------------------------------------------------------------
// Scheduler — manages a set of background jobs
// ---------------------------------------------------------------------------

/// Owns a set of `SchedulerJob` implementations. On `start_all`, each job
/// replays the journal independently and subscribes to live events.
/// `checkpoint_all` persists every job's state atomically.
pub struct Scheduler {
    jobs: RwLock<Vec<Arc<dyn SchedulerJob>>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            jobs: RwLock::new(Vec::new()),
        }
    }

    /// Register a job. Must be called before `start_all`.
    pub fn register(&self, job: Arc<dyn SchedulerJob>) {
        // justified: RwLock poison is unrecoverable
        self.jobs.write().unwrap().push(job);
    }

    /// Start all registered jobs. Each replays the journal from its
    /// current water mark, then subscribes to live events.
    pub fn start_all(&self, kernel: &Kernel) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        for job in self.jobs.read().unwrap().iter() {
            job.start(kernel)?;
        }
        Ok(())
    }

    /// Shut down all jobs and join their threads.
    pub fn shutdown_all(&self) {
        // justified: RwLock poison is unrecoverable
        for job in self.jobs.read().unwrap().iter() {
            job.shutdown();
        }
    }

    /// Persist every job's state under `dir/<job_name>/`.
    pub fn checkpoint_all(&self, dir: &std::path::Path) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        for job in self.jobs.read().unwrap().iter() {
            job.checkpoint(&dir.join(job.name()))?;
        }
        Ok(())
    }

    /// Number of registered jobs.
    pub fn len(&self) -> usize {
        // justified: RwLock poison is unrecoverable
        self.jobs.read().unwrap().len()
    }

    /// Access a job by name (for tests and introspection).
    pub fn job(&self, name: &str) -> Option<Arc<dyn SchedulerJob>> {
        self.jobs
            .read()
            .unwrap()
            .iter()
            .find(|j| j.name() == name)
            .cloned()
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// IndexMaintainer — KE-driven async index maintenance
// ---------------------------------------------------------------------------

struct MaintainerInner {
    water: AtomicU64,
    stop: AtomicBool,
    handle: Mutex<Option<JoinHandle<()>>>,
    /// TDD-INDEX-001: the last live-apply failure (cleared on success).
    last_error: Mutex<Option<String>>,
}

/// P4-M7 (TDD-VECTOR-001): live events are applied in batches — one
/// `upsert_many`/`remove_many` pair (a single Tantivy commit) per batch.
const MAINTAINER_BATCH: usize = 64;

pub struct IndexMaintainer {
    vectors: Arc<dyn VectorIndex>,
    text: Arc<dyn TextIndex>,
    inner: Arc<MaintainerInner>,
}

impl IndexMaintainer {
    /// Construct a new maintainer. Call `start()` to begin catch-up and
    /// live subscription.
    pub fn new(vectors: Arc<dyn VectorIndex>, text: Arc<dyn TextIndex>) -> Self {
        IndexMaintainer {
            vectors,
            text,
            inner: Arc::new(MaintainerInner {
                water: AtomicU64::new(0),
                stop: AtomicBool::new(false),
                handle: Mutex::new(None),
                last_error: Mutex::new(None),
            }),
        }
    }

    /// Convenience: construct and start in one call.
    pub fn start(
        kernel: &Kernel,
        vectors: Arc<dyn VectorIndex>,
        text: Arc<dyn TextIndex>,
    ) -> KResult<Arc<Self>> {
        Self::start_at(kernel, vectors, text, None)
    }

    /// Construct and start with an optional checkpoint water.
    pub fn start_at(
        kernel: &Kernel,
        vectors: Arc<dyn VectorIndex>,
        text: Arc<dyn TextIndex>,
        resume_water: Option<u64>,
    ) -> KResult<Arc<Self>> {
        let m = Arc::new(Self::new(vectors, text));
        m.do_start(kernel, resume_water)?;
        Ok(m)
    }

    /// Replay the journal from the current (or given) water mark, then
    /// subscribe to live events.
    fn do_start(&self, kernel: &Kernel, resume_water: Option<u64>) -> KResult<()> {
        let water = match resume_water {
            Some(w) => w,
            None => {
                let mut w = 0u64;
                for ke in kernel.journal()? {
                    Self::apply(kernel, &*self.vectors, &*self.text, &ke)?;
                    w = ke.seq;
                }
                w
            }
        };
        self.inner.water.store(water, Ordering::Relaxed);

        let rx = kernel.notify(EventFilter::default())?;
        let state = self.inner.clone();
        let v = self.vectors.clone();
        let t = self.text.clone();
        let k = kernel.clone_handle();
        let handle = std::thread::spawn(move || {
            let mut pending: Vec<KnowledgeEvent> = Vec::new();
            loop {
                if state.stop.load(Ordering::Relaxed) {
                    break;
                }
                match rx.recv_timeout(Duration::from_millis(25)) {
                    Ok(ke) => {
                        pending.push(ke);
                        if pending.len() < MAINTAINER_BATCH {
                            continue;
                        }
                        // justified: async-secondary swallow — the maintainer
                        // is a background service; a failed batch retries on
                        // the next events and the kernel path is unaffected
                        Self::drain_batch(&k, &*v, &*t, &pending, &state);
                        pending.clear();
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if !pending.is_empty() {
                            Self::drain_batch(&k, &*v, &*t, &pending, &state);
                            pending.clear();
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        // justified: Mutex poison is unrecoverable
        *self.inner.handle.lock().unwrap() = Some(handle);
        Ok(())
    }

    pub fn checkpoint(&self, dir: &std::path::Path) -> KResult<()> {
        let tmp = std::path::PathBuf::from(format!("{}.tmp", dir.display()));
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp)
                .map_err(|e| KError::Store(format!("remove stale checkpoint tmp: {}", e)))?;
        }
        std::fs::create_dir_all(&tmp)
            .map_err(|e| KError::Store(format!("create checkpoint tmp: {}", e)))?;
        self.vectors
            .checkpoint(&tmp.join("vectors"))
            .map_err(|e| KError::Store(format!("checkpoint vectors: {}", e)))?;
        self.text
            .checkpoint(&tmp.join("text"))
            .map_err(|e| KError::Store(format!("checkpoint text: {}", e)))?;
        let water = self.water();
        std::fs::write(tmp.join("water.txt"), water.to_string())
            .map_err(|e| KError::Store(format!("write checkpoint water: {}", e)))?;
        std::fs::write(tmp.join("COMPLETE"), b"1")
            .map_err(|e| KError::Store(format!("write checkpoint complete marker: {}", e)))?;
        if dir.exists() {
            std::fs::remove_dir_all(dir)
                .map_err(|e| KError::Store(format!("remove old checkpoint: {}", e)))?;
        }
        std::fs::rename(&tmp, dir)
            .map_err(|e| KError::Store(format!("finalize checkpoint: {}", e)))?;
        Ok(())
    }

    pub fn checkpoint_water(dir: &std::path::Path) -> KResult<Option<u64>> {
        if !dir.join("COMPLETE").exists() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(dir.join("water.txt"))
            .map_err(|e| KError::Store(format!("read checkpoint water: {}", e)))?;
        s.trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|e| KError::Store(format!("parse checkpoint water: {}", e)))
    }

    fn apply(
        kernel: &Kernel,
        vectors: &dyn VectorIndex,
        text: &dyn TextIndex,
        ke: &KnowledgeEvent,
    ) -> KResult<()> {
        Self::apply_batch(kernel, vectors, text, std::slice::from_ref(ke))
    }

    /// P4-M7 (TDD-VECTOR-001): apply a batch of events — per-item vector
    /// ops (HNSW has no commit concept) + ONE `upsert_many`/`remove_many`
    /// pair for the text index (a single Tantivy commit per batch). Event
    /// order is preserved, so batch answers == per-item answers.
    fn apply_batch(
        kernel: &Kernel,
        vectors: &dyn VectorIndex,
        text: &dyn TextIndex,
        events: &[KnowledgeEvent],
    ) -> KResult<()> {
        let mut upserts: Vec<(KOID, BTreeSet<String>)> = Vec::new();
        let mut removes: Vec<KOID> = Vec::new();
        for ke in events {
            match ke.kind {
                EventKind::Forgotten => removes.push(ke.koid),
                _ => match kernel.raw_object_at(&ke.koid, ke.commit_ts)? {
                    Some(ko) if ko.lifecycle.state != LifecycleState::Deleted => {
                        if let Some(sem) = &ko.semantic {
                            if let (Some(model), Some(emb)) = (&sem.embedding_model, &sem.embedding)
                            {
                                vectors.upsert(ke.koid, model, emb);
                            }
                        }
                        upserts.push((ke.koid, tokenize(&ko_text(&ko))));
                    }
                    _ => removes.push(ke.koid),
                },
            }
        }
        for rid in &removes {
            vectors.remove(rid);
        }
        text.upsert_many(&upserts)?;
        text.remove_many(&removes)?;
        // P4-M7 (TDD-VECTOR-002): the dead-ratio rebuild trigger — fires at
        // most once per threshold crossing, never per delete.
        vectors.maybe_rebuild();
        Ok(())
    }

    /// Apply the batch and record the outcome on the shared state.
    fn drain_batch(
        kernel: &Kernel,
        vectors: &dyn VectorIndex,
        text: &dyn TextIndex,
        events: &[KnowledgeEvent],
        state: &MaintainerInner,
    ) {
        match Self::apply_batch(kernel, vectors, text, events) {
            Ok(()) => {
                if let Some(last) = events.last() {
                    state.water.store(last.seq, Ordering::Relaxed);
                }
                // justified: Mutex poison is unrecoverable
                *state.last_error.lock().unwrap() = None;
            }
            Err(e) => {
                // justified: Mutex poison is unrecoverable
                *state.last_error.lock().unwrap() = Some(format!("{e}"));
            }
        }
    }

    pub fn water(&self) -> u64 {
        self.inner.water.load(Ordering::Relaxed)
    }

    pub fn wait_caught_up(&self, kernel: &Kernel, timeout: Duration) -> KResult<()> {
        let (head, _) = kernel.journal_head()?;
        let start = std::time::Instant::now();
        while self.water() < head {
            if start.elapsed() > timeout {
                return Err(KError::IndexLagExceeded);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        // justified: Mutex poison is unrecoverable
        if let Some(h) = self.inner.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

impl SchedulerJob for IndexMaintainer {
    fn name(&self) -> &str {
        "index-maintainer"
    }

    fn start(&self, kernel: &Kernel) -> KResult<()> {
        self.do_start(kernel, None)
    }

    fn shutdown(&self) {
        self.shutdown();
    }

    fn checkpoint(&self, dir: &std::path::Path) -> KResult<()> {
        self.checkpoint(dir)
    }

    fn water(&self) -> u64 {
        self.water()
    }
}

impl IndexMaintainerApi for IndexMaintainer {
    fn lag(&self, kernel: &Kernel) -> KResult<u64> {
        let (head, _) = kernel.journal_head()?;
        Ok(head.saturating_sub(self.water()))
    }

    fn vectors(&self) -> &Arc<dyn VectorIndex> {
        &self.vectors
    }

    fn text(&self) -> &Arc<dyn TextIndex> {
        &self.text
    }

    /// P4-M7 (TDD-INDEX-001): the real status — water/head plus the live
    /// loop's `last_error` (the trait default cannot see the error).
    fn status(&self, kernel: &Kernel) -> KResult<IndexStatus> {
        let (head, _) = kernel.journal_head()?;
        let applied = self.water();
        let lag = head.saturating_sub(applied);
        // justified: Mutex poison is unrecoverable
        let last_error = self.inner.last_error.lock().unwrap().clone();
        Ok(IndexStatus {
            applied_event_seq: applied,
            target_event_seq: head,
            lag,
            status: if last_error.is_some() {
                IndexStatusKind::Error
            } else if lag == 0 {
                IndexStatusKind::CaughtUp
            } else {
                IndexStatusKind::Syncing
            },
            last_error,
        })
    }
}

impl Drop for IndexMaintainer {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::{
        BruteForceVectorIndex, IndexStatusKind, ManualClock, MemoryEngine, Metadata,
        RememberRequest, Subject, TextIndex, TokenTextIndex,
    };
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn mk() -> Kernel {
        let clock = Arc::new(ManualClock::new(20_000));
        Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xCAFE).unwrap()
    }

    fn create(k: &Kernel, subj: &Subject, type_name: &str, body: &str) -> KOID {
        let mut props = PropertyMap::new();
        props.insert("body".into(), Value::Text(body.into()));
        k.remember(RememberRequest {
            context: subj.into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: type_name.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap()
        .koid
    }

    #[test]
    fn scheduler_runs_multiple_jobs() {
        let k = mk();
        let a = Subject::new("alice");

        // Job 1: standard index maintainer
        let v1: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
        let t1: Arc<dyn TextIndex> = Arc::new(TokenTextIndex::new());
        let m1 = Arc::new(IndexMaintainer::new(v1.clone(), t1.clone()));

        // Job 2: a second maintainer with its own indexes (e.g., per-tenant)
        let v2: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
        let t2: Arc<dyn TextIndex> = Arc::new(TokenTextIndex::new());
        let m2 = Arc::new(IndexMaintainer::new(v2.clone(), t2.clone()));

        let sched = Scheduler::new();
        sched.register(m1.clone());
        sched.register(m2.clone());
        assert_eq!(sched.len(), 2);

        // Commit before starting: both jobs catch up during start_all.
        create(&k, &a, "note", "hello world");
        sched.start_all(&k).unwrap();

        // Both maintainers should have caught up.
        m1.wait_caught_up(&k, Duration::from_secs(1)).unwrap();
        m2.wait_caught_up(&k, Duration::from_secs(1)).unwrap();
        assert_eq!(t1.len(), 1);
        assert_eq!(t2.len(), 1);

        // Live commit: both jobs should apply it.
        create(&k, &a, "note", "second doc");
        m1.wait_caught_up(&k, Duration::from_secs(1)).unwrap();
        m2.wait_caught_up(&k, Duration::from_secs(1)).unwrap();
        assert_eq!(t1.len(), 2);
        assert_eq!(t2.len(), 2);

        // Checkpoint all.
        let dir = std::env::temp_dir().join("scheduler_test_checkpoint");
        let _ = std::fs::remove_dir_all(&dir);
        sched.checkpoint_all(&dir).unwrap();
        assert!(dir.join("index-maintainer").join("COMPLETE").exists());

        sched.shutdown_all();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_maintainer_starts_via_scheduler_job_trait() {
        let k = mk();
        let a = Subject::new("alice");

        let v: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
        let t: Arc<dyn TextIndex> = Arc::new(TokenTextIndex::new());
        let m = Arc::new(IndexMaintainer::new(v.clone(), t.clone()));

        // Commit before starting.
        create(&k, &a, "note", "cats and dogs");
        // Start via the SchedulerJob trait.
        SchedulerJob::start(&*m, &k).unwrap();
        m.wait_caught_up(&k, Duration::from_secs(1)).unwrap();
        assert_eq!(t.len(), 1);

        m.shutdown();
    }

    #[test]
    fn scheduler_job_by_name() {
        let sched = Scheduler::new();
        let v: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
        let t: Arc<dyn TextIndex> = Arc::new(TokenTextIndex::new());
        let m = Arc::new(IndexMaintainer::new(v, t));
        sched.register(m);

        let found = sched.job("index-maintainer");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name(), "index-maintainer");

        assert!(sched.job("nonexistent").is_none());
    }

    // --- P4-M7 (TDD-INDEX-001) — idx001: lag fields on every index. RED:
    // `status()` / `IndexStatusKind` do not exist yet. ---

    #[test]
    fn idx001_status_reports_lag_then_caught_up() {
        let k = mk();
        let a = Subject::new("alice");
        create(&k, &a, "note", "one");
        create(&k, &a, "note", "two");
        let v: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
        let t: Arc<dyn TextIndex> = Arc::new(TokenTextIndex::new());
        let m = Arc::new(IndexMaintainer::new(v.clone(), t.clone()));

        let s = m.status(&k).unwrap();
        assert_eq!(s.applied_event_seq, 0, "nothing applied yet");
        assert!(s.lag >= 1, "committed events are unapplied");
        assert_eq!(s.target_event_seq, s.applied_event_seq + s.lag);
        assert_eq!(s.status, IndexStatusKind::Syncing);
        assert_eq!(s.last_error, None);

        SchedulerJob::start(&*m, &k).unwrap();
        m.wait_caught_up(&k, Duration::from_secs(1)).unwrap();
        let s = m.status(&k).unwrap();
        assert_eq!(s.applied_event_seq, s.target_event_seq);
        assert_eq!(s.lag, 0);
        assert_eq!(s.status, IndexStatusKind::CaughtUp);
        assert_eq!(s.last_error, None);
    }

    /// A text index that fails its first upsert, then works.
    struct FailingOnceText {
        inner: TokenTextIndex,
        fail: AtomicBool,
    }
    impl TextIndex for FailingOnceText {
        fn upsert(&self, koid: KOID, tokens: &BTreeSet<String>) -> KResult<()> {
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(KError::Store("forced apply failure".into()));
            }
            self.inner.upsert(koid, tokens)
        }
        fn remove(&self, koid: &KOID) -> KResult<()> {
            self.inner.remove(koid)
        }
        fn search(&self, tokens: &BTreeSet<String>, k: usize) -> KResult<Vec<(KOID, f32)>> {
            self.inner.search(tokens, k)
        }
        fn len(&self) -> usize {
            self.inner.len()
        }
    }

    #[test]
    fn idx001_status_records_last_error_and_recovers() {
        let k = mk();
        let a = Subject::new("alice");
        let v: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
        let t: Arc<dyn TextIndex> = Arc::new(FailingOnceText {
            inner: TokenTextIndex::new(),
            fail: AtomicBool::new(true),
        });
        let m = Arc::new(IndexMaintainer::new(v, t));
        SchedulerJob::start(&*m, &k).unwrap();

        create(&k, &a, "note", "first fails");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_error = false;
        while std::time::Instant::now() < deadline {
            let s = m.status(&k).unwrap();
            if s.status == IndexStatusKind::Error {
                saw_error = true;
                assert!(s.last_error.is_some(), "the failure reason is surfaced");
                assert!(s.lag >= 1, "water did not advance past the failed event");
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            saw_error,
            "the failed apply is surfaced as Error + last_error"
        );

        create(&k, &a, "note", "second succeeds");
        m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
        let s = m.status(&k).unwrap();
        assert_eq!(s.status, IndexStatusKind::CaughtUp);
        assert_eq!(s.last_error, None, "a successful apply clears last_error");
    }
}
