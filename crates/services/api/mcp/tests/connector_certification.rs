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
