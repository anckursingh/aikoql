//! Index Coordinator — similarity-search service.
//!
//! Owns the optional index maintainer (behind `IndexMaintainerApi`) and all
//! hybrid-recall scoring logic. The kernel delegates `find_similar` here so the
//! orchestrator does not embed index internals or scoring helpers.

use crate::index::IndexMaintainerApi;
use crate::knowledge::kom::{Action, KError, KResult, LifecycleState, KOID};
use crate::knowledge::scoring::{cosine, jaccard, ko_text, tokenize};
use crate::transaction::kernel::{Fusion, Kernel, ScoredKO, SimilarityQuery};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Similarity-search service. Holds an optional async index maintainer; when no
/// maintainer is attached it falls back to the exact inline path (same scoring,
/// zero lag). This keeps the kernel working out-of-the-box without background
/// threads while still allowing pluggable ANN/BM25 indexes.
#[derive(Default)]
pub struct IndexCoordinator {
    maintainer: Option<Arc<dyn IndexMaintainerApi>>,
}

impl IndexCoordinator {
    /// Exact-path coordinator: no background indexes, reads committed state
    /// directly via the kernel.
    pub fn new() -> Arc<Self> {
        Arc::new(Self { maintainer: None })
    }

    /// Coordinate over an existing async maintainer (live ANN/BM25 indexes).
    pub fn with_maintainer(maintainer: Arc<dyn IndexMaintainerApi>) -> Arc<Self> {
        Arc::new(Self {
            maintainer: Some(maintainer),
        })
    }

    /// Attach or replace the maintainer.
    pub fn attach(&mut self, maintainer: Arc<dyn IndexMaintainerApi>) {
        self.maintainer = Some(maintainer);
    }

    pub fn maintainer(&self) -> Option<&Arc<dyn IndexMaintainerApi>> {
        self.maintainer.as_ref()
    }

    /// Hybrid recall: vector cosine + text Jaccard, ACL/type/state filtered,
    /// with deterministic tie-breaking.
    pub fn search(&self, kernel: &Kernel, q: SimilarityQuery) -> KResult<Vec<ScoredKO>> {
        if q.k == 0 {
            return Err(KError::InvalidQuery("k must be >= 1".into()));
        }
        let snap = q.context.snapshot.unwrap_or_else(|| kernel.snapshot());
        // R9: a type-scoped query walks the type index instead of all heads.
        // The per-KO type filter below stays — it guards stale index entries.
        let heads: Vec<(KOID, u64, u64, LifecycleState)> =
            match q.filter.as_ref().and_then(|f| f.type_name.as_deref()) {
                Some(tn) => kernel
                    .heads_of_type(tn)?
                    .into_iter()
                    .map(|(koid, state)| (koid, 0, 0, state))
                    .collect(),
                None => kernel.scan_heads()?,
            };
        let mut vec_scored: Vec<(KOID, f32)> = Vec::new();
        let mut txt_scored: Vec<(KOID, f32)> = Vec::new();
        let mut merged: Vec<ScoredKO> = Vec::new();

        let q_tokens = q.text.as_ref().map(|t| tokenize(t));

        let lag = match &self.maintainer {
            Some(m) => m.lag(kernel)?,
            None => 0,
        };
        let vmap: Option<BTreeMap<KOID, f32>> = self.maintainer.as_ref().and_then(|m| {
            q.vector.as_ref().map(|qv| {
                m.vectors()
                    .search(qv, usize::MAX, q.embedding_model.as_deref())
                    .into_iter()
                    .collect()
            })
        });
        let tmap: Option<BTreeMap<KOID, f32>> = match (&self.maintainer, &q_tokens) {
            // R4: text().search() returns KResult — propagate, don't swallow
            (Some(m), Some(t)) => Some(m.text().search(t, usize::MAX)?.into_iter().collect()),
            _ => None,
        };

        for (koid, _version, _ts, state) in &heads {
            let ko = match kernel.object_at(koid, snap)? {
                Some(ko) => ko,
                None => continue,
            };
            if kernel
                .check_access(&q.context.subject, &ko, Action::Read)
                .is_err()
            {
                continue; // ACL-filtered, silently (no existence leak)
            }
            if *state == LifecycleState::Deleted {
                continue;
            }
            if let Some(f) = &q.filter {
                if let Some(tn) = &f.type_name {
                    if &ko.metadata.type_name != tn {
                        continue;
                    }
                }
                let mut ok = true;
                for (k, v) in &f.required {
                    if ko.properties.get(k) != Some(v) {
                        ok = false;
                        break;
                    }
                }
                if !ok {
                    continue;
                }
            }
            let mut vscore: f32 = 0.0;
            let mut tscore: f32 = 0.0;
            match &vmap {
                Some(m) => vscore = m.get(koid).copied().unwrap_or(0.0),
                None => {
                    if let Some(qv) = &q.vector {
                        if let Some(sem) = &ko.semantic {
                            if let Some(emb) = &sem.embedding {
                                vscore = cosine(qv, emb);
                            }
                        }
                    }
                }
            }
            match &tmap {
                Some(m) => tscore = m.get(koid).copied().unwrap_or(0.0),
                None => {
                    if let Some(tokens) = &q_tokens {
                        tscore = jaccard(tokens, &tokenize(&ko_text(&ko)));
                    }
                }
            }
            vec_scored.push((*koid, vscore));
            txt_scored.push((*koid, tscore));
            let score = match q.fusion {
                Fusion::VectorOnly => vscore,
                Fusion::TextOnly => tscore,
                Fusion::Weighted { wv, wt } => wv * vscore + wt * tscore,
                Fusion::Rrf { .. } => 0.0, // computed below
                Fusion::Exact => 0.0,      // bypasses index entirely, computed via direct scan
            };
            merged.push(ScoredKO {
                ko,
                score,
                index_lag_ms: lag,
            });
        }

        if let Fusion::Rrf { k0 } = q.fusion {
            vec_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            txt_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let rank_of = |list: &[(KOID, f32)], id: &KOID| -> Option<usize> {
                list.iter().position(|(k, s)| k == id && *s > 0.0)
            };
            for s in merged.iter_mut() {
                let mut rrf = 0.0f32;
                if let Some(r) = rank_of(&vec_scored, &s.ko.koid) {
                    rrf += 1.0 / (k0 as f32 + 1.0 + r as f32);
                }
                if let Some(r) = rank_of(&txt_scored, &s.ko.koid) {
                    rrf += 1.0 / (k0 as f32 + 1.0 + r as f32);
                }
                s.score = rrf;
            }
        }

        merged.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.ko.koid.cmp(&b.ko.koid))
        });
        merged.truncate(q.k);
        Ok(merged)
    }
}
