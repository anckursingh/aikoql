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
// MVP-CON-001 — PostgreSQL connector (item 2: update; item 3: delete/outage)
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
