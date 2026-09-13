//! P5-M5 (ND-05) — aggregation + sorting. ag001–ag008.
//!
//! The roadmap's ND-05 RED list: nulls, empty sets, duplicate values, mixed
//! types, large groups, deterministic ordering, snapshots, authorization
//! before aggregation. Aggregate output is `RowSet::Grouped(Vec<PropertyMap>)`:
//! one flat property map per group — group keys plus one entry per aggregate
//! call. Naming: `count` for COUNT(*), `func(field)` otherwise (e.g.
//! `sum(age)`). Groups appear in first-encounter order; ORDER BY makes order
//! explicit. SQL-style null handling: SUM/AVG/MIN/MAX ignore Null, COUNT(*)
//! counts every row, COUNT(field) counts non-null values, a missing group key
//! groups under Null. Global aggregate (no keys) over empty input = one row
//! (count=0, everything else Null); grouped input over empty = zero rows.
//!
//! Honest ledger: v1 aggregates in memory (hash grouping) — the spill
//! strategy is documented in IMPLEMENTATION-PLAN-PHASE5.md, not exercised
//! here; the memory-bound cell rides with the W-suite sampler like st004.

use aikoql_compiler::parser;
use aikoql_kernel::transaction::kernel::{KnowledgeContext, Subject};
use aikoql_kernel::*;
use aikoql_runtime::{Interpreter, RowSet};
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

fn ctx(name: &str) -> KnowledgeContext {
    KnowledgeContext::new(Subject::new(name))
}

/// Remember a Person with `dept`, `age`, `salary` set when provided.
fn person(
    k: &Kernel,
    name: &str,
    dept: Option<&str>,
    age: Option<i64>,
    salary: Option<i64>,
) -> KOID {
    let mut req = RememberRequest::create(
        ctx("alice"),
        Metadata {
            type_name: "Person".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("name".into(), Value::Text(name.into()));
    if let Some(d) = dept {
        req.properties.insert("dept".into(), Value::Text(d.into()));
    }
    if let Some(a) = age {
        req.properties.insert("age".into(), Value::Int(a));
    }
    if let Some(s) = salary {
        req.properties.insert("salary".into(), Value::Int(s));
    }
    k.remember(req).unwrap().koid
}

/// Execute `query` as alice; expect Grouped output.
fn groups(k: &Kernel, query: &str) -> Vec<aikoql_kernel::knowledge::kom::PropertyMap> {
    let plan = parser::compile_with_subject(query, "alice").unwrap();
    match Interpreter::execute(k, &plan).unwrap() {
        RowSet::Grouped(g) => g,
        other => panic!("expected Grouped, got {:?}", other),
    }
}

/// Execute `query` as alice; expect Objects output.
fn objects(k: &Kernel, query: &str) -> Vec<KnowledgeObject> {
    let plan = parser::compile_with_subject(query, "alice").unwrap();
    match Interpreter::execute(k, &plan).unwrap() {
        RowSet::Objects(kos) => kos,
        other => panic!("expected Objects, got {:?}", other),
    }
}

// --- ag001 — empty sets ---------------------------------------------------------

#[test]
fn ag001_empty_sets_global_agg_one_row_grouped_zero() {
    let k = mk();
    // Global aggregate (no keys) over empty input: one row, count=0, the
    // other aggregates Null (absent value = the null proxy).
    let g = groups(&k, "MATCH Person GROUP BY COUNT(*), SUM(age) RETURN *");
    assert_eq!(g.len(), 1, "global aggregate emits exactly one row");
    assert_eq!(g[0].get("count"), Some(&Value::Int(0)));
    assert_eq!(g[0].get("sum(age)"), Some(&Value::Null));
    // Grouped (with keys) over empty input: zero groups.
    let g = groups(&k, "MATCH Person GROUP BY dept, COUNT(*) RETURN *");
    assert!(g.is_empty(), "no rows → no groups");
}

// --- ag002 — duplicate values, exact aggregates -----------------------------------

#[test]
fn ag002_groups_over_duplicates_pin_exact_values() {
    let k = mk();
    // First-encounter order: eng, sales, eng → groups [eng, sales].
    person(&k, "A", Some("eng"), Some(30), Some(100));
    person(&k, "B", Some("sales"), Some(20), Some(50));
    person(&k, "C", Some("eng"), Some(30), Some(200));
    let g = groups(
        &k,
        "MATCH Person GROUP BY dept, COUNT(*), SUM(salary), AVG(age), MIN(age), MAX(age) RETURN *",
    );
    assert_eq!(g.len(), 2);
    let eng = &g[0];
    assert_eq!(eng.get("dept"), Some(&Value::Text("eng".into())));
    assert_eq!(eng.get("count"), Some(&Value::Int(2)));
    assert_eq!(eng.get("sum(salary)"), Some(&Value::Int(300)));
    assert_eq!(eng.get("avg(age)"), Some(&Value::Float(30.0)));
    assert_eq!(eng.get("min(age)"), Some(&Value::Int(30)));
    assert_eq!(eng.get("max(age)"), Some(&Value::Int(30)));
    let sales = &g[1];
    assert_eq!(sales.get("dept"), Some(&Value::Text("sales".into())));
    assert_eq!(sales.get("count"), Some(&Value::Int(1)));
    assert_eq!(sales.get("sum(salary)"), Some(&Value::Int(50)));
}

// --- ag003 — nulls and missing fields ----------------------------------------------

#[test]
fn ag003_null_semantics_skip_missing_count_star_counts_all() {
    let k = mk();
    person(&k, "A", Some("eng"), Some(30), Some(100));
    person(&k, "B", Some("eng"), Some(25), None); // salary missing
    person(&k, "C", None, Some(40), Some(300)); // dept missing → Null group
    let g = groups(
        &k,
        "MATCH Person GROUP BY dept, COUNT(*), COUNT(salary), SUM(salary), AVG(age) RETURN *",
    );
    assert_eq!(g.len(), 2, "eng + the Null-dept group");
    let eng = g
        .iter()
        .find(|m| m.get("dept") == Some(&Value::Text("eng".into())))
        .unwrap();
    assert_eq!(
        eng.get("count"),
        Some(&Value::Int(2)),
        "COUNT(*) counts every row"
    );
    assert_eq!(
        eng.get("count(salary)"),
        Some(&Value::Int(1)),
        "COUNT(field) counts non-null"
    );
    assert_eq!(
        eng.get("sum(salary)"),
        Some(&Value::Int(100)),
        "SUM skips nulls"
    );
    assert_eq!(eng.get("avg(age)"), Some(&Value::Float(27.5)));
    let null_dept = g
        .iter()
        .find(|m| m.get("dept") == Some(&Value::Null))
        .unwrap();
    assert_eq!(
        null_dept.get("count"),
        Some(&Value::Int(1)),
        "missing key groups under Null"
    );
}

// --- ag004 — mixed types ------------------------------------------------------------

#[test]
fn ag004_mixed_types_error_not_panic_and_int_float_promotes() {
    let k = mk();
    person(&k, "A", Some("eng"), Some(30), None);
    person(&k, "B", Some("eng"), Some(25), None);
    // age as Text on one row: SUM must fail closed with a precise error.
    let mut req = RememberRequest::create(
        ctx("alice"),
        Metadata {
            type_name: "Person".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("name".into(), Value::Text("C".into()));
    req.properties
        .insert("dept".into(), Value::Text("eng".into()));
    req.properties
        .insert("age".into(), Value::Text("old".into()));
    k.remember(req).unwrap();
    let plan =
        parser::compile_with_subject("MATCH Person GROUP BY dept, SUM(age) RETURN *", "alice")
            .unwrap();
    let err = Interpreter::execute(&k, &plan).unwrap_err();
    assert!(
        err.to_string().contains("mixed"),
        "SUM over mixed types fails closed, got: {}",
        err
    );

    // Int + Float mix in one group: promotes to Float (no error).
    let k2 = mk();
    person(&k2, "A", Some("eng"), Some(30), None);
    let mut req = RememberRequest::create(
        ctx("alice"),
        Metadata {
            type_name: "Person".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("name".into(), Value::Text("B".into()));
    req.properties
        .insert("dept".into(), Value::Text("eng".into()));
    req.properties.insert("age".into(), Value::Float(2.5));
    k2.remember(req).unwrap();
    let g = groups(&k2, "MATCH Person GROUP BY dept, SUM(age) RETURN *");
    assert_eq!(g[0].get("sum(age)"), Some(&Value::Float(32.5)));
}

// --- ag005 — deterministic ordering ---------------------------------------------------

#[test]
fn ag005_order_by_asc_desc_multikey_stable_and_over_aggregates() {
    let k = mk();
    person(&k, "B", Some("eng"), Some(30), None);
    person(&k, "A", Some("sales"), Some(20), None);
    person(&k, "C", Some("eng"), Some(25), None);
    // ASC, DESC, and multi-key (dept ASC, age DESC within).
    let asc: Vec<String> = objects(&k, "MATCH Person ORDER BY name ASC RETURN name")
        .iter()
        .map(|ko| match &ko.properties["name"] {
            Value::Text(t) => t.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(asc, vec!["A", "B", "C"]);
    let desc: Vec<String> = objects(&k, "MATCH Person ORDER BY name DESC RETURN name")
        .iter()
        .map(|ko| match &ko.properties["name"] {
            Value::Text(t) => t.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(desc, vec!["C", "B", "A"]);
    let multi: Vec<String> = objects(
        &k,
        "MATCH Person ORDER BY dept ASC, age DESC RETURN dept, age, name",
    )
    .iter()
    .map(|ko| match &ko.properties["name"] {
        Value::Text(t) => t.clone(),
        _ => unreachable!(),
    })
    .collect();
    assert_eq!(multi, vec!["B", "C", "A"], "dept ASC, age DESC within eng");
    // ORDER BY over aggregate output (Sort consumes Grouped).
    let g = groups(
        &k,
        "MATCH Person GROUP BY dept, COUNT(*) ORDER BY count DESC RETURN *",
    );
    assert_eq!(g[0].get("dept"), Some(&Value::Text("eng".into())));
    assert_eq!(g[0].get("count"), Some(&Value::Int(2)));
    assert_eq!(g[1].get("count"), Some(&Value::Int(1)));
}

// --- ag006 — large groups -------------------------------------------------------------

#[test]
fn ag006_large_group_counts_and_sums_exactly() {
    let k = mk();
    for i in 0..5_000i64 {
        person(&k, &format!("P{i}"), Some("eng"), Some(1), Some(i));
    }
    let g = groups(
        &k,
        "MATCH Person GROUP BY dept, COUNT(*), SUM(salary) RETURN *",
    );
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].get("count"), Some(&Value::Int(5_000)));
    assert_eq!(
        g[0].get("sum(salary)"),
        Some(&Value::Int(5_000 * 4_999 / 2))
    );
}

// --- ag007 — snapshots (AS OF) ---------------------------------------------------------

#[test]
fn ag007_as_of_aggregates_the_historical_version_set() {
    let (k, clock) = {
        let k = mk();
        let clock = Arc::new(ManualClock::new(10_000));
        // mk() already opened a kernel with its own clock; rebuild with the
        // clock handle we can advance.
        let _ = k;
        (
            Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0xC0FFEE).unwrap(),
            clock,
        )
    };
    let id = person(&k, "P", Some("eng"), Some(10), None); // v1 at t=10_000
                                                           // Between commits (after v1, before v2): the group sees v1 (age 10).
    let g = groups(
        &k,
        "MATCH Person AS_OF 12500 GROUP BY dept, SUM(age) RETURN *",
    );
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].get("sum(age)"), Some(&Value::Int(10)));

    clock.tick(5_000); // t=15_000
    let mut upd = RememberRequest::update(
        ctx("alice"),
        id,
        Metadata {
            type_name: "Person".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    upd.properties.insert("age".into(), Value::Int(20)); // v2 at t=15_000
    k.remember(upd).unwrap();
    clock.tick(5_000); // t=20_000 (now)

    // After the second commit: v2 (age 20); without AS OF, current truth agrees.
    let g = groups(
        &k,
        "MATCH Person AS_OF 17500 GROUP BY dept, SUM(age) RETURN *",
    );
    assert_eq!(g[0].get("sum(age)"), Some(&Value::Int(20)));
    let g = groups(&k, "MATCH Person GROUP BY dept, SUM(age) RETURN *");
    assert_eq!(g[0].get("sum(age)"), Some(&Value::Int(20)));
}

// --- ag008 — authorization before aggregation -------------------------------------------

#[test]
fn ag008_authorization_filters_rows_before_grouping() {
    let k = mk();
    let bob_visible = SecurityDescriptor {
        owner: "alice".into(),
        acl: vec![
            AclEntry {
                principal: "bob".into(),
                action: Action::Read,
                effect: Effect::Allow,
            },
            AclEntry {
                principal: "alice".into(),
                action: Action::Read,
                effect: Effect::Allow,
            },
        ],
        classification: None,
    };
    for i in 0..2 {
        let mut req = RememberRequest::create(
            ctx("alice"),
            Metadata {
                type_name: "Person".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
        );
        req.properties
            .insert("name".into(), Value::Text(format!("open{i}")));
        req.properties
            .insert("dept".into(), Value::Text("eng".into()));
        req.security = Some(bob_visible.clone());
        k.remember(req).unwrap();
    }
    for i in 0..3 {
        person(&k, &format!("locked{i}"), Some("eng"), None, None); // alice-only
    }
    // As bob: COUNT(*) must be 2 — rows bob cannot read never enter grouping.
    let plan = parser::compile_with_subject("MATCH Person GROUP BY dept, COUNT(*) RETURN *", "bob")
        .unwrap();
    match Interpreter::execute(&k, &plan).unwrap() {
        RowSet::Grouped(g) => {
            assert_eq!(g.len(), 1);
            assert_eq!(g[0].get("count"), Some(&Value::Int(2)));
        }
        other => panic!("expected Grouped, got {:?}", other),
    }
}
