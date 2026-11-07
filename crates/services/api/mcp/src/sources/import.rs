//! `POST /api/v1/sources/import`: copy chosen containers of a source into the
//! knowledge base as objects typed by the physical container name.

use crate::{Kernel, J};

pub(crate) fn tool_sources_import(_k: &Kernel, _args: &J, _db_path: &str) -> Result<J, String> {
    Err("todo".into())
}

#[cfg(test)]
mod tests {
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
                let b: Option<Vec<u8>> = (r.next() % 3 == 0).then(|| vec![0u8, 255, 7]);
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
