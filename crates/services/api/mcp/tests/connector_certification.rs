//! MVP-QA-001 Suite D/E — connector certification against LIVE databases
//! (GATE-04). One test file, one TDD item per test group; red first, then
//! the production fix, never the expected results.
//!
//! Runs for real in the CI `connectors` job (services: pg/pgvector/mongo/
//! neo4j); env-skip locally unless `docker compose --profile full up -d`
//! plus the AIKOQL_TEST_* vars (see connectors/mod.rs).

mod connectors;

/// Item 1 acceptance: every configured live source is reachable through the
/// harness probes. Red here means compose/CI wiring is broken, not the
/// product — it guards the other tests from silently skipping in CI.
#[test]
fn infra_live_db_connectivity() {
    let _ = connectors::Live::pg();
    let _ = connectors::Live::pgvector();
    let _ = connectors::Live::mongo();
    let _ = connectors::Live::neo4j();
}

// ---------------------------------------------------------------------------
// MVP-CON-001 — PostgreSQL connector (item 2: update; item 3: delete/outage;
// item 4: foreign keys)
// ---------------------------------------------------------------------------

/// One table per test — cargo runs test fns in parallel threads against one
/// shared live database, so a shared table name races DROP/CREATE/UPDATE.
fn con001_seed(dsn: &str, table: &str) {
    connectors::pg_exec(
        dsn,
        &[
            &format!("DROP TABLE IF EXISTS {table}"),
            &format!(
                "CREATE TABLE {table} (id SERIAL PRIMARY KEY, name TEXT NOT NULL, age INT NOT NULL)"
            ),
            &format!("INSERT INTO {table} (name, age) VALUES ('alice', 30), ('bob', 25)"),
        ],
    );
}

fn con001_age_of(k: &aikoql_kernel::Kernel, table: &str, name: &str) -> i64 {
    use aikoql_kernel::Value;
    let kos = connectors::scan_type(k, "pg-importer", table);
    let ko = kos
        .iter()
        .find(|ko| ko.properties.get("name") == Some(&Value::Text(name.into())))
        .unwrap_or_else(|| panic!("{name} missing among {} KOs", kos.len()));
    match ko.properties.get("age") {
        Some(Value::Int(a)) => *a,
        other => panic!("age of {name} not an int: {other:?}"),
    }
}

/// MVP-CON-001: re-importing after a source UPDATE must move the head KO to
/// the new value. Red today: the runner's constant idempotency key replays
/// the original commit, so the update is invisible.
#[test]
fn con001_pg_update_reflects_source_change() {
    let Some(live) = connectors::Live::pg() else {
        return;
    };
    let table = "cert_con001_update";
    con001_seed(&live.dsn, table);
    let db = connectors::temp_db("con001-update");

    let import = || {
        let out = connectors::run_import(&["import", "postgres", &live.dsn, &db, "--table", table]);
        connectors::assert_import_ok(&out, table);
    };

    import();
    {
        let k = connectors::open_kernel(&db);
        assert_eq!(con001_age_of(&k, table, "alice"), 30, "initial import");
        drop(k); // redb lock: the re-import below re-opens the same file
    }

    connectors::pg_exec(
        &live.dsn,
        &[&format!("UPDATE {table} SET age = 31 WHERE name = 'alice'")],
    );
    import();

    let k = connectors::open_kernel(&db);
    assert_eq!(
        con001_age_of(&k, table, "alice"),
        31,
        "re-import must reflect the source change (update)"
    );
}

/// MVP-CON-001 / EVO-005 analog: ten re-imports of an unchanged table add no
/// KOs and churn no versions. Pins the skip-if-identical path of ImportSink.
#[test]
fn con001_pg_reingest_ten_times_no_growth() {
    let Some(live) = connectors::Live::pg() else {
        return;
    };
    let table = "cert_con001_growth";
    con001_seed(&live.dsn, table);
    let db = connectors::temp_db("con001-growth");

    let import = || {
        let out = connectors::run_import(&["import", "postgres", &live.dsn, &db, "--table", table]);
        connectors::assert_import_ok(&out, table);
    };

    import();
    let (count0, ver0) = {
        let k = connectors::open_kernel(&db);
        let kos = connectors::scan_type(&k, "pg-importer", table);
        (
            kos.len(),
            kos.iter().map(|ko| ko.version).max().unwrap_or(0),
        )
    };

    for i in 0..10 {
        import();
        let k = connectors::open_kernel(&db);
        let kos = connectors::scan_type(&k, "pg-importer", table);
        let ver = kos.iter().map(|ko| ko.version).max().unwrap_or(0);
        assert_eq!(kos.len(), count0, "run {i}: KO count grew");
        assert_eq!(ver, ver0, "run {i}: version churned on identical re-import");
        drop(k);
    }
}

// ---------------------------------------------------------------------------
// MVP-CON-001 — delete reconcile + outage guard (item 3)
// ---------------------------------------------------------------------------

fn con001_ko_by_name(
    k: &aikoql_kernel::Kernel,
    table: &str,
    name: &str,
) -> aikoql_kernel::KnowledgeObject {
    use aikoql_kernel::Value;
    connectors::scan_type(k, "pg-importer", table)
        .into_iter()
        .find(|ko| ko.properties.get("name") == Some(&Value::Text(name.into())))
        .unwrap_or_else(|| panic!("{name} missing"))
}

/// MVP-CON-001: a row deleted at the source must be tombstoned on the next
/// successful re-import — stale KOs must not linger as live knowledge. Red
/// today: no prune machinery exists, so bob survives the DELETE.
#[test]
fn con001_pg_deleted_row_is_tombstoned() {
    use aikoql_kernel::LifecycleState;
    let Some(live) = connectors::Live::pg() else {
        return;
    };
    let table = "cert_con001_delete";
    con001_seed(&live.dsn, table);
    let db = connectors::temp_db("con001-delete");

    let import = || {
        let out = connectors::run_import(&["import", "postgres", &live.dsn, &db, "--table", table]);
        connectors::assert_import_ok(&out, table);
    };

    import();
    let bob = {
        let k = connectors::open_kernel(&db);
        let ko = con001_ko_by_name(&k, table, "bob");
        assert_eq!(con001_age_of(&k, table, "alice"), 30);
        drop(k);
        ko.koid
    };

    connectors::pg_exec(
        &live.dsn,
        &[&format!("DELETE FROM {table} WHERE name = 'bob'")],
    );
    import();

    let k = connectors::open_kernel(&db);
    let live_bob = connectors::scan_type(&k, "pg-importer", table)
        .iter()
        .any(|ko| ko.koid == bob);
    assert!(
        !live_bob,
        "bob must not appear as live knowledge after the source DELETE"
    );
    // The tombstone itself: the KO head is still there, but Deleted.
    let ctx = aikoql_kernel::KnowledgeContext::from(aikoql_kernel::Subject::new("pg-importer"));
    let head = k
        .get(ctx, &bob)
        .unwrap_or_else(|e| panic!("bob's head vanished: {e}"));
    assert_eq!(
        head.lifecycle.state,
        LifecycleState::Deleted,
        "deleted source row must be tombstoned"
    );
    // alice survives untouched.
    assert_eq!(con001_age_of(&k, table, "alice"), 30);
}

/// MVP-CON-001 / CON-007 groundwork: a FAILED import must never prune —
/// prior KOs stay live even when the source is unreachable. Red today: no
/// prune exists either way, so this pins the all-success gate once prune
/// lands; a leaked prune would delete both rows here.
#[test]
fn con001_pg_import_failure_never_prunes() {
    use aikoql_kernel::LifecycleState;
    let Some(live) = connectors::Live::pg() else {
        return;
    };
    let table = "cert_con001_fail";
    con001_seed(&live.dsn, table);
    let db = connectors::temp_db("con001-fail");

    let import = || {
        let out = connectors::run_import(&["import", "postgres", &live.dsn, &db, "--table", table]);
        connectors::assert_import_ok(&out, table);
    };

    import();
    let (alice, bob) = {
        let k = connectors::open_kernel(&db);
        let kos = connectors::scan_type(&k, "pg-importer", table);
        assert_eq!(kos.len(), 2);
        (kos[0].koid, kos[1].koid)
    };

    // Dead port: connect must fail (nothing listens on 59999).
    let dead_dsn = "host=localhost port=59999 user=aikoql password=x dbname=knowledge";
    let out = connectors::run_import(&["import", "postgres", dead_dsn, &db, "--table", table]);
    connectors::assert_import_fails(&out, "dead-port import");

    let k = connectors::open_kernel(&db);
    let ctx = aikoql_kernel::KnowledgeContext::from(aikoql_kernel::Subject::new("pg-importer"));
    for koid in [alice, bob] {
        let head = k
            .get(ctx.clone(), &koid)
            .unwrap_or_else(|e| panic!("KO {koid:?} lost after failed import: {e}"));
        assert_eq!(
            head.lifecycle.state,
            LifecycleState::Draft,
            "failed import must not tombstone {koid:?}"
        );
    }
    assert_eq!(con001_age_of(&k, table, "alice"), 30);
}

// ---------------------------------------------------------------------------
// MVP-CON-001 — foreign keys become relationships (item 4)
// ---------------------------------------------------------------------------

fn con001_child_by_note(
    k: &aikoql_kernel::Kernel,
    table: &str,
    note: &str,
) -> aikoql_kernel::KnowledgeObject {
    use aikoql_kernel::Value;
    connectors::scan_type(k, "pg-importer", table)
        .into_iter()
        .find(|ko| ko.properties.get("note") == Some(&Value::Text(note.into())))
        .unwrap_or_else(|| panic!("child {note} missing"))
}

/// MVP-CON-001: a row's foreign key must become an Outbound RelationshipRef
/// to the referenced row (rel_type = constraint name). Red today:
/// `import_table` emits `relationships: vec![]` and no FK introspection
/// exists (provider header: "Phase 2").
#[test]
fn con001_pg_fk_becomes_relationship() {
    use aikoql_kernel::{Direction, RelationshipRef};
    let Some(live) = connectors::Live::pg() else {
        return;
    };
    // Private DB so the filterless import sees ONLY the two FK tables.
    let dsn = connectors::pg_private_db(&live.dsn, "cert_con001_fk");
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE TABLE fk_parent (id SERIAL PRIMARY KEY, name TEXT NOT NULL)",
            "INSERT INTO fk_parent (name) VALUES ('alice'), ('bob')",
            "CREATE TABLE fk_child (id SERIAL PRIMARY KEY, parent_id INT REFERENCES fk_parent(id), note TEXT NOT NULL)",
            "INSERT INTO fk_child (parent_id, note) VALUES (1, 'first'), (2, 'second')",
        ],
    );
    let db = connectors::temp_db("con001-fk");

    let import = || {
        let out = connectors::run_import(&["import", "postgres", &dsn, &db]);
        connectors::assert_import_ok(&out, "fk tables");
    };

    import();
    let k = connectors::open_kernel(&db);
    let alice = con001_ko_by_name(&k, "fk_parent", "alice").koid;
    let first = con001_child_by_note(&k, "fk_child", "first");
    assert_eq!(
        first.relationships,
        vec![RelationshipRef {
            rel_type: "fk_child_parent_id_fkey".into(),
            target: alice,
            direction: Direction::Outbound,
        }],
        "FK column must link the child row to its parent"
    );
    let bob = con001_ko_by_name(&k, "fk_parent", "bob").koid;
    assert_eq!(
        con001_child_by_note(&k, "fk_child", "second").relationships,
        vec![RelationshipRef {
            rel_type: "fk_child_parent_id_fkey".into(),
            target: bob,
            direction: Direction::Outbound,
        }],
        "second child must link to bob"
    );
    drop(k);

    // Re-import: linking is deterministic — an unchanged re-import must not
    // duplicate the relationship (sink skip-if-identical covers the KO).
    import();
    let k = connectors::open_kernel(&db);
    assert_eq!(
        con001_child_by_note(&k, "fk_child", "first")
            .relationships
            .len(),
        1,
        "re-import must not duplicate relationships"
    );
}

// ---------------------------------------------------------------------------
// MVP-CON-002 — MongoDB connector (item 5)
// ---------------------------------------------------------------------------

fn con002_ko_by_name(
    k: &aikoql_kernel::Kernel,
    coll: &str,
    name: &str,
) -> aikoql_kernel::KnowledgeObject {
    use aikoql_kernel::Value;
    connectors::scan_type(k, "mongo-importer", coll)
        .into_iter()
        .find(|ko| ko.properties.get("name") == Some(&Value::Text(name.into())))
        .unwrap_or_else(|| panic!("{name} missing"))
}

fn con002_age_of(k: &aikoql_kernel::Kernel, coll: &str) -> i64 {
    use aikoql_kernel::Value;
    let kos = connectors::scan_type(k, "mongo-importer", coll);
    assert_eq!(kos.len(), 1, "one doc expected, got {}", kos.len());
    match kos[0].properties.get("age") {
        Some(Value::Int(a)) => *a,
        other => panic!("age not an int: {other:?}"),
    }
}

/// MVP-CON-002: nested documents and arrays must survive the import as
/// Value::Map / Value::List. Red today: mongo import is a silent no-op
/// (owner `mongodb-importer` vs runner subject `mongo-importer` →
/// ACCESS_DENIED on every commit, exit 0) — no KOs at all.
#[test]
fn con002_mongo_nested_structures_preserved() {
    use aikoql_kernel::Value;
    let Some(live) = connectors::Live::mongo() else {
        return;
    };
    let coll = "cert_con002_nested";
    connectors::mongo_seed(
        &live.mongo_uri,
        &live.mongo_db,
        coll,
        vec![mongodb::bson::doc! {
            "_id": "u1",
            "name": "alice",
            "profile": { "city": "Berlin", "age": 30 },
            "tags": ["a", "b"],
        }],
    );
    let db = connectors::temp_db("con002-nested");
    let out = connectors::run_import(&[
        "import",
        "mongodb",
        &live.mongo_uri,
        "--db",
        &live.mongo_db,
        "--collection",
        coll,
        &db,
    ]);
    connectors::assert_import_ok(&out, coll);

    let k = connectors::open_kernel(&db);
    let kos = connectors::scan_type(&k, "mongo-importer", coll);
    assert_eq!(kos.len(), 1, "one document must import as one KO");
    let props = &kos[0].properties;
    let mut m = std::collections::BTreeMap::new();
    m.insert("city".to_string(), Value::Text("Berlin".into()));
    m.insert("age".to_string(), Value::Int(30));
    assert_eq!(
        props.get("profile"),
        Some(&Value::Map(m)),
        "nested doc must import as Value::Map"
    );
    assert_eq!(
        props.get("tags"),
        Some(&Value::List(vec![
            Value::Text("a".into()),
            Value::Text("b".into())
        ])),
        "array must import as Value::List"
    );
}

/// MVP-CON-002: re-import after a source UPDATE must move the head KO.
/// Red today: constant idem key replays the original commit (same bug as
/// PG item 2) — and the owner mismatch no-ops even the first import.
#[test]
fn con002_mongo_update_reflects_source_change() {
    let Some(live) = connectors::Live::mongo() else {
        return;
    };
    let coll = "cert_con002_update";
    connectors::mongo_seed(
        &live.mongo_uri,
        &live.mongo_db,
        coll,
        vec![mongodb::bson::doc! {"_id": "u1", "name": "alice", "age": 30}],
    );
    let db = connectors::temp_db("con002-update");

    let import = || {
        let out = connectors::run_import(&[
            "import",
            "mongodb",
            &live.mongo_uri,
            "--db",
            &live.mongo_db,
            "--collection",
            coll,
            &db,
        ]);
        connectors::assert_import_ok(&out, coll);
    };

    import();
    {
        let k = connectors::open_kernel(&db);
        assert_eq!(con002_age_of(&k, coll), 30, "initial import");
        drop(k);
    }

    connectors::mongo_update(
        &live.mongo_uri,
        &live.mongo_db,
        coll,
        mongodb::bson::doc! {"_id": "u1"},
        mongodb::bson::doc! {"$set": {"age": 31}},
    );
    import();

    let k = connectors::open_kernel(&db);
    assert_eq!(
        con002_age_of(&k, coll),
        31,
        "re-import must reflect the source change (update)"
    );
}

/// MVP-CON-002: a document deleted at the source is tombstoned on the next
/// successful re-import.
#[test]
fn con002_mongo_deleted_doc_is_tombstoned() {
    use aikoql_kernel::LifecycleState;
    let Some(live) = connectors::Live::mongo() else {
        return;
    };
    let coll = "cert_con002_delete";
    connectors::mongo_seed(
        &live.mongo_uri,
        &live.mongo_db,
        coll,
        vec![
            mongodb::bson::doc! {"_id": "u1", "name": "alice", "age": 30},
            mongodb::bson::doc! {"_id": "u2", "name": "bob", "age": 25},
        ],
    );
    let db = connectors::temp_db("con002-delete");

    let import = || {
        let out = connectors::run_import(&[
            "import",
            "mongodb",
            &live.mongo_uri,
            "--db",
            &live.mongo_db,
            "--collection",
            coll,
            &db,
        ]);
        connectors::assert_import_ok(&out, coll);
    };

    import();
    let bob = {
        let k = connectors::open_kernel(&db);
        let ko = con002_ko_by_name(&k, coll, "bob");
        drop(k);
        ko.koid
    };

    connectors::mongo_delete(
        &live.mongo_uri,
        &live.mongo_db,
        coll,
        mongodb::bson::doc! {"_id": "u2"},
    );
    import();

    let k = connectors::open_kernel(&db);
    let live_bob = connectors::scan_type(&k, "mongo-importer", coll)
        .iter()
        .any(|ko| ko.koid == bob);
    assert!(
        !live_bob,
        "bob must not appear as live knowledge after the source DELETE"
    );
    let ctx = aikoql_kernel::KnowledgeContext::from(aikoql_kernel::Subject::new("mongo-importer"));
    let head = k
        .get(ctx, &bob)
        .unwrap_or_else(|e| panic!("bob's head vanished: {e}"));
    assert_eq!(
        head.lifecycle.state,
        LifecycleState::Deleted,
        "deleted source doc must be tombstoned"
    );
    assert_eq!(con002_age_of(&k, coll), 30);
}

// ---------------------------------------------------------------------------
// MVP-CON-003 — Neo4j connector (item 6)
// ---------------------------------------------------------------------------

fn con003_ko_by_name(
    k: &aikoql_kernel::Kernel,
    label: &str,
    name: &str,
) -> aikoql_kernel::KnowledgeObject {
    use aikoql_kernel::Value;
    connectors::scan_type(k, "neo4j-importer", label)
        .into_iter()
        .find(|ko| ko.properties.get("name") == Some(&Value::Text(name.into())))
        .unwrap_or_else(|| panic!("{name} missing among label {label}"))
}

fn con003_import(live: &connectors::Live, db: &str) {
    let out = connectors::run_import(&[
        "import",
        "neo4j",
        &live.neo4j_uri,
        "--user",
        &live.neo4j_user,
        "--password",
        &live.neo4j_password,
        db,
    ]);
    connectors::assert_import_ok(&out, "neo4j");
}

/// MVP-CON-003: relationship properties must survive the import, kept on the
/// source node as `properties["rel:<TYPE>"]` — a List of Maps, each tagged
/// with `target` (RelationshipRef itself has no props field). Red today:
/// `import_relationships` fetches `properties(r)` and drops it.
#[test]
fn con003_neo4j_rel_props_preserved() {
    use aikoql_kernel::{Direction, RelationshipRef, Value};
    let Some(live) = connectors::Live::neo4j() else {
        return;
    };
    let label = "P_Con003RelProps";
    let rel = "KNOWS_Con003RelProps";
    connectors::neo4j_exec(
        &live.neo4j_uri,
        &live.neo4j_user,
        &live.neo4j_password,
        &[
            &format!("MATCH (n:`{label}`) DETACH DELETE n"),
            &format!(
                "CREATE (a:`{label}` {{name:'alice'}}), (b:`{label}` {{name:'bob'}}), \
                 (a)-[:`{rel}` {{since:2020, strength:0.9}}]->(b)"
            ),
        ],
    );
    let db = connectors::temp_db("con003-relprops");
    con003_import(&live, &db);

    let k = connectors::open_kernel(&db);
    let alice = con003_ko_by_name(&k, label, "alice");
    let bob = con003_ko_by_name(&k, label, "bob").koid;
    let mut m = std::collections::BTreeMap::new();
    m.insert("since".to_string(), Value::Int(2020));
    m.insert("strength".to_string(), Value::Float(0.9));
    m.insert("target".to_string(), Value::Text(bob.to_hex()));
    assert_eq!(
        alice.properties.get(&format!("rel:{rel}")),
        Some(&Value::List(vec![Value::Map(m)])),
        "relationship properties must survive the import"
    );
    assert_eq!(
        alice.relationships,
        vec![RelationshipRef {
            rel_type: rel.into(),
            target: bob,
            direction: Direction::Outbound,
        }],
        "the relationship ref itself must be present"
    );
}

/// MVP-CON-003: when one node has relationships of two different types, both
/// must survive. Red today: the phase-2 loop re-remembers the source node per
/// rel type with `updated.relationships = rels.clone()` — REPLACE, so the
/// last type wins and the other is lost.
#[test]
fn con003_neo4j_multiple_rel_types_survive() {
    use aikoql_kernel::Direction;
    let Some(live) = connectors::Live::neo4j() else {
        return;
    };
    let label = "P_Con003Multi";
    let rel_a = "KNOWS_Con003Multi";
    let rel_b = "LIKES_Con003Multi";
    connectors::neo4j_exec(
        &live.neo4j_uri,
        &live.neo4j_user,
        &live.neo4j_password,
        &[
            &format!("MATCH (n:`{label}`) DETACH DELETE n"),
            &format!(
                "CREATE (a:`{label}` {{name:'alice'}}), (b:`{label}` {{name:'bob'}}), \
                 (a)-[:`{rel_a}`]->(b), (a)-[:`{rel_b}`]->(b)"
            ),
        ],
    );
    let db = connectors::temp_db("con003-multirel");
    con003_import(&live, &db);

    let k = connectors::open_kernel(&db);
    let alice = con003_ko_by_name(&k, label, "alice");
    let bob = con003_ko_by_name(&k, label, "bob").koid;
    let mut got: Vec<String> = alice
        .relationships
        .iter()
        .map(|r| r.rel_type.clone())
        .collect();
    got.sort();
    let mut want = vec![rel_a.to_string(), rel_b.to_string()];
    want.sort();
    assert_eq!(
        got, want,
        "both relationship types must survive (phase 2 must append, not replace)"
    );
    assert!(
        alice
            .relationships
            .iter()
            .all(|r| { r.target == bob && r.direction == Direction::Outbound }),
        "every ref must point at bob, outbound"
    );
    assert!(
        alice.properties.contains_key(&format!("rel:{rel_a}"))
            && alice.properties.contains_key(&format!("rel:{rel_b}")),
        "rel props keyed per type must coexist"
    );
}

/// MVP-CON-003: a node carrying two labels must import as exactly ONE KO
/// (primary label = first sorted label → type_name; the full label list kept
/// in `properties["labels"]`). Red today: `import_nodes` runs per label, so
/// the node is imported once per label — two KOs (type explosion).
#[test]
fn con003_neo4j_multilabel_node_single_ko() {
    use aikoql_kernel::Value;
    let Some(live) = connectors::Live::neo4j() else {
        return;
    };
    let label_a = "A_Con003Label";
    let label_b = "B_Con003Label";
    connectors::neo4j_exec(
        &live.neo4j_uri,
        &live.neo4j_user,
        &live.neo4j_password,
        &[
            &format!(
                "MATCH (n) WHERE any(l IN labels(n) WHERE l IN ['{label_a}','{label_b}']) DETACH DELETE n"
            ),
            &format!("CREATE (n:`{label_a}`:`{label_b}` {{name:'ann'}})"),
        ],
    );
    let db = connectors::temp_db("con003-multilabel");
    con003_import(&live, &db);

    let k = connectors::open_kernel(&db);
    let mut ann_kos: Vec<aikoql_kernel::KnowledgeObject> =
        connectors::scan_type(&k, "neo4j-importer", label_a)
            .into_iter()
            .chain(connectors::scan_type(&k, "neo4j-importer", label_b))
            .filter(|ko| ko.properties.get("name") == Some(&Value::Text("ann".into())))
            .collect();
    assert_eq!(
        ann_kos.len(),
        1,
        "a multi-label node must import as ONE KO, got {}",
        ann_kos.len()
    );
    let ann = ann_kos.pop().unwrap();
    assert_eq!(
        ann.metadata.type_name, label_a,
        "primary label = first sorted label"
    );
    assert_eq!(
        ann.properties.get("labels"),
        Some(&Value::List(vec![
            Value::Text(label_a.into()),
            Value::Text(label_b.into())
        ])),
        "the full sorted label list must be preserved"
    );
}

/// MVP-CON-003: the RelationshipRef must be pinned on the STORED start node.
/// Red today: the Cypher match returns endpoints in elementId order, not
/// stored direction — car (created first, lower elementId) would steal the
/// ref and point Outbound at alice, inverting the graph.
#[test]
fn con003_neo4j_direction_pinned() {
    use aikoql_kernel::{Direction, RelationshipRef};
    let Some(live) = connectors::Live::neo4j() else {
        return;
    };
    let label = "P_Con003Dir";
    let rel = "OWNS_Con003Dir";
    // car created FIRST: its elementId sorts below alice's, so an
    // orientation-agnostic match returns a=car and pins the ref on the
    // wrong KO.
    connectors::neo4j_exec(
        &live.neo4j_uri,
        &live.neo4j_user,
        &live.neo4j_password,
        &[
            &format!("MATCH (n:`{label}`) DETACH DELETE n"),
            &format!(
                "CREATE (car:`{label}` {{name:'car'}}), (a:`{label}` {{name:'alice'}}), \
                 (a)-[:`{rel}`]->(car)"
            ),
        ],
    );
    let db = connectors::temp_db("con003-dir");
    con003_import(&live, &db);

    let k = connectors::open_kernel(&db);
    let alice = con003_ko_by_name(&k, label, "alice");
    let car = con003_ko_by_name(&k, label, "car");
    assert_eq!(
        alice.relationships,
        vec![RelationshipRef {
            rel_type: rel.into(),
            target: car.koid,
            direction: Direction::Outbound,
        }],
        "the ref must be pinned on the stored START node (alice)"
    );
    assert!(
        car.relationships.is_empty(),
        "the stored end node must carry no outbound ref"
    );
}

// ---------------------------------------------------------------------------
// MVP-CON-004 — PGVector connector (item 7)
// ---------------------------------------------------------------------------

/// MVP-CON-004: a pgvector `vector(N)` column must import as a Value::List
/// of floats on the KO (association only — similarity retrieval is kernel
/// embedding territory). Red today: `import pgvector` is not a CLI source at
/// all ("Unknown import source"), and the provider has no vector handling.
#[test]
fn con004_pgvector_embedding_associated() {
    use aikoql_kernel::Value;
    let Some(live) = connectors::Live::pgvector() else {
        return;
    };
    let table = "cert_con004_vec";
    connectors::pg_exec(
        &live.dsn,
        &[
            "CREATE EXTENSION IF NOT EXISTS vector",
            &format!("DROP TABLE IF EXISTS {table}"),
            &format!(
                "CREATE TABLE {table} (id SERIAL PRIMARY KEY, name TEXT NOT NULL, embedding vector(3))"
            ),
            &format!("INSERT INTO {table} (name, embedding) VALUES ('alice', '[0.1,0.2,0.3]')"),
        ],
    );

    // Discovery pin: the connector must see it as a vector table, with the
    // dims visible in the introspected type string.
    let mut conn =
        aikoql_postgres::PostgresConnector::connect(&live.dsn).expect("pgvector connect");
    let vt = conn.list_vector_tables().expect("list_vector_tables");
    assert!(
        vt.contains(&table.to_string()),
        "vector table not discovered: {vt:?}"
    );
    let schema = conn.introspect_table(table).expect("introspect");
    let emb = schema
        .columns
        .iter()
        .find(|c| c.name == "embedding")
        .expect("embedding column");
    assert_eq!(
        emb.pg_type, "vector(3)",
        "dims must come from format_type, got {}",
        emb.pg_type
    );
    drop(conn);

    let db = connectors::temp_db("con004-vec");
    let import = || {
        let out = connectors::run_import(&["import", "pgvector", &live.dsn, &db, "--table", table]);
        connectors::assert_import_ok(&out, table);
    };

    import();
    {
        let k = connectors::open_kernel(&db);
        let kos = connectors::scan_type(&k, "pg-importer", table);
        assert_eq!(kos.len(), 1, "one row must import as one KO");
        assert_eq!(
            kos[0].properties.get("embedding"),
            Some(&Value::List(vec![
                Value::Float(0.1),
                Value::Float(0.2),
                Value::Float(0.3)
            ])),
            "the vector column must import as a List of floats"
        );
        drop(k);
    }

    // Update leg: the ImportSink idem path must reflect a changed embedding
    // (same machinery as CON-001 — pins that vector props participate in
    // skip-if-identical).
    connectors::pg_exec(
        &live.dsn,
        &[&format!(
            "UPDATE {table} SET embedding = '[0.9,0.8,0.7]' WHERE name = 'alice'"
        )],
    );
    import();

    let k = connectors::open_kernel(&db);
    let kos = connectors::scan_type(&k, "pg-importer", table);
    assert_eq!(
        kos[0].properties.get("embedding"),
        Some(&Value::List(vec![
            Value::Float(0.9),
            Value::Float(0.8),
            Value::Float(0.7)
        ])),
        "re-import must reflect the changed embedding"
    );
}

// ---------------------------------------------------------------------------
// MVP-CON-005 — timeouts + incomplete-run marker (item 8)
// ---------------------------------------------------------------------------

/// MVP-CON-005: a source that stalls past `--timeout-ms` must fail the run
/// non-zero, leave prior KOs intact, and leave a `connector_run` marker KO
/// with status "incomplete"; a retry of the same `--run-id` against a healed
/// source succeeds. Red today: `--timeout-ms` is no CLI flag — the parser
/// swallows it and its value becomes a positional db path, no timeout
/// exists, no marker exists.
#[test]
fn con005_pg_timeout_marks_incomplete_keeps_existing() {
    use aikoql_kernel::Value;
    let Some(live) = connectors::Live::pg() else {
        return;
    };
    let table = "cert_con005_timeout";
    let slow_view = "cert_con005_timeout_slow";
    connectors::pg_exec(
        &live.dsn,
        &[
            &format!("DROP VIEW IF EXISTS {slow_view}"),
            &format!("DROP TABLE IF EXISTS {table}"),
            &format!(
                "CREATE TABLE {table} (id SERIAL PRIMARY KEY, name TEXT NOT NULL, age INT NOT NULL)"
            ),
            &format!("INSERT INTO {table} (name, age) VALUES ('alice', 30), ('bob', 25)"),
            // pg_sleep(1) per row: ~2s per full scan — far past a 500ms
            // statement timeout. ::text because a view column cannot be void.
            &format!("CREATE VIEW {slow_view} AS SELECT id, pg_sleep(1)::text AS s FROM {table}"),
        ],
    );
    let db = connectors::temp_db("con005-timeout");

    // Baseline: the real table imports cleanly.
    let out = connectors::run_import(&["import", "postgres", &live.dsn, &db, "--table", table]);
    connectors::assert_import_ok(&out, "con005 baseline");

    // The slow view must fail within the timeout, not hang or succeed.
    let out = connectors::run_import(&[
        "import",
        "postgres",
        &live.dsn,
        &db,
        "--table",
        slow_view,
        "--timeout-ms",
        "500",
        "--run-id",
        "run-con005",
    ]);
    connectors::assert_import_fails(&out, "slow import with --timeout-ms 500");

    let k = connectors::open_kernel(&db);
    // Prior KOs untouched by the failed run.
    assert_eq!(
        con001_age_of(&k, table, "alice"),
        30,
        "prior KOs must survive the timed-out run"
    );
    // The failed run is explicitly marked, not silently rolled back.
    let markers = connectors::scan_type(&k, "pg-importer", "connector_run");
    assert_eq!(
        markers.len(),
        1,
        "exactly one incomplete-run marker expected"
    );
    assert_eq!(
        markers[0].properties.get("status"),
        Some(&Value::Text("incomplete".into())),
        "marker must carry status incomplete"
    );
    assert!(
        markers[0].properties.contains_key("error"),
        "marker must carry the failure reason"
    );
    drop(k);

    // Retry the SAME --run-id against a healed source: succeeds cleanly.
    connectors::pg_exec(&live.dsn, &[&format!("DROP VIEW {slow_view}")]);
    let out = connectors::run_import(&[
        "import",
        "postgres",
        &live.dsn,
        &db,
        "--table",
        table,
        "--timeout-ms",
        "5000",
        "--run-id",
        "run-con005",
    ]);
    connectors::assert_import_ok(&out, "con005 retry after source healed");
    let k = connectors::open_kernel(&db);
    assert_eq!(
        con001_age_of(&k, table, "alice"),
        30,
        "retry must import the healed source"
    );
}

// ---------------------------------------------------------------------------
// MVP-CON-006 — no credentials in ordinary output (item 9)
// ---------------------------------------------------------------------------

/// MVP-CON-006: credentials passed to a connector must never appear in
/// stdout or stderr, even on failure. Dead ports, no live DB needed. Red
/// today: the mongo and neo4j runners print the raw URI (`Connecting to
/// MongoDB: {uri}` / `Connecting to Neo4j: {uri}`) — a URI carrying
/// `user:pass@` leaks the credential verbatim.
#[test]
fn con006_credentials_never_in_ordinary_output() {
    let secret = "SUPERSECRET-CRED";
    let db = connectors::temp_db("con006-secrets");
    let assert_clean = |out: &connectors::ImportOut, what: &str| {
        assert!(
            !out.stdout.contains(secret) && !out.stderr.contains(secret),
            "import {what} leaked the credential:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.stdout,
            out.stderr
        );
    };

    // MongoDB: creds live inside the URI.
    let out = connectors::run_import(&[
        "import",
        "mongodb",
        &format!("mongodb://ancku:{secret}@127.0.0.1:1"),
        "--db",
        "knowledge",
        &db,
        "--timeout-ms",
        "2000",
    ]);
    connectors::assert_import_fails(&out, "mongo dead port");
    assert_clean(&out, "mongodb");

    // Neo4j: creds in the URI's userinfo.
    let out = connectors::run_import(&[
        "import",
        "neo4j",
        &format!("http://{secret}@127.0.0.1:1"),
        &db,
        "--timeout-ms",
        "2000",
    ]);
    connectors::assert_import_fails(&out, "neo4j dead port");
    assert_clean(&out, "neo4j");

    // PostgreSQL: creds in the conn string (pin — the runner never prints
    // it today; guards against a future regression).
    let out = connectors::run_import(&[
        "import",
        "postgres",
        &format!("host=127.0.0.1 port=1 user=ancku password={secret}"),
        &db,
        "--timeout-ms",
        "2000",
    ]);
    connectors::assert_import_fails(&out, "pg dead port");
    assert_clean(&out, "postgres");
}

// ---------------------------------------------------------------------------
// MVP-CON-007 — outage ≠ deletion (item 10)
// ---------------------------------------------------------------------------

/// MVP-CON-007: an unreachable source must NEVER be treated as an empty one —
/// the failed run must not prune or tombstone prior KOs (outage ≠ deletion).
/// The PG leg is already pinned by `con001_pg_import_failure_never_prunes`;
/// this pins the mongo and neo4j runners, which carry their own prune passes.
/// Green day one: connect failure aborts before the all-success prune gate
/// (items 5/6/8) — red only if a runner leaks a prune on the failure path.
#[test]
fn con007_unreachable_source_never_prunes() {
    use aikoql_kernel::{LifecycleState, Value};

    // MongoDB leg: baseline import, then the same source goes unreachable.
    if let Some(live) = connectors::Live::mongo() {
        let coll = "cert_con007_mongo";
        connectors::mongo_seed(
            &live.mongo_uri,
            &live.mongo_db,
            coll,
            vec![mongodb::bson::doc! {"_id": "u1", "name": "alice", "age": 30}],
        );
        let db = connectors::temp_db("con007-mongo");
        let out = connectors::run_import(&[
            "import",
            "mongodb",
            &live.mongo_uri,
            "--db",
            &live.mongo_db,
            "--collection",
            coll,
            &db,
        ]);
        connectors::assert_import_ok(&out, coll);

        let out = connectors::run_import(&[
            "import",
            "mongodb",
            "mongodb://127.0.0.1:1",
            "--db",
            &live.mongo_db,
            "--collection",
            coll,
            &db,
            "--timeout-ms",
            "2000",
        ]);
        connectors::assert_import_fails(&out, "mongo dead port");

        let k = connectors::open_kernel(&db);
        let kos = connectors::scan_type(&k, "mongo-importer", coll);
        assert_eq!(kos.len(), 1, "outage must not prune the collection KO");
        assert_eq!(
            kos[0].properties.get("age"),
            Some(&Value::Int(30)),
            "prior value must survive the outage"
        );
        let ctx =
            aikoql_kernel::KnowledgeContext::from(aikoql_kernel::Subject::new("mongo-importer"));
        let head = k
            .get(ctx, &kos[0].koid)
            .unwrap_or_else(|e| panic!("KO lost after outage: {e}"));
        assert_eq!(
            head.lifecycle.state,
            LifecycleState::Draft,
            "outage must not tombstone the KO"
        );
        let markers = connectors::scan_type(&k, "mongo-importer", "connector_run");
        assert_eq!(markers.len(), 1, "the failed run must be marked incomplete");
        drop(k);
    }

    // Neo4j leg: baseline import, then the same source goes unreachable.
    if let Some(live) = connectors::Live::neo4j() {
        let label = "P_Con007";
        connectors::neo4j_exec(
            &live.neo4j_uri,
            &live.neo4j_user,
            &live.neo4j_password,
            &[
                &format!("MATCH (n:`{label}`) DETACH DELETE n"),
                &format!("CREATE (a:`{label}` {{name:'alice'}}), (b:`{label}` {{name:'bob'}})"),
            ],
        );
        let db = connectors::temp_db("con007-neo4j");
        let out = connectors::run_import(&[
            "import",
            "neo4j",
            &live.neo4j_uri,
            "--user",
            &live.neo4j_user,
            "--password",
            &live.neo4j_password,
            &db,
        ]);
        connectors::assert_import_ok(&out, "neo4j");

        let out = connectors::run_import(&[
            "import",
            "neo4j",
            "http://127.0.0.1:1",
            &db,
            "--timeout-ms",
            "2000",
        ]);
        connectors::assert_import_fails(&out, "neo4j dead port");

        let k = connectors::open_kernel(&db);
        let kos = connectors::scan_type(&k, "neo4j-importer", label);
        assert_eq!(kos.len(), 2, "outage must not prune label KOs");
        for ko in &kos {
            let ctx = aikoql_kernel::KnowledgeContext::from(aikoql_kernel::Subject::new(
                "neo4j-importer",
            ));
            let head = k
                .get(ctx, &ko.koid)
                .unwrap_or_else(|e| panic!("KO lost after outage: {e}"));
            assert_eq!(
                head.lifecycle.state,
                LifecycleState::Draft,
                "outage must not tombstone the KO"
            );
        }
        let markers = connectors::scan_type(&k, "neo4j-importer", "connector_run");
        assert_eq!(markers.len(), 1, "the failed run must be marked incomplete");
    }
}

// ---------------------------------------------------------------------------
// MVP-ONT-001 — live ontology discovery over three sources (item 11)
// ---------------------------------------------------------------------------

/// MVP-ONT-001: live connector schemas (pg/mongo/neo4j) must flow through
/// the bridge → KnowledgeIr → discover_ontology_from_ir pipeline and merge
/// into one coherent proposal: classes per source container type, FK/graph
/// relationships, property proposals typed from schema facts, and
/// `connector://` provenance on every proposal. Red today: connector-bridge
/// schema facts yield ZERO property proposals (discover_ontology_from_ir
/// only mines dates/numbers from prose), and the neo4j provider cannot
/// introspect label properties at all.
#[test]
fn ont001_live_discovery_merges_three_sources() {
    use aikoql_ingestion::{
        connector_metadata_to_ir, discover_ontology_from_ir, merge_proposals, ConnectorMetadata,
        ContainerInfo, FieldInfo, ReferenceInfo,
    };

    let mut metas: Vec<ConnectorMetadata> = Vec::new();

    // ── Postgres: two tables with a FK, in a private database ──
    if let Some(live) = connectors::Live::pg() {
        let dsn = connectors::pg_private_db(&live.dsn, "cert_ont001");
        connectors::pg_exec(
            &dsn,
            &[
                "CREATE TABLE ont_parent (id SERIAL PRIMARY KEY, name TEXT NOT NULL, age INT NOT NULL)",
                "INSERT INTO ont_parent (name, age) VALUES ('alice', 30), ('bob', 25)",
                "CREATE TABLE ont_child (id SERIAL PRIMARY KEY, parent_id INT REFERENCES ont_parent(id), note TEXT)",
                "INSERT INTO ont_child (parent_id, note) VALUES (1, 'first')",
            ],
        );
        let mut conn = aikoql_postgres::PostgresConnector::connect(&dsn).expect("pg connect");
        let tables = conn.introspect_all().expect("introspect_all");
        let fks = conn
            .introspect_foreign_keys()
            .expect("introspect_foreign_keys");
        drop(conn);

        let containers = tables
            .into_iter()
            .map(|t| ContainerInfo {
                name: t.name,
                row_count: Some(t.row_count_estimate.max(0) as u64),
                fields: t
                    .columns
                    .into_iter()
                    .map(|c| FieldInfo {
                        name: c.name,
                        data_type: c.pg_type,
                        is_primary_key: c.is_primary_key,
                        nullable: c.is_nullable,
                        is_unique: false,
                    })
                    .collect(),
            })
            .collect();
        let references = fks
            .into_iter()
            .map(|f| ReferenceInfo {
                from_container: f.child_table,
                from_fields: vec![f.child_column],
                to_container: f.parent_table,
                to_fields: vec![f.parent_column],
                name: Some(f.constraint_name),
            })
            .collect();
        metas.push(ConnectorMetadata {
            connector_type: "postgres".into(),
            label: "cert_ont001".into(),
            containers,
            references,
            version: None,
        });
    }

    // ── MongoDB: one collection with nested + typed fields ──
    if let Some(live) = connectors::Live::mongo() {
        let coll = "cert_ont001_people";
        connectors::mongo_seed(
            &live.mongo_uri,
            &live.mongo_db,
            coll,
            vec![mongodb::bson::doc! {
                "_id": "u1",
                "name": "alice",
                "age": 30,
                "profile": { "city": "Berlin" },
            }],
        );
        let conn = aikoql_mongodb::MongoConnector::connect(&live.mongo_uri, &live.mongo_db)
            .expect("mongo");
        let schemas = conn.introspect_all().expect("mongo introspect_all");
        // Shared live DB: filter to this test's collection.
        let containers = schemas
            .into_iter()
            .filter(|s| s.name == coll)
            .map(|s| ContainerInfo {
                name: s.name,
                row_count: Some(s.document_count),
                fields: s
                    .properties
                    .into_iter()
                    .map(|p| FieldInfo {
                        is_primary_key: p == "_id",
                        name: p,
                        data_type: "bson".into(),
                        nullable: true,
                        is_unique: false,
                    })
                    .collect(),
            })
            .collect();
        metas.push(ConnectorMetadata {
            connector_type: "mongodb".into(),
            label: live.mongo_db.clone(),
            containers,
            references: vec![],
            version: None,
        });
    }

    // ── Neo4j: one label with props + one relationship type ──
    if let Some(live) = connectors::Live::neo4j() {
        let label = "P_Ont001";
        let rel = "KNOWS_Ont001";
        connectors::neo4j_exec(
            &live.neo4j_uri,
            &live.neo4j_user,
            &live.neo4j_password,
            &[
                &format!("MATCH (n:`{label}`) DETACH DELETE n"),
                &format!(
                    "CREATE (a:`{label}` {{name:'alice', age:30}}), (b:`{label}` {{name:'bob'}}), (a)-[:`{rel}`]->(b)"
                ),
            ],
        );
        let conn = aikoql_neo4j::Neo4jConnector::connect(
            &live.neo4j_uri,
            &live.neo4j_user,
            &live.neo4j_password,
        )
        .expect("neo4j connect");
        // Shared live DB: filter to this test's label/rel type.
        let containers: Vec<ContainerInfo> = conn
            .list_labels()
            .expect("list_labels")
            .into_iter()
            .filter(|l| l == label)
            .map(|l| ContainerInfo {
                fields: conn
                    .introspect_label_props(&l)
                    .expect("introspect_label_props")
                    .into_iter()
                    .map(|p| FieldInfo {
                        name: p,
                        data_type: "any".into(),
                        is_primary_key: false,
                        nullable: true,
                        is_unique: false,
                    })
                    .collect(),
                name: l,
                row_count: conn.count_nodes(label).ok(),
            })
            .collect();
        let references: Vec<ReferenceInfo> = conn
            .list_rel_types()
            .expect("list_rel_types")
            .into_iter()
            .filter(|r| r == rel)
            .map(|r| ReferenceInfo {
                from_container: label.into(),
                from_fields: vec![],
                to_container: label.into(),
                to_fields: vec![],
                name: Some(r),
            })
            .collect();
        metas.push(ConnectorMetadata {
            connector_type: "neo4j".into(),
            label: "aikoql".into(),
            containers,
            references,
            version: None,
        });
    }

    if metas.is_empty() {
        return; // env-skip (no live sources configured)
    }

    let merged = merge_proposals(
        &metas
            .iter()
            .map(|m| discover_ontology_from_ir(&connector_metadata_to_ir(m)))
            .collect::<Vec<_>>(),
    );

    // Classes: one per source container type — no per-table type explosion.
    let class_names: Vec<&str> = merged.classes.iter().map(|c| c.name.as_str()).collect();
    assert!(
        class_names.contains(&"postgresTable"),
        "classes: {class_names:?}"
    );
    assert!(
        class_names.contains(&"mongodbCollection"),
        "classes: {class_names:?}"
    );
    assert!(
        class_names.contains(&"neo4jNodeLabel"),
        "classes: {class_names:?}"
    );
    assert!(
        merged.classes.len() < 15,
        "no type explosion, got {}",
        merged.classes.len()
    );

    // Relationships: the FK and the graph rel both survive.
    assert!(
        merged
            .relationships
            .iter()
            .any(|r| r.name == "ont_child_parent_id_fkey"),
        "FK must become a relationship proposal: {:?}",
        merged.relationships
    );
    assert!(
        merged.relationships.iter().any(|r| {
            r.domain.as_deref() == Some("neo4jNodeLabel")
                && r.name.replace('_', "") == "knowsont001"
        }),
        "graph rel must become a relationship proposal: {:?}",
        merged.relationships
    );

    // Properties: schema facts must become typed proposals (RED today —
    // discover_ontology_from_ir yields none for connector-bridge facts).
    assert!(
        merged.properties.iter().any(|p| {
            p.name == "name" && p.class_name == "postgresTable" && p.value_type == "Text"
        }),
        "pg text column must propose a property: {:?}",
        merged.properties
    );
    assert!(
        merged.properties.iter().any(|p| {
            p.name == "age" && p.class_name == "postgresTable" && p.value_type == "Int"
        }),
        "pg int column must propose an Int property: {:?}",
        merged.properties
    );
    assert!(
        merged
            .properties
            .iter()
            .any(|p| p.name == "id" && p.class_name == "postgresTable" && p.required),
        "pk column must propose a required property: {:?}",
        merged.properties
    );
    assert!(
        merged
            .properties
            .iter()
            .any(|p| p.name == "note" && p.class_name == "postgresTable" && !p.required),
        "nullable column must propose an optional property: {:?}",
        merged.properties
    );
    assert!(
        merged
            .properties
            .iter()
            .any(|p| p.name == "name" && p.class_name == "mongodbCollection"),
        "mongo field must propose a property: {:?}",
        merged.properties
    );
    assert!(
        merged
            .properties
            .iter()
            .any(|p| p.name == "profile.city" && p.class_name == "mongodbCollection"),
        "nested mongo field must propose a dotted property: {:?}",
        merged.properties
    );
    assert!(
        merged
            .properties
            .iter()
            .any(|p| p.name == "name" && p.class_name == "neo4jNodeLabel"),
        "neo4j label prop must propose a property: {:?}",
        merged.properties
    );

    // Provenance: every proposal carries connector:// evidence.
    for (what, evidence) in merged
        .classes
        .iter()
        .map(|c| ("class", &c.evidence))
        .chain(merged.properties.iter().map(|p| ("property", &p.evidence)))
        .chain(
            merged
                .relationships
                .iter()
                .map(|r| ("relationship", &r.evidence)),
        )
    {
        assert!(
            evidence.iter().all(|e| e
                .document_id
                .as_deref()
                .unwrap_or("")
                .starts_with("connector://")),
            "{what} evidence must come from connector:// sources: {:?}",
            evidence
        );
    }
}

// ---------------------------------------------------------------------------
// MVP-E2E-001 — PostgreSQL → KO → query with provenance (item 12)
// ---------------------------------------------------------------------------

/// MVP-E2E-001: a live PostgreSQL table must flow end-to-end — import row →
/// KnowledgeObject → MCP `aikoql` query → resolvable provenance. The query
/// returns the seeded rows with their KOIDs, and a result KOID must resolve
/// through the kernel to a KO tagged `source:postgres`.
#[test]
fn e2e001_pg_to_ko_to_query_with_provenance() {
    let Some(live) = connectors::Live::pg() else {
        return;
    };
    let dsn = connectors::pg_private_db(&live.dsn, "cert_e2e001");
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE TABLE e2e_customer (id SERIAL PRIMARY KEY, name TEXT NOT NULL, city TEXT NOT NULL)",
            "INSERT INTO e2e_customer (name, city) VALUES ('alice', 'Berlin'), ('bob', 'London')",
        ],
    );
    let db = connectors::temp_db("e2e001");

    let out = connectors::run_import(&["import", "postgres", &dsn, &db]);
    connectors::assert_import_ok(&out, "e2e_customer");

    // Query through the product surface (MCP server + aikoql), as the owner
    // subject — the scan is authz-filtered (item 2 lesson).
    let mut client = connectors::McpQueryClient::start(&db);
    let all = client.call(
        "aikoql",
        serde_json::json!({"subject": "pg-importer", "query": "MATCH e2e_customer RETURN *"}),
    );
    let rows = all["results"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "both seeded rows must be queryable: {all}");
    let alice = rows
        .iter()
        .find(|r| r["properties"]["name"] == "alice")
        .unwrap_or_else(|| panic!("alice missing: {all}"));
    assert_eq!(alice["properties"]["city"], "Berlin");
    assert_eq!(alice["type_name"], "e2e_customer");
    let alice_koid = alice["koid"].as_str().unwrap().to_string();

    let filtered = client.call(
        "aikoql",
        serde_json::json!({
            "subject": "pg-importer",
            "query": "MATCH e2e_customer WHERE name == \"alice\" RETURN *"
        }),
    );
    let frows = filtered["results"].as_array().unwrap();
    assert_eq!(frows.len(), 1, "WHERE filter must narrow: {filtered}");
    assert_eq!(
        frows[0]["koid"], alice_koid,
        "same KO must back both results"
    );
    drop(client); // release the redb lock before the kernel read

    // Provenance: the query result's KOID must resolve to a KO carrying the
    // source tag — the answer is traceable back to PostgreSQL.
    let k = connectors::open_kernel(&db);
    let ko = connectors::scan_type(&k, "pg-importer", "e2e_customer")
        .into_iter()
        .find(|ko| ko.koid.to_hex() == alice_koid)
        .unwrap_or_else(|| panic!("koid {alice_koid} not resolvable"));
    assert!(
        ko.metadata.tags.iter().any(|t| t == "source:postgres"),
        "tags: {:?}",
        ko.metadata.tags
    );
    assert!(ko.metadata.tags.iter().any(|t| t == "imported"));
    assert_eq!(
        ko.properties.get("name"),
        Some(&aikoql_kernel::Value::Text("alice".into()))
    );
}
