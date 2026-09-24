//! alg002 — property tests over the planner's Scan-identity rule.
//!
//! Dedup is allowed ONLY on a full-tuple match (type, subject, roles, tenant)
//! and ONLY between consecutive Scans. Deterministic: proptest's fixed
//! default seed; 512 cases per property. Companion to ppl001–005 (the
//! hand-picked pins) — these are the random-search version of the same
//! contract, per docs/compiler/operator-algebra.md §3.

use aikoql_compiler::planner::Planner;
use aikoql_kernel::ir::{IrOp, IrPlan};
use proptest::prelude::*;

type ScanTuple = (String, String, Vec<String>, Option<String>);

fn scan_tuple() -> impl Strategy<Value = ScanTuple> {
    (
        prop_oneof!["fact", "event", "Person", "Task"],
        prop_oneof!["alice", "bob", "query-user"],
        proptest::collection::vec(prop_oneof!["admin", "auditor", "writer"], 0..3),
        proptest::option::of("[a-z]{2,4}"),
    )
}

fn scan(t: &ScanTuple) -> IrOp {
    IrOp::Scan {
        type_name: t.0.clone(),
        subject: t.1.clone(),
        roles: t.2.clone(),
        tenant: t.3.clone(),
    }
}

fn scan_count(plan: &IrPlan) -> usize {
    plan.operators
        .iter()
        .filter(|op| matches!(op, IrOp::Scan { .. }))
        .count()
}

fn opt(tuples: &[ScanTuple]) -> IrPlan {
    Planner::optimize(&IrPlan::new(tuples.iter().map(scan).collect()))
}

/// Output scan tuples of a plan (ignoring non-scan ops) in order.
fn output_scans(plan: &IrPlan) -> Vec<ScanTuple> {
    plan.operators
        .iter()
        .filter_map(|op| match op {
            IrOp::Scan {
                type_name,
                subject,
                roles,
                tenant,
            } => Some((
                type_name.clone(),
                subject.clone(),
                roles.clone(),
                tenant.clone(),
            )),
            _ => None,
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn dedup_iff_full_tuple_match(a in scan_tuple(), b in scan_tuple()) {
        // Two consecutive Scans collapse to one ONLY when every dimension —
        // type, subject, roles, tenant — matches. A partial match (the P0
        // this closed in P4-M1) would serve one subject's row set under
        // another subject's query context.
        let plan = opt(&[a.clone(), b.clone()]);
        let expected = if a == b { 1 } else { 2 };
        prop_assert_eq!(scan_count(&plan), expected);
    }

    #[test]
    fn output_scan_sequence_is_input_with_consecutive_duplicates_collapsed(
        tuples in proptest::collection::vec(scan_tuple(), 1..8)
    ) {
        // dedup_scans collapses each run of consecutive identical Scans to
        // its first element and never touches anything else: the output scan
        // sequence is exactly the input with consecutive duplicates removed.
        let expected: Vec<ScanTuple> = {
            let mut out: Vec<ScanTuple> = Vec::new();
            for t in &tuples {
                if out.last() != Some(t) {
                    out.push(t.clone());
                }
            }
            out
        };
        let plan = opt(&tuples);
        prop_assert_eq!(output_scans(&plan), expected);
    }

    #[test]
    fn filter_between_scans_blocks_dedup(t in scan_tuple()) {
        // ppl005 generalized: a Filter between two identical Scans makes the
        // second Scan's row set the downstream source — dedup across it is
        // unsound (it would make the dead Filter live). Any intervening op
        // resets the adjacency window.
        let plan = Planner::optimize(&IrPlan::new(vec![
            scan(&t),
            IrOp::Filter {
                predicates: vec![aikoql_kernel::ir::Predicate::eq(
                    "temp",
                    aikoql_kernel::Value::Int(35),
                )],
            },
            scan(&t),
        ]));
        prop_assert_eq!(scan_count(&plan), 2);
    }
}
