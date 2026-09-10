//! Knowledge IR Planner — rule-based plan optimization (MRFC-0005 §Compiler).
//!
//! Applies heuristic rewrites to an `IrPlan` to improve execution efficiency:
//! 1. Merge consecutive Filters into one
//! 2. Rewrite Filter,Search → Search,Filter — Search-then-Filter semantics
//!    (full-set ranking, then filter; filtering candidates before scoring
//!    would change ANN/text recall — TDD-COMP-003)
//! 3. Deduplicate semantically identical Scans (TDD-COMP-002)
//!
//! v1 is purely rule-based. Cost-based optimization (CBO) arrives post-1.0
//! when workload statistics exist (per Architecture Review R2).

use aikoql_kernel::ir::*;

pub struct Planner;

impl Planner {
    /// Optimize an IR plan. Returns a new plan (the input is unchanged).
    pub fn optimize(plan: &IrPlan) -> IrPlan {
        let mut ops = plan.operators.clone();
        ops = Self::merge_filters(ops);
        ops = Self::pushdown_filters(ops);
        ops = Self::dedup_scans(ops);
        IrPlan {
            operators: ops,
            description: plan.description.clone(),
        }
    }

    /// Merge consecutive Filter operators into one.
    fn merge_filters(mut ops: Vec<IrOp>) -> Vec<IrOp> {
        let mut i = 0;
        while i + 1 < ops.len() {
            if let (IrOp::Filter { predicates: p1 }, IrOp::Filter { predicates: p2 }) =
                (&ops[i], &ops[i + 1])
            {
                let mut merged = p1.clone();
                merged.extend(p2.clone());
                ops[i] = IrOp::Filter { predicates: merged };
                ops.remove(i + 1);
            } else {
                i += 1;
            }
        }
        ops
    }

    /// Rewrite Filter,Search → Search,Filter. The chosen semantics (TDD-COMP-003)
    /// is Search-then-Filter: rank the full set, then filter — pushing a
    /// filter BEFORE search would restrict the candidate set and change
    /// ANN/text recall (index-dependent). Pinned by ppl006.
    fn pushdown_filters(mut ops: Vec<IrOp>) -> Vec<IrOp> {
        for i in (0..ops.len().saturating_sub(1)).rev() {
            let is_filter = matches!(ops[i], IrOp::Filter { .. });
            let is_search = matches!(ops[i + 1], IrOp::AnnSearch { .. } | IrOp::TextSearch { .. });
            if is_filter && is_search {
                ops.swap(i, i + 1);
            }
        }
        ops
    }

    /// Deduplicate consecutive identical Scan operators. This is the
    /// foundational cross-program optimization (MRFC-0030 Phase 7d).
    ///
    /// TDD-COMP-002: two scans are equivalent only when every semantic and
    /// security dimension matches — type, subject, roles, tenant — AND no
    /// intervening op changes the row set. Deduping on type alone can serve
    /// one subject's row set under another subject's query context, and a
    /// Filter between the scans is dead code that dedup would make live
    /// (predicate algebra doesn't exist here, so dedup never crosses an
    /// intervening op). The full destructure means a new Scan field becomes
    /// a compile error here rather than a silently ignored dimension.
    fn dedup_scans(mut ops: Vec<IrOp>) -> Vec<IrOp> {
        type ScanKey = (String, String, Vec<String>, Option<String>);
        let mut last: Option<ScanKey> = None;
        ops.retain(|op| {
            if let IrOp::Scan {
                type_name,
                subject,
                roles,
                tenant,
            } = op
            {
                let key = (
                    type_name.clone(),
                    subject.clone(),
                    roles.clone(),
                    tenant.clone(),
                );
                if last.as_ref() == Some(&key) {
                    return false; // consecutive identical scan — pure duplicate
                }
                last = Some(key);
            } else {
                last = None; // intervening op changes the row set — no dedup across it
            }
            true
        });
        ops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_two_filters() {
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "f".into(),
                subject: "a".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("x", aikoql_kernel::Value::Int(1))],
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("y", aikoql_kernel::Value::Int(2))],
            },
        ]);
        let opt = Planner::optimize(&plan);
        assert_eq!(opt.operators.len(), 2); // Scan + merged Filter
        match &opt.operators[1] {
            IrOp::Filter { predicates } => assert_eq!(predicates.len(), 2),
            _ => panic!("expected Filter"),
        }
    }

    #[test]
    fn dedup_consecutive_scans_on_same_type() {
        // TDD-COMP-002: same type + different subject → MUST NOT dedup.
        // (This test previously pinned the bug — subjects "a" and "b"
        // deduped into one scan, silently serving subject "a"'s row set
        // under subject "b". Corrected with the fix.)
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "Employee".into(),
                subject: "a".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq(
                    "dept",
                    aikoql_kernel::Value::Text("Eng".into()),
                )],
            },
            IrOp::Scan {
                type_name: "Employee".into(),
                subject: "b".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("salary", aikoql_kernel::Value::Int(100000))],
            },
        ]);
        let opt = Planner::optimize(&plan);
        // Subjects differ → both scans kept → Scan + Filter + Scan + Filter = 4 ops
        assert_eq!(opt.operators.len(), 4);
        let scan_count = opt
            .operators
            .iter()
            .filter(|op| matches!(op, IrOp::Scan { .. }))
            .count();
        assert_eq!(scan_count, 2);
    }

    #[test]
    fn ppl001_scan_different_subject_not_deduped() {
        let plan = two_scan_plan(
            ("Employee", "a", vec![], None),
            ("Employee", "b", vec![], None),
        );
        assert_eq!(scan_count(&Planner::optimize(&plan)), 2);
    }

    #[test]
    fn ppl002_scan_different_tenant_not_deduped() {
        let plan = two_scan_plan(
            ("Employee", "a", vec![], Some("t1")),
            ("Employee", "a", vec![], Some("t2")),
        );
        assert_eq!(scan_count(&Planner::optimize(&plan)), 2);
    }

    #[test]
    fn ppl003_scan_different_roles_not_deduped() {
        let plan = two_scan_plan(
            ("Employee", "a", vec!["admin".into()], None),
            ("Employee", "a", vec!["auditor".into()], None),
        );
        assert_eq!(scan_count(&Planner::optimize(&plan)), 2);
    }

    #[test]
    fn ppl004_identical_scans_deduped() {
        let plan = two_scan_plan(
            ("Employee", "a", vec!["admin".into()], Some("t1")),
            ("Employee", "a", vec!["admin".into()], Some("t1")),
        );
        assert_eq!(scan_count(&Planner::optimize(&plan)), 1);
    }

    #[test]
    fn ppl005_identical_scans_separated_by_ops_not_deduped() {
        // TDD-COMP-002: predicates attached to later operators — combine only
        // when predicate algebra proves equivalence (we have none). Between
        // two scans a Filter is dead code (the second Scan resets the row
        // set); removing the second Scan would make the Filter live and
        // change the plan's result set. Conservative: no dedup across
        // intervening ops.
        let plan = IrPlan::new(vec![
            scan("Employee", "a", vec![], None),
            IrOp::Filter {
                predicates: vec![Predicate::eq(
                    "dept",
                    aikoql_kernel::Value::Text("Eng".into()),
                )],
            },
            scan("Employee", "a", vec![], None),
        ]);
        assert_eq!(scan_count(&Planner::optimize(&plan)), 2);
    }

    #[test]
    fn ppl006_filter_search_ordering_semantics() {
        // TDD-COMP-003: the planner's rewrite Filter,Search → Search,Filter
        // implements Search-then-Filter semantics (full-set ranking, then
        // filter) — the recall-preserving choice for ANN/text search, where
        // filtering candidates before scoring changes recall. Pinned here so
        // no future rule flips it silently.
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "f".into(),
                subject: "a".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("x", aikoql_kernel::Value::Int(1))],
            },
            IrOp::TextSearch {
                query: "test".into(),
                k: 5,
                scoring: None,
            },
        ]);
        let opt = Planner::optimize(&plan);
        match (&opt.operators[1], &opt.operators[2]) {
            (IrOp::TextSearch { .. }, IrOp::Filter { .. }) => {} // Search, Filter
            _ => panic!("expected Search then Filter — Search-then-Filter semantics"),
        }
    }

    fn scan(type_name: &str, subject: &str, roles: Vec<String>, tenant: Option<&str>) -> IrOp {
        IrOp::Scan {
            type_name: type_name.into(),
            subject: subject.into(),
            roles,
            tenant: tenant.map(|t| t.into()),
        }
    }

    fn two_scan_plan(
        a: (&str, &str, Vec<String>, Option<&str>),
        b: (&str, &str, Vec<String>, Option<&str>),
    ) -> IrPlan {
        IrPlan::new(vec![scan(a.0, a.1, a.2, a.3), scan(b.0, b.1, b.2, b.3)])
    }

    fn scan_count(plan: &IrPlan) -> usize {
        plan.operators
            .iter()
            .filter(|op| matches!(op, IrOp::Scan { .. }))
            .count()
    }

    #[test]
    fn pushdown_filter_before_search() {
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "f".into(),
                subject: "a".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("x", aikoql_kernel::Value::Int(1))],
            },
            IrOp::TextSearch {
                query: "test".into(),
                k: 5,
                scoring: None,
            },
        ]);
        let opt = Planner::optimize(&plan);
        // Filter should be pushed after Search (swapped)
        match (&opt.operators[1], &opt.operators[2]) {
            (IrOp::TextSearch { .. }, IrOp::Filter { .. }) => {} // correct order
            _ => panic!("expected Search then Filter after pushdown"),
        }
    }
}
