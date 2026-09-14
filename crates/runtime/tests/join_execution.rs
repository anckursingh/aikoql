//! P5-M6 (ND-06) — join engine. jn001–jn009.
//!
//! The roadmap's ND-06 RED list: inner join, left join, empty side,
//! duplicate keys, null keys, tenant boundaries, snapshot boundaries,
//! authorization, skewed distributions (+ the acceptance criteria: a
//! correctness oracle, deterministic output, EXPLAIN exposes the strategy).
//!
//! Observable contract pinned here (the implementation must satisfy it):
//! join output is `RowSet::Joined(Vec<(KnowledgeObject,
//! Option<KnowledgeObject>)>)` — the left row plus its right-side match;
//! INNER drops unmatched left rows (every pair has Some), LEFT keeps them
//! with None. Deterministic order: left rows in scan order, and within one
//! left row the right matches in right-scan order. Both sides resolve
//! through the kernel's read filters (ACL, Deleted, tenant scope), so a
//! join never sees rows the subject cannot read. v1 executes the nested
//! loop as THE join executor; hash join is the documented upgrade path
//! (strategy selection = P5-M9's CBO seam) — the oracle in jn009 pins
//! equivalence against a reference nested loop.

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

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn emp(k: &Kernel, name: &str, dept_id: Option<i64>) -> KOID {
    let mut req = RememberRequest::create(ctx("alice"), meta("Employee"));
    req.properties
        .insert("name".into(), Value::Text(name.into()));
    if let Some(d) = dept_id {
        req.properties.insert("dept_id".into(), Value::Int(d));
    }
    k.remember(req).unwrap().koid
}

fn dept(k: &Kernel, title: &str, id: i64) -> KOID {
    let mut req = RememberRequest::create(ctx("alice"), meta("Department"));
    req.properties
        .insert("title".into(), Value::Text(title.into()));
    req.properties.insert("id".into(), Value::Int(id));
    k.remember(req).unwrap().koid
}

/// Execute the join as alice; expect Joined output as (left name, right title).
fn pairs(k: &Kernel, query: &str) -> Vec<(String, Option<String>)> {
    let plan = parser::compile_with_subject(query, "alice").unwrap();
    match Interpreter::execute(k, &plan).unwrap() {
        RowSet::Joined(p) => p
            .into_iter()
            .map(|(l, r)| {
                let ln = match &l.properties["name"] {
                    Value::Text(t) => t.clone(),
                    other => panic!("expected Text name, got {:?}", other),
                };
                let rt = r.map(|ro| match &ro.properties["title"] {
                    Value::Text(t) => t.clone(),
                    other => panic!("expected Text title, got {:?}", other),
                });
                (ln, rt)
            })
            .collect(),
        other => panic!("expected Joined, got {:?}", other),
    }
}

// --- jn001 — inner join ---------------------------------------------------------

#[test]
fn jn001_inner_join_matches_and_drops_unmatched() {
    let k = mk();
    let _ = dept(&k, "Eng", 1);
    let _ = dept(&k, "Sales", 2);
    emp(&k, "A", Some(1));
    emp(&k, "B", Some(2));
    emp(&k, "C", Some(99)); // unmatched
    let got = pairs(
        &k,
        "MATCH Employee JOIN Department ON dept_id == id RETURN *",
    );
    assert_eq!(
        got,
        vec![
            ("A".into(), Some("Eng".into())),
            ("B".into(), Some("Sales".into()))
        ],
        "inner join keeps only matched pairs, left scan order"
    );
}

// --- jn002 — left join -----------------------------------------------------------

#[test]
fn jn002_left_join_keeps_unmatched_left_rows_with_none() {
    // LEFT JOIN is the P5-M6 grammar addition — RED today: parse error.
    let k = mk();
    let _ = dept(&k, "Eng", 1);
    emp(&k, "A", Some(1));
    emp(&k, "C", Some(99)); // unmatched — kept on the left, None on the right
    let got = pairs(
        &k,
        "MATCH Employee LEFT JOIN Department ON dept_id == id RETURN *",
    );
    assert_eq!(
        got,
        vec![("A".into(), Some("Eng".into())), ("C".into(), None)],
        "left join keeps unmatched left rows"
    );
}

// --- jn003 — empty sides ----------------------------------------------------------

#[test]
fn jn003_empty_sides_produce_no_pairs() {
    let k = mk();
    let _ = dept(&k, "Eng", 1);
    assert!(
        pairs(
            &k,
            "MATCH Employee JOIN Department ON dept_id == id RETURN *"
        )
        .is_empty(),
        "empty left side → no pairs"
    );
    let k2 = mk();
    emp(&k2, "A", Some(1));
    assert!(
        pairs(
            &k2,
            "MATCH Employee JOIN Department ON dept_id == id RETURN *"
        )
        .is_empty(),
        "empty right side → no pairs"
    );
}

// --- jn004 — duplicate keys ---------------------------------------------------------

#[test]
fn jn004_duplicate_keys_expand_to_all_combinations() {
    let k = mk();
    // Two departments with the same id: one employee matches BOTH.
    let _ = dept(&k, "Eng", 1);
    let _ = dept(&k, "Ops", 1);
    emp(&k, "A", Some(1));
    let mut got = pairs(
        &k,
        "MATCH Employee JOIN Department ON dept_id == id RETURN *",
    );
    assert_eq!(got.len(), 2, "duplicate right keys → one pair each");
    got.sort();
    assert_eq!(
        got,
        vec![
            ("A".into(), Some("Eng".into())),
            ("A".into(), Some("Ops".into()))
        ]
    );
}

// --- jn005 — null keys ---------------------------------------------------------------

#[test]
fn jn005_null_keys_never_match() {
    let k = mk();
    let _ = dept(&k, "Eng", 1);
    emp(&k, "A", Some(1));
    emp(&k, "B", None); // dept_id missing → Null key
    let mut req = RememberRequest::create(ctx("alice"), meta("Department"));
    req.properties
        .insert("title".into(), Value::Text("NoId".into()));
    k.remember(req).unwrap(); // id missing → Null key on the right side too
    let got = pairs(
        &k,
        "MATCH Employee JOIN Department ON dept_id == id RETURN *",
    );
    assert_eq!(
        got,
        vec![("A".into(), Some("Eng".into()))],
        "Null keys never match — not even Null == Null"
    );
}

// --- jn006 — tenant boundaries --------------------------------------------------------

#[test]
fn jn006_join_never_crosses_tenant_scope() {
    let k = mk();
    // Left and right rows in alice's tenant + one right row in another tenant.
    let _ = dept(&k, "Eng", 1);
    emp(&k, "A", Some(1));
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("bob").in_tenant("other")),
        meta("Department"),
    );
    req.properties
        .insert("title".into(), Value::Text("Foreign".into()));
    req.properties.insert("id".into(), Value::Int(1));
    k.remember(req).unwrap();
    let got = pairs(
        &k,
        "MATCH Employee JOIN Department ON dept_id == id RETURN *",
    );
    assert_eq!(
        got,
        vec![("A".into(), Some("Eng".into()))],
        "the foreign-tenant right row never joins"
    );
}

// --- jn007 — snapshot boundaries -------------------------------------------------------

#[test]
fn jn007_deleted_right_rows_never_join() {
    let k = mk();
    let d = dept(&k, "Eng", 1);
    emp(&k, "A", Some(1));
    k.forget(
        Subject::new("alice"),
        &d,
        ForgetMode::Tombstone,
        None,
        None,
    )
    .unwrap();
    let got = pairs(
        &k,
        "MATCH Employee JOIN Department ON dept_id == id RETURN *",
    );
    assert!(
        got.is_empty(),
        "a tombstoned right row is not a live join endpoint"
    );
}

// --- jn008 — authorization ---------------------------------------------------------------

#[test]
fn jn008_join_sees_only_rows_the_subject_can_read() {
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
    // One employee + one department bob can read; one of each locked to alice.
    let mut e = RememberRequest::create(ctx("alice"), meta("Employee"));
    e.properties
        .insert("name".into(), Value::Text("Open".into()));
    e.properties.insert("dept_id".into(), Value::Int(1));
    e.security = Some(bob_visible.clone());
    k.remember(e).unwrap();
    let mut d = RememberRequest::create(ctx("alice"), meta("Department"));
    d.properties
        .insert("title".into(), Value::Text("OpenDept".into()));
    d.properties.insert("id".into(), Value::Int(1));
    d.security = Some(bob_visible.clone());
    k.remember(d).unwrap();
    emp(&k, "Locked", Some(1)); // alice-only left row
    let mut d2 = RememberRequest::create(ctx("alice"), meta("Department"));
    d2.properties
        .insert("title".into(), Value::Text("LockedDept".into()));
    d2.properties.insert("id".into(), Value::Int(1));
    k.remember(d2).unwrap();

    let plan = parser::compile_with_subject(
        "MATCH Employee JOIN Department ON dept_id == id RETURN *",
        "bob",
    )
    .unwrap();
    match Interpreter::execute(&k, &plan).unwrap() {
        RowSet::Joined(p) => {
            assert_eq!(p.len(), 1, "bob sees exactly the one readable pair");
            match &p[0].0.properties["name"] {
                Value::Text(t) => assert_eq!(t, "Open"),
                other => panic!("unexpected {:?}", other),
            }
        }
        other => panic!("expected Joined, got {:?}", other),
    }
}

// --- jn009 — skewed distributions + correctness oracle ----------------------------------

#[test]
fn jn009_skewed_distribution_matches_the_reference_nested_loop() {
    let k = mk();
    // One hot department (1000 employees) + 999 cold employees matching nothing.
    let _ = dept(&k, "Hot", 7);
    for i in 0..1000 {
        emp(&k, &format!("hot{i}"), Some(7));
    }
    for i in 0..999 {
        emp(&k, &format!("cold{i}"), Some(8));
    }
    let plan = parser::compile_with_subject(
        "MATCH Employee JOIN Department ON dept_id == id RETURN *",
        "alice",
    )
    .unwrap();
    let joined = match Interpreter::execute(&k, &plan).unwrap() {
        RowSet::Joined(p) => p,
        other => panic!("expected Joined, got {:?}", other),
    };
    // The oracle: a reference nested loop over the kernel's own scans —
    // same read filters, same order contract.
    let left = k.scan_by_type(&Subject::new("alice"), "Employee").unwrap();
    let right = k
        .scan_by_type(&Subject::new("alice"), "Department")
        .unwrap();
    let mut expected = Vec::new();
    for l in &left {
        for r in &right {
            if l.properties.get("dept_id") == r.properties.get("id") {
                expected.push((l.clone(), Some(r.clone())));
            }
        }
    }
    assert_eq!(joined.len(), expected.len(), "skew: 1000 hot pairs, 0 cold");
    assert_eq!(
        joined, expected,
        "runtime join ≡ reference nested loop, row for row"
    );
}
