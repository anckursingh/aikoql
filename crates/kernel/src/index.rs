//! Index lifecycle machinery (MRFC-0009 semantics, draft).
//!
//! Architecture:
//! - Indexes are SECONDARY structures maintained asynchronously from the
//!   Knowledge Event stream — never written on the commit path (Determinism Law).
//! - The `IndexMaintainerApi` trait defines the contract for KE-driven index
//!   maintenance. The concrete `IndexMaintainer` lives in `aikoql-scheduler`
//!   (HLD: engines around the kernel).
//! - `find_similar` routes through `IndexCoordinator`, which orchestrates
//!   hybrid recall across `VectorIndex` / `TextIndex` traits.
//! - Lightweight exact implementations live here (BruteForce, TokenText).
//!   Heavy ANN/BM25 implementations live in `aikoql-vector` (HNSW, Tantivy)
//!   and are injected via the same traits — following the HLD engine pattern.

use crate::knowledge::kom::*;
use crate::knowledge::scoring::{cosine, jaccard};
use crate::transaction::kernel::Kernel;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

pub mod coordinator;

pub use coordinator::IndexCoordinator;

// ---------------------------------------------------------------------------
// IndexMaintainerApi — kernel-side contract for background index maintenance
// ---------------------------------------------------------------------------

/// Minimal trait for the coordinator to interact with a background index
/// maintainer. The concrete implementation lives in `aikoql-scheduler`;
/// the kernel only knows this interface.
pub trait IndexMaintainerApi: Send + Sync {
    /// Events committed but not yet applied to the indexes.
    fn lag(&self, kernel: &Kernel) -> KResult<u64>;
    /// Access the vector index for similarity search.
    fn vectors(&self) -> &Arc<dyn VectorIndex>;
    /// Access the text index for full-text search.
    fn text(&self) -> &Arc<dyn TextIndex>;
    /// P4-M7 (TDD-INDEX-001): lag semantics — applied/target seqs, lag,
    /// status, last_error. The exact-vs-eventual choice lives at the
    /// coordinator (`search` = eventual via the indexes when attached;
    /// `search_exact` = committed state, zero lag).
    fn status(&self, kernel: &Kernel) -> KResult<IndexStatus> {
        let lag = self.lag(kernel)?;
        let (head, _) = kernel.journal_head()?;
        Ok(IndexStatus {
            applied_event_seq: head.saturating_sub(lag),
            target_event_seq: head,
            lag,
            status: if lag == 0 {
                IndexStatusKind::CaughtUp
            } else {
                IndexStatusKind::Syncing
            },
            last_error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// IndexStatus — lag semantics shared by every index (TDD-INDEX-001)
// ---------------------------------------------------------------------------

/// P4-M7 (TDD-INDEX-001). The maintainer applies vector + text from the one
/// event stream in ONE `apply` pass, so both indexes share one water — this
/// struct IS the whole per-index status (fake per-index waters would lie).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStatus {
    /// Last event seq applied to the indexes.
    pub applied_event_seq: u64,
    /// Journal head — the seq a caught-up index would be at.
    pub target_event_seq: u64,
    /// `target - applied`: events committed but not yet applied.
    pub lag: u64,
    pub status: IndexStatusKind,
    /// The last apply failure (cleared by a successful apply).
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexStatusKind {
    /// lag > 0, no error — applying committed events.
    Syncing,
    /// lag == 0, no error.
    CaughtUp,
    /// The last apply failed; retried on the next event.
    Error,
}

// ---------------------------------------------------------------------------
// VectorIndex trait
// ---------------------------------------------------------------------------

/// Pluggable vector index for ANN or exact nearest-neighbor search.
/// Kernel defines the contract; engine crates provide implementations.
pub trait VectorIndex: Send + Sync {
    fn upsert(&self, koid: KOID, model: &str, vec: &[f32]);
    fn remove(&self, koid: &KOID);
    /// Cosine-similarity ranking, descending, deterministic tie-break by KOID.
    /// When `model` is `Some`, only vectors from that embedding model are
    /// considered; `None` searches all models (backward-compatible).
    fn search(&self, qv: &[f32], k: usize, model: Option<&str>) -> Vec<(KOID, f32)>;
    fn len(&self) -> usize;
    fn checkpoint(&self, _dir: &std::path::Path) -> KResult<()> {
        Ok(())
    }
    /// P4-M7 (TDD-VECTOR-002): health metrics. `None` for indexes without a
    /// tombstone/physical split (the brute-force reference).
    fn health(&self) -> Option<VectorHealth> {
        None
    }
    /// P4-M7 (TDD-VECTOR-002): rebuild ONCE when the dead ratio crossed the
    /// threshold. No-op for non-tombstone indexes. Returns whether a
    /// rebuild ran. Called after maintenance applies (never inside `remove`
    /// itself — a delete must stay O(1)).
    fn maybe_rebuild(&self) -> bool {
        false
    }
}

/// P4-M7 (TDD-VECTOR-002) — tombstones vs physical nodes. `dead_ratio` is
/// `tombstones / physical` (0 when `physical` is 0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorHealth {
    /// Live (koid, model) pairs the index answers for.
    pub live: usize,
    /// Nodes physically present in the graph (removes only tombstone).
    pub physical: usize,
    pub tombstones: usize,
    pub dead_ratio: f64,
}

// ---------------------------------------------------------------------------
// BruteForceVectorIndex — exact in-memory reference (parity oracle)
// ---------------------------------------------------------------------------

pub struct BruteForceVectorIndex {
    /// Keyed by (KOID, model) so the same KO with different embedding models
    /// are independent vectors (R7 — model-namespaced partitioning).
    inner: RwLock<BTreeMap<(KOID, String), Vec<f32>>>,
}

impl BruteForceVectorIndex {
    pub fn new() -> Self {
        BruteForceVectorIndex {
            inner: RwLock::new(BTreeMap::new()),
        }
    }
}

impl Default for BruteForceVectorIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorIndex for BruteForceVectorIndex {
    fn upsert(&self, koid: KOID, model: &str, vec: &[f32]) {
        // justified: RwLock poison is unrecoverable
        self.inner
            .write()
            .unwrap()
            .insert((koid, model.to_string()), vec.to_vec());
    }

    fn remove(&self, koid: &KOID) {
        // justified: RwLock poison is unrecoverable
        self.inner.write().unwrap().retain(|(k, _), _| k != koid);
    }

    fn search(&self, qv: &[f32], k: usize, model: Option<&str>) -> Vec<(KOID, f32)> {
        // justified: RwLock poison is unrecoverable
        let map = self.inner.read().unwrap();
        let mut scored: Vec<(KOID, f32)> = map
            .iter()
            .filter(|((_, m), _)| model.is_none_or(|f| m == f))
            .map(|((id, _), v)| (*id, cosine(qv, v)))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                // justified: NaN (zero-vector cosine) ties deterministically
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        scored
    }

    fn len(&self) -> usize {
        // justified: RwLock poison is unrecoverable
        self.inner.read().unwrap().len()
    }
}

// ---------------------------------------------------------------------------
// TextIndex trait
// ---------------------------------------------------------------------------

/// Pluggable full-text index. Kernel defines the contract; engine crates
/// provide implementations (TokenText for exact, Tantivy for BM25).
///
/// R4: `upsert`/`remove`/`search` return KResult — tantivy writes/reads can
/// fail (disk full, permission denied); callers must propagate, not panic.
pub trait TextIndex: Send + Sync {
    fn upsert(&self, koid: KOID, tokens: &BTreeSet<String>) -> KResult<()>;
    fn remove(&self, koid: &KOID) -> KResult<()>;
    fn search(&self, tokens: &BTreeSet<String>, k: usize) -> KResult<Vec<(KOID, f32)>>;
    fn len(&self) -> usize;
    fn checkpoint(&self, _dir: &std::path::Path) -> KResult<()> {
        Ok(())
    }
    /// P4-M7 (TDD-VECTOR-001): batch upsert — answers == per-item answers.
    /// Default loops per item; Tantivy overrides to commit ONCE per batch.
    fn upsert_many(&self, items: &[(KOID, BTreeSet<String>)]) -> KResult<()> {
        for (koid, tokens) in items {
            self.upsert(*koid, tokens)?;
        }
        Ok(())
    }
    /// P4-M7 (TDD-VECTOR-001): batch remove — answers == per-item removals.
    fn remove_many(&self, koids: &[KOID]) -> KResult<()> {
        for koid in koids {
            self.remove(koid)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TokenTextIndex — exact inverted-index reference (parity oracle)
// ---------------------------------------------------------------------------

pub struct TokenTextIndex {
    docs: RwLock<BTreeMap<KOID, BTreeSet<String>>>,
    inv: RwLock<BTreeMap<String, BTreeSet<KOID>>>,
}

impl TokenTextIndex {
    pub fn new() -> Self {
        TokenTextIndex {
            docs: RwLock::new(BTreeMap::new()),
            inv: RwLock::new(BTreeMap::new()),
        }
    }
}

impl Default for TokenTextIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl TextIndex for TokenTextIndex {
    fn upsert(&self, koid: KOID, tokens: &BTreeSet<String>) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        let mut docs = self.docs.write().unwrap();
        let mut inv = self.inv.write().unwrap();
        if let Some(old) = docs.get(&koid) {
            for t in old.clone() {
                if let Some(set) = inv.get_mut(&t) {
                    set.remove(&koid);
                }
            }
        }
        for t in tokens {
            inv.entry(t.clone()).or_default().insert(koid);
        }
        docs.insert(koid, tokens.clone());
        Ok(())
    }

    fn remove(&self, koid: &KOID) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        let mut docs = self.docs.write().unwrap();
        let mut inv = self.inv.write().unwrap();
        if let Some(old) = docs.remove(koid) {
            for t in old {
                if let Some(set) = inv.get_mut(&t) {
                    set.remove(koid);
                }
            }
        }
        Ok(())
    }

    fn search(&self, tokens: &BTreeSet<String>, k: usize) -> KResult<Vec<(KOID, f32)>> {
        // justified: RwLock poison is unrecoverable
        let docs = self.docs.read().unwrap();
        let inv = self.inv.read().unwrap();
        let mut cands: BTreeSet<KOID> = BTreeSet::new();
        for t in tokens {
            if let Some(set) = inv.get(t) {
                cands.extend(set.iter().copied());
            }
        }
        let mut scored: Vec<(KOID, f32)> = cands
            .into_iter()
            .map(|id| {
                // justified: KOID may vanish between the two lock reads
                // (concurrent remove); an empty token set scores 0
                let d = docs.get(&id).cloned().unwrap_or_default();
                (id, jaccard(tokens, &d))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                // justified: NaN (zero-vector cosine) ties deterministically
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        Ok(scored)
    }

    fn len(&self) -> usize {
        // justified: RwLock poison is unrecoverable
        self.docs.read().unwrap().len()
    }
}

// ---------------------------------------------------------------------------
// Tests (lightweight impls only; HNSW/Tantivy tests live in aikoql-vector)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::store::MemoryEngine;
    use crate::transaction::kernel::{
        Fusion, ManualClock, RememberRequest, SimilarityQuery, Subject,
    };

    fn kid(n: u8) -> KOID {
        KOID([n; KOID_LEN])
    }

    // --- P4-M7 (TDD-INDEX-001) — idx001: the unambiguous exact-vs-eventual
    // choice. RED: `search_exact` does not exist yet (and the fake
    // maintainer below will not build once `status()` lands on the trait).
    // ---

    /// A maintainer whose indexes deliberately lag the kernel: it has
    /// indexed only `a`'s vector, and reports lag 1.
    struct FakeMaintainer {
        vectors: Arc<dyn VectorIndex>,
        text: Arc<dyn TextIndex>,
    }

    impl IndexMaintainerApi for FakeMaintainer {
        fn lag(&self, _kernel: &Kernel) -> KResult<u64> {
            Ok(1)
        }
        fn vectors(&self) -> &Arc<dyn VectorIndex> {
            &self.vectors
        }
        fn text(&self) -> &Arc<dyn TextIndex> {
            &self.text
        }
    }

    fn embed(k: &Kernel, subj: &Subject, vec: Vec<f32>) -> KOID {
        k.remember(RememberRequest {
            context: subj.clone().into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: PropertyMap::new(),
            semantic: Some(SemanticBlock {
                embedding_model: Some("m".into()),
                embedding: Some(vec),
                confidence: None,
                source: None,
                summary: None,
            }),
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

    /// A lagging maintainer scores from stale index data; the exact path
    /// scores committed state. The choice must be visible at the call site.
    #[test]
    fn coordinator_exact_choice_is_unambiguous_against_a_lagging_maintainer() {
        let clock = Arc::new(ManualClock::new(5_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 1).unwrap();
        let alice = Subject::new("alice");
        let a = embed(&k, &alice, vec![0.0, 1.0]);
        let b = embed(&k, &alice, vec![1.0, 0.0]);

        // The maintainer holds a STALE copy of a's vector ([1,1] instead of
        // the committed [0,1]) and has never indexed b.
        let vectors: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
        vectors.upsert(a, "m", &[1.0, 1.0]);
        let fake = Arc::new(FakeMaintainer {
            vectors,
            text: Arc::new(TokenTextIndex::new()),
        });
        let coord = IndexCoordinator::with_maintainer(fake);
        let q = || {
            SimilarityQuery::new(alice.clone(), 1, Fusion::VectorOnly).with_vector(vec![0.9, 0.1])
        };

        let eventual = coord.search(&k, q()).unwrap();
        assert_eq!(
            eventual[0].ko.koid, a,
            "the lagging index scores only its stale copy — eventual answers may lag"
        );
        assert_eq!(
            eventual[0].index_lag_ms, 1,
            "the lag is surfaced on the result"
        );

        let exact = coord.search_exact(&k, q()).unwrap();
        assert_eq!(
            exact[0].ko.koid, b,
            "the exact path scores committed state and wins"
        );
        assert_eq!(exact[0].index_lag_ms, 0, "the exact path consults no index");
    }

    /// With no maintainer attached, exact and eventual are the same path.
    #[test]
    fn exact_matches_search_when_no_maintainer_is_attached() {
        let clock = Arc::new(ManualClock::new(5_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 1).unwrap();
        let alice = Subject::new("alice");
        let a = embed(&k, &alice, vec![0.0, 1.0]);
        let b = embed(&k, &alice, vec![1.0, 0.0]);
        assert_ne!(a, b);
        let coord = IndexCoordinator::new();
        let q = || {
            SimilarityQuery::new(alice.clone(), 2, Fusion::VectorOnly).with_vector(vec![0.9, 0.1])
        };
        let via_search = coord.search(&k, q()).unwrap();
        let via_exact = coord.search_exact(&k, q()).unwrap();
        assert_eq!(via_search.len(), via_exact.len());
        for (s, e) in via_search.iter().zip(&via_exact) {
            assert_eq!(s.ko.koid, e.ko.koid);
            assert_eq!(s.score, e.score);
            assert_eq!(s.index_lag_ms, 0);
            assert_eq!(e.index_lag_ms, 0);
        }
    }

    #[test]
    fn brute_force_vector_orders_and_removes() {
        let idx = BruteForceVectorIndex::new();
        let a = kid(1);
        let b = kid(2);
        let c = kid(3);
        idx.upsert(a, "m", &[1.0, 0.0]);
        idx.upsert(b, "m", &[0.9, 0.1]);
        idx.upsert(c, "n", &[0.0, 1.0]); // different model
        let r = idx.search(&[1.0, 0.0], 2, None);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, a);
        assert_eq!(r[1].0, b);
        // Model filter: only model "m"
        let filtered = idx.search(&[1.0, 0.0], 5, Some("m"));
        assert_eq!(filtered.len(), 2);
        // Model filter: only model "n"
        let n_only = idx.search(&[0.0, 1.0], 5, Some("n"));
        assert_eq!(n_only.len(), 1);
        assert_eq!(n_only[0].0, c);
        idx.remove(&a);
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.search(&[1.0, 0.0], 1, None)[0].0, b);
    }

    #[test]
    fn model_namespaced_partitioning() {
        // R7: same KOID with different models are independent vectors.
        let idx = BruteForceVectorIndex::new();
        let a = kid(1);
        idx.upsert(a, "bge-m3", &[1.0, 0.0, 0.0]);
        idx.upsert(a, "text-embed-3", &[0.0, 1.0, 0.0]);
        idx.upsert(a, "bge-m3", &[0.9, 0.1, 0.0]); // overwrite bge-m3 entry
        assert_eq!(idx.len(), 2); // two distinct (koid, model) pairs
                                  // Search without model filter: returns the KOID once (best score per KOID).
        let all = idx.search(&[1.0, 0.0, 0.0], 10, None);
        assert!(!all.is_empty());
        // Search with model filter.
        let bge = idx.search(&[1.0, 0.0, 0.0], 10, Some("bge-m3"));
        assert_eq!(bge.len(), 1);
        assert_eq!(bge[0].0, a);
        let te3 = idx.search(&[0.0, 1.0, 0.0], 10, Some("text-embed-3"));
        assert_eq!(te3.len(), 1);
        assert_eq!(te3[0].0, a);
        // Remove removes all model entries for the KOID.
        idx.remove(&a);
        assert_eq!(idx.len(), 0);
        assert!(idx.search(&[1.0, 0.0, 0.0], 1, None).is_empty());
    }

    #[test]
    fn token_text_index_jaccard_and_remove() {
        let idx = TokenTextIndex::new();
        let a = kid(1);
        let b = kid(2);
        idx.upsert(a, &BTreeSet::from(["cats".to_string(), "dogs".to_string()]))
            .unwrap();
        idx.upsert(b, &BTreeSet::from(["birds".to_string()]))
            .unwrap();
        let r = idx
            .search(&BTreeSet::from(["cats".to_string()]), 5)
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, a);
        idx.remove(&a).unwrap();
        assert!(idx
            .search(&BTreeSet::from(["cats".to_string()]), 5)
            .unwrap()
            .is_empty());
    }
}
