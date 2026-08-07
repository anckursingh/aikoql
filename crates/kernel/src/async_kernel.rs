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
use std::sync::{mpsc, Arc};

#[derive(Clone)]
pub struct AsyncKernel {
    inner: Arc<Kernel>,
}

impl AsyncKernel {
    pub fn new(kernel: Kernel) -> Self {
        AsyncKernel {
            inner: Arc::new(kernel),
        }
    }

    pub fn from_shared(kernel: Arc<Kernel>) -> Self {
        AsyncKernel { inner: kernel }
    }

    pub fn raw(&self) -> &Arc<Kernel> {
        &self.inner
    }

    async fn run<T, F>(&self, f: F) -> KResult<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Kernel>) -> KResult<T> + Send + 'static,
    {
        let k = self.inner.clone();
        tokio::task::spawn_blocking(move || f(k))
            .await
            .map_err(|e| KError::Store(format!("async join: {}", e)))?
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
}
