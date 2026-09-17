//! P5-M9 (ND-08) — the cost-based optimizer.
//!
//! Cost-based where the M9 statistics exist, rule-based where they don't —
//! never worse than today (the M0 plan-equivalence oracle pins that, cbo010).
//! The ONE real executable choice in v1: a Scan whose following Filter
//! carries an Eq predicate over a property index executes as an
//! index-assisted scan (`Strategy::PropertyIndex`) — chosen only when the
//! stats are fresh AND the covering index verifies clean against the
//! canonical heads (missing == 0 ∧ stale == 0), and the executor re-pins the
//! journal head captured at optimize before serving the assist (P5-M21:
//! `applied_seq == pinned head` or full-scan fallback). Eq never matches a
//! missing property, and PropertyIndex excludes missing-key rows — the two
//! agree by construction, so a clean index answers exactly the committed
//! truth. Everything else (traversal, search modalities, temporal) is
//! cost-modeled from the statistics with the v1 strategies — the
//! honest-ledger rows document which alternatives do not exist yet.

use aikoql_compiler::parser;
use aikoql_kernel::ir::{IrOp, IrPlan, PhysicalOp, PhysicalPlan, PredOp, Strategy};
use aikoql_kernel::transaction::kernel::Kernel;
use aikoql_kernel::{KError, KResult, Statistics};

/// Candidate dimensions in the v1 cost model (bge-m3 class, P4-M7 evidence).
pub const EMBEDDING_DIM: u64 = 768;

/// One operator's standalone cost: estimated output rows and cpu units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cost {
    pub rows: u64,
    pub cpu: u64,
}

/// The optimizer's verdict: the chosen plan, its per-op costs, and what the
/// decision was based on.
#[derive(Debug)]
pub struct CostReport {
    pub plan: PhysicalPlan,
    pub costs: Vec<Cost>,
    /// Fresh statistics existed and drove the decision.
    pub stats_used: bool,
    /// Statistics exist but are stale (a journal event after the capture).
    pub stats_stale: bool,
    /// The property index the scan runs through, when one was chosen.
    pub index_used: Option<String>,
}

/// The first Eq predicate in the Filter IMMEDIATELY after op `i` — the
/// predicate an index-assisted scan would serve. PR6 P1-13: the binding is
/// positional — only adjacency proves the semantic dependency, so an Eq
/// behind any other op is never answered from an index.
pub(crate) fn first_eq_after(
    ops: &[PhysicalOp],
    i: usize,
) -> Option<&aikoql_kernel::ir::Predicate> {
    match ops.get(i + 1).map(|po| &po.op) {
        Some(IrOp::Filter { predicates }) => predicates.iter().find(|p| p.op == PredOp::Eq),
        _ => None,
    }
}

/// The uniform match fraction of that predicate: 1/distinct from the stats,
/// 1.0 (no information) when the property was never seen.
fn eq_selectivity(pred: &aikoql_kernel::ir::Predicate, stats: Option<&Statistics>) -> f64 {
    stats
        .and_then(|s| s.cardinality(&pred.property))
        .filter(|&d| d > 0)
        .map_or(1.0, |d| 1.0 / d as f64)
}

/// P1-04 (P5-M19 idx3-005): may this index answer an exact equality scan?
/// A CandidateOnly index is a post-filter candidate, never the scan's
/// answer set — anything else (Exact, SnapshotExact, EventuallyConsistent)
/// is allowed through; the optimizer's verify gate judges freshness.
fn may_serve_exact(idx: &dyn aikoql_kernel::Index) -> bool {
    idx.consistency() != aikoql_kernel::ConsistencyLevel::CandidateOnly
}

/// The executor's index assist for a Scan the CBO flagged `PropertyIndex`:
/// the same derivation the optimizer used, so the flag's koid list comes
/// from the covering index. A flag without an index falls back to the full
/// scan — always the committed truth. The EXECUTOR gates the flag itself
/// (P5-M21, PR6 P0-07): it re-pins the journal head and serves the assist
/// only while `applied_seq == pinned head`.
pub(crate) fn scan_assist(
    kernel: &Kernel,
    ops: &[PhysicalOp],
    i: usize,
) -> KResult<Option<Vec<aikoql_kernel::KOID>>> {
    let IrOp::Scan { type_name, .. } = &ops[i].op else {
        return Ok(None);
    };
    let Some(pred) = first_eq_after(ops, i) else {
        return Ok(None);
    };
    for idx in kernel.property_indexes()? {
        if !may_serve_exact(&*idx) {
            continue; // CandidateOnly never answers an exact scan
        }
        if idx.covers(type_name, &pred.property) {
            return Ok(Some(idx.scan_eq(std::slice::from_ref(&pred.value))?));
        }
    }
    Ok(None)
}

/// Per-op standalone costs over the given plan and statistics. The rows of
/// each op feed the next (costs[i-1].rows); the Scan seeds from the type's
/// cardinality. Unknown statistics → every cost is 0 (unknown, not wrong).
pub fn cost_plan(ops: &[PhysicalOp], stats: Option<&Statistics>) -> Vec<Cost> {
    let mut costs: Vec<Cost> = Vec::with_capacity(ops.len());
    for (i, po) in ops.iter().enumerate() {
        let input = if i == 0 {
            stats.map_or(0, |s| s.row_count)
        } else {
            costs[i - 1].rows
        };
        // Search legs (ANN/text) fan out from the SCAN, not from each other:
        // the hybrid pipeline is two independent legs joined at the Fuse.
        let search_input = match &po.op {
            IrOp::AnnSearch { .. } | IrOp::TextSearch { .. } => costs[0].rows,
            _ => input,
        };
        costs.push(match &po.op {
            IrOp::Scan { .. } if po.strategy == Strategy::PropertyIndex => {
                // Rows = the uniform match count of the served Eq. The
                // probe is an O(1) hash lookup plus ONE point read per
                // matched row (P5-M17b: scan_by_type_range materializes
                // exactly the matched koids) — cpu per matched row, so the
                // index wins whenever matched < the scan.
                let matched = first_eq_after(ops, i)
                    .map(|p| (eq_selectivity(p, stats) * input as f64).ceil() as u64)
                    .unwrap_or(0)
                    .min(input);
                Cost {
                    rows: matched,
                    cpu: matched,
                }
            }
            IrOp::Scan { .. } => Cost {
                rows: input,
                cpu: input,
            },
            IrOp::Filter { predicates } => {
                // ponytail: the first Eq drives the estimate; conjuncts are
                // assumed uniform and one already covers it.
                let sel = predicates
                    .iter()
                    .find(|p| p.op == PredOp::Eq)
                    .map(|p| eq_selectivity(p, stats))
                    .unwrap_or(1.0)
                    .min(1.0);
                Cost {
                    rows: (input as f64 * sel).ceil() as u64,
                    cpu: input,
                }
            }
            IrOp::Traverse { depth, .. } => {
                // ponytail: type-level average fanout for every hop — a
                // rel-type-scoped degree would need per-edge statistics.
                let fanout = stats.map_or(1.0, |s| s.fanout.max(1.0));
                let rows = (input as f64 * fanout.powi(*depth as i32)).ceil() as u64;
                Cost { rows, cpu: rows }
            }
            IrOp::AnnSearch { .. } => {
                let rows =
                    (search_input as f64 * stats.map_or(0.0, |s| s.vector_density)).ceil() as u64;
                Cost {
                    rows,
                    cpu: rows * EMBEDDING_DIM,
                }
            }
            IrOp::TextSearch { .. } => {
                // Delegated BM25: the kernel index does the scoring; the
                // runtime walks the rows.
                Cost {
                    rows: search_input,
                    cpu: search_input,
                }
            }
            IrOp::Temporal { .. } => {
                let cpu = (input as f64 * (1.0 + stats.map_or(0.0, |s| s.temporal_density))).ceil()
                    as u64;
                Cost { rows: input, cpu }
            }
            // Everything else is Inline row processing (v1): rows pass
            // through, cpu per row. Alternatives don't exist yet — the
            // honest-ledger rows say so.
            _ => Cost {
                rows: input,
                cpu: input,
            },
        });
    }
    costs
}

fn total_cpu(costs: &[Cost]) -> u64 {
    costs.iter().map(|c| c.cpu).sum()
}

/// Optimize a logical plan: physicalize with the v1 rules, then replace the
/// Scan strategy with `PropertyIndex` when a clean covering index beats the
/// full scan. Everything else is cost-modeled, never rewritten (cbo006: the
/// CBO does not touch fusion or op order — scores are oracle bits).
pub fn cost_optimize(kernel: &Kernel, plan: &IrPlan) -> KResult<CostReport> {
    let mut ops = PhysicalPlan::from_ops(plan.operators.clone()).operators;
    let type_name = match ops.first().map(|po| &po.op) {
        Some(IrOp::Scan { type_name, .. }) => Some(type_name.clone()),
        _ => None,
    };
    let stats = match &type_name {
        Some(t) => kernel.statistics(t)?,
        None => None,
    };
    // P5-M17b: the head seq IS the journal length (the append-only journal
    // behind the M9 watermark contract) — O(1), not a whole-journal
    // materialization per query.
    let journal_len = kernel.journal_head()?.0;
    let stats_stale = stats.as_ref().is_some_and(|s| s.is_stale(journal_len));
    let stats_used = stats.is_some() && !stats_stale;

    let mut index_used: Option<String> = None;
    if let Some(stats) = &stats {
        if stats_used {
            if let Some(pred) = first_eq_after(&ops, 0) {
                for idx in kernel.property_indexes()? {
                    if !idx.covers(&stats.type_name, &pred.property) {
                        continue;
                    }
                    // The exactness gate: the index must hold the committed
                    // truth — verified clean, nothing missing, nothing stale.
                    let report = idx.verify(kernel)?;
                    if !report.verified || !report.missing.is_empty() || !report.stale.is_empty() {
                        continue;
                    }
                    let mut cand_ops = ops.clone();
                    cand_ops[0].strategy = Strategy::PropertyIndex;
                    if total_cpu(&cost_plan(&cand_ops, Some(stats)))
                        < total_cpu(&cost_plan(&ops, Some(stats)))
                    {
                        ops = cand_ops;
                        index_used = Some(idx.name().to_string());
                    }
                    break;
                }
            }
        }
    }
    let costs = cost_plan(&ops, stats.as_ref());
    // P5-M21 (PR6 P0-07): pin the journal head the decision was made at —
    // the plan carries its snapshot; the executor re-pins before serving
    // the assist and falls back to the full scan on a mismatch. Only
    // index-assisted plans pin: every other plan stays byte-identical to
    // the rules (cbo_default_003).
    let mut plan = PhysicalPlan::new(ops);
    if index_used.is_some() {
        plan.pinned_head = journal_len;
    }
    Ok(CostReport {
        plan,
        costs,
        stats_used,
        stats_stale,
        index_used,
    })
}

/// EXPLAIN COST: compile, optimize, and render one line per operator
/// (strategy + estimated rows/cpu) plus a statistics-freshness footer.
/// Unknown statistics render as `?` — visible, not invented.
pub fn explain_cost(kernel: &Kernel, query: &str) -> KResult<Vec<String>> {
    let plan = parser::compile_with_subject(query, "alice").map_err(KError::InvalidQuery)?;
    let report = cost_optimize(kernel, &plan)?;
    let known = report.stats_used || report.stats_stale;
    let mut lines = Vec::with_capacity(report.plan.operators.len() + 1);
    for (i, po) in report.plan.operators.iter().enumerate() {
        let name = format!("{:?}", po.op);
        let name = name.split('{').next().unwrap_or(&name).trim();
        let (rows, cpu) = if known {
            (
                report.costs[i].rows.to_string(),
                report.costs[i].cpu.to_string(),
            )
        } else {
            ("?".into(), "?".into())
        };
        lines.push(format!(
            "{i:>2}: {name} [{:?}] rows={rows} cpu={cpu}",
            po.strategy
        ));
    }
    let footer = if !known {
        "statistics: none"
    } else if report.stats_stale {
        "statistics: stale"
    } else {
        "statistics: fresh"
    };
    lines.push(footer.to_string());
    // PR6 P0-07: every index-assisted plan states its snapshot — the journal
    // head the index read is assumed fresh at.
    if report.index_used.is_some() {
        lines.push(format!(
            "snapshot: journal head pinned at {}",
            report.plan.pinned_head
        ));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::knowledge::kom::{KnowledgeObject, KOID};

    /// A covering index whose consistency class the guard must judge.
    struct FakeIndex {
        consistency: aikoql_kernel::ConsistencyLevel,
    }
    impl aikoql_kernel::Index for FakeIndex {
        fn name(&self) -> &str {
            "fake"
        }
        fn upsert(&self, _koid: KOID, _ko: &KnowledgeObject) -> KResult<()> {
            Ok(())
        }
        fn remove(&self, _koid: &KOID) -> KResult<()> {
            Ok(())
        }
        fn len(&self) -> usize {
            0
        }
        fn covers(&self, _type_name: &str, _property: &str) -> bool {
            true
        }
        fn consistency(&self) -> aikoql_kernel::ConsistencyLevel {
            self.consistency
        }
    }

    // --- P5-M19 — idx3-005 (PR6 P1-04): consistency classes. RED: the
    // runtime has no notion of an index's consistency level, so nothing can
    // stop a CandidateOnly index from serving an exact scan. Pin: only a
    // SnapshotExact (or stronger) declaration may serve; CandidateOnly is
    // a post-filter candidate, never the scan's answer set. ---
    #[test]
    fn idx3_005_candidate_only_never_serves_exact() {
        use aikoql_kernel::ConsistencyLevel;
        assert!(may_serve_exact(&FakeIndex {
            consistency: ConsistencyLevel::SnapshotExact
        }));
        assert!(may_serve_exact(&FakeIndex {
            consistency: ConsistencyLevel::Exact
        }));
        assert!(may_serve_exact(&FakeIndex {
            consistency: ConsistencyLevel::EventuallyConsistent
        }));
        assert!(
            !may_serve_exact(&FakeIndex {
                consistency: ConsistencyLevel::CandidateOnly
            }),
            "CandidateOnly never answers an exact scan"
        );
    }
}
