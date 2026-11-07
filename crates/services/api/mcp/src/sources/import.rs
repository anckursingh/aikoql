//! `POST /api/v1/sources/import`: copy chosen containers of a source into the
//! knowledge base as objects typed by the physical container name.

use super::introspect::{scrub, GATE};
use super::{
    args_obj, audit, find_source, name_arg, store, tenant_arg, SourceKind, INVALID, NO_SCHEMA,
};
use crate::imports::{fresh_run_id, ImportSink};
use crate::{json, Kernel, Subject, J};
use aikoql_kernel::KnowledgeObject;
use std::collections::{BTreeMap, HashSet};

/// Rows of one container, capped; `true` = the container had more.
type Fetched = (Vec<KnowledgeObject>, bool);

fn fetch(
    kind: SourceKind,
    conn: &str,
    database: &str,
    ms: u64,
    container: &str,
    tenant: Option<&str>,
    cap: u64,
    _deadline: std::time::Duration,
) -> Result<Fetched, String> {
    match kind {
        SourceKind::Sqlite => {
            let c = aikoql_sqlite::SqliteConnector::open_read_only(conn, ms)?;
            let schema = c.introspect_table_with(container, false)?;
            c.import_table_limited(&schema, tenant, cap)
        }
        SourceKind::Postgres => {
            let mut c = aikoql_postgres::PostgresConnector::connect_with_timeout(conn, Some(ms))?;
            c.read_only_session()?;
            let schema = c.introspect_table_with(container, false)?;
            c.import_table_limited(&schema, tenant, cap)
        }
        SourceKind::Mongodb => {
            let c = aikoql_mongodb::MongoConnector::connect_with_timeout(conn, database, Some(ms))?;
            let schema = aikoql_mongodb::CollectionSchema {
                name: container.to_string(),
                document_count: 0,
                properties: Vec::new(),
            };
            c.import_collection_limited(&schema, tenant, cap)
        }
        other => Err(format!(
            "import is not supported for {} sources",
            other.as_str()
        )),
    }
}

/// `POST /api/v1/sources/import {source, containers, tenant?}`.
pub(crate) fn tool_sources_import(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let result = run(k, args);
    match &result {
        Ok(r) => audit(
            db_path,
            args,
            "imported",
            &format!(
                "source={} containers={} truncated={}",
                args.get("source").and_then(|s| s.as_str()).unwrap_or(""),
                r["imported"]
                    .as_object()
                    .map(|m| m.keys().cloned().collect::<Vec<_>>().join(","))
                    .unwrap_or_default(),
                r["truncated"]
            ),
        ),
        Err(e) => audit(
            db_path,
            args,
            "error",
            &format!("import reason={}", e.chars().take(60).collect::<String>()),
        ),
    }
    result
}

fn run(k: &Kernel, args: &J) -> Result<J, String> {
    let o = args_obj(args)?;
    let name = name_arg(o.get("source"), "source")?;
    let tenant = tenant_arg(o)?;
    let containers: Vec<String> = match o.get("containers") {
        Some(J::Array(a)) if !a.is_empty() && a.len() <= 100 => a
            .iter()
            .map(|v| match v {
                J::String(s) if !s.is_empty() && s.chars().count() <= 1_000 => Ok(s.clone()),
                _ => Err(format!(
                    "{INVALID}containers must be strings of 1-1000 characters"
                )),
            })
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(format!(
                "{INVALID}containers must be a list of 1-100 container names"
            ))
        }
    };
    let src = find_source(&name, tenant.as_deref())?;
    let importer = match src.kind {
        SourceKind::Sqlite => "sqlite-importer",
        SourceKind::Postgres => "pg-importer",
        SourceKind::Mongodb => "mongo-importer",
        other => {
            return Err(format!(
                "{INVALID}import is not supported for {} sources; use the CLI import",
                other.as_str()
            ))
        }
    };
    let snapshot = store::latest_snapshot(k, &src)?
        .ok_or_else(|| format!("source {name} has {NO_SCHEMA} - discover it first"))?;
    let known: HashSet<&str> = snapshot
        .containers
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    for c in &containers {
        if !known.contains(c.as_str()) {
            let shown: String = c.chars().take(40).flat_map(char::escape_debug).collect();
            return Err(format!(
                "{INVALID}unknown container '{shown}' - discover the source first"
            ));
        }
    }
    let conn = src.connection()?;
    let mut subject = Subject::new(importer);
    if let Some(t) = &src.tenant {
        subject = subject.in_tenant(t);
    }
    let run_id = fresh_run_id();
    let sink = ImportSink::new(k, run_id.clone(), subject.clone(), vec![conn.clone()]);
    let tag = format!("src:{}", src.name);
    let (mut imported, mut unchanged) = (BTreeMap::new(), BTreeMap::new());
    let (mut truncated, mut skipped) = (Vec::new(), Vec::new());
    let skip = |c: &str, why: String| json!({"container": c, "reason": why});

    for c in &containers {
        // Objects are keyed by (container, primary key): never overwrite another source's.
        let foreign = k
            .scan_by_type(&subject, c)
            .map_err(|e| e.to_string())?
            .iter()
            .any(|ko| {
                ko.metadata.tenant.as_deref() == src.tenant.as_deref()
                    && !ko.metadata.tags.contains(&tag)
            });
        if foreign {
            skipped.push(skip(
                c,
                "this type already holds objects from another source (or a CLI import); not overwritten".into(),
            ));
            continue;
        }
        let (kind, conn_c, container, tenant_c) =
            (src.kind, conn.clone(), c.clone(), src.tenant.clone());
        let (database, ms, cap) = (
            src.database.clone().unwrap_or_default(),
            src.timeout_secs.saturating_mul(1000),
            src.import_max_rows,
        );
        // an import may legitimately run long, but not forever: a generous deadline
        let deadline =
            std::time::Duration::from_millis(ms.saturating_mul(20).clamp(60_000, 600_000));
        let (objects, cut) = match GATE.run(deadline, move || {
            fetch(
                kind,
                &conn_c,
                &database,
                ms,
                &container,
                tenant_c.as_deref(),
                cap,
                deadline,
            )
        }) {
            Ok(v) => v,
            Err(e) => {
                skipped.push(skip(c, scrub(&e, &conn)));
                continue;
            }
        };
        let (mut wrote, mut same) = (0u64, 0u64);
        let mut failure = None;
        for mut ko in objects {
            ko.metadata.tags.push(tag.clone());
            match sink.put(ko, importer, "imported by source discovery") {
                Ok(true) => wrote += 1,
                Ok(false) => same += 1,
                Err(e) => {
                    failure = Some(scrub(&e.to_string(), &conn));
                    break;
                }
            }
        }
        if let Some(e) = failure {
            skipped.push(skip(c, format!("failed after {} rows: {e}", wrote + same)));
            continue;
        }
        imported.insert(c.clone(), wrote);
        unchanged.insert(c.clone(), same);
        if cut {
            truncated.push(c.clone());
        }
    }
    Ok(json!({
        "run_id": run_id, "imported": imported, "unchanged": unchanged,
        "truncated": truncated, "skipped": skipped,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_hung_source_call_ends_and_frees_its_gate_slot() {
        use crate::sources::introspect::Gate;
        use crate::sources::testkit::{make, tmp_db};
        // a view whose every read runs forever: the source never answers
        let p = tmp_db("hang");
        make(
            &p,
            "CREATE TABLE seed (id INTEGER);
             CREATE VIEW v AS SELECT count(*) AS n FROM
               (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT x FROM c);",
        );
        let gate = Gate::new(2);
        let conn = p.to_str().unwrap().to_string();
        let deadline = std::time::Duration::from_millis(400);
        let r = gate.run(std::time::Duration::from_millis(1500), move || {
            fetch(SourceKind::Sqlite, &conn, "", 300, "v", None, 10, deadline)
        });
        assert!(r.is_err(), "a hung source must fail the call");
        // the abandoned thread must actually end, not spin until the process dies
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while gate.stuck() > 0 && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(gate.stuck(), 0, "the gate slot was never freed");
    }

    use super::*;
    use crate::json;
    use crate::sources::service::tool_sources_discover;
    use crate::sources::testkit::*;
    use crate::sources::{with_test_sources, SourceConfig, SourceKind, INVALID, NO_SCHEMA};
    use aikoql_kernel::Value;

    fn run<R>(src: SourceConfig, db: &std::path::Path, f: impl FnOnce() -> R) -> R {
        let name = src.name.clone();
        with_test_sources(vec![src], vec![(name, conn(db))], f)
    }

    fn live(k: &Kernel, ty: &str, tenant: Option<&str>) -> Vec<aikoql_kernel::KnowledgeObject> {
        let s = crate::Subject::with_roles("test", &["admin"]);
        let s = match tenant {
            Some(t) => s.in_tenant(t),
            None => s,
        };
        k.scan_by_type(&s, ty)
            .unwrap()
            .into_iter()
            .filter(|ko| ko.metadata.tenant.as_deref() == tenant)
            .collect()
    }

    fn head(k: &Kernel) -> u64 {
        k.journal_head().unwrap().0
    }

    fn items_db(tag: &str, rows: u32) -> std::path::PathBuf {
        let p = tmp_db(tag);
        let mut sql =
            String::from("CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT); BEGIN;");
        for i in 1..=rows {
            sql.push_str(&format!("INSERT INTO items VALUES ({i}, 'item {i}');"));
        }
        sql.push_str("COMMIT;");
        make(&p, &sql);
        p
    }

    fn import(k: &Kernel, names: &[&str]) -> Result<J, String> {
        tool_sources_import(k, &json!({"source": "crm", "containers": names}), "x")
    }

    #[test]
    fn import_creates_objects_typed_by_the_physical_container_with_the_source_tag() {
        let (k, db) = (kernel(), crm_db("imp1"));
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let r = import(&k, &["customers"]).unwrap();
            assert_eq!(r["imported"]["customers"], 1);
            assert_eq!(r["truncated"], json!([]));
            let kos = live(&k, "customers", None);
            assert_eq!(kos.len(), 1);
            assert_eq!(
                kos[0].properties.get("email"),
                Some(&Value::Text("a@x.test".into()))
            );
            assert!(
                kos[0].metadata.tags.iter().any(|t| t == "src:crm"),
                "{:?}",
                kos[0].metadata.tags
            );
        });
    }

    #[test]
    fn a_tenant_sources_objects_land_in_that_tenant_only() {
        let (k, db) = (kernel(), crm_db("imp2"));
        run(sqlite_source("crm", Some("acme")), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm", "tenant": "acme"}), "x").unwrap();
            tool_sources_import(
                &k,
                &json!({"source": "crm", "tenant": "acme", "containers": ["customers"]}),
                "x",
            )
            .unwrap();
            assert_eq!(live(&k, "customers", Some("acme")).len(), 1);
            assert!(live(&k, "customers", None).is_empty());
            assert!(live(&k, "customers", Some("globex")).is_empty());
        });
    }

    #[test]
    fn unknown_undiscovered_and_malformed_containers_are_client_errors() {
        let (k, db) = (kernel(), crm_db("imp3"));
        run(sqlite_source("crm", None), &db, || {
            let e = import(&k, &["customers"]).unwrap_err();
            assert!(e.contains(NO_SCHEMA), "not discovered yet: {e}");
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let e = import(&k, &["nope"]).unwrap_err();
            assert!(e.starts_with(INVALID), "{e}");
            for bad in [
                json!({"source": "crm"}),
                json!({"source": "crm", "containers": []}),
                json!({"source": "crm", "containers": "customers"}),
                json!({"source": "crm", "containers": [1]}),
                json!({"source": "crm", "containers": (0..101).map(|i| format!("t{i}")).collect::<Vec<_>>()}),
            ] {
                let e = tool_sources_import(&k, &bad, "x").unwrap_err();
                assert_eq!(crate::sources::status_for(&e), Some(400), "{bad} -> {e}");
            }
            assert!(
                live(&k, "customers", None).is_empty(),
                "nothing was written"
            );
        });
    }

    #[test]
    fn the_row_cap_is_reported_never_silent() {
        let (k, db) = (kernel(), items_db("imp4", 5));
        let mut s = sqlite_source("crm", None);
        s.import_max_rows = 2;
        run(s, &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let r = import(&k, &["items"]).unwrap();
            assert_eq!(r["imported"]["items"], 2);
            assert_eq!(r["truncated"], json!(["items"]));
            assert_eq!(live(&k, "items", None).len(), 2);
        });
    }

    #[test]
    fn importing_twice_changes_nothing_and_a_changed_row_updates_in_place() {
        let (k, db) = (kernel(), items_db("imp5", 4));
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            import(&k, &["items"]).unwrap();
            let before = head(&k);
            let again = import(&k, &["items"]).unwrap();
            assert_eq!(again["imported"]["items"], 0);
            assert_eq!(again["unchanged"]["items"], 4);
            assert_eq!(
                head(&k),
                before,
                "an identical re-import must not move the journal"
            );
            assert_eq!(live(&k, "items", None).len(), 4);
            make(&db, "UPDATE items SET label = 'renamed' WHERE id = 2;");
            let r = import(&k, &["items"]).unwrap();
            assert_eq!(
                (
                    r["imported"]["items"].clone(),
                    r["unchanged"]["items"].clone()
                ),
                (json!(1), json!(3))
            );
            let ko = live(&k, "items", None)
                .into_iter()
                .find(|ko| ko.properties.get("id") == Some(&Value::Int(2)))
                .unwrap();
            assert_eq!(
                ko.properties.get("label"),
                Some(&Value::Text("renamed".into()))
            );
            assert_eq!(ko.version, 2);
        });
    }

    #[test]
    fn a_type_already_holding_another_sources_objects_is_refused_not_overwritten() {
        let k = kernel();
        let (a, b) = (crm_db("imp6a"), crm_db("imp6b"));
        with_test_sources(
            vec![sqlite_source("crm", None), sqlite_source("other", None)],
            vec![("crm".into(), conn(&a)), ("other".into(), conn(&b))],
            || {
                for n in ["crm", "other"] {
                    tool_sources_discover(&k, &json!({"source": n}), "x").unwrap();
                }
                import(&k, &["customers"]).unwrap();
                let r = tool_sources_import(
                    &k,
                    &json!({"source": "other", "containers": ["customers"]}),
                    "x",
                )
                .unwrap();
                assert_eq!(r["imported"].get("customers"), None);
                let skipped = r["skipped"].as_array().unwrap();
                assert_eq!(skipped.len(), 1, "{r}");
                assert!(
                    skipped[0]["reason"]
                        .as_str()
                        .unwrap()
                        .contains("another source"),
                    "{r}"
                );
                assert!(live(&k, "customers", None)[0]
                    .metadata
                    .tags
                    .iter()
                    .any(|t| t == "src:crm"));
            },
        );
    }

    #[test]
    fn kinds_without_an_in_server_importer_are_refused_clearly() {
        let k = kernel();
        for kind in [SourceKind::Neo4j, SourceKind::Clickhouse] {
            let mut s = sqlite_source("crm", None);
            s.kind = kind;
            s.database = Some("d".into());
            with_test_sources(vec![s], vec![("crm".into(), "x".into())], || {
                let e = import(&k, &["t"]).unwrap_err();
                assert!(
                    e.starts_with(INVALID) && e.contains("not supported") && e.contains("CLI"),
                    "{e}"
                );
            });
        }
    }

    #[test]
    fn import_never_writes_to_the_source_file() {
        let (k, db) = (kernel(), items_db("imp7", 3));
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let before = std::fs::read(&db).unwrap();
            import(&k, &["items"]).unwrap();
            assert_eq!(std::fs::read(&db).unwrap(), before);
        });
    }

    #[test]
    fn hostile_container_names_import_as_exact_types() {
        let (k, db) = (kernel(), tmp_db("imp8"));
        make(&db, "CREATE TABLE \"we\"\"ird name\" (id INTEGER PRIMARY KEY, \"it's\" TEXT); INSERT INTO \"we\"\"ird name\" VALUES (1, 'x'); CREATE TABLE \"order\" (id INTEGER PRIMARY KEY); INSERT INTO \"order\" VALUES (9);");
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let r = import(&k, &["we\"ird name", "order"]).unwrap();
            assert_eq!(r["imported"]["we\"ird name"], 1);
            assert_eq!(r["imported"]["order"], 1);
            assert_eq!(
                live(&k, "we\"ird name", None)[0].properties.get("it's"),
                Some(&Value::Text("x".into()))
            );
        });
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn fuzzed_rows_import_exactly_and_reimport_is_a_no_op() {
        let iters = std::env::var("AIKOQL_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_000)
            / 100;
        for seed in 1..=iters.max(8) {
            let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (k, db) = (kernel(), tmp_db(&format!("imp-f{seed}")));
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE f (id INTEGER PRIMARY KEY, t TEXT, n INTEGER, x REAL, b BLOB)",
            )
            .unwrap();
            let rows = 1 + r.next() % 40;
            for id in 0..rows as i64 {
                let text: Option<String> = match r.next() % 6 {
                    0 => None,
                    1 => Some(String::new()),
                    2 => Some("a\u{0}b\n\"'`;--".into()),
                    3 => Some("💥\u{202e}ünï".into()),
                    4 => Some("L".repeat(100_000)),
                    _ => Some(format!("row {id}")),
                };
                let n: Option<i64> = [None, Some(i64::MIN), Some(i64::MAX), Some(0), Some(-1)]
                    [(r.next() % 5) as usize];
                let x: Option<f64> =
                    [None, Some(f64::INFINITY), Some(-0.0), Some(1.5e300)][(r.next() % 4) as usize];
                let b: Option<Vec<u8>> = r.next().is_multiple_of(3).then(|| vec![0u8, 255, 7]);
                c.execute(
                    "INSERT INTO f VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![id, text, n, x, b],
                )
                .unwrap();
            }
            drop(c);
            run(sqlite_source("crm", None), &db, || {
                tool_sources_discover(&k, &json!({"source": "crm"}), "x")
                    .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
                let r1 = import(&k, &["f"]).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
                assert_eq!(r1["imported"]["f"], rows, "seed {seed}");
                assert_eq!(live(&k, "f", None).len() as u64, rows, "seed {seed}");
                let before = head(&k);
                let r2 = import(&k, &["f"]).unwrap();
                assert_eq!(
                    (r2["imported"]["f"].clone(), r2["unchanged"]["f"].clone()),
                    (json!(0), json!(rows)),
                    "seed {seed}"
                );
                assert_eq!(head(&k), before, "seed {seed}: re-import must be a no-op");
            });
        }
    }

    #[test]
    fn the_audit_line_names_ids_only() {
        let (k, db) = (kernel(), crm_db("imp9"));
        let log = std::env::temp_dir().join(format!("aikoql-import-audit-{}", std::process::id()));
        let log = log.to_str().unwrap().to_string();
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            tool_sources_import(
                &k,
                &json!({"source": "crm", "containers": ["customers"], "subject": "alice"}),
                &log,
            )
            .unwrap();
        });
        let path = crate::audit::audit_log_path(&log);
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(
            text.contains("imported") && text.contains("customers"),
            "{text}"
        );
        assert!(
            !text.contains("a@x.test") && !text.contains(db.to_str().unwrap()),
            "no row values or paths: {text}"
        );
    }
}
