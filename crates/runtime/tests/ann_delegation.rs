//! P5-M18 (ann003) — RED: the IR AnnSearch op must consult the kernel's
//! index coordinator. Today the IR branch scores every scanned object inline
//! (brute force) and never reads the indexes — the MCP vector path stays
//! O(store) while the SDK's `find_similar` is candidate-driven. The
//! materialization counter is the observable: only the coordinator path
//! bumps it, so a plan that executes without it fails this pin.

use std::sync::Arc;

use aikoql_kernel::index::{IndexMaintainerApi, TextIndex, VectorIndex};
use aikoql_kernel::ir::{IrOp, IrPlan};
use aikoql_kernel::transaction::kernel::Kernel;
use aikoql_kernel::{
    BruteForceVectorIndex, KResult, ManualClock, MemoryEngine, Metadata, RememberRequest,
    SemanticBlock, Subject, TokenTextIndex, KOID,
};
use aikoql_runtime::{Interpreter, RowSet};

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

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn create_vec(k: &Kernel, type_name: &str, model: &str, emb: Vec<f32>) -> KOID {
    let mut req = RememberRequest::create(Subject::new("alice"), meta(type_name));
    req.semantic = Some(SemanticBlock {
        embedding_model: Some(model.into()),
        embedding: Some(emb),
        confidence: None,
        source: None,
        summary: None,
    });
    k.remember(req).unwrap().koid
}

#[test]
fn ann003_ir_annsearch_consults_the_coordinator() {
    let clock = Arc::new(ManualClock::new(20_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0x1D4).unwrap();
    let a = create_vec(&k, "fact", "m", vec![0.0, 1.0]);
    let b = create_vec(&k, "fact", "m", vec![1.0, 0.0]);
    // Both have a HIGHER committed cosine than B — the wrong type must be
    // excluded by the Scan's type scope, the wrong model by the model scope.
    create_vec(&k, "other", "m", vec![0.99, 0.02]);
    create_vec(&k, "fact", "n", vec![0.99, 0.02]);

    let vectors: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
    vectors.upsert(a, "m", &[0.0, 1.0]);
    vectors.upsert(b, "m", &[1.0, 0.0]);
    let fake = Arc::new(FakeMaintainer {
        vectors,
        text: Arc::new(TokenTextIndex::new()),
    });
    k.attach_indexes(fake.clone());
    // P5-M18: the coordinator holds the maintainer weakly — the owner keeps it.
    let _owner = fake;

    let plan = IrPlan::new(vec![
        IrOp::Scan {
            type_name: "fact".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        },
        IrOp::AnnSearch {
            vector: vec![0.9, 0.1],
            query_text: None,
            embedding_model: Some("m".into()),
            k: 1,
        },
    ]);

    let before = k.similarity_materializations();
    let result = Interpreter::execute(&k, &plan).unwrap();
    let scored = match result {
        RowSet::Scored(s) => s,
        other => panic!("expected Scored from AnnSearch, got {other:?}"),
    };
    let consumed = k.similarity_materializations() - before;
    assert!(
        consumed > 0,
        "AnnSearch must consult the coordinator — the IR branch is brute-force \
         and never reads the indexes (materializations consumed: {consumed})"
    );
    assert_eq!(scored.len(), 1, "k=1 over two in-scope candidates");
    assert_eq!(scored[0].0, b, "committed cosine ranks B first");
    assert!((scored[0].1 - 0.9939).abs() < 1e-3, "score {}", scored[0].1);
}

/// The delegation must not change the answer when NO maintainer is attached:
/// the exact coordinator path scores committed state — same top-k as the old
/// brute force, same scores.
#[test]
fn ann003_delegation_without_maintainer_keeps_committed_ranking() {
    let clock = Arc::new(ManualClock::new(20_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0x1D4).unwrap();
    create_vec(&k, "fact", "m", vec![0.0, 1.0]);
    create_vec(&k, "fact", "m", vec![1.0, 0.0]);

    let plan = IrPlan::new(vec![
        IrOp::Scan {
            type_name: "fact".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        },
        IrOp::AnnSearch {
            vector: vec![0.9, 0.1],
            query_text: None,
            embedding_model: Some("m".into()),
            k: 2,
        },
    ]);
    let result = Interpreter::execute(&k, &plan).unwrap();
    let scored = match result {
        RowSet::Scored(s) => s,
        other => panic!("expected Scored from AnnSearch, got {other:?}"),
    };
    assert_eq!(scored.len(), 2, "both in-scope objects rank");
    assert!(
        (scored[0].1 - 0.9939).abs() < 1e-3,
        "B first with its committed cosine, got {}",
        scored[0].1
    );
    assert!(
        (scored[1].1 - 0.1104).abs() < 1e-3,
        "A second with its committed cosine, got {}",
        scored[1].1
    );
}
