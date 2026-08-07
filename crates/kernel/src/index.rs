//! Index lifecycle machinery (MRFC-0009 semantics, draft).
//!
//! Architecture:
//! - Indexes are SECONDARY structures maintained asynchronously from the
//!   Knowledge Event stream — never written on the commit path (Determinism Law).
//! - The `IndexMaintainerApi` trait defines the contract for KE-driven index
//!   maintenance. The concrete `IndexMaintainer` lives in `mnemosyne-scheduler`
//!   (HLD: engines around the kernel).
//! - `find_similar` routes through `IndexCoordinator`, which orchestrates
//!   hybrid recall across `VectorIndex` / `TextIndex` traits.
//! - Lightweight exact implementations live here (BruteForce, TokenText).
//!   Heavy ANN/BM25 implementations live in `mnemosyne-vector` (HNSW, Tantivy)
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
/// maintainer. The concrete implementation lives in `mnemosyne-scheduler`;
/// the kernel only knows this interface.
pub trait IndexMaintainerApi: Send + Sync {
    /// Events committed but not yet applied to the indexes.
    fn lag(&self, kernel: &Kernel) -> KResult<u64>;
    /// Access the vector index for similarity search.
    fn vectors(&self) -> &Arc<dyn VectorIndex>;
    /// Access the text index for full-text search.
    fn text(&self) -> &Arc<dyn TextIndex>;
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
        self.inner
            .write()
            .unwrap()
            .insert((koid, model.to_string()), vec.to_vec());
    }

    fn remove(&self, koid: &KOID) {
        self.inner.write().unwrap().retain(|(k, _), _| k != koid);
    }

    fn search(&self, qv: &[f32], k: usize, model: Option<&str>) -> Vec<(KOID, f32)> {
        let map = self.inner.read().unwrap();
        let mut scored: Vec<(KOID, f32)> = map
            .iter()
            .filter(|((_, m), _)| model.map_or(true, |f| m == f))
            .map(|((id, _), v)| (*id, cosine(qv, v)))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        scored
    }

    fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }
}

// ---------------------------------------------------------------------------
// TextIndex trait
// ---------------------------------------------------------------------------

/// Pluggable full-text index. Kernel defines the contract; engine crates
/// provide implementations (TokenText for exact, Tantivy for BM25).
pub trait TextIndex: Send + Sync {
    fn upsert(&self, koid: KOID, tokens: &BTreeSet<String>);
    fn remove(&self, koid: &KOID);
    fn search(&self, tokens: &BTreeSet<String>, k: usize) -> Vec<(KOID, f32)>;
    fn len(&self) -> usize;
    fn checkpoint(&self, _dir: &std::path::Path) -> KResult<()> {
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
    fn upsert(&self, koid: KOID, tokens: &BTreeSet<String>) {
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
    }

    fn remove(&self, koid: &KOID) {
        let mut docs = self.docs.write().unwrap();
        let mut inv = self.inv.write().unwrap();
        if let Some(old) = docs.remove(koid) {
            for t in old {
                if let Some(set) = inv.get_mut(&t) {
                    set.remove(koid);
                }
            }
        }
    }

    fn search(&self, tokens: &BTreeSet<String>, k: usize) -> Vec<(KOID, f32)> {
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
                let d = docs.get(&id).cloned().unwrap_or_default();
                (id, jaccard(tokens, &d))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        scored
    }

    fn len(&self) -> usize {
        self.docs.read().unwrap().len()
    }
}

// ---------------------------------------------------------------------------
// Tests (lightweight impls only; HNSW/Tantivy tests live in mnemosyne-vector)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn kid(n: u8) -> KOID {
        KOID([n; KOID_LEN])
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
        idx.upsert(a, &BTreeSet::from(["cats".to_string(), "dogs".to_string()]));
        idx.upsert(b, &BTreeSet::from(["birds".to_string()]));
        let r = idx.search(&BTreeSet::from(["cats".to_string()]), 5);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, a);
        idx.remove(&a);
        assert!(idx
            .search(&BTreeSet::from(["cats".to_string()]), 5)
            .is_empty());
    }
}
