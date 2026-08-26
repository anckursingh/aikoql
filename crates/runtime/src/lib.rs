//! Aikoql Runtime — physical-plan interpreter for Knowledge IR.
//!
//! Executes `IrPlan` operators against the Knowledge Kernel. v1 is a
//! tree-walking interpreter over a linear pipeline; bytecode compilation
//! and parallel execution land post-1.0 (per Architecture Review R11).
//!
//! MRFC-0005 §Runtime Layer: executes plans, schedules operators,
//! coordinates with the kernel. The interpreter is the runtime.

use aikoql_kernel::ir::*;
use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::knowledge::scoring::{cosine, jaccard, ko_text, tokenize};
use aikoql_kernel::transaction::kernel::{Kernel, KnowledgeContext, Subject};
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

/// EXE-006: apply LIMIT/OFFSET over a deterministic row order — skip
/// `offset` rows, then keep at most `limit`. Preserves relative order, so
/// pages concatenated in query order reconstruct the unpaged rowset.
fn skip_take<T>(v: Vec<T>, offset: usize, limit: usize) -> Vec<T> {
    v.into_iter().skip(offset).take(limit).collect()
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

/// Physical-plan interpreter with state for hybrid search fusion.
///
/// Not stateless — caches the last `Objects` and `Scored` row sets so that
/// `Fuse` can combine the preceding two search results (AnnSearch + TextSearch)
/// in a linear pipeline.
pub struct Interpreter {
    /// Cached objects from the most recent Scan/Filter — reused when a search
    /// op receives Scored input instead of Objects (hybrid pipeline).
    cached_objects: Option<Vec<KnowledgeObject>>,
    /// Subject from the most recent Scan, for BM25 delegation to find_similar.
    cached_subject: Option<Subject>,
    /// The previous scored result, stored for Fuse to combine with the current.
    prev_scored: Option<Vec<(KOID, f32, String, u64)>>,
    /// v0.3 K2: temporal plans own their time semantics (AS_OF/BETWEEN/
    /// HISTORICAL), so the Scan arm skips its default "valid now" filter.
    temporal_mode: bool,
}

impl Interpreter {
    /// Execute a plan. Returns the final `RowSet`.
    pub fn execute(kernel: &Kernel, plan: &IrPlan) -> KResult<RowSet> {
        let mut interp = Interpreter {
            cached_objects: None,
            cached_subject: None,
            prev_scored: None,
            temporal_mode: plan
                .operators
                .iter()
                .any(|op| matches!(op, IrOp::Temporal { .. })),
        };
        let mut rows = RowSet::Objects(Vec::new());
        for op in &plan.operators {
            rows = interp.exec_op(kernel, op, rows)?;
        }
        Ok(rows)
    }

    /// Resolve input to objects: if Scored, use cached objects; otherwise use as-is.
    fn resolve_objects(&self, input: &RowSet) -> KResult<Vec<KnowledgeObject>> {
        match input {
            RowSet::Objects(kos) => Ok(kos.clone()),
            RowSet::Scored(_) => self
                .cached_objects
                .clone()
                .ok_or_else(|| KError::InvalidQuery("no cached objects for search op".into())),
            _ => Err(KError::InvalidQuery(
                "search op requires Objects or Scored input".into(),
            )),
        }
    }

    fn exec_op(&mut self, kernel: &Kernel, op: &IrOp, input: RowSet) -> KResult<RowSet> {
        match op {
            IrOp::Scan {
                type_name,
                subject,
                roles,
                tenant,
            } => {
                // R9: rebuild the full Subject — roles and tenant scope from
                // the planner's hints, not just the bare name.
                let subj = Subject::with_roles(
                    subject,
                    &roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                );
                let subj = match tenant {
                    Some(t) => subj.in_tenant(t),
                    None => subj,
                };
                let mut kos = kernel.scan_by_type(&subj, type_name)?;
                // v0.3 K2: default MATCH answers with current truth — facts
                // not valid at "now" stay out of relational results. Temporal
                // plans (AS_OF/BETWEEN/HISTORICAL) handle time themselves.
                if !self.temporal_mode {
                    let now = kernel.clock_now();
                    kos.retain(|ko| ko.valid_at(now));
                }
                self.cached_objects = Some(kos.clone());
                self.cached_subject = Some(subj);
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
                                PredOp::Gt => {
                                    compare_values(val, Some(&p.value))
                                        == Some(std::cmp::Ordering::Greater)
                                }
                                PredOp::Lt => {
                                    compare_values(val, Some(&p.value))
                                        == Some(std::cmp::Ordering::Less)
                                }
                                PredOp::Gte => matches!(
                                    compare_values(val, Some(&p.value)),
                                    Some(std::cmp::Ordering::Greater)
                                        | Some(std::cmp::Ordering::Equal)
                                ),
                                PredOp::Lte => matches!(
                                    compare_values(val, Some(&p.value)),
                                    Some(std::cmp::Ordering::Less)
                                        | Some(std::cmp::Ordering::Equal)
                                ),
                            }
                        })
                    })
                    .collect();
                self.cached_objects = Some(filtered.clone());
                Ok(RowSet::Objects(filtered))
            }
            IrOp::Traverse {
                start_koid,
                rel_type,
                depth,
            } => {
                let start_koids: Vec<KOID> = if start_koid.is_empty() {
                    match &input {
                        RowSet::Objects(kos) => kos.iter().map(|ko| ko.koid).collect(),
                        _ => {
                            return Err(KError::InvalidQuery(
                                "set-based Traverse requires Object input from Scan".into(),
                            ))
                        }
                    }
                } else {
                    vec![KOID::from_hex(start_koid)
                        .map_err(|e| KError::InvalidObject(format!("invalid koid: {}", e)))?]
                };

                // MVP-KO-003: an edge target whose KO is gone (Erase) or
                // tombstoned (Delete) is a dangling endpoint — traversal must
                // not expose it as a live relationship result.
                // ponytail: per-edge get; join against a liveness snapshot if
                // deep traversals become a perf hotspot.
                let endpoint_live = |target: &KOID| -> bool {
                    let ctx = self
                        .cached_subject
                        .clone()
                        .map(KnowledgeContext::new)
                        .unwrap_or_else(|| KnowledgeContext::new(Subject::new("system")));
                    matches!(
                        kernel.get(ctx, target),
                        Ok(ko) if ko.lifecycle.state != LifecycleState::Deleted
                    )
                };

                let mut results = Vec::new();
                let mut visited = std::collections::HashSet::new();
                let mut queue: std::collections::VecDeque<(KOID, usize)> =
                    std::collections::VecDeque::new();

                for start in &start_koids {
                    if !visited.insert(*start) {
                        continue;
                    }
                    if let Ok(edges) = kernel.outbound_edges(start, rel_type.as_deref()) {
                        for (rt, target) in &edges {
                            if visited.insert(*target) && endpoint_live(target) {
                                results.push((*target, rt.clone(), 1usize));
                                if *depth > 1 {
                                    queue.push_back((*target, 1));
                                }
                            }
                        }
                    }
                }

                while let Some((cur, d)) = queue.pop_front() {
                    if d >= *depth {
                        continue;
                    }
                    if let Ok(next_edges) = kernel.outbound_edges(&cur, rel_type.as_deref()) {
                        for (rt, target) in next_edges {
                            if visited.insert(target) && endpoint_live(&target) {
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
                query_text,
                embedding_model,
                k,
            } => {
                // Generate embedding from query_text if no explicit vector.
                let vector = if vector.is_empty() {
                    match query_text.as_deref() {
                        Some(qt) => match kernel.embed_text(qt, embedding_model.as_deref()) {
                            Ok(v) => v,
                            // Graceful degrade: no provider or error -> Jaccard.
                            Err(_) => return self.exec_text_search(kernel, qt, k, &input),
                        },
                        None => {
                            return Err(KError::InvalidQuery(
                                "AnnSearch requires vector or query_text".into(),
                            ));
                        }
                    }
                } else {
                    vector.clone()
                };
                // Shared cosine-scoring path (embedded + explicit vectors).
                let kos = self.resolve_objects(&input)?;
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
                        Some((
                            ko.koid,
                            cosine(&vector, emb),
                            ko.metadata.type_name.clone(),
                            ko.version,
                        ))
                    })
                    .collect();
                scored.sort_by(|a, b| {
                    b.1.partial_cmp(&a.1)
                        // justified: NaN score ties deterministically
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.0.cmp(&b.0))
                });
                scored.truncate(*k);
                self.prev_scored = Some(scored.clone());
                Ok(RowSet::Scored(scored))
            }
            IrOp::TextSearch { query, k, scoring } => {
                // BM25 path: delegate to kernel's IndexCoordinator when available.
                if scoring.as_deref() == Some("bm25") {
                    if let Some(ref subj) = self.cached_subject {
                        if let Ok(scored) = kernel.type_scoped_text_search(subj, query, *k) {
                            if !scored.is_empty() {
                                self.prev_scored = Some(scored.clone());
                                return Ok(RowSet::Scored(scored));
                            }
                        }
                    }
                    // Fall through to Jaccard on empty result or error.
                }
                self.exec_text_search(kernel, query, k, &input)
            }
            IrOp::Fuse { mode } => {
                let current = match &input {
                    RowSet::Scored(s) => s.clone(),
                    _ => return Ok(input), // nothing to fuse, pass through
                };
                let prev = match self.prev_scored.take() {
                    Some(p) => p,
                    None => return Ok(RowSet::Scored(current)), // no previous, pass through
                };
                let fused = Self::fuse_scored(&prev, &current, mode);
                Ok(RowSet::Scored(fused))
            }
            IrOp::Temporal { op } => {
                let kos = match input {
                    RowSet::Objects(kos) => kos,
                    _ => {
                        return Err(KError::InvalidQuery(
                            "Temporal requires Object input".into(),
                        ))
                    }
                };
                let subj = self.cached_subject.clone().ok_or_else(|| {
                    KError::InvalidQuery("Temporal requires a Scan subject".into())
                })?;
                let out = match op {
                    // Transaction-time reconstruction: the version committed
                    // at/before `at`. Rows that did not exist yet (or were
                    // already tombstones) are dropped.
                    TemporalOp::AsOf(at) => {
                        let mut out = Vec::new();
                        for ko in &kos {
                            if let Some(v) = kernel.get_as_of(&subj, &ko.koid, *at)? {
                                if v.lifecycle.state != LifecycleState::Deleted {
                                    out.push(v);
                                }
                            }
                        }
                        out
                    }
                    // Valid-time overlap with [from, to): half-open. None
                    // bounds are unbounded (None valid_from = -inf, None
                    // valid_to = +inf) — `0` is NOT the semantic representation
                    // of the unbounded past (review P0-2).
                    TemporalOp::Between { from, to } => kos
                        .into_iter()
                        .filter(|ko| {
                            ko.valid_from().map(|vf| vf < *to).unwrap_or(true)
                                && ko.valid_to().map(|t| t > *from).unwrap_or(true)
                        })
                        .collect(),
                    // Historical reconstruction: every committed version of
                    // every scanned KOID, ascending commit order.
                    TemporalOp::Historical => {
                        let mut out = Vec::new();
                        for ko in &kos {
                            for (_ts, v) in kernel.history(&subj, &ko.koid)? {
                                out.push(v);
                            }
                        }
                        out
                    }
                };
                self.cached_objects = Some(out.clone());
                Ok(RowSet::Objects(out))
            }
            IrOp::EpistemicFilter { allowed } => {
                let kos = match input {
                    RowSet::Objects(kos) => kos,
                    _ => {
                        return Err(KError::InvalidQuery(
                            "EpistemicFilter requires Object input".into(),
                        ))
                    }
                };
                let out: Vec<KnowledgeObject> = kos
                    .into_iter()
                    .filter(|ko| allowed.iter().any(|s| s == ko.epistemic_status().as_str()))
                    .collect();
                self.cached_objects = Some(out.clone());
                Ok(RowSet::Objects(out))
            }
            IrOp::ProvenanceFilter { source } => {
                let kos = match input {
                    RowSet::Objects(kos) => kos,
                    _ => {
                        return Err(KError::InvalidQuery(
                            "ProvenanceFilter requires Object input".into(),
                        ))
                    }
                };
                let out: Vec<KnowledgeObject> = kos
                    .into_iter()
                    .filter(|ko| ko.evidence().iter().any(|e| e.source_artifact == *source))
                    .collect();
                self.cached_objects = Some(out.clone());
                Ok(RowSet::Objects(out))
            }
            IrOp::Limit { limit, offset } => {
                // EXE-006: trims whatever rowset shape arrives (Objects,
                // Scored, Traversal) — pagination is shape-agnostic.
                Ok(match input {
                    RowSet::Objects(kos) => RowSet::Objects(skip_take(kos, *offset, *limit)),
                    RowSet::Scored(s) => RowSet::Scored(skip_take(s, *offset, *limit)),
                    RowSet::Traversal(t) => RowSet::Traversal(skip_take(t, *offset, *limit)),
                })
            }
            IrOp::Project { fields } => {
                let mut kos = match input {
                    RowSet::Objects(kos) => kos,
                    _ => return Err(KError::InvalidQuery("Project requires Object input".into())),
                };
                if fields.contains(&"*".to_string()) {
                    return Ok(RowSet::Objects(kos));
                }
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

    /// Inline Jaccard text search over the input objects.
    fn exec_text_search(
        &mut self,
        _kernel: &Kernel,
        query: &str,
        k: &usize,
        input: &RowSet,
    ) -> KResult<RowSet> {
        let kos = self.resolve_objects(input)?;
        let q_tokens = tokenize(query);
        let mut scored: Vec<(KOID, f32, String, u64)> = kos
            .iter()
            .map(|ko| {
                let doc_tokens = tokenize(&ko_text(ko));
                (
                    ko.koid,
                    jaccard(&q_tokens, &doc_tokens),
                    ko.metadata.type_name.clone(),
                    ko.version,
                )
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                // justified: NaN score ties deterministically
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(*k);
        self.prev_scored = Some(scored.clone());
        Ok(RowSet::Scored(scored))
    }

    /// RRF or weighted fusion of two scored result sets.
    fn fuse_scored(
        a: &[(KOID, f32, String, u64)],
        b: &[(KOID, f32, String, u64)],
        mode: &FuseMode,
    ) -> Vec<(KOID, f32, String, u64)> {
        match mode {
            FuseMode::VectorOnly => {
                let mut out = a.to_vec();
                // justified: NaN score ties deterministically
                out.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(Ordering::Equal));
                out
            }
            FuseMode::TextOnly => {
                let mut out = b.to_vec();
                // justified: NaN score ties deterministically
                out.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(Ordering::Equal));
                out
            }
            FuseMode::Weighted { wv, wt } => {
                let bmap: std::collections::BTreeMap<KOID, f32> =
                    b.iter().map(|(koid, s, ..)| (*koid, *s)).collect();
                let mut merged: Vec<(KOID, f32, String, u64)> = a
                    .iter()
                    .map(|(koid, sv, tn, ver)| {
                        let tb = bmap.get(koid).copied().unwrap_or(0.0);
                        (*koid, wv * sv + wt * tb, tn.clone(), *ver)
                    })
                    .collect();
                // Include entries only in b.
                let a_set: std::collections::BTreeSet<KOID> =
                    a.iter().map(|(koid, ..)| *koid).collect();
                for (koid, sb, tn, ver) in b {
                    if !a_set.contains(koid) {
                        merged.push((*koid, *wt * sb, tn.clone(), *ver));
                    }
                }
                merged.sort_by(|x, y| {
                    y.1.partial_cmp(&x.1)
                        // justified: NaN score ties deterministically
                        .unwrap_or(Ordering::Equal)
                        .then_with(|| x.0.cmp(&y.0))
                });
                merged
            }
            FuseMode::Rrf { k0 } => {
                let k0f = *k0 as f32;
                // Build rank maps (1-indexed ranks, only entries with score > 0).
                let rank = |scored: &[(KOID, f32, String, u64)]| -> std::collections::BTreeMap<KOID, usize> {
                    let mut r = std::collections::BTreeMap::new();
                    for (i, (koid, s, ..)) in scored.iter().enumerate() {
                        if *s > 0.0 {
                            r.entry(*koid).or_insert(i + 1);
                        }
                    }
                    r
                };
                let ra = rank(a);
                let rb = rank(b);
                let all_koids: std::collections::BTreeSet<KOID> =
                    ra.keys().chain(rb.keys()).copied().collect();
                let mut merged: Vec<(KOID, f32, String, u64)> = all_koids
                    .into_iter()
                    .map(|koid| {
                        let mut rrf = 0.0f32;
                        if let Some(ra_val) = ra.get(&koid) {
                            rrf += 1.0 / (k0f + 1.0 + *ra_val as f32);
                        }
                        if let Some(rb_val) = rb.get(&koid) {
                            rrf += 1.0 / (k0f + 1.0 + *rb_val as f32);
                        }
                        // Carry type_name/version from whichever set has it.
                        let tn = a
                            .iter()
                            .find(|(k, ..)| *k == koid)
                            .map(|(_, _, tn, _)| tn.clone())
                            .or_else(|| {
                                b.iter()
                                    .find(|(k, ..)| *k == koid)
                                    .map(|(_, _, tn, _)| tn.clone())
                            })
                            // justified: koid is drawn from a/b union keys — fallback is dead code
                            .unwrap_or_default();
                        let ver = a
                            .iter()
                            .find(|(k, ..)| *k == koid)
                            .map(|(_, _, _, v)| *v)
                            .or_else(|| b.iter().find(|(k, ..)| *k == koid).map(|(_, _, _, v)| *v))
                            .unwrap_or(0);
                        (koid, rrf, tn, ver)
                    })
                    .collect();
                merged.sort_by(|x, y| {
                    y.1.partial_cmp(&x.1)
                        // justified: NaN score ties deterministically
                        .unwrap_or(Ordering::Equal)
                        .then_with(|| x.0.cmp(&y.0))
                });
                merged
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
                .thread_name("aikoql-runtime")
                .build()
                // justified: worker-thread allocation failure is unrecoverable;
                // no production callers construct Runtime today (Interpreter only)
                .expect("build tokio runtime"),
        }
    }

    /// Execute multiple plans in parallel. Each plan runs independently
    /// via `spawn_blocking`; results are collected in order.
    pub fn execute_all(&self, kernel: Arc<Kernel>, plans: &[IrPlan]) -> KResult<Vec<RowSet>> {
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
    use aikoql_kernel::{
        Clock, DeriveRequest, Evidence, EvidenceMethod, ManualClock, MemoryEngine, Metadata,
        RememberRequest, SemanticBlock,
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
                roles: vec![],
                tenant: None,
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
            IrOp::Scan {
                type_name: "fact".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::gt("temp", Value::Int(20))],
            },
        ]);
        let r = Interpreter::execute(&k, &plan).unwrap();
        assert_eq!(r.object_count(), 1);

        // Gte: temp >= 20 → 35, 20
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "fact".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::gte("temp", Value::Int(20))],
            },
        ]);
        let r = Interpreter::execute(&k, &plan).unwrap();
        assert_eq!(r.object_count(), 2);

        // Lt: temp < 20 → only 10
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "fact".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::lt("temp", Value::Int(20))],
            },
        ]);
        let r = Interpreter::execute(&k, &plan).unwrap();
        assert_eq!(r.object_count(), 1);

        // Lte: temp <= 20 → 10, 20
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "fact".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::lte("temp", Value::Int(20))],
            },
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
                roles: vec![],
                tenant: None,
            },
            IrOp::TextSearch {
                query: "cats".into(),
                k: 5,
                scoring: None,
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

        let plan1 = IrPlan::new(vec![IrOp::Scan {
            type_name: "note".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        }]);
        let plan2 = IrPlan::new(vec![IrOp::Scan {
            type_name: "fact".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        }]);

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

    // ---- R13: Fuse (RRF / Weighted) tests ----

    #[test]
    fn fuse_rrf_combines_two_scored_sets() {
        use aikoql_kernel::ir::FuseMode;
        let k1 = KOID::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1").unwrap();
        let k2 = KOID::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2").unwrap();
        let k3 = KOID::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa3").unwrap();
        let a = vec![
            (k1, 0.9, "note".into(), 1u64),
            (k2, 0.5, "note".into(), 1u64),
        ];
        let b = vec![
            (k2, 0.8, "note".into(), 1u64),
            (k3, 0.3, "note".into(), 1u64),
        ];
        let fused = Interpreter::fuse_scored(&a, &b, &FuseMode::Rrf { k0: 60 });
        // All three koids should appear, sorted by RRF score desc.
        assert_eq!(fused.len(), 3);
        // k2 appears in both lists → highest RRF score.
        assert_eq!(fused[0].0, k2);
        // Scores should be in descending order.
        assert!(fused[0].1 >= fused[1].1);
        assert!(fused[1].1 >= fused[2].1);
    }

    #[test]
    fn fuse_weighted_combines_scores() {
        use aikoql_kernel::ir::FuseMode;
        let k1 = KOID::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1").unwrap();
        let k2 = KOID::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2").unwrap();
        let a = vec![(k1, 1.0, "note".into(), 1u64)];
        let b = vec![(k2, 1.0, "note".into(), 1u64)];
        let fused = Interpreter::fuse_scored(&a, &b, &FuseMode::Weighted { wv: 0.7, wt: 0.3 });
        assert_eq!(fused.len(), 2);
        // k1: 0.7*1.0 + 0.3*0 = 0.7; k2: 0.7*0 + 0.3*1.0 = 0.3
        let k1_score = fused.iter().find(|(k, ..)| *k == k1).unwrap().1;
        let k2_score = fused.iter().find(|(k, ..)| *k == k2).unwrap().1;
        assert!((k1_score - 0.7).abs() < 0.001);
        assert!((k2_score - 0.3).abs() < 0.001);
    }

    #[test]
    fn fuse_vector_only_picks_first() {
        use aikoql_kernel::ir::FuseMode;
        let k1 = KOID::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1").unwrap();
        let k2 = KOID::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2").unwrap();
        let a = vec![(k1, 0.9, "note".into(), 1u64)];
        let b = vec![(k2, 0.8, "note".into(), 1u64)];
        let fused = Interpreter::fuse_scored(&a, &b, &FuseMode::VectorOnly);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].0, k1);
    }

    #[test]
    fn fuse_text_only_picks_second() {
        use aikoql_kernel::ir::FuseMode;
        let k1 = KOID::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1").unwrap();
        let k2 = KOID::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2").unwrap();
        let a = vec![(k1, 0.9, "note".into(), 1u64)];
        let b = vec![(k2, 0.8, "note".into(), 1u64)];
        let fused = Interpreter::fuse_scored(&a, &b, &FuseMode::TextOnly);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].0, k2);
    }

    #[test]
    fn ann_search_with_query_text_falls_back_to_text() {
        let k = mk();
        let alice = Subject::new("alice");

        let mut p1 = PropertyMap::new();
        p1.insert("body".into(), Value::Text("cats are great".into()));
        create_ko(&k, &alice, "note", p1, None);

        let mut p2 = PropertyMap::new();
        p2.insert("body".into(), Value::Text("unrelated fish".into()));
        create_ko(&k, &alice, "note", p2, None);

        // AnnSearch with empty vector + query_text → falls back to text search.
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "note".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::AnnSearch {
                vector: vec![],
                query_text: Some("cats".into()),
                embedding_model: None,
                k: 5,
            },
        ]);

        let result = Interpreter::execute(&k, &plan).unwrap();
        match result {
            RowSet::Scored(scored) => {
                assert!(!scored.is_empty());
                // "cats are great" should score higher.
                assert!(scored[0].1 > 0.0);
            }
            _ => panic!("expected Scored"),
        }
    }

    #[test]
    fn text_search_with_bm25_scoring_falls_back_to_jaccard() {
        let k = mk();
        let alice = Subject::new("alice");

        let mut p1 = PropertyMap::new();
        p1.insert("body".into(), Value::Text("machine learning basics".into()));
        create_ko(&k, &alice, "note", p1, None);

        // BM25 scoring without a maintainer → falls back to Jaccard.
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "note".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::TextSearch {
                query: "machine learning".into(),
                k: 5,
                scoring: Some("bm25".into()),
            },
        ]);

        let result = Interpreter::execute(&k, &plan).unwrap();
        match result {
            RowSet::Scored(scored) => {
                assert!(!scored.is_empty());
                assert!(scored[0].1 > 0.0);
            }
            _ => panic!("expected Scored"),
        }
    }

    #[test]
    fn hybrid_ann_then_text_then_fuse_pipeline() {
        let k = mk();
        let alice = Subject::new("alice");

        // Create a KO with both embedding and text content.
        let mut p1 = PropertyMap::new();
        p1.insert("body".into(), Value::Text("cats are wonderful".into()));
        let emb = vec![0.5; 128]; // dummy 128-dim embedding
        let sem = SemanticBlock {
            embedding: Some(emb.clone()),
            embedding_model: Some("test-model".into()),
            summary: Some("about cats".into()),
            confidence: None,
            source: None,
        };
        create_ko(&k, &alice, "note", p1, Some(sem));

        let mut p2 = PropertyMap::new();
        p2.insert("body".into(), Value::Text("unrelated fish".into()));
        let emb2 = vec![0.1; 128];
        let sem2 = SemanticBlock {
            embedding: Some(emb2.clone()),
            embedding_model: Some("test-model".into()),
            summary: Some("about fish".into()),
            confidence: None,
            source: None,
        };
        create_ko(&k, &alice, "note", p2, Some(sem2));

        // Hybrid plan: Scan → AnnSearch → TextSearch → Fuse
        let query_emb = vec![0.5; 128]; // matches "cats" doc
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "note".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::AnnSearch {
                vector: query_emb,
                query_text: None,
                embedding_model: Some("test-model".into()),
                k: 5,
            },
            IrOp::TextSearch {
                query: "cats".into(),
                k: 5,
                scoring: None,
            },
            IrOp::Fuse {
                mode: FuseMode::Rrf { k0: 60 },
            },
        ]);

        let result = Interpreter::execute(&k, &plan).unwrap();
        match result {
            RowSet::Scored(scored) => {
                assert!(!scored.is_empty(), "hybrid search should return results");
                // The "cats" document should be top-ranked (high vector + text score).
                // Verify the top result has a nonzero score.
                assert!(scored[0].1 > 0.0);
            }
            _ => panic!("expected Scored from hybrid pipeline"),
        }
    }

    #[test]
    fn ann_search_with_provider_uses_real_embedding() {
        use aikoql_semantic::provider::MockEmbeddingProvider;
        use std::sync::Arc;

        // Build kernel with a MockEmbeddingProvider wired in.
        let clock = Arc::new(ManualClock::new(20_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xCAFE)
            .unwrap()
            .with_embedding_provider(Arc::new(MockEmbeddingProvider::with_dim(3)));
        let alice = Subject::new("alice");

        // KO with an embedding similar to what MockEmbeddingProvider produces.
        let mut p1 = PropertyMap::new();
        p1.insert("body".into(), Value::Text("cats are great".into()));
        let sem1 = SemanticBlock {
            embedding: Some(vec![0.1, 0.1, 0.1]), // matches mock ([0.1; 3])
            embedding_model: None,
            summary: None,
            confidence: None,
            source: None,
        };
        create_ko(&k, &alice, "note", p1, Some(sem1));

        // KO with an opposite embedding.
        let mut p2 = PropertyMap::new();
        p2.insert("body".into(), Value::Text("unrelated fish".into()));
        let sem2 = SemanticBlock {
            embedding: Some(vec![-1.0, -1.0, -1.0]), // opposite direction
            embedding_model: None,
            summary: None,
            confidence: None,
            source: None,
        };
        create_ko(&k, &alice, "note", p2, Some(sem2));

        // AnnSearch with query_text → kernel.embed_text("cats") → [0.1; 3].
        // Cosine([0.1,0.1,0.1], [0.1,0.1,0.1]) > cosine([0.1,0.1,0.1], [-1,-1,-1]).
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "note".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::AnnSearch {
                vector: vec![],
                query_text: Some("cats".into()),
                embedding_model: None,
                k: 5,
            },
        ]);

        let result = Interpreter::execute(&k, &plan).unwrap();
        match result {
            RowSet::Scored(scored) => {
                assert_eq!(
                    scored.len(),
                    2,
                    "both KOs have embeddings, both should match"
                );
                // The "cats" KO (matching embedding) must rank higher.
                assert!(
                    scored[0].1 > scored[1].1,
                    "matching embedding should score higher than opposite: {:?}",
                    scored
                );
            }
            _ => panic!("expected Scored from AnnSearch with provider"),
        }
    }

    #[test]
    fn ann_search_without_provider_falls_back_to_jaccard() {
        // Same as ann_search_with_query_text_falls_back_to_text but making the
        // graceful-degrade contract explicit: when no EmbeddingProvider is
        // configured, AnnSearch with query_text uses Jaccard text search.
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
                roles: vec![],
                tenant: None,
            },
            IrOp::AnnSearch {
                vector: vec![],
                query_text: Some("cats".into()),
                embedding_model: None,
                k: 5,
            },
        ]);

        let result = Interpreter::execute(&k, &plan).unwrap();
        match result {
            RowSet::Scored(scored) => {
                assert!(!scored.is_empty());
                // "cats are great" should score higher via Jaccard.
                assert!(scored[0].1 > 0.0);
            }
            _ => panic!("expected Scored"),
        }
    }

    // ---- v0.3 K2: temporal + epistemic operators ----

    fn mk_with_clock() -> (Kernel, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(20_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0xCAFE).unwrap();
        (k, clock)
    }

    fn fact_with_validity(
        k: &Kernel,
        clock: &ManualClock,
        who: &str,
        prop: &str,
        v: i64,
        valid: Option<(u64, u64)>,
    ) -> KOID {
        let (from, to) = match valid {
            Some((f, t)) => (Some(f), Some(t)),
            None => (None, None),
        };
        fact_with_open_validity(k, clock, who, prop, v, from, to)
    }

    /// Fact with independently-optional bounds: None valid_from = -inf,
    /// None valid_to = +inf (never `0`-as-unbounded — review P0-2).
    /// `valid_to` is kernel-managed (review P0-1): the bound is closed by
    /// the privileged Superseded transition at the closing instant
    /// (`close_valid_time` collapses future starts to a zero-duration
    /// interval — the fixture only uses `from <= to`).
    fn fact_with_open_validity(
        k: &Kernel,
        clock: &ManualClock,
        who: &str,
        prop: &str,
        v: i64,
        from: Option<u64>,
        to: Option<u64>,
    ) -> KOID {
        let mut ext = ExtensionMap::new();
        if let Some(f) = from {
            ext.insert("valid_from".into(), Value::Int(f as i64));
        }
        let mut props = PropertyMap::new();
        props.insert(prop.into(), Value::Int(v));
        let id = k
            .remember(RememberRequest {
                context: Subject::new(who).into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: "fact".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: ext,
                origin: Origin::Human,
                note: None,
                referential_policy: ReferentialPolicy::default(),
            })
            .unwrap()
            .koid;
        if let Some(t) = to {
            // The close is stamped at instant `t`; restore the fixture clock
            // afterwards so `now` (default-scan filtering) stays untouched.
            let now = clock.millis();
            clock.set(t);
            k.admin_transition_epistemic(
                Subject::new(who),
                &id,
                EpistemicStatus::Superseded,
                Origin::System,
                None,
                None,
                Some("test fixture: close validity".into()),
            )
            .unwrap();
            clock.set(now);
        }
        id
    }

    fn update_val(k: &Kernel, who: &str, id: KOID, expected: u64, prop: &str, v: i64) {
        let mut props = PropertyMap::new();
        props.insert(prop.into(), Value::Int(v));
        k.remember(RememberRequest {
            context: Subject::new(who).into(),
            koid: Some(id),
            expected_version: Some(expected),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap();
    }

    fn scan_plan() -> IrPlan {
        IrPlan::new(vec![IrOp::Scan {
            type_name: "fact".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        }])
    }

    fn objects(result: RowSet) -> Vec<KnowledgeObject> {
        match result {
            RowSet::Objects(kos) => kos,
            other => panic!("expected Objects, got {:?}", other),
        }
    }

    #[test]
    fn default_match_excludes_facts_not_valid_now() {
        let (k, clock) = mk_with_clock(); // now = 20_000
        fact_with_validity(&k, &clock, "alice", "a", 1, None); // timeless: included
        fact_with_validity(&k, &clock, "alice", "b", 2, Some((30_000, 40_000))); // future: excluded
        fact_with_validity(&k, &clock, "alice", "c", 3, Some((0, 10_000))); // expired: excluded
        fact_with_validity(&k, &clock, "alice", "d", 4, Some((10_000, 30_000))); // valid now

        let kos = objects(Interpreter::execute(&k, &scan_plan()).unwrap());
        assert_eq!(kos.len(), 2, "timeless and currently-valid facts only");
        let vals: Vec<i64> = kos
            .iter()
            .map(
                |ko| match ko.properties.get("a").or(ko.properties.get("d")) {
                    Some(Value::Int(v)) => *v,
                    other => panic!("unexpected row: {:?}", other),
                },
            )
            .collect();
        assert_eq!(vals, vec![1, 4]);
    }

    #[test]
    fn as_of_reconstructs_committed_versions() {
        let (k, clock) = mk_with_clock();
        let id = fact_with_validity(&k, &clock, "alice", "a", 1, None); // v1 at 20_000
        clock.tick(10_000);
        update_val(&k, "alice", id, 1, "a", 2); // v2 at 30_000

        let as_of = |t: u64| {
            let mut plan = scan_plan();
            plan.operators.push(IrOp::Temporal {
                op: TemporalOp::AsOf(t),
            });
            objects(Interpreter::execute(&k, &plan).unwrap())
        };

        // Between the commits: v1.
        let mid = as_of(25_000);
        assert_eq!(mid.len(), 1);
        assert_eq!(mid[0].version, 1);
        assert_eq!(mid[0].properties.get("a"), Some(&Value::Int(1)));
        // After the second commit: v2.
        let late = as_of(35_000);
        assert_eq!(late[0].version, 2);
        // Before creation: nothing.
        assert!(as_of(5_000).is_empty());
    }

    #[test]
    fn between_uses_valid_time_overlap_semantics() {
        let (k, clock) = mk_with_clock();
        fact_with_validity(&k, &clock, "alice", "a", 1, Some((25_000, 35_000))); // overlaps first window
        fact_with_validity(&k, &clock, "alice", "b", 2, None); // timeless: any window
        fact_with_validity(&k, &clock, "alice", "c", 3, Some((0, 10_000))); // long expired

        let between = |from: u64, to: u64| {
            let mut plan = scan_plan();
            plan.operators.push(IrOp::Temporal {
                op: TemporalOp::Between { from, to },
            });
            objects(Interpreter::execute(&k, &plan).unwrap())
        };

        // [30_000, 40_000): a overlaps, timeless included, c excluded.
        let rows = between(30_000, 40_000);
        assert_eq!(rows.len(), 2);
        // [35_000, 40_000): half-open — a's valid_to == from, excluded.
        assert_eq!(between(35_000, 40_000).len(), 1);
        // [5_000, 10_000): a not yet valid; c overlaps (valid at 9_999) →
        // included with the timeless fact.
        assert_eq!(between(5_000, 10_000).len(), 2);
    }

    #[test]
    fn between_boundary_matrix_and_unbounded_sides() {
        // Review P0-2/P1-6: a fact valid on [1000, 2000) against the full
        // window matrix, with independently-unbounded sides.
        let (k, clock) = mk_with_clock();
        fact_with_validity(&k, &clock, "alice", "windowed", 1, Some((1_000, 2_000)));
        // valid on (-inf, 1000): valid_to only.
        fact_with_open_validity(&k, &clock, "alice", "past_only", 2, None, Some(1_000));
        // valid on [2000, +inf): valid_from only.
        fact_with_open_validity(&k, &clock, "alice", "future_only", 3, Some(2_000), None);
        // timeless: both bounds None.
        fact_with_validity(&k, &clock, "alice", "timeless", 4, None);

        let between = |from: u64, to: u64| {
            let mut plan = scan_plan();
            plan.operators.push(IrOp::Temporal {
                op: TemporalOp::Between { from, to },
            });
            let mut vals: Vec<i64> = objects(Interpreter::execute(&k, &plan).unwrap())
                .iter()
                .filter_map(|ko| match ko.properties.iter().next() {
                    Some((_, Value::Int(v))) => Some(*v),
                    _ => None,
                })
                .collect();
            vals.sort();
            vals
        };

        // [0, 1000): windowed [1000, 2000) only TOUCHES at 1000 — half-open,
        // so excluded; past_only overlaps; timeless included.
        assert_eq!(between(0, 1_000), vec![2, 4]);
        // [1000, 2000): the windowed fact's home window.
        assert_eq!(between(1_000, 2_000), vec![1, 4]);
        // [2000, 3000): windowed touches at 2000 — excluded; future_only
        // included.
        assert_eq!(between(2_000, 3_000), vec![3, 4]);
        // A window spanning everything sees all four facts.
        assert_eq!(between(0, 5_000), vec![1, 2, 3, 4]);
    }

    #[test]
    fn historical_enumerates_all_versions_ascending() {
        let (k, clock) = mk_with_clock();
        let id = fact_with_validity(&k, &clock, "alice", "a", 1, None);
        clock.tick(10_000);
        update_val(&k, "alice", id, 1, "a", 2);
        clock.tick(10_000);
        update_val(&k, "alice", id, 2, "a", 3);

        let mut plan = scan_plan();
        plan.operators.push(IrOp::Temporal {
            op: TemporalOp::Historical,
        });
        let kos = objects(Interpreter::execute(&k, &plan).unwrap());
        assert_eq!(kos.len(), 3, "one row per committed version");
        let versions: Vec<u64> = kos.iter().map(|ko| ko.version).collect();
        assert_eq!(versions, vec![1, 2, 3], "ascending commit order");
    }

    #[test]
    fn epistemic_filter_selects_by_status() {
        let (k, clock) = mk_with_clock();
        let id = fact_with_validity(&k, &clock, "alice", "a", 1, None);
        fact_with_validity(&k, &clock, "alice", "b", 2, None);
        k.admin_transition_epistemic(
            Subject::new("alice"),
            &id,
            EpistemicStatus::Verified,
            Origin::Human,
            None,
            None,
            None,
        )
        .unwrap();

        let mut plan = scan_plan();
        plan.operators.push(IrOp::EpistemicFilter {
            allowed: vec!["verified".into()],
        });
        let kos = objects(Interpreter::execute(&k, &plan).unwrap());
        assert_eq!(kos.len(), 1);
        assert_eq!(kos[0].epistemic_status(), EpistemicStatus::Verified);
    }

    #[test]
    fn provenance_filter_keeps_kos_by_source_artifact() {
        let (k, _clock) = mk_with_clock();
        let mut d1 = DeriveRequest::new(Subject::new("alice"), "fact");
        d1.evidence = vec![Evidence::new(
            "sec-filing.pdf",
            EvidenceMethod::DocExtraction,
        )];
        k.derive(d1).unwrap();
        let mut d2 = DeriveRequest::new(Subject::new("alice"), "fact");
        d2.evidence = vec![Evidence::new(
            "meeting-notes.md",
            EvidenceMethod::DocExtraction,
        )];
        k.derive(d2).unwrap();
        // a bare remember()d fact carries no evidence → always dropped
        fact_with_validity(&k, &_clock, "alice", "plain", 3, None);

        let run = |source: &str| {
            let mut plan = scan_plan();
            plan.operators.push(IrOp::ProvenanceFilter {
                source: source.into(),
            });
            objects(Interpreter::execute(&k, &plan).unwrap())
        };

        assert_eq!(run("sec-filing.pdf").len(), 1);
        assert_eq!(
            run("sec-filing.pdf")[0].evidence()[0].source_artifact,
            "sec-filing.pdf"
        );
        assert_eq!(run("meeting-notes.md").len(), 1);
        // exact match: prefix and absent artifacts yield nothing
        assert!(run("sec-filing").is_empty());
        assert!(run("nope.md").is_empty());
    }

    #[test]
    fn limit_offset_paginates_deterministic_order() {
        let (k, clock) = mk_with_clock();
        for v in 1..=5 {
            fact_with_validity(&k, &clock, "alice", "v", v, None);
        }

        let page = |limit: usize, offset: usize| {
            let mut plan = scan_plan();
            plan.operators.push(IrOp::Limit { limit, offset });
            objects(Interpreter::execute(&k, &plan).unwrap())
                .into_iter()
                .map(|ko| match ko.properties.get("v") {
                    Some(Value::Int(v)) => *v,
                    other => panic!("unexpected row: {:?}", other),
                })
                .collect::<Vec<i64>>()
        };

        let full = page(100, 0);
        assert_eq!(full.len(), 5, "unpaged sees everything");
        let p1 = page(2, 0);
        let p2 = page(2, 2);
        let p3 = page(2, 4);
        // pages are disjoint and their union reconstructs the full order
        let mut union = p1.clone();
        union.extend(p2.iter().chain(p3.iter()));
        assert_eq!(union, full, "pages union == full, same order");
        // boundary behavior
        assert!(page(0, 0).is_empty(), "LIMIT 0 is empty, not an error");
        assert!(page(10, 6).is_empty(), "offset past the end is empty");
        assert_eq!(page(3, 3).len(), 2, "last partial page");
    }
}
