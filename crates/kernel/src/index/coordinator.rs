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
use std::sync::{Arc, Weak};

/// Similarity-search service. Holds an optional async index maintainer; when no
/// maintainer is attached it falls back to the exact inline path (same scoring,
/// zero lag). This keeps the kernel working out-of-the-box without background
/// threads while still allowing pluggable ANN/BM25 indexes.
#[derive(Default)]
pub struct IndexCoordinator {
    /// P5-M18: WEAK — a strong edge here closes kernel→maintainer→thread→
    /// kernel, a refcount cycle that keeps the store lock held after every
    /// kernel and maintainer drop. The host owns the strong Arc (SDK
    /// `Aikoql.maintainer`, MCP `MAINTAINER` static); a dead maintainer
    /// degrades to the exact path.
    maintainer: Option<Weak<dyn IndexMaintainerApi>>,
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
            maintainer: Some(Arc::downgrade(&maintainer)),
        })
    }

    /// Attach or replace the maintainer.
    pub fn attach(&mut self, maintainer: Arc<dyn IndexMaintainerApi>) {
        self.maintainer = Some(Arc::downgrade(&maintainer));
    }

    pub fn maintainer(&self) -> Option<&Weak<dyn IndexMaintainerApi>> {
        self.maintainer.as_ref()
    }

    /// Hybrid recall: vector cosine + text Jaccard, ACL/type/state filtered,
    /// with deterministic tie-breaking. EVENTUALLY CONSISTENT when a
    /// maintainer is attached — scores come from the async indexes and
    /// `index_lag_ms` surfaces how far they lag (P4-M7, TDD-INDEX-001).
    pub fn search(&self, kernel: &Kernel, q: SimilarityQuery) -> KResult<Vec<ScoredKO>> {
        self.search_inner(kernel, q, true)
    }

    /// P4-M7 (TDD-INDEX-001): the UNAMBIGUOUS exact choice — committed state
    /// only, zero lag, no index reads even when a maintainer is attached.
    /// Use when staleness is unacceptable; `search` is the eventual path.
    pub fn search_exact(&self, kernel: &Kernel, q: SimilarityQuery) -> KResult<Vec<ScoredKO>> {
        self.search_inner(kernel, q, false)
    }

    fn search_inner(
        &self,
        kernel: &Kernel,
        q: SimilarityQuery,
        use_indexes: bool,
    ) -> KResult<Vec<ScoredKO>> {
        if q.k == 0 {
            return Err(KError::InvalidQuery("k must be >= 1".into()));
        }
        let snap = q.context.snapshot.unwrap_or_else(|| kernel.snapshot());
        // R9: a type-scoped query walks the type index instead of all heads.
        // The per-KO type filter below stays — it guards stale index entries.
        // M18: computed lazily — the ANN path ranks the index's own
        // candidates and never needs this O(store) scan.
        let heads = || -> KResult<Vec<(KOID, u64, u64, LifecycleState)>> {
            Ok(
                match q.filter.as_ref().and_then(|f| f.type_name.as_deref()) {
                    Some(tn) => kernel
                        .heads_of_type(tn)?
                        .into_iter()
                        .map(|(koid, state)| (koid, 0, 0, state))
                        .collect(),
                    None => kernel.scan_heads()?,
                },
            )
        };
        let mut vec_scored: Vec<(KOID, f32)> = Vec::new();
        let mut txt_scored: Vec<(KOID, f32)> = Vec::new();
        let mut merged: Vec<ScoredKO> = Vec::new();

        let q_tokens = q.text.as_ref().map(|t| tokenize(t));

        // One upgrade per search; the rest of this function treats `m` as
        // `Option<Arc>` exactly as before the weak edge (P5-M18).
        let m = self.maintainer.as_ref().and_then(|w| w.upgrade());

        let lag = if use_indexes {
            match &m {
                Some(m) => m.lag(kernel)?,
                None => 0,
            }
        } else {
            0
        };
        let vmap: Option<BTreeMap<KOID, f32>> = if use_indexes {
            m.as_ref().and_then(|mm| {
                q.vector.as_ref().map(|qv| {
                    mm.vectors()
                        .search(qv, usize::MAX, q.embedding_model.as_deref())
                        .into_iter()
                        .collect()
                })
            })
        } else {
            None
        };
        let tmap: Option<BTreeMap<KOID, f32>> = if use_indexes {
            match (&m, &q_tokens) {
                // R4: text().search() returns KResult — propagate, don't swallow
                (Some(m), Some(t)) => Some(m.text().search(t, usize::MAX)?.into_iter().collect()),
                _ => None,
            }
        } else {
            None
        };

        // P5-M16 — the vector-only leg ranks slim scoring records instead of
        // materializing every head KO. The slim read decodes the same wire
        // blob through the same predecessor walk as the full read, so it can
        // never filter, authorize or score differently — only the top-k
        // survivors are materialized (in rank order, full-KO ACL re-check),
        // and the counter below makes that observable (vs_scan_002).
        // ponytail: ceiling — text-bearing fusions (text scoring needs
        // ko_text over the whole property map) and Exact keep the full loop
        // below; extend the slim read only if those fusions need the win.
        if q.text.is_none() && matches!(q.fusion, Fusion::VectorOnly) {
            let required: Vec<String> = q
                .filter
                .as_ref()
                .map(|f| f.required.iter().map(|(k, _)| k.clone()).collect())
                .unwrap_or_default();
            let mut ranked: Vec<(KOID, f32)> = Vec::new();
            // One guard chain for both legs; the score is always the
            // committed cosine from the slim embedding — the index only
            // nominates candidates, its own sim never becomes a published
            // score.
            let mut consider = |koid: &KOID, deleted: bool| -> KResult<()> {
                if deleted {
                    return Ok(());
                }
                let Some(rec) = kernel.object_scoring(koid, snap, &required)? else {
                    return Ok(());
                };
                if kernel
                    .check_access_parts(&q.context.subject, &rec, Action::Read)
                    .is_err()
                {
                    return Ok(()); // ACL-filtered, silently (no existence leak)
                }
                if let Some(f) = &q.filter {
                    if let Some(tn) = &f.type_name {
                        if rec.type_name != *tn {
                            return Ok(());
                        }
                    }
                    let mut ok = true;
                    for (k, v) in &f.required {
                        if rec.props.get(k) != Some(v) {
                            ok = false;
                            break;
                        }
                    }
                    if !ok {
                        return Ok(());
                    }
                }
                let vscore = match (&q.vector, &rec.embedding) {
                    (Some(qv), Some(emb)) => cosine(qv, emb),
                    _ => 0.0,
                };
                ranked.push((*koid, vscore));
                Ok(())
            };
            if let Some(vmap) = &vmap {
                // M18 (ann001): candidate-driven ranking. The ANN index is
                // capacity-capped, so rank ONLY the candidates it returned —
                // scoring every head with vmap.get(koid).unwrap_or(0.0)
                // hands non-candidates a 0.0 "hole" that outranks real
                // negative-cosine neighbors. The index lags deletes
                // (eventual): a tombstoned koid can still be a candidate,
                // and its head state is the authority.
                for koid in vmap.keys() {
                    let deleted = match kernel.head_object(koid)? {
                        Some(head) => head.lifecycle.state == LifecycleState::Deleted,
                        None => true, // head erased — not a live candidate
                    };
                    consider(koid, deleted)?;
                }
            } else {
                // Exact fallback: all heads, inline cosine (no index reads).
                for (koid, _version, _ts, state) in &heads()? {
                    consider(koid, *state == LifecycleState::Deleted)?;
                }
            }
            ranked.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            for (koid, score) in ranked {
                if merged.len() >= q.k {
                    break;
                }
                let Some(ko) = kernel.object_at(&koid, snap)? else {
                    continue;
                };
                kernel
                    .similarity_materializations
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // The full-KO re-check is the committed-bytes authority.
                if kernel
                    .check_access(&q.context.subject, &ko, Action::Read)
                    .is_err()
                {
                    continue;
                }
                if let Some(f) = &q.filter {
                    if let Some(tn) = &f.type_name {
                        if ko.metadata.type_name != *tn {
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
                merged.push(ScoredKO {
                    ko,
                    score,
                    index_lag_ms: lag,
                });
            }
            return Ok(merged);
        }

        for (koid, _version, _ts, state) in &heads()? {
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
