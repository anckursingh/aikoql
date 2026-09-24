//! Async facade over the synchronous Kernel (tokio).
//!
//! Semantics are IDENTICAL to the sync surface — each call is dispatched to
//! the blocking thread pool; the commit pipeline remains single-writer under
//! its mutex. This facade exists so servers (MCP, REST/gRPC later) never block
//! an async runtime thread on storage I/O.

use crate::eval::{
    Contradiction, EvalContradictionQuery, EvalRecallQuery, EvalRecallReport, EvalStalenessQuery,
    EvalStalenessReport,
};
use crate::knowledge::kom::{
    Action, KError, KResult, KnowledgeEvent, KnowledgeObject, LifecycleState, Origin, KOID,
};
use crate::transaction::kernel::{
    Evolved, Explanation, ForgetMode, Forgotten, Kernel, Lineage, Proof, RememberRequest,
    Remembered, ScoredKO, SimilarityQuery, Subject,
};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use tokio::sync::Semaphore;

/// Default concurrency bound for `AsyncKernel::new`.
const DEFAULT_PERMITS: usize = 64;
/// Ring size for queue-wait samples — `wait_p99_ns` is the 99th percentile
/// over the most recent `WAIT_SAMPLES` waits (a sample, not a full histogram).
const WAIT_SAMPLES: usize = 256;

/// Queue-wait metrics for the bounded async facade (TDD-RUNTIME-001).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncQueueStats {
    /// Tasks currently waiting for a permit.
    pub waiting: usize,
    /// Total number of waits that actually queued.
    pub queued: u64,
    /// Total queue-wait time in ns.
    pub wait_ns_total: u64,
    /// Largest single queue wait in ns.
    pub max_wait_ns: u64,
    /// 99th percentile of recent queue waits (ring sample) in ns.
    pub wait_p99_ns: u64,
}

struct QueueStats {
    waiting: AtomicUsize,
    queued: AtomicU64,
    wait_ns_total: AtomicU64,
    max_wait_ns: AtomicU64,
    samples: [AtomicU64; WAIT_SAMPLES],
    next: AtomicUsize,
}

impl Default for QueueStats {
    fn default() -> Self {
        QueueStats {
            waiting: AtomicUsize::new(0),
            queued: AtomicU64::new(0),
            wait_ns_total: AtomicU64::new(0),
            max_wait_ns: AtomicU64::new(0),
            samples: std::array::from_fn(|_| AtomicU64::new(0)),
            next: AtomicUsize::new(0),
        }
    }
}

#[derive(Clone)]
pub struct AsyncKernel {
    inner: Arc<Kernel>,
    permits: Arc<Semaphore>,
    stats: Arc<QueueStats>,
}

impl AsyncKernel {
    pub fn new(kernel: Kernel) -> Self {
        Self::bounded(kernel, DEFAULT_PERMITS)
    }

    pub fn from_shared(kernel: Arc<Kernel>) -> Self {
        AsyncKernel {
            inner: kernel,
            permits: Arc::new(Semaphore::new(DEFAULT_PERMITS)),
            stats: Arc::new(QueueStats::default()),
        }
    }

    /// P4-M7 (TDD-RUNTIME-001): an explicit concurrency bound — at most
    /// `permits` kernel tasks execute at once, the rest queue on the
    /// semaphore. Permits are NOT re-entrant: a task holding one must not
    /// await another kernel call (the sync Kernel cannot, so this holds).
    pub fn bounded(kernel: Kernel, permits: usize) -> Self {
        AsyncKernel {
            inner: Arc::new(kernel),
            permits: Arc::new(Semaphore::new(permits)),
            stats: Arc::new(QueueStats::default()),
        }
    }

    pub fn raw(&self) -> &Arc<Kernel> {
        &self.inner
    }

    pub fn queue_stats(&self) -> AsyncQueueStats {
        let s = &self.stats;
        let n = s.next.load(Ordering::Relaxed).min(WAIT_SAMPLES);
        let mut samples: Vec<u64> = s.samples[..n]
            .iter()
            .map(|a| a.load(Ordering::Relaxed))
            .collect();
        samples.sort_unstable();
        let p99 = if n == 0 {
            0
        } else {
            samples[(n - 1) * 99 / 100]
        };
        AsyncQueueStats {
            waiting: s.waiting.load(Ordering::Relaxed),
            queued: s.queued.load(Ordering::Relaxed),
            wait_ns_total: s.wait_ns_total.load(Ordering::Relaxed),
            max_wait_ns: s.max_wait_ns.load(Ordering::Relaxed),
            wait_p99_ns: p99,
        }
    }

    async fn run<T, F>(&self, f: F) -> KResult<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Kernel>) -> KResult<T> + Send + 'static,
    {
        let permit = {
            let start = std::time::Instant::now();
            let s = &self.stats;
            s.waiting.fetch_add(1, Ordering::Relaxed);
            let p = self
                .permits
                .acquire()
                .await
                .map_err(|e| KError::Store(format!("async acquire: {}", e)))?;
            let w = start.elapsed().as_nanos() as u64;
            s.waiting.fetch_sub(1, Ordering::Relaxed);
            if w > 0 {
                s.queued.fetch_add(1, Ordering::Relaxed);
                s.wait_ns_total.fetch_add(w, Ordering::Relaxed);
                s.max_wait_ns.fetch_max(w, Ordering::Relaxed);
                let i = s.next.fetch_add(1, Ordering::Relaxed) % WAIT_SAMPLES;
                s.samples[i].store(w, Ordering::Relaxed);
            }
            p
        };
        let k = self.inner.clone();
        let out = tokio::task::spawn_blocking(move || f(k))
            .await
            .map_err(|e| KError::Store(format!("async join: {}", e)))?;
        drop(permit); // released only after the kernel work completes
        out
    }

    pub async fn remember(&self, req: RememberRequest) -> KResult<Remembered> {
        self.run(move |k| k.remember(req)).await
    }

    pub async fn evolve(
        &self,
        subject: Subject,
        koid: KOID,
        to: LifecycleState,
        origin: Origin,
        expected_version: Option<u64>,
        note: Option<String>,
    ) -> KResult<Evolved> {
        self.run(move |k| k.evolve(&subject, &koid, to, origin, expected_version, note))
            .await
    }

    pub async fn forget(
        &self,
        subject: Subject,
        koid: KOID,
        mode: ForgetMode,
        expected_version: Option<u64>,
        note: Option<String>,
    ) -> KResult<Forgotten> {
        self.run(move |k| k.forget(&subject, &koid, mode, expected_version, note))
            .await
    }

    pub async fn get(&self, subject: Subject, koid: KOID) -> KResult<KnowledgeObject> {
        self.run(move |k| k.get(&subject, &koid)).await
    }

    pub async fn verify(&self, subject: Subject, koid: KOID, action: Action) -> KResult<()> {
        self.run(move |k| k.verify(&subject, &koid, action)).await
    }

    pub async fn find_similar(&self, q: SimilarityQuery) -> KResult<Vec<ScoredKO>> {
        self.run(move |k| k.find_similar(q)).await
    }

    pub async fn trace(&self, subject: Subject, koid: KOID) -> KResult<Lineage> {
        self.run(move |k| k.trace(&subject, &koid)).await
    }

    pub async fn explain(
        &self,
        subject: Subject,
        koid: KOID,
        version: Option<u64>,
    ) -> KResult<Explanation> {
        self.run(move |k| k.explain(&subject, &koid, version)).await
    }

    pub async fn prove(&self, subject: Subject, koid: KOID) -> KResult<Proof> {
        self.run(move |k| k.prove(&subject, &koid)).await
    }

    pub async fn subscribe(
        &self,
        id: String,
        filter: crate::transaction::kernel::EventFilter,
    ) -> KResult<mpsc::Receiver<KnowledgeEvent>> {
        self.run(move |k| k.subscribe(id, filter)).await
    }

    pub async fn unsubscribe(&self, id: String) -> KResult<()> {
        self.run(move |k| k.unsubscribe(&id)).await
    }

    pub async fn ack(&self, id: String, seq: u64) -> KResult<()> {
        self.run(move |k| k.ack(&id, seq)).await
    }

    pub async fn replay(&self, id: String) -> KResult<Vec<KnowledgeEvent>> {
        self.run(move |k| k.replay(&id)).await
    }

    pub async fn eval_recall(&self, q: EvalRecallQuery) -> KResult<EvalRecallReport> {
        self.run(move |k| k.eval_recall(q)).await
    }

    pub async fn eval_staleness(&self, q: EvalStalenessQuery) -> KResult<EvalStalenessReport> {
        self.run(move |k| k.eval_staleness(q)).await
    }

    pub async fn eval_contradictions(
        &self,
        q: EvalContradictionQuery,
    ) -> KResult<Vec<Contradiction>> {
        self.run(move |k| k.eval_contradictions(q)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::kom::{Metadata, Value};
    use crate::storage::store::MemoryEngine;
    use crate::transaction::kernel::ManualClock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn meta(t: &str) -> Metadata {
        Metadata {
            type_name: t.into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_round_trip_matches_sync_semantics() {
        let clock = Arc::new(ManualClock::new(5_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 1).unwrap();
        let ak = AsyncKernel::new(k);
        let alice = Subject::new("alice");

        let r = ak
            .remember(RememberRequest::create(alice.clone(), meta("fact")))
            .await
            .unwrap();
        assert_eq!(r.version, 1);

        let mut up = RememberRequest::update(alice.clone(), r.koid, meta("fact"));
        up.properties.insert("n".into(), Value::Int(7));
        let r2 = ak.remember(up).await.unwrap();
        assert_eq!(r2.version, 2);

        let ko = ak.get(alice.clone(), r.koid).await.unwrap();
        assert_eq!(ko.properties.get("n"), Some(&Value::Int(7)));

        let proof = ak.prove(alice.clone(), r.koid).await.unwrap();
        assert!(proof.chain_valid);
    }

    // -----------------------------------------------------------------------
    // P4-M7 (TDD-RUNTIME-001) — run001: overload is bounded + measurable.
    // RED state: `bounded`/`queue_stats`/`wait_p99_ns` do not exist yet.
    // -----------------------------------------------------------------------

    /// `permits` tasks may execute kernel work at once; the rest queue on
    /// the semaphore and drain — overload creates bounded concurrent work,
    /// never unbounded spawn_blocking depth.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn run001_concurrency_is_bounded_by_permits() {
        let clock = Arc::new(ManualClock::new(5_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 1).unwrap();
        let ak = AsyncKernel::bounded(k, 2);
        let current = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let ak = ak.clone();
            let current = current.clone();
            let peak = peak.clone();
            tasks.push(tokio::spawn(async move {
                ak.run(move |_| {
                    let c = current.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(c, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    current.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(
            peak.load(Ordering::SeqCst),
            2,
            "only `permits` kernel tasks may run at once"
        );
        let s = ak.queue_stats();
        assert_eq!(s.waiting, 0, "the queue drains — no leaked permits");
    }

    /// Overload: with both permits held by slow tasks, the queued tasks
    /// record a measurable, bounded queue wait (p99 above zero, under 1 s
    /// here) and complete once the blockers finish.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn run001_overload_queue_wait_is_measured_and_bounded() {
        let clock = Arc::new(ManualClock::new(5_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 1).unwrap();
        let ak = AsyncKernel::bounded(k, 2);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut blockers = Vec::new();
        for _ in 0..2 {
            let ak = ak.clone();
            let tx = tx.clone();
            blockers.push(tokio::spawn(async move {
                ak.run(move |_| {
                    let _ = tx.send(());
                    std::thread::sleep(std::time::Duration::from_millis(120));
                    Ok(())
                })
                .await
                .unwrap();
            }));
        }
        rx.recv().unwrap();
        rx.recv().unwrap(); // both permits are now held by sleeping tasks

        let mut queued = Vec::new();
        for _ in 0..4 {
            let ak = ak.clone();
            queued.push(tokio::spawn(async move {
                ak.run(|_| Ok(())).await.unwrap();
            }));
        }
        for t in queued {
            t.await.unwrap();
        }
        for t in blockers {
            t.await.unwrap();
        }

        let s = ak.queue_stats();
        assert!(s.queued >= 4, "every overflow task counted a queue wait");
        assert!(s.wait_ns_total > 0, "queue wait is measured");
        let p99 = s.wait_p99_ns;
        assert!(p99 > 0, "p99 queue wait is measurable under overload");
        assert!(
            p99 < 1_000_000_000 && s.max_wait_ns < 1_000_000_000,
            "queue wait stays bounded ({p99} ns) — no unbounded blocked work"
        );
        assert_eq!(s.waiting, 0, "the overload fully drains");
    }
}
