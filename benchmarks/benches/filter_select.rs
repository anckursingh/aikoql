//! AG-21: structured filters by selectivity — warm p50 of a KOQL query
//! with and without property indexes, plus the index build time.
//!
//!   AIKOQL_BENCH_SCALE=100000 cargo bench -p aikoql-benchmarks --bench filter_select
//!
//! Cells: eq 1% (topic), eq 0.1% (tag), range 0.1% (seq >=), composite
//! (topic AND status, 0.5%), bulk 50% (status — result transfer, not a
//! filter test). Prints one JSON line per cell. Machine-dependent.

use aikoql_compiler::parser;
use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use aikoql_runtime::{Interpreter, RowSet};
use std::sync::Arc;
use std::time::Instant;

const RUNS: usize = 20;

fn rows(r: &RowSet) -> usize {
    match r {
        RowSet::Objects(kos) => kos.len(),
        other => panic!("expected Objects, got {}", other.shape()),
    }
}

fn p50(k: &Kernel, query: &str) -> (f64, usize) {
    let plan = parser::compile_with_subject(query, "alice").unwrap();
    let n = rows(&Interpreter::execute(k, &plan).unwrap()); // warm
    let mut ms: Vec<f64> = (0..RUNS)
        .map(|_| {
            let t = Instant::now();
            assert_eq!(rows(&Interpreter::execute(k, &plan).unwrap()), n);
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    (ms[RUNS / 2], n)
}

fn main() {
    let n: usize = std::env::var("AIKOQL_BENCH_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let k = Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(20_000)),
        0xC0FFEE,
    )
    .unwrap();
    let alice = KnowledgeContext::new(Subject::new("alice"));
    for i in 0..n {
        let mut req = RememberRequest::create(
            alice.clone(),
            Metadata {
                type_name: "note".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
        );
        let p = &mut req.properties;
        p.insert("topic".into(), Value::Text(format!("t{}", i % 100)));
        p.insert("tag".into(), Value::Text(format!("g{}", i % 1000)));
        p.insert("status".into(), Value::Text(format!("s{}", i % 2)));
        p.insert("seq".into(), Value::Int(i as i64));
        k.remember(req).unwrap();
    }
    let cells = [
        (
            "eq_1pct",
            "MATCH note WHERE topic == \"t7\" RETURN *".to_string(),
        ),
        (
            "eq_0.1pct",
            "MATCH note WHERE tag == \"g7\" RETURN *".to_string(),
        ),
        (
            "range_0.1pct",
            format!("MATCH note WHERE seq >= {} RETURN *", n - n / 1000),
        ),
        (
            "composite_0.5pct",
            "MATCH note WHERE topic == \"t7\" AND status == \"s1\" RETURN *".to_string(),
        ),
        (
            "bulk_50pct",
            "MATCH note WHERE status == \"s1\" RETURN *".to_string(),
        ),
    ];
    let scan: Vec<(f64, usize)> = cells.iter().map(|(_, q)| p50(&k, q)).collect();

    for (name, props) in [
        ("by_topic", &["topic"][..]),
        ("by_tag", &["tag"]),
        ("by_seq", &["seq"]),
        ("by_status", &["status"]),
        ("by_topic_status", &["topic", "status"]),
    ] {
        k.catalog_create_index(name, "note", props).unwrap();
    }
    // the maintainer's catch-up, timed: every live row into every index
    let t = Instant::now();
    let idx = k.property_indexes().unwrap();
    for (id, _v, ts, state) in k.scan_heads().unwrap() {
        if state == LifecycleState::Deleted {
            continue;
        }
        if let Some(ko) = k.raw_object_at(&id, ts).unwrap() {
            for i in &idx {
                i.upsert(id, &ko).unwrap();
            }
        }
    }
    let build_s = t.elapsed().as_secs_f64();

    k.analyze("note").unwrap();
    // analyze commits; then the maintainer stamps each index with the seq it applied (the freshness
    // proof the CBO's verify gate short-circuits on)
    let (head, _) = k.journal_head().unwrap();
    for i in &idx {
        i.set_applied_seq(head);
    }
    let measured: Vec<(f64, usize)> = cells.iter().map(|(_, q)| p50(&k, q)).collect();
    // AG-21b: 100 live writes the indexes have not applied — the eq cell
    // must stay indexed (stale stats, lagging index, tail-patched)
    for i in 0..100 {
        let mut req = RememberRequest::create(
            alice.clone(),
            Metadata {
                type_name: "note".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
        );
        req.properties
            .insert("tag".into(), Value::Text(format!("g{}", i % 1000)));
        k.remember(req).unwrap();
    }
    let (live_ms, live_rows) = p50(&k, &cells[1].1);
    println!(
        "{{\"cell\":\"eq_0.1pct_live_writes\",\"n\":{n},\"rows\":{live_rows},\"unapplied\":100,\"indexed_p50_ms\":{live_ms:.3}}}"
    );
    for (((cell, _), (scan_ms, rows)), (ms, r)) in cells.iter().zip(scan).zip(measured) {
        assert_eq!(r, rows, "{cell}: indexed rows match the scan");
        println!(
            "{{\"cell\":\"{cell}\",\"n\":{n},\"rows\":{rows},\"scan_p50_ms\":{scan_ms:.3},\
             \"indexed_p50_ms\":{ms:.3},\"index_build_s\":{build_s:.2}}}"
        );
    }
}
