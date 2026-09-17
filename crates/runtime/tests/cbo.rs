//! P5-M9 (ND-08) — cost-based optimizer. cbo001–cbo012 + st011 + idx4-004.
//!
//! The CBO is cost-based where statistics exist, rule-based where they
//! don't (never worse than today — the M0 plan-equivalence oracle pins it,
//! cbo010). The REAL executable choice in v1: a Scan whose following Filter
//! carries an Eq predicate over a property index can execute as an
//! index-assisted scan (`Strategy::PropertyIndex`) — chosen only when the
//! stats are fresh AND the index verifies clean against the canonical heads
//! (missing == 0 ∧ stale == 0), which makes the assisted scan exactly the
//! committed truth (Eq on a missing property never matches, and rows whose
//! key property is missing are not indexed — the two agree by construction).
//! Everything else (traversal, search modalities, temporal) is COST-MODELED
//! from the statistics with the v1 strategies — the honest-ledger rows
//! document which alternatives do not exist yet.
//!
//! - cbo001 highly selective property → index scan over type scan
//! - cbo002 zero selectivity gain (every row matches) → tie, scan stays
//! - cbo003 high graph fanout → fanout-aware traversal cost
//! - cbo004 vector-heavy → density-aware ANN cost
//! - cbo005 text-heavy → text-path cost (BM25 delegation)
//! - cbo006 hybrid → modality order pinned, no fusion rewrite
//! - cbo007 temporal → snapshot-aware cost
//! - cbo008 stale statistics detected, plan not silently wrong
//! - cbo009 EXPLAIN COST lines
//! - cbo010 gate-6 plan-equivalence oracle over CBO scenarios

use aikoql_compiler::parser;
use aikoql_kernel::ir::*;
use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use aikoql_runtime::cbo::{cost_optimize, cost_plan, CostReport};
use aikoql_runtime::plan_oracle::{run_costed, OracleEntry};
use aikoql_runtime::Interpreter;
use std::sync::Arc;

// --- helpers ------------------------------------------------------------------

fn mk() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
}

fn alice() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new("alice"))
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn person(k: &Kernel, name: &str, dept: &str) -> KOID {
    let mut req = RememberRequest::create(alice(), meta("Person"));
    req.properties
        .insert("name".into(), Value::Text(name.into()));
    req.properties
        .insert("dept".into(), Value::Text(dept.into()));
    k.remember(req).unwrap().koid
}

/// Feed every live row of `Person` into every property index — the
/// maintainer catch-up (the runtime tests carry no scheduler; idx2-008).
fn catch_up_indexes(k: &Kernel) {
    for (id, _v, ts, state) in k.scan_heads().unwrap() {
        if state == LifecycleState::Deleted {
            continue;
        }
        if let Some(ko) = k.raw_object_at(&id, ts).unwrap() {
            if ko.metadata.type_name == "Person" {
                for idx in k.property_indexes().unwrap() {
                    idx.upsert(id, &ko).unwrap();
                }
            }
        }
    }
}

/// koids of the executed rows, for equality assertions.
fn exec_ids(rows: &aikoql_runtime::RowSet) -> Vec<KOID> {
    match rows {
        aikoql_runtime::RowSet::Objects(kos) => kos.iter().map(|ko| ko.koid).collect(),
        other => panic!("expected Objects, got {other:?}"),
    }
}

fn plan_of(_k: &Kernel, query: &str) -> IrPlan {
    parser::compile_with_subject(query, "alice").unwrap()
}

fn rule_based(k: &Kernel, query: &str) -> Vec<KOID> {
    let plan = plan_of(k, query);
    exec_ids(&Interpreter::execute(k, &plan).unwrap())
}

fn costed(k: &Kernel, query: &str) -> Vec<KOID> {
    let plan = plan_of(k, query);
    exec_ids(&Interpreter::execute_costed(k, &plan).unwrap())
}

fn scan_strategy(r: &CostReport) -> Strategy {
    r.plan.operators[0].strategy
}

/// Total cpu of a cost vector.
fn total_cpu(costs: &[aikoql_runtime::cbo::Cost]) -> u64 {
    costs.iter().map(|c| c.cpu).sum()
}

// --- cbo001 — highly selective property → index scan ----------------------------

#[test]
fn cbo001_highly_selective_property_chooses_the_index_scan() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..100 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    let query = "MATCH Person WHERE name == \"P042\" RETURN *";
    let report = cost_optimize(&k, &plan_of(&k, query)).unwrap();
    assert!(report.stats_used, "fresh stats drive the choice");
    assert!(!report.stats_stale);
    assert_eq!(scan_strategy(&report), Strategy::PropertyIndex);
    assert_eq!(report.index_used.as_deref(), Some("by_name"));

    // The assisted plan beats the rule-based plan on cost.
    let baseline = cost_plan(
        &PhysicalPlan::from_ops(plan_of(&k, query).operators.clone()).operators,
        Some(&k.statistics("Person").unwrap().unwrap()),
    );
    assert!(
        total_cpu(&report.costs) < total_cpu(&baseline),
        "index scan ({} cpu) cheaper than full scan ({} cpu)",
        total_cpu(&report.costs),
        total_cpu(&baseline)
    );

    // And returns exactly the committed truth.
    assert_eq!(costed(&k, query), rule_based(&k, query));
}

// --- cbo002 — zero selectivity gain → tie, the scan stays -----------------------

/// The M17b model prices the probe at ONE point read per matched row (the
/// M15 INDEX_ROW_COST is gone — cbo_default_005 pins the 50%-selectivity
/// production shape selecting the index). The remaining boundary: an Eq
/// every row satisfies ties the scan, and the strictly-cheaper guard keeps
/// the FullScan.
#[test]
fn cbo002_zero_selectivity_gain_ties_and_keeps_the_full_scan() {
    let k = mk();
    k.catalog_create_index("by_dept", "Person", &["dept"])
        .unwrap();
    for i in 0..100 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    let query = "MATCH Person WHERE dept == \"Eng\" RETURN *";
    let report = cost_optimize(&k, &plan_of(&k, query)).unwrap();
    assert!(report.stats_used);
    assert_eq!(
        scan_strategy(&report),
        Strategy::FullScan,
        "every row matches — probe + filter ties the scan, and the choice is strictly cheaper only"
    );
    assert!(report.index_used.is_none());
    assert_eq!(costed(&k, query), rule_based(&k, query));
}

// --- cbo003 — graph fanout drives the traversal cost ----------------------------

#[test]
fn cbo003_high_fanout_makes_the_traversal_cost_geometric() {
    let k = mk();
    // The relationship index is an EDGE SET — identical (src, rel_type, dst)
    // triples collapse to one entry — so the fanout has to come from
    // DISTINCT targets, not repeated edges.
    let targets: Vec<KOID> = (0..10)
        .map(|i| {
            let mut req = RememberRequest::create(alice(), meta("Node"));
            req.properties.insert("id".into(), Value::Int(i));
            k.remember(req).unwrap().koid
        })
        .collect();
    for i in 0..10 {
        let id = {
            let mut req = RememberRequest::create(alice(), meta("Hub"));
            req.properties.insert("id".into(), Value::Int(i));
            k.remember(req).unwrap().koid
        };
        let mut req = RememberRequest::update(alice(), id, meta("Hub"));
        for target in &targets {
            req.relationships.push(RelationshipRef {
                rel_type: "knows".into(),
                target: *target,
                direction: Direction::Outbound,
            });
        }
        k.remember(req).unwrap();
    }
    for i in 0..10 {
        let id = {
            let mut req = RememberRequest::create(alice(), meta("Leaf"));
            req.properties.insert("id".into(), Value::Int(i));
            k.remember(req).unwrap().koid
        };
        let mut req = RememberRequest::update(alice(), id, meta("Leaf"));
        req.relationships.push(RelationshipRef {
            rel_type: "knows".into(),
            target: targets[0],
            direction: Direction::Outbound,
        });
        k.remember(req).unwrap();
    }
    k.analyze("Hub").unwrap();
    k.analyze("Leaf").unwrap();

    let traverse = |type_name: &str| -> IrPlan {
        IrPlan::new(vec![
            IrOp::Scan {
                type_name: type_name.into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::Traverse {
                start_koid: String::new(),
                rel_type: Some("knows".into()),
                depth: 2,
            },
        ])
    };
    let high = cost_optimize(&k, &traverse("Hub")).unwrap();
    let low = cost_optimize(&k, &traverse("Leaf")).unwrap();
    // rows = start × fanout^depth: 10×10² = 1000 vs 10×1² = 10.
    assert_eq!(high.costs[1].rows, 1000, "geometric in the fanout");
    assert_eq!(low.costs[1].rows, 10);
    assert!(total_cpu(&high.costs) > total_cpu(&low.costs));
}

// --- cbo004 — vector density drives the ANN cost --------------------------------

#[test]
fn cbo004_vector_heavy_types_cost_their_candidates() {
    let k = mk();
    let embed = |name: &str, emb: bool| {
        let mut req = RememberRequest::create(alice(), meta("Doc"));
        req.properties
            .insert("name".into(), Value::Text(name.into()));
        if emb {
            req.semantic = Some(SemanticBlock {
                embedding_model: Some("bge-m3".into()),
                embedding: Some(vec![0.1, 0.2]),
                confidence: None,
                source: None,
                summary: None,
            });
        }
        k.remember(req).unwrap()
    };
    for i in 0..8 {
        embed(&format!("d{i}"), false);
    }
    embed("v1", true);
    embed("v2", true);
    k.analyze("Doc").unwrap(); // density 0.2

    let plan = IrPlan::new(vec![
        IrOp::Scan {
            type_name: "Doc".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        },
        IrOp::AnnSearch {
            vector: vec![0.5, 0.5],
            query_text: None,
            embedding_model: Some("bge-m3".into()),
            k: 3,
        },
    ]);
    let report = cost_optimize(&k, &plan).unwrap();
    // candidates = rows × density = 2; cpu = candidates × dim.
    assert_eq!(report.costs[1].rows, 2);
    assert_eq!(report.costs[1].cpu, 2 * aikoql_runtime::cbo::EMBEDDING_DIM);
    // The M3 rule strategy survives the CBO (the executor still dispatches on
    // the op; the cost is what the optimizer knows now).
    assert_eq!(report.plan.operators[1].strategy, Strategy::VectorIndex);
}

// --- cbo005 — the text path costs as delegation ---------------------------------

#[test]
fn cbo005_text_search_costs_the_bm25_path() {
    let k = mk();
    for i in 0..10 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    k.analyze("Person").unwrap();
    let plan = IrPlan::new(vec![
        IrOp::Scan {
            type_name: "Person".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        },
        IrOp::TextSearch {
            query: "alice".into(),
            k: 3,
            scoring: Some("bm25".into()),
        },
    ]);
    let report = cost_optimize(&k, &plan).unwrap();
    assert_eq!(report.costs[1].rows, 10, "delegated search walks the rows");
    assert_eq!(report.costs[1].cpu, 10, "the kernel index does the scoring");
    assert_eq!(report.plan.operators[1].strategy, Strategy::TextIndex);
}

// --- cbo006 — hybrid: modality order pinned, no fusion rewrite ------------------

#[test]
fn cbo006_hybrid_pins_modality_order_and_costs_each_leg() {
    let k = mk();
    let embed = |name: &str, emb: bool| {
        let mut req = RememberRequest::create(alice(), meta("Doc"));
        req.properties
            .insert("name".into(), Value::Text(name.into()));
        if emb {
            req.semantic = Some(SemanticBlock {
                embedding_model: Some("bge-m3".into()),
                embedding: Some(vec![0.1, 0.2]),
                confidence: None,
                source: None,
                summary: None,
            });
        }
        k.remember(req).unwrap()
    };
    for i in 0..5 {
        embed(&format!("d{i}"), false);
    }
    for i in 0..5 {
        embed(&format!("v{i}"), true);
    }
    k.analyze("Doc").unwrap(); // density 0.5

    let hybrid = |fuse: FuseMode| -> IrPlan {
        IrPlan::new(vec![
            IrOp::Scan {
                type_name: "Doc".into(),
                subject: "alice".into(),
                roles: vec![],
                tenant: None,
            },
            IrOp::AnnSearch {
                vector: vec![0.5, 0.5],
                query_text: None,
                embedding_model: Some("bge-m3".into()),
                k: 5,
            },
            IrOp::TextSearch {
                query: "x".into(),
                k: 5,
                scoring: Some("bm25".into()),
            },
            IrOp::Fuse { mode: fuse },
        ])
    };
    let weighted = FuseMode::Weighted { wv: 0.5, wt: 0.5 };
    let report = cost_optimize(&k, &hybrid(weighted.clone())).unwrap();

    // The physical plan keeps the grammar order — ANN first, then text, Fuse
    // last — and the CBO never rewrites the fusion (scores are oracle bits).
    assert!(matches!(
        report.plan.operators[1].op,
        IrOp::AnnSearch { .. }
    ));
    assert!(matches!(
        report.plan.operators[2].op,
        IrOp::TextSearch { .. }
    ));
    assert_eq!(
        report.plan.operators[3].op,
        IrOp::Fuse {
            mode: weighted.clone()
        },
        "fusion untouched"
    );
    // Vector-heavy (density 0.5): the ANN leg dominates the text leg.
    assert!(report.costs[1].cpu > report.costs[2].cpu);

    // Text-heavy (no embeddings): the text leg dominates.
    let k2 = mk();
    for i in 0..10 {
        embed_text_only(&k2, &format!("t{i}"));
    }
    k2.analyze("Doc").unwrap();
    let report2 = cost_optimize(&k2, &hybrid(weighted)).unwrap();
    assert!(report2.costs[1].cpu < report2.costs[2].cpu);
}

fn embed_text_only(k: &Kernel, name: &str) {
    let mut req = RememberRequest::create(alice(), meta("Doc"));
    req.properties
        .insert("name".into(), Value::Text(name.into()));
    k.remember(req).unwrap();
}

// --- cbo007 — temporal density drives the snapshot cost -------------------------

#[test]
fn cbo007_temporal_plans_cost_the_version_reconstruction() {
    let k = mk();
    for i in 0..6 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    for i in 0..4 {
        let mut req = RememberRequest::create(alice(), meta("Person"));
        req.properties
            .insert("name".into(), Value::Text(format!("T{i}")));
        req.extensions
            .insert("valid_from".into(), Value::Int(10_000));
        k.remember(req).unwrap();
    }
    k.analyze("Person").unwrap(); // temporal density 0.4

    let plan = IrPlan::new(vec![
        IrOp::Scan {
            type_name: "Person".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        },
        IrOp::Temporal {
            op: TemporalOp::AsOf(12_000),
        },
    ]);
    let report = cost_optimize(&k, &plan).unwrap();
    // cpu = rows × (1 + temporal_density): 10 × 1.4 = 14.
    assert_eq!(report.costs[1].rows, 10);
    assert_eq!(report.costs[1].cpu, 14);
}

// --- cbo008 — stale statistics: the plan is not silently wrong ------------------

#[test]
fn cbo008_stale_statistics_fall_back_to_the_rules() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..100 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    // A later write makes the stats stale.
    let late = person(&k, "Late", "Eng");
    let query = "MATCH Person WHERE name == \"P042\" RETURN *";
    let report = cost_optimize(&k, &plan_of(&k, query)).unwrap();
    assert!(report.stats_stale, "staleness is detected");
    assert!(!report.stats_used, "stale stats never drive the plan");
    assert_eq!(
        scan_strategy(&report),
        Strategy::FullScan,
        "rule-based fallback, never worse than today"
    );
    assert!(report.index_used.is_none());
    let _ = late;
}

#[test]
fn cbo008b_an_index_that_has_not_caught_up_is_not_used() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..10 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    // One row the index never saw — created after catch-up.
    let unseen = person(&k, "Unseen", "Eng");
    k.analyze("Person").unwrap(); // stats fresh, index NOT caught up

    let query = "MATCH Person WHERE name == \"Unseen\" RETURN *";
    let report = cost_optimize(&k, &plan_of(&k, query)).unwrap();
    assert!(
        report.stats_used,
        "stats are fresh — the index is the rejected piece"
    );
    assert_eq!(scan_strategy(&report), Strategy::FullScan);
    assert!(report.index_used.is_none());
    // Not silently wrong: the unindexed row answers the committed truth.
    let rows = costed(&k, query);
    assert_eq!(rows, vec![unseen]);
}

// --- cbo009 — EXPLAIN COST ------------------------------------------------------

#[test]
fn cbo009_explain_cost_shows_per_op_costs_and_stats_freshness() {
    let k = mk();
    let lines =
        aikoql_runtime::cbo::explain_cost(&k, "MATCH Person WHERE name == \"P042\" RETURN *")
            .unwrap();
    assert_eq!(lines[0], " 0: Scan [FullScan] rows=? cpu=?");
    assert!(
        lines.last().is_some_and(|l| l.contains("statistics: none")),
        "no stats → rule-based, visible"
    );

    let k2 = mk();
    k2.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..100 {
        person(&k2, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k2);
    k2.analyze("Person").unwrap();
    let lines =
        aikoql_runtime::cbo::explain_cost(&k2, "MATCH Person WHERE name == \"P042\" RETURN *")
            .unwrap();
    assert!(
        lines[0].contains("[PropertyIndex]"),
        "the assisted access path is visible: {}",
        lines[0]
    );
    assert!(lines[0].contains("rows=") && lines[0].contains("cpu="));
    assert!(
        lines.iter().any(|l| l.contains("statistics: fresh")),
        "fresh stats, visible"
    );

    // Stale case.
    person(&k2, "Late", "Eng");
    let lines =
        aikoql_runtime::cbo::explain_cost(&k2, "MATCH Person WHERE name == \"P042\" RETURN *")
            .unwrap();
    assert!(
        lines[0].contains("[FullScan]"),
        "stale → fallback: {}",
        lines[0]
    );
    assert!(
        lines
            .last()
            .is_some_and(|l| l.contains("statistics: stale")),
        "staleness is visible"
    );
}

// --- cbo010 — gate-6 oracle over CBO scenarios ----------------------------------

#[test]
fn cbo010_plan_equivalence_oracle_green_over_cbo_scenarios() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..50 {
        person(
            &k,
            &format!("P{i:03}"),
            if i % 2 == 0 { "Eng" } else { "Ops" },
        );
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    let q = |s: &str| OracleEntry {
        id: s.into(),
        baseline: plan_of(&k, s),
        candidate: plan_of(&k, s),
    };
    let entries = vec![
        q("MATCH Person WHERE name == \"P042\" RETURN *"), // index-assisted
        q("MATCH Person WHERE dept == \"Eng\" RETURN *"),  // full scan
        q("MATCH Person RETURN *"),                        // no filter
        q("MATCH Person WHERE dept == \"Eng\" RETURN name, dept ORDER BY name LIMIT 10"),
        q("MATCH Person WHERE dept == \"Eng\" RETURN dept GROUP BY dept"),
        q("MATCH Person AS_OF 15000 RETURN *"),
    ];
    let report = run_costed(&k, &entries);
    assert_eq!(
        report.divergences,
        Vec::<String>::new(),
        "CBO plans never change results (gate-6)"
    );
}

// --- st011 — the assisted plan states its snapshot (PR6 P0-07) ------------------

/// Every index-assisted plan must state the journal head it was optimized
/// against — the snapshot its index read is assumed fresh at. Today EXPLAIN
/// renders no such line, so the freshness assumption is invisible.
#[test]
fn st011_explain_states_the_snapshot_of_an_index_assist() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..100 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    let lines =
        aikoql_runtime::cbo::explain_cost(&k, "MATCH Person WHERE name == \"P042\" RETURN *")
            .unwrap();
    assert!(
        lines.iter().any(|l| l.starts_with("snapshot: ")),
        "an index-assisted plan states its snapshot: {:?}",
        lines
    );
    let head = k.journal_head().unwrap().0;
    let line = lines.iter().find(|l| l.starts_with("snapshot: ")).unwrap();
    assert!(
        line.contains(&format!("{head}")),
        "the stated snapshot is the pinned journal head: {line}"
    );
}

// --- cbo011 — write between optimize and execute (PR6 P0-07) --------------------

/// The optimizer verifies the index at optimize time; a committed write
/// between optimize and execute must not produce a silently incomplete set —
/// the executor re-pins the journal head and falls back to the full scan.
/// Today the stale assisted plan serves the index's answer and drops the
/// late row.
#[test]
fn cbo011_write_between_optimize_and_execute_never_serves_an_incomplete_set() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..100 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    let query = "MATCH Person WHERE name == \"P042\" RETURN *";
    let report = cost_optimize(&k, &plan_of(&k, query)).unwrap();
    assert!(
        report.index_used.is_some(),
        "precondition: the optimizer chose the index assist"
    );

    // A committed write the index never saw, between optimize and execute.
    let late = person(&k, "P042", "Ops");

    // Executing the OLD plan: the head moved, so the executor must fall back
    // to the full scan — the committed row is never silently dropped.
    let rows = Interpreter::execute_physical(&k, &report.plan).unwrap();
    let ids = exec_ids(&rows);
    assert!(
        ids.contains(&late),
        "the late committed row is never silently dropped"
    );
    assert_eq!(
        ids.len(),
        2,
        "the set is complete: the original row and the late one"
    );
}

// --- cbo012 — positional adjacency (PR6 P1-13) ----------------------------------

/// The index assist binds only to the Filter immediately after the Scan —
/// the plan-shape dependency is positional. A plan with a Project between
/// Scan and Filter is never index-assisted, however covering the index.
/// Today `first_eq_after` scans every later op for any Eq Filter and assists.
#[test]
fn cbo012_a_non_adjacent_filter_is_never_index_assisted() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..100 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    // Plan shape: Scan → Project → Filter — the Eq predicate is not the op
    // immediately after the Scan.
    let plan = IrPlan::new(vec![
        IrOp::Scan {
            type_name: "Person".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        },
        IrOp::Project {
            fields: vec!["name".into()],
        },
        IrOp::Filter {
            predicates: vec![Predicate {
                property: "name".into(),
                op: PredOp::Eq,
                value: Value::Text("P042".into()),
            }],
        },
    ]);
    let report = cost_optimize(&k, &plan).unwrap();
    assert_eq!(
        scan_strategy(&report),
        Strategy::FullScan,
        "no index assist for a non-adjacent Filter"
    );
    assert!(report.index_used.is_none());
}

// --- idx4-004 — a DROPPING index is never chosen by the CBO (PR6 P1-16) ---------

/// The CBO's choice gate is the index's lifecycle state, not just fresh
/// stats: a DROPPING index must never be chosen. The drop is parked after
/// its durable mark and the statistics are re-analyzed INSIDE the window —
/// fresh, so the stats gate alone admits the assist, and the registry entry
/// is the only thing left to gate on. Today the dropped index serves the
/// assist.
#[test]
fn idx4_004_a_dropping_index_is_never_chosen_by_the_cbo() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    for i in 0..100 {
        person(&k, &format!("P{i:03}"), "Eng");
    }
    catch_up_indexes(&k);
    k.analyze("Person").unwrap();

    let query = "MATCH Person WHERE name == \"P042\" RETURN *";
    let report = cost_optimize(&k, &plan_of(&k, query)).unwrap();
    assert!(
        report.index_used.is_some(),
        "precondition: the optimizer chose the index assist"
    );

    std::env::set_var("INDEX_DROP_PARK", "1");
    let h = k.clone_handle();
    let dropper = std::thread::spawn(move || h.catalog_drop_index("by_name").unwrap());
    let mut waited = 0u64;
    while std::env::var_os("INDEX_DROP_PARK_AT").is_none() && waited < 10_000 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        waited += 10;
    }
    assert!(
        std::env::var_os("INDEX_DROP_PARK_AT").is_some(),
        "precondition: the drop is parked in the window"
    );

    // Fresh statistics inside the parked window — only the state gate can
    // refuse the assist now.
    k.analyze("Person").unwrap();
    let during = cost_optimize(&k, &plan_of(&k, query)).unwrap();

    std::env::remove_var("INDEX_DROP_PARK");
    dropper.join().unwrap();
    std::env::remove_var("INDEX_DROP_PARK_AT");

    assert_eq!(
        scan_strategy(&during),
        Strategy::FullScan,
        "a DROPPING index is never chosen by the CBO"
    );
    assert!(
        during.index_used.is_none(),
        "the assist names no dropping index"
    );
}

// --- ann006 — cost rows price the LIVE ANN dim (PR6 P1-05) --------------------

/// The v1 cost model prices AnnSearch at rows × EMBEDDING_DIM (768,
/// bge-m3 class). The constant is wrong the moment the attached ANN adopts
/// any other dim — the row must price the live index's dim, and fall back
/// to the 768 model default only when no maintainer is attached. Today the
/// row is always 768: a live dim-2 index still prices rows × 768.
#[test]
fn ann006_cost_rows_use_the_live_ann_dim() {
    let k = mk();
    // One person with a 2-d embedding, one without — vector_density = 0.5.
    let mut req = RememberRequest::create(alice(), meta("Person"));
    req.properties
        .insert("name".into(), Value::Text("Vec".into()));
    req.semantic = Some(SemanticBlock {
        embedding_model: Some("m".into()),
        embedding: Some(vec![1.0, 0.0]),
        confidence: None,
        source: None,
        summary: None,
    });
    k.remember(req).unwrap();
    person(&k, "Plain", "Eng");
    k.analyze("Person").unwrap();

    let m = aikoql_scheduler::IndexMaintainer::start(
        &k,
        Arc::new(aikoql_vector::HnswVectorIndex::new(2, 100)),
        Arc::new(TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m.clone());

    let plan = IrPlan::new(vec![
        IrOp::Scan {
            type_name: "Person".into(),
            subject: "alice".into(),
            roles: vec![],
            tenant: None,
        },
        IrOp::AnnSearch {
            vector: vec![1.0, 0.0],
            query_text: None,
            embedding_model: Some("m".into()),
            k: 1,
        },
    ]);
    let report = cost_optimize(&k, &plan).unwrap();
    assert_eq!(report.costs.len(), 2, "one cost row per operator");
    let ann = report.costs[1];
    assert!(
        ann.rows > 0,
        "vector density must price a positive candidate set"
    );
    assert_eq!(
        ann.cpu,
        ann.rows * 2,
        "cpu must price the live ANN dim 2 (got {} for {} rows)",
        ann.cpu,
        ann.rows
    );
    m.shutdown();

    // Without a maintainer the 768 model default stays the fallback.
    let k2 = mk();
    let mut req = RememberRequest::create(alice(), meta("Person"));
    req.semantic = Some(SemanticBlock {
        embedding_model: Some("m".into()),
        embedding: Some(vec![1.0, 0.0]),
        confidence: None,
        source: None,
        summary: None,
    });
    k2.remember(req).unwrap();
    person(&k2, "Plain", "Eng");
    k2.analyze("Person").unwrap();
    let report = cost_optimize(&k2, &plan).unwrap();
    let ann = report.costs[1];
    assert!(ann.rows > 0);
    assert_eq!(ann.cpu, ann.rows * 768, "no live ANN → the 768 model default");
}
