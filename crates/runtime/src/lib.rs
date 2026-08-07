//! Mnemosyne Runtime — physical-plan interpreter for Knowledge IR.
//!
//! Executes `IrPlan` operators against the Knowledge Kernel. v1 is a
//! tree-walking interpreter over a linear pipeline; bytecode compilation
//! and parallel execution land post-1.0 (per Architecture Review R11).
//!
//! MRFC-0005 §Runtime Layer: executes plans, schedules operators,
//! coordinates with the kernel. The interpreter is the runtime.

use mnemosyne_kernel::ir::*;
use mnemosyne_kernel::knowledge::kom::*;
use mnemosyne_kernel::knowledge::scoring::{cosine, jaccard, ko_text, tokenize};
use mnemosyne_kernel::transaction::kernel::{Kernel, Subject};
use std::cmp::Ordering;

// ---------------------------------------------------------------------------
// Value comparison helper
// ---------------------------------------------------------------------------

/// Compare two `Value`s for range predicates. Returns `None` for
/// incomparable types (e.g., Text vs Int).
fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Option<Ordering> {
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        _ => return None,
    };
    match (a, b) {
        (Value::Int(ai), Value::Int(bi)) => ai.partial_cmp(bi),
        (Value::Float(af), Value::Float(bf)) => af.partial_cmp(bf),
        (Value::Text(at), Value::Text(bt)) => Some(at.cmp(bt)),
        (Value::Bool(ab), Value::Bool(bb)) => Some(ab.cmp(bb)),
        _ => None, // type mismatch
    }
}

// ---------------------------------------------------------------------------
// Intermediate result set
// ---------------------------------------------------------------------------

/// The result of executing one IR operator. Carries the data type to the
/// next operator in the pipeline.
#[derive(Clone, Debug)]
pub enum RowSet {
    /// Full KnowledgeObjects (from Scan, Filter).
    Objects(Vec<KnowledgeObject>),
    /// Scored results with metadata: (koid, score, type_name, version).
    Scored(Vec<(KOID, f32, String, u64)>),
    /// Traversal hits: (koid, rel_type, depth).
    Traversal(Vec<(KOID, String, usize)>),
}

impl RowSet {
    pub fn into_objects(self) -> KResult<Vec<KnowledgeObject>> {
        match self {
            RowSet::Objects(kos) => Ok(kos),
            _ => Err(KError::InvalidQuery("expected Objects row set".into())),
        }
    }

    pub fn into_scored(self) -> KResult<Vec<(KOID, f32, String, u64)>> {
        match self {
            RowSet::Scored(s) => Ok(s),
            _ => Err(KError::InvalidQuery("expected Scored row set".into())),
        }
    }

    pub fn object_count(&self) -> usize {
        match self {
            RowSet::Objects(kos) => kos.len(),
            _ => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Interpreter
// ---------------------------------------------------------------------------

/// Stateless physical-plan interpreter. Executes one `IrPlan` against a
/// `Kernel` and returns the final result set.
pub struct Interpreter;

impl Interpreter {
    /// Execute a plan. Returns the final `RowSet` — callers destructure it
    /// into the expected output format (JSON, protobuf, etc.).
    pub fn execute(kernel: &Kernel, plan: &IrPlan) -> KResult<RowSet> {
        let mut rows = RowSet::Objects(Vec::new());
        for op in &plan.operators {
            rows = Self::exec_op(kernel, op, rows)?;
        }
        Ok(rows)
    }

    fn exec_op(kernel: &Kernel, op: &IrOp, input: RowSet) -> KResult<RowSet> {
        match op {
            IrOp::Scan {
                type_name,
                subject,
            } => {
                let subj = Subject::new(subject);
                let kos = kernel.scan_by_type(&subj, type_name)?;
                Ok(RowSet::Objects(kos))
            }
            IrOp::Filter { predicates } => {
                let kos = match input {
                    RowSet::Objects(kos) => kos,
                    _ => return Err(KError::InvalidQuery("Filter requires Object input".into())),
                };
                let filtered: Vec<KnowledgeObject> = kos
                    .into_iter()
                    .filter(|ko| {
                        predicates.iter().all(|p| {
                            let val = ko.properties.get(&p.property);
                            match p.op {
                                PredOp::Eq => val == Some(&p.value),
                                PredOp::Neq => val != Some(&p.value),
                                PredOp::Gt => compare_values(val, Some(&p.value)) == Some(std::cmp::Ordering::Greater),
                                PredOp::Lt => compare_values(val, Some(&p.value)) == Some(std::cmp::Ordering::Less),
                                PredOp::Gte => matches!(compare_values(val, Some(&p.value)), Some(std::cmp::Ordering::Greater) | Some(std::cmp::Ordering::Equal)),
                                PredOp::Lte => matches!(compare_values(val, Some(&p.value)), Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal)),
                            }
                        })
                    })
                    .collect();
                Ok(RowSet::Objects(filtered))
            }
            IrOp::Traverse {
                start_koid,
                rel_type,
                depth,
            } => {
                let koid = KOID::from_hex(start_koid)
                    .map_err(|e| KError::InvalidObject(format!("invalid koid: {}", e)))?;
                let edges = kernel.outbound_edges(&koid, rel_type.as_deref())?;
                // BFS traversal.
                let mut results = Vec::new();
                let mut visited = std::collections::HashSet::new();
                let mut queue: std::collections::VecDeque<(KOID, usize)> =
                    std::collections::VecDeque::new();
                visited.insert(koid);
                // Collect depth-1 edges from start.
                for (rt, target) in &edges {
                    if visited.insert(*target) {
                        results.push((*target, rt.clone(), 1usize));
                        if *depth > 1 {
                            queue.push_back((*target, 1));
                        }
                    }
                }
                // BFS deeper levels via outbound_edges.
                while let Some((cur, d)) = queue.pop_front() {
                    if d >= *depth {
                        continue;
                    }
                    if let Ok(next_edges) = kernel.outbound_edges(&cur, rel_type.as_deref()) {
                        for (rt, target) in next_edges {
                            if visited.insert(target) {
                                results.push((target, rt.clone(), d + 1));
                                queue.push_back((target, d + 1));
                            }
                        }
                    }
                }
                Ok(RowSet::Traversal(results))
            }
            IrOp::AnnSearch {
                vector,
                embedding_model,
                k,
            } => {
                let kos = match &input {
                    RowSet::Objects(kos) => kos.clone(),
                    _ => return Err(KError::InvalidQuery("AnnSearch requires Object input".into())),
                };
                let model = embedding_model.as_deref();
                let mut scored: Vec<(KOID, f32, String, u64)> = kos
                    .iter()
                    .filter_map(|ko| {
                        let emb = ko.semantic.as_ref()?.embedding.as_ref()?;
                        if let Some(m) = model {
                            let ko_model = ko.semantic.as_ref()?.embedding_model.as_deref()?;
                            if ko_model != m {
                                return None;
                            }
                        }
                        Some((ko.koid, cosine(vector, emb), ko.metadata.type_name.clone(), ko.version))
                    })
                    .collect();
                scored.sort_by(|a, b| {
                    b.1.partial_cmp(&a.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.0.cmp(&b.0))
                });
                scored.truncate(*k);
                Ok(RowSet::Scored(scored))
            }
            IrOp::TextSearch { query, k } => {
                let kos = match &input {
                    RowSet::Objects(kos) => kos.clone(),
                    _ => return Err(KError::InvalidQuery("TextSearch requires Object input".into())),
                };
                let q_tokens = tokenize(query);
                let mut scored: Vec<(KOID, f32, String, u64)> = kos
                    .iter()
                    .map(|ko| {
                        let doc_tokens = tokenize(&ko_text(ko));
                        (ko.koid, jaccard(&q_tokens, &doc_tokens), ko.metadata.type_name.clone(), ko.version)
                    })
                    .collect();
                scored.sort_by(|a, b| {
                    b.1.partial_cmp(&a.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.0.cmp(&b.0))
                });
                scored.truncate(*k);
                Ok(RowSet::Scored(scored))
            }
            IrOp::Fuse { .. } => {
                // ponytail: Fuse consumes the last two Scored row sets from
                // preceding AnnSearch + TextSearch. Full multi-input Fuse
                // with explicit input wiring lands when DAG plans arrive.
                Ok(input)
            }
            IrOp::Project { fields } => {
                let mut kos = match input {
                    RowSet::Objects(kos) => kos,
                    _ => return Err(KError::InvalidQuery("Project requires Object input".into())),
                };
                if fields.contains(&"*".to_string()) {
                    return Ok(RowSet::Objects(kos));
                }
                // Filter properties to only the requested fields.
                for ko in &mut kos {
                    let mut filtered = PropertyMap::new();
                    for f in fields {
                        if let Some(v) = ko.properties.get(f) {
                            filtered.insert(f.clone(), v.clone());
                        }
                    }
                    ko.properties = filtered;
                }
                Ok(RowSet::Objects(kos))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime — tokio-backed worker pool for parallel plan execution
// ---------------------------------------------------------------------------

use std::sync::Arc;

/// Manages a tokio runtime for parallel IR plan execution.
pub struct Runtime {
    rt: tokio::runtime::Runtime,
}

impl Runtime {
    pub fn new() -> Self {
        Runtime {
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_name("mnemosyne-runtime")
                .build()
                .expect("build tokio runtime"),
        }
    }

    /// Execute multiple plans in parallel. Each plan runs independently
    /// via `spawn_blocking`; results are collected in order.
    pub fn execute_all(
        &self,
        kernel: Arc<Kernel>,
        plans: &[IrPlan],
    ) -> KResult<Vec<RowSet>> {
        let mut handles = Vec::new();
        for plan in plans.iter() {
            let k = kernel.clone();
            let p = plan.clone();
            handles.push(self.rt.spawn_blocking(move || Interpreter::execute(&k, &p)));
        }
        let mut results = Vec::with_capacity(handles.len());
        for h in handles {
            match self.rt.block_on(h) {
                Ok(Ok(rows)) => results.push(rows),
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(KError::Store("worker panicked".into())),
            }
        }
        Ok(results)
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mnemosyne_kernel::{
        ManualClock, MemoryEngine, Metadata, RememberRequest, SemanticBlock,
    };
    use std::sync::Arc;

    fn mk() -> Kernel {
        let clock = Arc::new(ManualClock::new(20_000));
        Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xCAFE).unwrap()
    }

    fn create_ko(
        k: &Kernel,
        subj: &Subject,
        type_name: &str,
        props: PropertyMap,
        semantic: Option<SemanticBlock>,
    ) -> KOID {
        k.remember(RememberRequest {
            context: subj.into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: type_name.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic,
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

    #[test]
    fn scan_and_filter_plan() {
        let k = mk();
        let alice = Subject::new("alice");

        let mut hot = PropertyMap::new();
        hot.insert("temp".into(), Value::Int(35));
        create_ko(&k, &alice, "fact", hot, None);

        let mut cold = PropertyMap::new();
        cold.insert("temp".into(), Value::Int(10));
        create_ko(&k, &alice, "fact", cold, None);

        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "fact".into(),
                subject: "alice".into(),
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("temp", Value::Int(35))],
            },
        ]);

        let result = Interpreter::execute(&k, &plan).unwrap();
        match result {
            RowSet::Objects(kos) => {
                assert_eq!(kos.len(), 1);
                assert_eq!(kos[0].properties.get("temp"), Some(&Value::Int(35)));
            }
            _ => panic!("expected Objects"),
        }
    }

    #[test]
    fn filter_range_predicates() {
        let k = mk();
        let alice = Subject::new("alice");

        let mut hot = PropertyMap::new();
        hot.insert("temp".into(), Value::Int(35));
        create_ko(&k, &alice, "fact", hot, None);

        let mut cold = PropertyMap::new();
        cold.insert("temp".into(), Value::Int(10));
        create_ko(&k, &alice, "fact", cold, None);

        let mut warm = PropertyMap::new();
        warm.insert("temp".into(), Value::Int(20));
        create_ko(&k, &alice, "fact", warm, None);

        // Gt: temp > 20 → only 35
        let plan = IrPlan::new(vec![
            IrOp::Scan { type_name: "fact".into(), subject: "alice".into() },
            IrOp::Filter { predicates: vec![Predicate::gt("temp", Value::Int(20))] },
        ]);
        let r = Interpreter::execute(&k, &plan).unwrap();
        assert_eq!(r.object_count(), 1);

        // Gte: temp >= 20 → 35, 20
        let plan = IrPlan::new(vec![
            IrOp::Scan { type_name: "fact".into(), subject: "alice".into() },
            IrOp::Filter { predicates: vec![Predicate::gte("temp", Value::Int(20))] },
        ]);
        let r = Interpreter::execute(&k, &plan).unwrap();
        assert_eq!(r.object_count(), 2);

        // Lt: temp < 20 → only 10
        let plan = IrPlan::new(vec![
            IrOp::Scan { type_name: "fact".into(), subject: "alice".into() },
            IrOp::Filter { predicates: vec![Predicate::lt("temp", Value::Int(20))] },
        ]);
        let r = Interpreter::execute(&k, &plan).unwrap();
        assert_eq!(r.object_count(), 1);

        // Lte: temp <= 20 → 10, 20
        let plan = IrPlan::new(vec![
            IrOp::Scan { type_name: "fact".into(), subject: "alice".into() },
            IrOp::Filter { predicates: vec![Predicate::lte("temp", Value::Int(20))] },
        ]);
        let r = Interpreter::execute(&k, &plan).unwrap();
        assert_eq!(r.object_count(), 2);
    }

    #[test]
    fn scan_and_text_search_plan() {
        let k = mk();
        let alice = Subject::new("alice");

        let mut p1 = PropertyMap::new();
        p1.insert("body".into(), Value::Text("cats are great".into()));
        create_ko(&k, &alice, "note", p1, None);

        let mut p2 = PropertyMap::new();
        p2.insert("body".into(), Value::Text("unrelated fish".into()));
        create_ko(&k, &alice, "note", p2, None);

        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "note".into(),
                subject: "alice".into(),
            },
            IrOp::TextSearch {
                query: "cats".into(),
                k: 5,
            },
        ]);

        let result = Interpreter::execute(&k, &plan).unwrap();
        match result {
            RowSet::Scored(scored) => {
                assert_eq!(scored.len(), 2);
                // "cats are great" should score higher than "unrelated fish"
                assert!(scored[0].1 > scored[1].1);
            }
            _ => panic!("expected Scored"),
        }
    }

    #[test]
    fn parallel_execution() {
        let k = Arc::new(mk());
        let alice = Subject::new("alice");

        // Create KOs for two different types.
        let mut p1 = PropertyMap::new();
        p1.insert("body".into(), Value::Text("cats".into()));
        create_ko(&k, &alice, "note", p1, None);

        let mut p2 = PropertyMap::new();
        p2.insert("body".into(), Value::Text("dogs".into()));
        create_ko(&k, &alice, "fact", p2, None);

        let plan1 = IrPlan::new(vec![IrOp::Scan { type_name: "note".into(), subject: "alice".into() }]);
        let plan2 = IrPlan::new(vec![IrOp::Scan { type_name: "fact".into(), subject: "alice".into() }]);

        let rt = Runtime::new();
        let results = rt.execute_all(k, &[plan1, plan2]).unwrap();
        assert_eq!(results.len(), 2);
        match &results[0] {
            RowSet::Objects(kos) => assert_eq!(kos.len(), 1),
            _ => panic!("expected Objects"),
        }
        match &results[1] {
            RowSet::Objects(kos) => assert_eq!(kos.len(), 1),
            _ => panic!("expected Objects"),
        }
    }
}
