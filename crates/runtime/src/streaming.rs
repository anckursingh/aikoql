//! P5-M4 (ND-04) — streaming/batch execution.
//!
//! A pull-based pipeline (`PhysicalOperator::next_batch`) over the supported
//! streaming shape: Scan → Filter → Project → Limit. Backpressure is by
//! construction — a slow consumer stops pulling and no operator holds more
//! than one batch. The Scan pins the koid list at `open()` (snapshot at open:
//! rows created after open never appear) and resolves payload per batch via
//! the kernel's `scan_by_type_range` — the SAME read filters as the
//! materializing `scan_by_type` (ACL/Deleted/type re-check live in one place:
//! `Kernel::readable_object`). Anything outside the supported shape fails
//! closed; the materializing executor (`Interpreter::execute_physical`) keeps
//! handling full plans.
//!
//! Honest ledger: the koid list is O(N) index materialization (32 B/row);
//! streaming the index itself needs a cursor API in storage, deferred. The
//! gate-7 RSS cell lands with the W-suite sampler harness (st004).

use crate::row_matches;
use aikoql_kernel::ir::*;
use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::transaction::kernel::{Kernel, Subject};
use aikoql_kernel::{KError, KResult};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Cancellation (amendment 3)
// ---------------------------------------------------------------------------

/// Cooperative cancellation token: the operator checks it at every batch
/// boundary and fails with `KError::Cancelled` — a dropped scan leaves no
/// partial write.
#[derive(Clone)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        CancellationToken(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Identity comparison — for registries that must remove the exact
    /// token a query was registered with (clones share the same Arc).
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Operator trait (the roadmap's conceptual API, with KO batches as rows)
// ---------------------------------------------------------------------------

/// A streaming operator: `open` pins its source state, `next_batch` is
/// demand-driven (None = exhausted), `close` releases.
pub trait PhysicalOperator {
    fn open(&mut self) -> KResult<()>;
    fn next_batch(&mut self) -> KResult<Option<Vec<KnowledgeObject>>>;
    fn close(&mut self) -> KResult<()>;
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

/// Streams a type scan: the koid list is captured at `open()` (the snapshot
/// the runtime controls — rows created after open never appear), payload
/// batches resolve through the kernel's shared read filters.
pub struct ScanOperator<'a> {
    kernel: &'a Kernel,
    subject: Subject,
    type_name: String,
    batch_size: usize,
    cancel: CancellationToken,
    koids: Vec<KOID>,
    cursor: usize,
    opened: bool,
}

impl<'a> ScanOperator<'a> {
    pub fn new(
        kernel: &'a Kernel,
        subject: Subject,
        type_name: &str,
        batch_size: usize,
        cancel: CancellationToken,
    ) -> Self {
        ScanOperator {
            kernel,
            subject,
            type_name: type_name.into(),
            batch_size: batch_size.max(1),
            cancel,
            koids: Vec::new(),
            cursor: 0,
            opened: false,
        }
    }

    /// A scan whose koid list is pinned at construction — the index-assisted
    /// path (idx2-008). `open()` must not refetch it.
    pub fn with_koids(
        kernel: &'a Kernel,
        subject: Subject,
        type_name: &str,
        batch_size: usize,
        cancel: CancellationToken,
        koids: Vec<KOID>,
    ) -> Self {
        ScanOperator {
            kernel,
            subject,
            type_name: type_name.into(),
            batch_size: batch_size.max(1),
            cancel,
            koids,
            cursor: 0,
            opened: true,
        }
    }
}

impl PhysicalOperator for ScanOperator<'_> {
    fn open(&mut self) -> KResult<()> {
        if !self.opened {
            self.koids = self.kernel.type_koids(&self.type_name)?;
            self.opened = true;
        }
        self.cursor = 0;
        Ok(())
    }

    fn next_batch(&mut self) -> KResult<Option<Vec<KnowledgeObject>>> {
        if self.cancel.is_cancelled() {
            return Err(KError::Cancelled);
        }
        if !self.opened {
            return Err(KError::InvalidQuery(
                "ScanOperator::open() not called".into(),
            ));
        }
        // Skip slices that filter down to nothing (ACL exclusion), and keep
        // pulling until a non-empty batch or exhaustion.
        loop {
            if self.cursor >= self.koids.len() {
                return Ok(None);
            }
            let end = (self.cursor + self.batch_size).min(self.koids.len());
            let slice = &self.koids[self.cursor..end];
            self.cursor = end;
            let mut kos = self
                .kernel
                .scan_by_type_range(&self.subject, &self.type_name, slice)?;
            // Same freshness contract as the materializing executor: default
            // MATCH answers with current truth (facts not valid at "now" stay
            // out). Temporal plans fail closed below — they materialize.
            let now = self.kernel.clock_now();
            kos.retain(|ko| ko.valid_at(now));
            if !kos.is_empty() {
                return Ok(Some(kos));
            }
        }
    }

    fn close(&mut self) -> KResult<()> {
        self.koids.clear();
        self.cursor = 0;
        self.opened = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Filter / Project / Limit — per-batch wrappers
// ---------------------------------------------------------------------------

struct FilterOperator<'a> {
    inner: Box<dyn PhysicalOperator + 'a>,
    predicates: Vec<Predicate>,
}

impl<'a> FilterOperator<'a> {
    fn new(inner: Box<dyn PhysicalOperator + 'a>, predicates: Vec<Predicate>) -> Self {
        FilterOperator { inner, predicates }
    }
}

impl PhysicalOperator for FilterOperator<'_> {
    fn open(&mut self) -> KResult<()> {
        self.inner.open()
    }
    fn next_batch(&mut self) -> KResult<Option<Vec<KnowledgeObject>>> {
        let Some(mut kos) = self.inner.next_batch()? else {
            return Ok(None);
        };
        kos.retain(|ko| row_matches(ko, &self.predicates));
        Ok(Some(kos))
    }
    fn close(&mut self) -> KResult<()> {
        self.inner.close()
    }
}

struct ProjectOperator<'a> {
    inner: Box<dyn PhysicalOperator + 'a>,
    fields: Vec<String>,
}

impl<'a> ProjectOperator<'a> {
    fn new(inner: Box<dyn PhysicalOperator + 'a>, fields: Vec<String>) -> Self {
        ProjectOperator { inner, fields }
    }
}

impl PhysicalOperator for ProjectOperator<'_> {
    fn open(&mut self) -> KResult<()> {
        self.inner.open()
    }
    fn next_batch(&mut self) -> KResult<Option<Vec<KnowledgeObject>>> {
        let Some(mut kos) = self.inner.next_batch()? else {
            return Ok(None);
        };
        if !self.fields.contains(&"*".to_string()) {
            for ko in &mut kos {
                let mut filtered = PropertyMap::new();
                for f in &self.fields {
                    if let Some(v) = ko.properties.get(f) {
                        filtered.insert(f.clone(), v.clone());
                    }
                }
                ko.properties = filtered;
            }
        }
        Ok(Some(kos))
    }
    fn close(&mut self) -> KResult<()> {
        self.inner.close()
    }
}

struct LimitOperator<'a> {
    inner: Box<dyn PhysicalOperator + 'a>,
    offset: usize,
    remaining: usize,
}

impl<'a> LimitOperator<'a> {
    fn new(inner: Box<dyn PhysicalOperator + 'a>, offset: usize, limit: usize) -> Self {
        LimitOperator {
            inner,
            offset,
            remaining: limit,
        }
    }
}

impl PhysicalOperator for LimitOperator<'_> {
    fn open(&mut self) -> KResult<()> {
        self.inner.open()
    }
    fn next_batch(&mut self) -> KResult<Option<Vec<KnowledgeObject>>> {
        if self.remaining == 0 {
            return Ok(None); // never pulls upstream again — early stop
        }
        loop {
            let Some(mut kos) = self.inner.next_batch()? else {
                return Ok(None);
            };
            if self.offset > 0 {
                let drop = self.offset.min(kos.len());
                kos.drain(..drop);
                self.offset -= drop;
            }
            if kos.is_empty() {
                continue;
            }
            let take = self.remaining.min(kos.len());
            kos.truncate(take);
            self.remaining -= take;
            return Ok(Some(kos));
        }
    }
    fn close(&mut self) -> KResult<()> {
        self.inner.close()
    }
}

// ---------------------------------------------------------------------------
// Pipeline construction
// ---------------------------------------------------------------------------

/// The assembled streaming pipeline — the executor's streaming entry point.
pub struct StreamingPipeline<'a> {
    inner: Box<dyn PhysicalOperator + 'a>,
}

impl PhysicalOperator for StreamingPipeline<'_> {
    fn open(&mut self) -> KResult<()> {
        self.inner.open()
    }
    fn next_batch(&mut self) -> KResult<Option<Vec<KnowledgeObject>>> {
        self.inner.next_batch()
    }
    fn close(&mut self) -> KResult<()> {
        self.inner.close()
    }
}

/// Execution options for a streaming plan.
pub struct StreamOptions {
    pub batch_size: usize,
    pub cancel: CancellationToken,
    /// P5-M8 (idx2-008): opt in to index-assisted scans — the first Eq
    /// predicate over an indexed (type, property) is answered from the
    /// property index. EVENTUAL semantics: the maintainer fills the index,
    /// so never-indexed rows stay invisible. Off by default — the plain scan
    /// answers the committed truth. The choice is visible at the call site
    /// (the idx001 discipline).
    pub use_indexes: bool,
}

/// Build and open a streaming pipeline for the supported shape
/// Scan → Filter → Project → Limit. Anything else fails closed — the
/// materializing executor (`Interpreter::execute_physical`) handles full
/// plans; this entry point is for plans whose memory must be batch-bounded.
pub fn execute_streaming<'a>(
    kernel: &'a Kernel,
    plan: &PhysicalPlan,
    opts: &StreamOptions,
) -> KResult<StreamingPipeline<'a>> {
    let not_streaming = |what: &str| {
        KError::UnsupportedOperation(format!(
            "{what} is not streaming in P5-M4; use the materializing executor"
        ))
    };
    let ops = &plan.operators;
    let Some(first) = ops.first() else {
        return Err(not_streaming("an empty plan"));
    };
    let (type_name, subject, roles, tenant) = match &first.op {
        IrOp::Scan {
            type_name,
            subject,
            roles,
            tenant,
        } => (type_name, subject, roles, tenant),
        _ => return Err(not_streaming("a plan whose first op is not Scan")),
    };
    let mut subj = Subject::with_roles(
        subject,
        &roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
    );
    if let Some(t) = tenant {
        subj = subj.in_tenant(t);
    }
    // P5-M8 (idx2-008): with the opt-in, the first Eq predicate over an
    // indexed (type, property) is answered from the property index — an
    // EVENTUAL scan (never-indexed rows stay invisible; the default scan
    // answers the committed truth). The koids pin at construction, before
    // the first pull.
    let mut assist: Option<Vec<KOID>> = None;
    if opts.use_indexes {
        'outer: for po in &ops[1..] {
            if let IrOp::Filter { predicates } = &po.op {
                for p in predicates {
                    if p.op == PredOp::Eq {
                        for idx in kernel.property_indexes()? {
                            if idx.covers(type_name, &p.property) {
                                assist = Some(idx.scan_eq(std::slice::from_ref(&p.value))?);
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
    }
    let mut chain: Box<dyn PhysicalOperator + 'a> = match assist {
        Some(koids) => Box::new(ScanOperator::with_koids(
            kernel,
            subj,
            type_name,
            opts.batch_size,
            opts.cancel.clone(),
            koids,
        )),
        None => Box::new(ScanOperator::new(
            kernel,
            subj,
            type_name,
            opts.batch_size,
            opts.cancel.clone(),
        )),
    };
    for po in &ops[1..] {
        match &po.op {
            IrOp::Filter { predicates } => {
                chain = Box::new(FilterOperator::new(chain, predicates.clone()));
            }
            IrOp::Project { fields } => {
                chain = Box::new(ProjectOperator::new(chain, fields.clone()));
            }
            IrOp::Limit { limit, offset } => {
                chain = Box::new(LimitOperator::new(chain, *offset, *limit));
            }
            other => {
                let name = format!("{:?}", other);
                let name = name.split('{').next().unwrap_or(&name).trim();
                return Err(not_streaming(name));
            }
        }
    }
    let mut pipe = StreamingPipeline { inner: chain };
    pipe.open()?; // pins the koid snapshot now, before the first pull
    Ok(pipe)
}
