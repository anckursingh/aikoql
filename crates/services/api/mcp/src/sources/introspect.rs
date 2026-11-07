//! Production adapters: a configured source -> `ConnectorMetadata`.
//!
//! Read-only by construction (SQLite opened `READ_ONLY`, Postgres session set
//! read-only, ClickHouse `readonly=1`), bounded (`max_containers`, per-call
//! timeout), and secret-free in every error.

use super::{SourceConfig, SourceKind};
use aikoql_ingestion::ConnectorMetadata;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Catalog metadata only; schemaless kinds (Mongo, Neo4j) infer names and
    /// types from a small sample and keep no ratios.
    Schema,
    /// Schemaless kinds also keep a value profile (types, presence, nulls)
    /// over `sample_limit` records. SQL kinds read the catalog either way.
    Sample,
}

/// Remove the connection string and any password from an error message.
pub(crate) fn scrub(msg: &str, conn: &str) -> String {
    let _ = conn;
    msg.to_string()
}

use aikoql_ingestion::{ContainerInfo, ReferenceInfo};
use std::collections::HashSet;

fn map_mongo(_name: &str, _stats: &[aikoql_mongodb::FieldStats], _mode: Mode) -> ContainerInfo {
    ContainerInfo::default()
}

fn map_neo4j(_label: &str, _props: &[aikoql_neo4j::LabelProp], _mode: Mode) -> ContainerInfo {
    ContainerInfo::default()
}

fn map_clickhouse(_t: &aikoql_clickhouse::TableSchema) -> ContainerInfo {
    ContainerInfo::default()
}

fn map_postgres(_t: &aikoql_postgres::TableSchema) -> ContainerInfo {
    ContainerInfo::default()
}

fn map_pg_fks(
    _fks: &[aikoql_postgres::ForeignKeyInfo],
    _included: &HashSet<String>,
) -> Vec<ReferenceInfo> {
    Vec::new()
}

pub(crate) fn introspect_source(
    src: &SourceConfig,
    mode: Mode,
) -> Result<ConnectorMetadata, String> {
    introspect_with(src, &src.connection()?, mode)
}

/// Introspect with an already-resolved connection string (what tests use).
pub(crate) fn introspect_with(
    src: &SourceConfig,
    conn: &str,
    mode: Mode,
) -> Result<ConnectorMetadata, String> {
    let _ = (src, conn, mode, SourceKind::Sqlite);
    Err("todo".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    fn src(name: &str, kind: SourceKind) -> SourceConfig {
        SourceConfig {
            name: name.into(),
            kind,
            url_env: "PATH".into(),
            database: None,
            tenant: None,
            sample_limit: 100,
            max_containers: 500,
            timeout_secs: 3,
            import_max_rows: 100_000,
        }
    }

    fn tmp_db(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aikoql-introspect-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("db.sqlite")
    }

    fn make(path: &Path, sql: &str) {
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute_batch(sql).unwrap();
    }

    fn sqlite(path: &Path, mode: Mode) -> Result<ConnectorMetadata, String> {
        introspect_with(
            &src("crm", SourceKind::Sqlite),
            path.to_str().unwrap(),
            mode,
        )
    }

    #[test]
    fn sqlite_tables_keys_references_and_uniques_are_reported() {
        let p = tmp_db("basic");
        make(
            &p,
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, note TEXT);
             CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), total REAL);
             INSERT INTO customers VALUES (1, 'a@x', NULL);",
        );
        let m = sqlite(&p, Mode::Schema).unwrap();
        assert_eq!(
            (m.connector_type.as_str(), m.label.as_str()),
            ("sqlite", "crm")
        );
        assert!(!m.truncated);
        let names: Vec<&str> = m.containers.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["customers", "orders"]);
        let cust = &m.containers[0];
        let f = |n: &str| cust.fields.iter().find(|f| f.name == n).unwrap();
        assert!(f("id").is_primary_key && !f("id").nullable);
        assert!(f("email").is_unique && !f("email").nullable && f("note").nullable);
        assert_eq!(cust.unique_groups, vec![vec!["email".to_string()]]);
        assert!(cust
            .fields
            .iter()
            .all(|f| f.provenance == aikoql_ingestion::Provenance::Declared));
        assert_eq!(
            cust.row_count, None,
            "schema discovery does not scan tables"
        );
        assert_eq!(m.references.len(), 1);
        let r = &m.references[0];
        assert_eq!(
            (r.from_container.as_str(), r.to_container.as_str()),
            ("orders", "customers")
        );
        assert_eq!(
            (r.from_fields.as_slice(), r.to_fields.as_slice()),
            (&["customer_id".to_string()][..], &["id".to_string()][..])
        );
    }

    #[test]
    fn max_containers_truncates_deterministically_and_flags_it() {
        let p = tmp_db("trunc");
        make(
            &p,
            "CREATE TABLE c (x); CREATE TABLE a (x); CREATE TABLE b (x);",
        );
        let mut s = src("crm", SourceKind::Sqlite);
        s.max_containers = 2;
        let m = introspect_with(&s, p.to_str().unwrap(), Mode::Schema).unwrap();
        assert!(m.truncated);
        assert_eq!(
            m.containers
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        s.max_containers = 3;
        assert!(
            !introspect_with(&s, p.to_str().unwrap(), Mode::Schema)
                .unwrap()
                .truncated
        );
    }

    #[test]
    fn unusable_sources_fail_short_and_leak_neither_path_nor_secret() {
        let secret_dir = format!("very-secret-dir-{}", std::process::id());
        let missing = std::env::temp_dir().join(&secret_dir).join("nope.sqlite");
        let e = sqlite(&missing, Mode::Schema).unwrap_err();
        assert!(
            !e.contains(&secret_dir) && e.starts_with("source crm:"),
            "{e}"
        );
        assert!(
            !missing.exists() && !missing.parent().unwrap().exists(),
            "nothing created"
        );

        let garbage = tmp_db("garbage");
        std::fs::write(&garbage, b"not a database ".repeat(100)).unwrap();
        let e = sqlite(&garbage, Mode::Schema).unwrap_err();
        assert!(e.starts_with("source crm:") && e.len() < 200, "{e}");

        let dir = tmp_db("dir").parent().unwrap().to_path_buf();
        assert!(
            sqlite(&dir, Mode::Schema).is_err(),
            "a directory is not a database"
        );
    }

    #[test]
    fn discovery_leaves_the_source_file_byte_identical() {
        let p = tmp_db("ro");
        make(&p, "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT); INSERT INTO t VALUES (1,'x'),(2,'y');");
        let before = std::fs::read(&p).unwrap();
        for mode in [Mode::Schema, Mode::Sample] {
            sqlite(&p, mode).unwrap();
        }
        assert_eq!(
            std::fs::read(&p).unwrap(),
            before,
            "introspection must not modify the source"
        );
        assert!(
            !p.with_extension("sqlite-journal").exists()
                && !p.with_extension("sqlite-wal").exists()
        );
    }

    #[test]
    fn five_thousand_tables_truncate_fast() {
        let p = tmp_db("many");
        let mut sql = String::from("BEGIN;");
        for i in 0..5_000 {
            sql.push_str(&format!(
                "CREATE TABLE t{i:05} (id INTEGER PRIMARY KEY, v TEXT);"
            ));
        }
        sql.push_str("COMMIT;");
        make(&p, &sql);
        let started = Instant::now();
        let m = sqlite(&p, Mode::Schema).unwrap();
        assert!(
            m.truncated && m.containers.len() == 500,
            "{} containers",
            m.containers.len()
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_2000_column_table_is_fully_reported() {
        let p = tmp_db("wide");
        let cols: Vec<String> = (0..2_000).map(|i| format!("c{i} INTEGER")).collect();
        make(&p, &format!("CREATE TABLE wide ({});", cols.join(", ")));
        let m = sqlite(&p, Mode::Schema).unwrap();
        assert_eq!(m.containers[0].fields.len(), 2_000);
    }

    #[test]
    fn hostile_names_round_trip_exactly() {
        let p = tmp_db("hostile");
        let long = "L".repeat(300);
        make(
            &p,
            &format!(
                "CREATE TABLE \"a\"\"b\" (\"x y\" INTEGER PRIMARY KEY, \"it's\" TEXT);
                 CREATE TABLE \"order\" (id INTEGER);
                 CREATE TABLE \"{long}\" (id INTEGER);
                 CREATE TABLE \"ünï\u{202e}💥\" (id INTEGER);"
            ),
        );
        let m = sqlite(&p, Mode::Schema).unwrap();
        let names: Vec<&str> = m.containers.iter().map(|c| c.name.as_str()).collect();
        for want in ["a\"b", "order", long.as_str(), "ünï\u{202e}💥"] {
            assert!(names.contains(&want), "{want:?} missing from {names:?}");
        }
        let ab = m.containers.iter().find(|c| c.name == "a\"b").unwrap();
        assert!(
            ab.fields.iter().any(|f| f.name == "x y") && ab.fields.iter().any(|f| f.name == "it's")
        );
    }

    #[test]
    fn a_locked_source_fails_within_the_timeout_not_forever() {
        let p = tmp_db("locked");
        make(&p, "CREATE TABLE t (id INTEGER PRIMARY KEY);");
        let writer = rusqlite::Connection::open(&p).unwrap();
        writer
            .execute_batch("BEGIN EXCLUSIVE; INSERT INTO t VALUES (1);")
            .unwrap();
        let mut s = src("crm", SourceKind::Sqlite);
        s.timeout_secs = 1;
        let started = Instant::now();
        let e = introspect_with(&s, p.to_str().unwrap(), Mode::Schema).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        assert!(e.starts_with("source crm:"), "{e}");
        writer.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn scrub_removes_the_connection_string_and_every_password_form() {
        let cases = [
            (
                "host=db user=app password=s3cr3t dbname=x",
                "failed: host=db user=app password=s3cr3t dbname=x refused",
            ),
            (
                "host=db user=app password=s3cr3t dbname=x",
                "bad value s3cr3t in config",
            ),
            (
                "http://neo4j:pw-123@h:7474",
                "request to http://neo4j:pw-123@h:7474 failed: pw-123",
            ),
            (
                "mongodb://u:p%40ss@h/db",
                "error for mongodb://u:p%40ss@h/db (p@ss)",
            ),
        ];
        for (conn, msg) in cases {
            let out = scrub(msg, conn);
            for secret in ["s3cr3t", "pw-123", "p%40ss", "p@ss"] {
                assert!(!out.contains(secret), "{secret:?} survived in {out:?}");
            }
            assert!(!out.contains(conn), "{out}");
        }
        assert_eq!(scrub("plain error", "host=h"), "plain error");
    }

    #[test]
    fn sources_that_are_not_wired_report_a_clear_error() {
        // a kind whose server is absent must fail like any unreachable source
        let mut s = src("pg", SourceKind::Postgres);
        s.timeout_secs = 1;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let started = Instant::now();
        let e = introspect_with(
            &s,
            &format!("host=127.0.0.1 port={port} user=u password=topsecret dbname=d"),
            Mode::Schema,
        )
        .unwrap_err();
        assert!(
            !e.contains("topsecret") && e.starts_with("source pg:"),
            "{e}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "{:?}",
            started.elapsed()
        );
    }

    // ---- pure mappers: provider structs -> ConnectorMetadata pieces ----

    #[test]
    fn mongo_fields_are_observed_and_keep_a_profile_only_in_sample_mode() {
        let stats = vec![
            aikoql_mongodb::FieldStats {
                name: "_id".into(),
                types: vec!["ObjectId".into()],
                present: 10,
                nulls: 0,
                samples: 10,
            },
            aikoql_mongodb::FieldStats {
                name: "age".into(),
                types: vec!["Int32".into(), "Null".into()],
                present: 9,
                nulls: 2,
                samples: 10,
            },
            aikoql_mongodb::FieldStats {
                name: "addr.city".into(),
                types: vec!["String".into()],
                present: 5,
                nulls: 0,
                samples: 10,
            },
        ];
        let sample = map_mongo("people", &stats, Mode::Sample);
        let f =
            |c: &ContainerInfo, n: &str| c.fields.iter().find(|f| f.name == n).cloned().unwrap();
        assert!(f(&sample, "_id").is_primary_key && !f(&sample, "_id").nullable);
        assert!(f(&sample, "age").nullable, "a null was seen");
        assert!(
            f(&sample, "addr.city").nullable,
            "absent from half the documents"
        );
        assert!(sample
            .fields
            .iter()
            .all(|f| f.provenance == aikoql_ingestion::Provenance::Observed));
        let p = f(&sample, "age").profile.unwrap();
        assert_eq!(p.types, ["Int32", "Null"]);
        assert!((p.presence_ratio - 0.9).abs() < 1e-9 && p.samples == 10);
        assert_eq!(
            f(&sample, "age").data_type,
            "Int32",
            "declared type shows the non-null observed type"
        );
        let schema = map_mongo("people", &stats, Mode::Schema);
        assert!(
            schema.fields.iter().all(|f| f.profile.is_none()),
            "Schema mode keeps no ratios"
        );
        assert!(map_mongo("empty", &[], Mode::Sample).fields.is_empty());
    }

    #[test]
    fn neo4j_properties_map_types_and_mandatory() {
        let props = vec![
            aikoql_neo4j::LabelProp {
                name: "name".into(),
                types: vec!["String".into()],
                mandatory: true,
            },
            aikoql_neo4j::LabelProp {
                name: "age".into(),
                types: vec!["Long".into(), "String".into()],
                mandatory: false,
            },
        ];
        let c = map_neo4j("Person", &props, Mode::Sample);
        assert_eq!(c.name, "Person");
        let name = c.fields.iter().find(|f| f.name == "name").unwrap();
        let age = c.fields.iter().find(|f| f.name == "age").unwrap();
        assert!(!name.nullable && age.nullable);
        assert_eq!(age.profile.as_ref().unwrap().types, ["Long", "String"]);
        assert!(map_neo4j("Person", &props, Mode::Schema)
            .fields
            .iter()
            .all(|f| f.profile.is_none()));
    }

    #[test]
    fn clickhouse_tables_map_keys_and_nullability() {
        let t = aikoql_clickhouse::TableSchema {
            name: "events".into(),
            engine: "MergeTree".into(),
            columns: vec![
                aikoql_clickhouse::ColumnInfo {
                    name: "ts".into(),
                    ch_type: "DateTime".into(),
                    is_nullable: false,
                    is_low_cardinality: false,
                    in_primary_key: true,
                },
                aikoql_clickhouse::ColumnInfo {
                    name: "note".into(),
                    ch_type: "String".into(),
                    is_nullable: true,
                    is_low_cardinality: false,
                    in_primary_key: false,
                },
            ],
            sorting_key: vec!["ts".into()],
            primary_key: vec!["ts".into()],
            partition_key: String::new(),
            row_count_estimate: 42,
        };
        let c = map_clickhouse(&t);
        assert_eq!(
            (c.name.as_str(), c.row_count),
            ("events", None),
            "estimates are not stored as counts"
        );
        let ts = c.fields.iter().find(|f| f.name == "ts").unwrap();
        assert!(ts.is_primary_key && !ts.nullable && ts.data_type == "DateTime");
        assert!(c.fields.iter().find(|f| f.name == "note").unwrap().nullable);
    }

    #[test]
    fn postgres_tables_map_schema_uniques_and_composite_fks() {
        let t = aikoql_postgres::TableSchema {
            name: "child".into(),
            schema: "hr".into(),
            columns: vec![
                aikoql_postgres::ColumnInfo {
                    name: "id".into(),
                    pg_type: "integer".into(),
                    is_nullable: false,
                    is_primary_key: true,
                },
                aikoql_postgres::ColumnInfo {
                    name: "code".into(),
                    pg_type: "text".into(),
                    is_nullable: true,
                    is_primary_key: false,
                },
                aikoql_postgres::ColumnInfo {
                    name: "x".into(),
                    pg_type: "integer".into(),
                    is_nullable: true,
                    is_primary_key: false,
                },
            ],
            primary_keys: vec!["id".into()],
            row_count_estimate: -1,
            unique_groups: vec![vec!["code".into()], vec!["x".into(), "id".into()]],
        };
        let c = map_postgres(&t);
        assert_eq!(c.schema.as_deref(), Some("hr"));
        assert_eq!(
            c.row_count, None,
            "-1 (never analyzed) is unknown, not a count"
        );
        assert!(
            c.fields
                .iter()
                .find(|f| f.name == "code")
                .unwrap()
                .is_unique,
            "single-column unique"
        );
        assert!(
            !c.fields.iter().find(|f| f.name == "x").unwrap().is_unique,
            "a member of a composite group is not unique alone"
        );
        assert_eq!(c.unique_groups.len(), 2);
        let fks = vec![
            aikoql_postgres::ForeignKeyInfo {
                constraint_name: "child_parent_fk".into(),
                child_table: "child".into(),
                child_column: "pa".into(),
                parent_table: "parent".into(),
                parent_column: "a".into(),
                child_columns: vec!["pa".into(), "pb".into()],
                parent_columns: vec!["a".into(), "b".into()],
            },
            aikoql_postgres::ForeignKeyInfo {
                constraint_name: "to_elsewhere".into(),
                child_table: "child".into(),
                child_column: "z".into(),
                parent_table: "not_included".into(),
                parent_column: "id".into(),
                child_columns: vec!["z".into()],
                parent_columns: vec!["id".into()],
            },
        ];
        let included: HashSet<String> = ["child", "parent"].iter().map(|s| s.to_string()).collect();
        let refs = map_pg_fks(&fks, &included);
        assert_eq!(
            refs.len(),
            1,
            "a reference outside the included containers is dropped"
        );
        assert_eq!(refs[0].from_fields, ["pa", "pb"]);
        assert_eq!(refs[0].to_fields, ["a", "b"]);
        assert_eq!(refs[0].name.as_deref(), Some("child_parent_fk"));
    }

    // ---- live (env-gated): seeded here, introspected through the production path ----

    fn live(var: &str) -> Option<String> {
        std::env::var(var).ok().filter(|v| !v.trim().is_empty())
    }

    #[test]
    fn live_postgres_reports_composite_keys_uniques_schema_and_is_read_only() {
        let Some(dsn) = live("AIKOQL_TEST_PG_DSN") else {
            return;
        };
        let admin = format!("{dsn}");
        let mut c = postgres::Client::connect(&admin, postgres::NoTls).unwrap();
        c.batch_execute("DROP DATABASE IF EXISTS src_t7_pg")
            .unwrap();
        c.batch_execute("CREATE DATABASE src_t7_pg").unwrap();
        let private = admin.replace("dbname=knowledge", "dbname=src_t7_pg");
        let mut p = postgres::Client::connect(&private, postgres::NoTls).unwrap();
        p.batch_execute(
            "CREATE TABLE parent (a INT, b INT, PRIMARY KEY (a, b));
             CREATE TABLE child (id INT PRIMARY KEY, pa INT, pb INT, code TEXT UNIQUE,
               FOREIGN KEY (pa, pb) REFERENCES parent (a, b));
             CREATE SCHEMA hr; CREATE TABLE hr.staff (id INT PRIMARY KEY);
             INSERT INTO parent VALUES (1,1);",
        )
        .unwrap();
        let m = introspect_with(&src("pg", SourceKind::Postgres), &private, Mode::Schema).unwrap();
        let child = m.containers.iter().find(|c| c.name == "child").unwrap();
        assert_eq!(child.schema.as_deref(), Some("public"));
        assert!(
            child
                .fields
                .iter()
                .find(|f| f.name == "code")
                .unwrap()
                .is_unique
        );
        assert_eq!(
            m.containers
                .iter()
                .find(|c| c.name == "staff")
                .unwrap()
                .schema
                .as_deref(),
            Some("hr")
        );
        let fk = m
            .references
            .iter()
            .find(|r| r.from_container == "child")
            .unwrap();
        assert_eq!(fk.from_fields, ["pa", "pb"]);
        // the data was not touched
        let rows = p.query("SELECT count(*) FROM parent", &[]).unwrap();
        assert_eq!(rows[0].get::<_, i64>(0), 1);
    }

    #[test]
    fn live_clickhouse_reports_keys_and_wrapped_types() {
        let (Some(url), Some(db)) = (
            live("AIKOQL_TEST_CLICKHOUSE_URL"),
            live("AIKOQL_TEST_CLICKHOUSE_DB"),
        ) else {
            return;
        };
        let (scheme, rest) = url.split_once("://").unwrap();
        let (ui, host) = rest.rsplit_once('@').unwrap();
        let (u, pw) = ui.split_once(':').unwrap();
        for sql in [
            "DROP TABLE IF EXISTS t7_events",
            "CREATE TABLE t7_events (ts DateTime, note Nullable(String)) ENGINE = MergeTree ORDER BY ts",
        ] {
            ureq::post(&format!("{scheme}://{host}/")).query("database", &db).set("X-ClickHouse-User", u).set("X-ClickHouse-Key", pw).send_string(sql).unwrap();
        }
        let mut s = src("ch", SourceKind::Clickhouse);
        s.database = Some(db);
        let m = introspect_with(&s, &url, Mode::Schema).unwrap();
        let ev = m.containers.iter().find(|c| c.name == "t7_events").unwrap();
        assert!(
            ev.fields
                .iter()
                .find(|f| f.name == "note")
                .unwrap()
                .nullable
        );
        assert!(
            ev.fields
                .iter()
                .find(|f| f.name == "ts")
                .unwrap()
                .is_primary_key
        );
    }

    #[test]
    fn live_neo4j_reports_label_properties_and_relationship_patterns() {
        let (Some(uri), Some(user), Some(pass)) = (
            live("AIKOQL_TEST_NEO4J_URI"),
            live("AIKOQL_TEST_NEO4J_USER"),
            live("AIKOQL_TEST_NEO4J_PASSWORD"),
        ) else {
            return;
        };
        let auth = format!(
            "Basic {}",
            aikoql_neo4j::base64_encode(&format!("{user}:{pass}"))
        );
        let body = serde_json::json!({"statements": [
            {"statement": "MATCH (n:T7Person) DETACH DELETE n"},
            {"statement": "CREATE (:T7Person {name:'a'})-[:T7_KNOWS]->(:T7Person {name:'b', age:3})"},
        ]});
        ureq::post(&format!("{}/db/neo4j/tx/commit", uri.trim_end_matches('/')))
            .set("Authorization", &auth)
            .send_json(&body)
            .unwrap();
        let conn = uri.replacen("://", &format!("://{user}:{pass}@"), 1);
        let m = introspect_with(&src("g", SourceKind::Neo4j), &conn, Mode::Sample).unwrap();
        let person = m.containers.iter().find(|c| c.name == "T7Person").unwrap();
        assert!(
            !person
                .fields
                .iter()
                .find(|f| f.name == "name")
                .unwrap()
                .nullable
        );
        assert!(
            person
                .fields
                .iter()
                .find(|f| f.name == "age")
                .unwrap()
                .nullable
        );
        assert!(m.references.iter().any(|r| r.from_container == "T7Person"
            && r.to_container == "T7Person"
            && r.name.as_deref() == Some("T7_KNOWS")));
    }
}
