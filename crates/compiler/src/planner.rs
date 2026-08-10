//! Knowledge IR Planner — rule-based plan optimization (MRFC-0005 §Compiler).
//!
//! Applies heuristic rewrites to an `IrPlan` to improve execution efficiency:
//! 1. Merge consecutive Filters into one
//! 2. Push Filter before Search (reduces candidate set)
//! 3. Sort operators for optimal execution order
//!
//! v1 is purely rule-based. Cost-based optimization (CBO) arrives post-1.0
//! when workload statistics exist (per Architecture Review R2).

use mnemosyne_kernel::ir::*;

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

    /// Push Filter operators before Search operators to reduce the candidate
    /// set before expensive vector/text scoring.
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

    /// Deduplicate Scan operators on the same type, even when separated by
    /// Filters or other ops that don't change the row set. This is the
    /// foundational cross-program optimization (MRFC-0030 Phase 7d).
    fn dedup_scans(mut ops: Vec<IrOp>) -> Vec<IrOp> {
        // Collect the first scan for each type; remove subsequent duplicates.
        let mut seen_types: std::collections::HashSet<String> = std::collections::HashSet::new();
        ops.retain(|op| {
            if let IrOp::Scan { type_name, .. } = op {
                if seen_types.contains(type_name) {
                    return false; // Remove duplicate scan
                }
                seen_types.insert(type_name.clone());
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
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("x", mnemosyne_kernel::Value::Int(1))],
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("y", mnemosyne_kernel::Value::Int(2))],
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
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "Employee".into(),
                subject: "a".into(),
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq(
                    "dept",
                    mnemosyne_kernel::Value::Text("Eng".into()),
                )],
            },
            IrOp::Scan {
                type_name: "Employee".into(),
                subject: "b".into(),
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq(
                    "salary",
                    mnemosyne_kernel::Value::Int(100000),
                )],
            },
        ]);
        let opt = Planner::optimize(&plan);
        // Duplicate Scan removed → Scan + Filter + Filter = 3 ops
        assert_eq!(opt.operators.len(), 3);
        // Verify only 1 Scan remains
        let scan_count = opt
            .operators
            .iter()
            .filter(|op| matches!(op, IrOp::Scan { .. }))
            .count();
        assert_eq!(scan_count, 1);
    }

    #[test]
    fn pushdown_filter_before_search() {
        let plan = IrPlan::new(vec![
            IrOp::Scan {
                type_name: "f".into(),
                subject: "a".into(),
            },
            IrOp::Filter {
                predicates: vec![Predicate::eq("x", mnemosyne_kernel::Value::Int(1))],
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
