//! P5-M8 (ND-07) — the unified index trait: ONE database-level abstraction
//! over catalog-registered indexes (P5-M7 rows of kind "index"). Property and
//! composite hash indexes implement it directly; the vector/text engines
//! become implementations through thin adapters, so the async maintainer
//! applies every index through one surface.

use crate::knowledge::kom::*;
use crate::knowledge::scoring::{ko_text, tokenize};
use crate::transaction::kernel::Kernel;
use crate::{TextIndex, VectorIndex};
use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

/// The database-level index abstraction. `upsert`/`remove` carry the
/// committed object; `commit_batch` is the one flush hook per applied batch
/// (text engines commit once per batch); `scan_eq` is the equality surface
/// property indexes answer; `verify`/`rebuild` reconcile against the store.
pub trait Index: Send + Sync {
    fn name(&self) -> &str;
    fn upsert(&self, koid: KOID, ko: &KnowledgeObject) -> KResult<()>;
    fn remove(&self, koid: &KOID) -> KResult<()>;
    /// One flush point per applied batch (default: nothing to do).
    fn commit_batch(&self) -> KResult<()> {
        Ok(())
    }
    /// Equality scan on the indexed key. Indexes that do not answer equality
    /// scans (vector/text) fail closed.
    fn scan_eq(&self, _key: &[Value]) -> KResult<Vec<KOID>> {
        Err(KError::UnsupportedOperation(
            "this index does not answer equality scans".into(),
        ))
    }
    /// Whether this index answers `property` equality scans over `type_name`
    /// — the M4 read-path assist hook (idx2-008).
    fn covers(&self, _type_name: &str, _property: &str) -> bool {
        false
    }
    fn len(&self) -> usize;
    /// P5-M17b — the freshness stamp: the last committed event seq this
    /// index has fully applied. 0 = no proof; `verify` then walks.
    fn set_applied_seq(&self, _seq: u64) {}
    /// The current freshness stamp.
    fn applied_seq(&self) -> u64 {
        0
    }
    /// Reconcile the index against the store. The default is an UNVERIFIED
    /// report — honest about having checked nothing.
    fn verify(&self, _kernel: &Kernel) -> KResult<VerifyReport> {
        Ok(VerifyReport::unverified(self.name()))
    }
    /// Rebuild from the store. Indexes without a rebuild story fail closed.
    fn rebuild(&self, _kernel: &Kernel) -> KResult<()> {
        Err(KError::UnsupportedOperation(
            "this index does not support rebuild".into(),
        ))
    }
}

/// P5-M8 — the verify checker's reconciliation report (idx2-009).
#[derive(Debug, Clone, PartialEq)]
pub struct VerifyReport {
    pub name: String,
    /// Entries the index holds at verify time.
    pub indexed: usize,
    /// Live store rows the index should hold but does not.
    pub missing: Vec<KOID>,
    /// Index entries no live head backs (unknown or tombstoned koids).
    pub stale: Vec<KOID>,
    /// False for indexes whose `verify` is the default no-op.
    pub verified: bool,
}

impl VerifyReport {
    pub fn unverified(name: &str) -> VerifyReport {
        VerifyReport {
            name: name.into(),
            indexed: 0,
            missing: Vec::new(),
            stale: Vec::new(),
            verified: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Engine adapters — vector/text as Index implementations
// ---------------------------------------------------------------------------

/// Vector engine as an `Index`: immediate upsert (HNSW has no commit
/// concept); `commit_batch` carries the dead-ratio rebuild trigger.
pub struct VectorIndexAdapter {
    inner: Arc<dyn VectorIndex>,
}

impl VectorIndexAdapter {
    pub fn new(inner: Arc<dyn VectorIndex>) -> Self {
        VectorIndexAdapter { inner }
    }
}

impl Index for VectorIndexAdapter {
    fn name(&self) -> &str {
        "vectors"
    }
    fn upsert(&self, koid: KOID, ko: &KnowledgeObject) -> KResult<()> {
        if let Some(sem) = &ko.semantic {
            if let Some(emb) = &sem.embedding {
                // P5-M18 (ann002): model-less embeddings are ""-model entries
                // (the R7 label stays "{model}:{koid_hex}") — the exact path
                // scores them, the ANN must index them too.
                let model = sem.embedding_model.as_deref().unwrap_or("");
                self.inner.upsert(koid, model, emb);
            }
        }
        Ok(())
    }
    fn remove(&self, koid: &KOID) -> KResult<()> {
        self.inner.remove(koid);
        Ok(())
    }
    fn commit_batch(&self) -> KResult<()> {
        // P4-M7 (TDD-VECTOR-002): fires at most once per threshold crossing,
        // never per delete.
        let _ = self.inner.maybe_rebuild();
        Ok(())
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
}

/// Text engine as an `Index`: buffers pending ops and flushes ONE
/// `upsert_many`/`remove_many` pair per `commit_batch` — a single Tantivy
/// commit per batch (the P4-M7 TDD-VECTOR-001 contract, preserved behind the
/// unified surface).
pub struct TextIndexAdapter {
    inner: Arc<dyn TextIndex>,
    pending: RwLock<(Vec<(KOID, BTreeSet<String>)>, Vec<KOID>)>,
}

impl TextIndexAdapter {
    pub fn new(inner: Arc<dyn TextIndex>) -> Self {
        TextIndexAdapter {
            inner,
            pending: RwLock::new((Vec::new(), Vec::new())),
        }
    }
}

impl Index for TextIndexAdapter {
    fn name(&self) -> &str {
        "text"
    }
    fn upsert(&self, koid: KOID, ko: &KnowledgeObject) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        self.pending
            .write()
            .unwrap()
            .0
            .push((koid, tokenize(&ko_text(ko))));
        Ok(())
    }
    fn remove(&self, koid: &KOID) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        self.pending.write().unwrap().1.push(*koid);
        Ok(())
    }
    fn commit_batch(&self) -> KResult<()> {
        let (upserts, removes) = {
            // justified: RwLock poison is unrecoverable
            let mut p = self.pending.write().unwrap();
            (std::mem::take(&mut p.0), std::mem::take(&mut p.1))
        };
        self.inner.upsert_many(&upserts)?;
        self.inner.remove_many(&removes)
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Fails its first `remove_many`, then works (the P1-03 stage-2 shape).
    struct FailingRemoveOnceText {
        inner: crate::TokenTextIndex,
        fail: AtomicBool,
    }
    impl TextIndex for FailingRemoveOnceText {
        fn upsert(&self, koid: KOID, tokens: &BTreeSet<String>) -> KResult<()> {
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
        fn remove_many(&self, koids: &[KOID]) -> KResult<()> {
            if !koids.is_empty() && self.fail.swap(false, Ordering::SeqCst) {
                return Err(KError::Store("forced stage-2 failure".into()));
            }
            self.inner.remove_many(koids)
        }
    }

    // --- P5-M19 — idx3-004 (PR6 P1-03): commit_batch takes BOTH pending
    // halves before either stage runs — a stage-2 failure loses the removes
    // forever. Pin: a retry flushes the removes with no caller re-queue. ---
    #[test]
    fn idx3_004_stage_two_failure_retries_without_loss() {
        let inner = Arc::new(FailingRemoveOnceText {
            inner: crate::TokenTextIndex::new(),
            fail: AtomicBool::new(true),
        });
        let adapter = TextIndexAdapter::new(inner.clone());
        let koid = KOID::from_bytes([1u8; KOID_LEN]);
        let mut ko = KnowledgeObject::new(
            koid,
            Metadata {
                type_name: "note".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("body".into(), Value::Text("retry me".into()));

        adapter.upsert(koid, &ko).unwrap();
        adapter.commit_batch().unwrap();
        assert_eq!(adapter.len(), 1);
        adapter.remove(&koid).unwrap();
        assert!(adapter.commit_batch().is_err(), "stage 2 fails once");
        adapter.commit_batch().unwrap();
        assert_eq!(adapter.len(), 0, "the retry flushes the removes");
    }
}
