//! AG-20a: vector recall through the real HNSW index — warm p50 and
//! recall@10 against an exact (brute-force cosine) oracle, plus how many
//! candidates the coordinator asked the index for.
//!
//!   AIKOQL_BENCH_SCALE=100000 AIKOQL_BENCH_DIM=64 \
//!     cargo bench -p aikoql-benchmarks --bench ann_recall
//!
//! Prints one JSON line. Machine-dependent: compare runs on one machine.

use aikoql_kernel::index::{IndexMaintainerApi, TextIndex, TokenTextIndex, VectorIndex};
use aikoql_kernel::transaction::kernel::{Fusion, KnowledgeContext, SimilarityQuery};
use aikoql_kernel::*;
use aikoql_vector::HnswVectorIndex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

const K: usize = 10;
const QUERIES: usize = 50;

/// The real HNSW, counting the candidates each search asks for.
struct Counted {
    inner: HnswVectorIndex,
    asked: AtomicUsize,
}

impl VectorIndex for Counted {
    fn upsert(&self, koid: KOID, model: &str, vec: &[f32]) {
        self.inner.upsert(koid, model, vec)
    }
    fn remove(&self, koid: &KOID) {
        self.inner.remove(koid)
    }
    fn search(&self, qv: &[f32], k: usize, model: Option<&str>) -> Vec<(KOID, f32)> {
        self.asked.fetch_add(k.min(1 << 40), Ordering::Relaxed);
        self.inner.search(qv, k, model)
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
}

struct Indexes {
    vectors: Arc<dyn VectorIndex>,
    text: Arc<dyn TextIndex>,
}

impl IndexMaintainerApi for Indexes {
    fn lag(&self, _kernel: &Kernel) -> KResult<u64> {
        Ok(0)
    }
    fn vectors(&self) -> &Arc<dyn VectorIndex> {
        &self.vectors
    }
    fn text(&self) -> &Arc<dyn TextIndex> {
        &self.text
    }
}

/// Seeded xorshift — vectors on the unit sphere, reproducible across runs.
fn unit(seed: &mut u64, dim: usize) -> Vec<f32> {
    let mut v: Vec<f32> = (0..dim)
        .map(|_| {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            (*seed >> 11) as f32 / (1u64 << 53) as f32 - 0.5
        })
        .collect();
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter_mut().for_each(|x| *x /= n);
    v
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let (n, dim) = (
        env("AIKOQL_BENCH_SCALE", 100_000),
        env("AIKOQL_BENCH_DIM", 64),
    );
    let kernel = Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(20_000)),
        0xC0FFEE,
    )
    .unwrap();
    let alice = KnowledgeContext::new(Subject::new("alice"));
    let counted = Arc::new(Counted {
        inner: HnswVectorIndex::new(dim, n),
        asked: AtomicUsize::new(0),
    });
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut corpus: Vec<(KOID, Vec<f32>)> = Vec::with_capacity(n);
    let t = Instant::now();
    for _ in 0..n {
        let emb = unit(&mut seed, dim);
        let mut req = RememberRequest::create(
            alice.clone(),
            Metadata {
                type_name: "doc".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
        );
        req.semantic = Some(SemanticBlock {
            embedding_model: Some("bench".into()),
            embedding: Some(emb.clone()),
            confidence: None,
            source: None,
            summary: None,
        });
        let koid = kernel.remember(req).unwrap().koid;
        counted.upsert(koid, "bench", &emb);
        corpus.push((koid, emb));
    }
    let build_s = t.elapsed().as_secs_f64();
    let indexes = Arc::new(Indexes {
        vectors: counted.clone(),
        text: Arc::new(TokenTextIndex::new()),
    });
    kernel.attach_indexes(indexes.clone());

    let queries: Vec<Vec<f32>> = (0..QUERIES).map(|_| unit(&mut seed, dim)).collect();
    let run = |qv: &Vec<f32>| {
        kernel
            .find_similar(
                SimilarityQuery::new(alice.clone(), K, Fusion::VectorOnly).with_vector(qv.clone()),
            )
            .unwrap()
    };
    run(&queries[0]); // warm
    counted.asked.store(0, Ordering::Relaxed);
    let (mut ms, mut found) = (Vec::with_capacity(QUERIES), 0usize);
    for qv in &queries {
        let t = Instant::now();
        let hits = run(qv);
        ms.push(t.elapsed().as_secs_f64() * 1e3);
        let mut exact: Vec<(f32, KOID)> = corpus
            .iter()
            .map(|(id, e)| (e.iter().zip(qv).map(|(a, b)| a * b).sum(), *id))
            .collect();
        exact.sort_by(|a, b| b.0.total_cmp(&a.0));
        let truth: Vec<KOID> = exact.iter().take(K).map(|x| x.1).collect();
        found += hits.iter().filter(|h| truth.contains(&h.ko.koid)).count();
    }
    ms.sort_by(f64::total_cmp);
    println!(
        "{{\"cell\":\"ann_recall\",\"n\":{n},\"dim\":{dim},\"k\":{K},\"queries\":{QUERIES},\
         \"build_s\":{build_s:.1},\"p50_ms\":{:.3},\"p95_ms\":{:.3},\"recall_at_10\":{:.3},\
         \"candidates_per_query\":{}}}",
        ms[QUERIES / 2],
        ms[QUERIES * 95 / 100],
        found as f64 / (QUERIES * K) as f64,
        counted.asked.load(Ordering::Relaxed) / QUERIES
    );
    drop(indexes);
}
