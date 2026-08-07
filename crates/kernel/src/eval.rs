//! Memory Evals suite — recall, staleness, contradiction metrics as queries.
//!
//! These are not external benchmarks; they are queries over the store that
//! produce quantitative memory-quality signals agents can act on.

use crate::knowledge::kom::{KError, KResult, KOID};
use crate::transaction::kernel::{
    Fusion, Kernel, KnowledgeContext, PropertyFilter, SimilarityQuery, Subject,
};
use std::collections::HashSet;

/// Recall evaluation: how many of the expected KOIDs appear in the top-k
/// hybrid-recall results.
#[derive(Clone, Debug)]
pub struct EvalRecallQuery {
    pub context: KnowledgeContext,
    pub type_name: Option<String>,
    pub text: Option<String>,
    pub vector: Option<Vec<f32>>,
    pub k: usize,
    pub fusion: Fusion,
    pub expected: HashSet<KOID>,
}

impl Default for EvalRecallQuery {
    fn default() -> Self {
        EvalRecallQuery {
            context: KnowledgeContext::new(Subject::new("evaluator")),
            type_name: None,
            text: None,
            vector: None,
            k: 10,
            fusion: Fusion::Rrf { k0: 60 },
            expected: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EvalRecallReport {
    pub k: usize,
    pub returned: usize,
    pub expected: usize,
    pub hits: usize,
    pub recall: f32,
    pub missing: Vec<KOID>,
    pub mean_lag_ms: u64,
    pub max_lag_ms: u64,
    pub p95_lag_ms: u64,
}

/// Staleness evaluation: distribution of `index_lag_ms` reported by the
/// recall path. A lag of zero means results came from the exact path.
#[derive(Clone, Debug)]
pub struct EvalStalenessQuery {
    pub context: KnowledgeContext,
    pub type_name: Option<String>,
    pub text: Option<String>,
    pub vector: Option<Vec<f32>>,
    pub k: usize,
    pub fusion: Fusion,
}

impl Default for EvalStalenessQuery {
    fn default() -> Self {
        EvalStalenessQuery {
            context: KnowledgeContext::new(Subject::new("evaluator")),
            type_name: None,
            text: None,
            vector: None,
            k: 10,
            fusion: Fusion::Rrf { k0: 60 },
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EvalStalenessReport {
    pub results: usize,
    pub mean_lag_ms: u64,
    pub max_lag_ms: u64,
    pub p95_lag_ms: u64,
}

/// Contradiction evaluation: pairs of same-type objects whose embeddings are
/// very similar but whose value for `property` differs.
#[derive(Clone, Debug)]
pub struct EvalContradictionQuery {
    pub context: KnowledgeContext,
    pub type_name: String,
    pub property: String,
    pub similarity_threshold: f32,
    pub max_results: usize,
}

#[derive(Clone, Debug)]
pub struct Contradiction {
    pub left: KOID,
    pub right: KOID,
    pub score: f32,
    pub reason: String,
}

impl Kernel {
    pub fn eval_recall(&self, q: EvalRecallQuery) -> KResult<EvalRecallReport> {
        let k = q.k.max(1);
        let filter = q.type_name.map(|tn| PropertyFilter {
            type_name: Some(tn),
            required: vec![],
        });
        let results = self.find_similar(SimilarityQuery {
            context: q.context.clone(),
            filter,
            text: q.text,
            vector: q.vector,
            embedding_model: None,
            k,
            fusion: q.fusion,
        })?;
        let returned = results.len();
        let mut hits = 0usize;
        let mut missing: Vec<KOID> = Vec::new();
        for expected in &q.expected {
            if results.iter().any(|r| r.ko.koid == *expected) {
                hits += 1;
            } else {
                missing.push(*expected);
            }
        }
        let lags: Vec<u64> = results.iter().map(|r| r.index_lag_ms).collect();
        let recall = if q.expected.is_empty() {
            0.0
        } else {
            hits as f32 / q.expected.len() as f32
        };
        Ok(EvalRecallReport {
            k,
            returned,
            expected: q.expected.len(),
            hits,
            recall,
            missing,
            mean_lag_ms: mean(&lags),
            max_lag_ms: lags.iter().copied().max().unwrap_or(0),
            p95_lag_ms: percentile(&lags, 0.95),
        })
    }

    pub fn eval_staleness(&self, q: EvalStalenessQuery) -> KResult<EvalStalenessReport> {
        let k = q.k.max(1);
        let filter = q.type_name.map(|tn| PropertyFilter {
            type_name: Some(tn),
            required: vec![],
        });
        let results = self.find_similar(SimilarityQuery {
            context: q.context.clone(),
            filter,
            text: q.text,
            vector: q.vector,
            embedding_model: None,
            k,
            fusion: q.fusion,
        })?;
        let lags: Vec<u64> = results.iter().map(|r| r.index_lag_ms).collect();
        Ok(EvalStalenessReport {
            results: results.len(),
            mean_lag_ms: mean(&lags),
            max_lag_ms: lags.iter().copied().max().unwrap_or(0),
            p95_lag_ms: percentile(&lags, 0.95),
        })
    }

    /// Detect candidate contradictions among objects of a single type.
    ///
    /// ponytail: O(n²) exact scan; sufficient for evaluation corpora and small
    /// stores. Replace with an ANN contradiction index if this becomes hot.
    pub fn eval_contradictions(&self, q: EvalContradictionQuery) -> KResult<Vec<Contradiction>> {
        if q.similarity_threshold < 0.0 || q.similarity_threshold > 1.0 {
            return Err(KError::InvalidQuery(
                "similarity_threshold must be in [0,1]".into(),
            ));
        }
        let objects = self.accessible_objects(&q.context.subject, Some(&q.type_name))?;
        let mut out: Vec<Contradiction> = Vec::new();
        for i in 0..objects.len() {
            let a = &objects[i];
            let emb_a = match a.semantic.as_ref().and_then(|s| s.embedding.as_ref()) {
                Some(e) => e,
                None => continue,
            };
            for j in (i + 1)..objects.len() {
                let b = &objects[j];
                let emb_b = match b.semantic.as_ref().and_then(|s| s.embedding.as_ref()) {
                    Some(e) => e,
                    None => continue,
                };
                let score = cosine(emb_a, emb_b);
                if score < q.similarity_threshold {
                    continue;
                }
                let val_a = a.properties.get(&q.property);
                let val_b = b.properties.get(&q.property);
                let differs = match (val_a, val_b) {
                    (Some(va), Some(vb)) => va != vb,
                    _ => false,
                };
                if !differs {
                    continue;
                }
                out.push(Contradiction {
                    left: a.koid,
                    right: b.koid,
                    score,
                    reason: format!(
                        "property '{}' differs: {:?} vs {:?}",
                        q.property, val_a, val_b
                    ),
                });
            }
        }
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.truncate(q.max_results.max(1));
        Ok(out)
    }
}

fn mean(xs: &[u64]) -> u64 {
    if xs.is_empty() {
        return 0;
    }
    xs.iter().sum::<u64>() / xs.len() as u64
}

fn percentile(xs: &[u64], p: f32) -> u64 {
    if xs.is_empty() {
        return 0;
    }
    let mut sorted: Vec<u64> = xs.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() - 1) as f32 * p) as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::kom::{Metadata, Value};
    use crate::storage::store::MemoryEngine;
    use crate::transaction::kernel::{ManualClock, RememberRequest};
    use std::sync::Arc;

    fn meta(t: &str) -> Metadata {
        Metadata {
            type_name: t.into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        }
    }

    fn fact(k: &Kernel, body: &str, embedding: &[f32]) -> KOID {
        let mut req = RememberRequest::create(Subject::new("evaluator"), meta("fact"));
        req.properties
            .insert("body".into(), Value::Text(body.into()));
        req.semantic = Some(crate::knowledge::kom::SemanticBlock {
            embedding_model: Some("test".into()),
            embedding: Some(embedding.to_vec()),
            confidence: None,
            source: None,
            summary: None,
        });
        k.remember(req).unwrap().koid
    }

    #[test]
    fn recall_perfect_and_partial() {
        let clock = Arc::new(ManualClock::new(1_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 1).unwrap();
        let a = fact(&k, "alpha", &[1.0, 0.0]);
        let b = fact(&k, "beta", &[0.0, 1.0]);
        let c = fact(&k, "gamma", &[1.0, 1.0]);

        let q = EvalRecallQuery {
            context: KnowledgeContext::new(Subject::new("evaluator")),
            type_name: Some("fact".into()),
            text: Some("alpha".into()),
            k: 5,
            fusion: Fusion::TextOnly,
            expected: [a, b].iter().copied().collect(),
            ..Default::default()
        };
        let r = k.eval_recall(q).unwrap();
        assert_eq!(r.hits, 2);
        assert!((r.recall - 1.0).abs() < 1e-6);
        assert!(r.missing.is_empty());

        let q2 = EvalRecallQuery {
            context: KnowledgeContext::new(Subject::new("evaluator")),
            type_name: Some("fact".into()),
            text: Some("alpha".into()),
            k: 1,
            fusion: Fusion::TextOnly,
            expected: [a, c].iter().copied().collect(),
            ..Default::default()
        };
        let r2 = k.eval_recall(q2).unwrap();
        assert_eq!(r2.hits, 1);
        assert!((r2.recall - 0.5).abs() < 1e-6);
        assert_eq!(r2.missing.len(), 1);
    }

    #[test]
    fn staleness_exact_path_is_zero() {
        let clock = Arc::new(ManualClock::new(1_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 1).unwrap();
        fact(&k, "hello", &[1.0, 0.0]);
        let q = EvalStalenessQuery {
            context: KnowledgeContext::new(Subject::new("evaluator")),
            text: Some("hello".into()),
            k: 5,
            fusion: Fusion::TextOnly,
            ..Default::default()
        };
        let r = k.eval_staleness(q).unwrap();
        assert_eq!(r.max_lag_ms, 0);
        assert_eq!(r.mean_lag_ms, 0);
    }

    #[test]
    fn contradictions_detect_differing_property() {
        let clock = Arc::new(ManualClock::new(1_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 1).unwrap();
        let mut yes = RememberRequest::create(Subject::new("evaluator"), meta("claim"));
        yes.properties.insert("answer".into(), Value::Bool(true));
        yes.properties
            .insert("claim".into(), Value::Text("AGI is possible".into()));
        yes.semantic = Some(crate::knowledge::kom::SemanticBlock {
            embedding_model: Some("test".into()),
            embedding: Some(vec![1.0, 0.0]),
            confidence: None,
            source: None,
            summary: None,
        });
        let a = k.remember(yes).unwrap().koid;

        let mut no = RememberRequest::create(Subject::new("evaluator"), meta("claim"));
        no.properties.insert("answer".into(), Value::Bool(false));
        no.properties
            .insert("claim".into(), Value::Text("AGI is impossible".into()));
        no.semantic = Some(crate::knowledge::kom::SemanticBlock {
            embedding_model: Some("test".into()),
            embedding: Some(vec![0.99, 0.01]),
            confidence: None,
            source: None,
            summary: None,
        });
        let b = k.remember(no).unwrap().koid;

        let q = EvalContradictionQuery {
            context: KnowledgeContext::new(Subject::new("evaluator")),
            type_name: "claim".into(),
            property: "answer".into(),
            similarity_threshold: 0.9,
            max_results: 10,
        };
        let hits = k.eval_contradictions(q).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].score >= 0.9);
        assert!(
            (hits[0].left == a && hits[0].right == b) || (hits[0].left == b && hits[0].right == a)
        );
    }
}
