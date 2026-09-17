//! P5-M4 (ND-04) — streaming/batch execution.
//!
//! A pull-based pipeline (`PhysicalOperator::next_batch`) over the supported
//! streaming shape: Scan → Filter → Project → Limit. Backpressure is by
//! construction — a slow consumer stops pulling and no operator holds more
//! than one batch. The Scan pins the koid list AND a snapshot timestamp at
//! `open()` (rows created after open never appear, and every batch resolves
//! through the kernel's `get_at` path at that timestamp — the SAME read
//! filters as the materializing `scan_by_type`, so streaming ≡ materialized
//! for the same snapshot). Anything outside the supported shape fails
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

/// Streams a type scan: the koid list AND a snapshot timestamp are captured
/// at `open()` (the snapshot the runtime controls — rows created after open
/// never appear, and every payload batch resolves through the `get_at` path
/// at that timestamp: one consistent version set, not mixed-time heads).
pub struct ScanOperator<'a> {
    kernel: &'a Kernel,
    subject: Subject,
    type_name: String,
    batch_size: usize,
    cancel: CancellationToken,
    koids: Vec<KOID>,
    cursor: usize,
    opened: bool,
    snapshot_ts: u64,
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
            snapshot_ts: 0,
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
            snapshot_ts: 0,
        }
    }
}

impl PhysicalOperator for ScanOperator<'_> {
    fn open(&mut self) -> KResult<()> {
        if !self.opened {
            self.koids = self.kernel.type_koids(&self.type_name)?;
            self.opened = true;
        }
        // The version-set pin: payloads resolve at this instant's HLC — the
        // same snapshot `begin_transaction` would pin (P5-M21, PR6 P0-08).
        self.snapshot_ts = self.kernel.snapshot_now();
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
            let mut kos = self.kernel.scan_by_type_range_at(
                &self.subject,
                &self.type_name,
                slice,
                self.snapshot_ts,
            )?;
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

/// The index-assist contract for a stream (PR6 P0-09) — explicit, so no
/// `bool` silently varies the semantics of the same query shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IndexStrategy {
    /// No index assist — the type scan pins the koid list at open, so the
    /// stream is exactly the open snapshot (exact by construction).
    #[default]
    Scan,
    /// Index assist with a completeness guarantee for the open snapshot:
    /// the index verifies clean and the journal head stays pinned from
    /// verify through open, else the stream falls back to the Scan.
    /// ponytail: the verify is O(index) per stream open — that is the
    /// price of the guarantee; EventualIndex is the cheap path.
    Exact,
    /// Index assist, EVENTUAL by contract: the maintainer fills the index,
    /// so never-indexed rows stay invisible. The visible choice for a
    /// lagging index (the pre-P0-09 `use_indexes: true` semantics).
    EventualIndex,
}

/// Execution options for a streaming plan.
pub struct StreamOptions {
    pub batch_size: usize,
    pub cancel: CancellationToken,
    /// The index-assist strategy for the Scan (PR6 P0-09). Default Scan:
    /// no assist — the plain scan answers the committed open snapshot.
    pub index_strategy: IndexStrategy,
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
    // PR6 P1-13: the assist binds only to the Filter IMMEDIATELY after the
    // Scan — positional adjacency is the proven semantic dependency.
    let adjacent_eq = match ops.get(1).map(|po| &po.op) {
        Some(IrOp::Filter { predicates }) => predicates.iter().find(|p| p.op == PredOp::Eq),
        _ => None,
    };

    // PR6 P0-09: the strategy contract. EXACT re-checks the journal head
    // around verify + probe and once more after open — a commit anywhere in
    // that window falls back to the Scan, which is complete for the open
    // snapshot by construction.
    let mut assist: Option<Vec<KOID>> = None;
    let mut exact_head: Option<u64> = None;
    if let Some(p) = adjacent_eq {
        if opts.index_strategy != IndexStrategy::Scan {
            for idx in kernel.property_indexes()? {
                // P5-M22 (P1-16): the DDL state gate — a non-Ready index is
                // never the streaming dispatch's assist either.
                if idx.state() != aikoql_kernel::IndexState::Ready {
                    continue;
                }
                if !idx.covers(type_name, &p.property) {
                    continue;
                }
                match opts.index_strategy {
                    IndexStrategy::Scan => {}
                    IndexStrategy::EventualIndex => {
                        assist = Some(idx.scan_eq(std::slice::from_ref(&p.value))?);
                    }
                    IndexStrategy::Exact => {
                        let mut head = kernel.journal_head()?.0;
                        for _ in 0..2 {
                            let report = idx.verify(kernel)?;
                            if !report.verified
                                || !report.missing.is_empty()
                                || !report.stale.is_empty()
                            {
                                break; // the index lags the store — Scan answers the truth
                            }
                            let koids = idx.scan_eq(std::slice::from_ref(&p.value))?;
                            let now = kernel.journal_head()?.0;
                            if now == head {
                                assist = Some(koids);
                                exact_head = Some(head);
                                break;
                            }
                            head = now; // a commit raced the attempt — retry once
                        }
                    }
                }
                break; // first covering index, same as the CBO
            }
        }
    }
    let build = |assist: Option<Vec<KOID>>| -> KResult<Box<dyn PhysicalOperator + 'a>> {
        let mut chain: Box<dyn PhysicalOperator + 'a> = match assist {
            Some(koids) => Box::new(ScanOperator::with_koids(
                kernel,
                subj.clone(),
                type_name,
                opts.batch_size,
                opts.cancel.clone(),
                koids,
            )),
            None => Box::new(ScanOperator::new(
                kernel,
                subj.clone(),
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
        Ok(chain)
    };
    let mut pipe = StreamingPipeline {
        inner: build(assist)?,
    };
    pipe.open()?; // pins the koid snapshot now, before the first pull
                  // The [probe..open] window: the pinned koids must still be complete for
                  // the open snapshot — a commit in between falls back to the type scan.
    if let Some(head) = exact_head {
        if kernel.journal_head()?.0 != head {
            pipe = StreamingPipeline {
                inner: build(None)?,
            };
            pipe.open()?;
        }
    }
    Ok(pipe)
}
