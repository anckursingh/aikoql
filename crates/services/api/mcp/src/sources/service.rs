//! `GET /api/v1/sources` and `POST /api/v1/sources/discover`.

use super::introspect::{introspect_source, Mode};
use super::{args_obj, audit, find_source, name_arg, store, tenant_arg, INVALID};
use crate::{json, Kernel, J};

/// `GET /api/v1/sources`: the sources of the request's tenant with their last run.
pub(crate) fn tool_sources_list(k: &Kernel, args: &J) -> Result<J, String> {
    let empty = serde_json::Map::new();
    let o = if args.is_null() {
        &empty
    } else {
        args_obj(args)?
    };
    let tenant = tenant_arg(o)?;
    let mut out = Vec::new();
    for s in super::configured()
        .into_iter()
        .filter(|s| s.tenant == tenant)
    {
        out.push(json!({
            "name": s.name,
            "kind": s.kind.as_str(),
            "tenant": s.tenant,
            "database": s.database,
            "last": store::last_summary(k, &s)?,
        }));
    }
    Ok(J::Array(out))
}

/// `POST /api/v1/sources/discover {source, mode?, tenant?}`.
pub(crate) fn tool_sources_discover(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let result = discover(k, args);
    match &result {
        Ok(r) => audit(
            db_path,
            args,
            "discovered",
            &format!(
                "source={} changed={} fp={}",
                r["source"].as_str().unwrap_or(""),
                r["changed"],
                r["fingerprint"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(12)
                    .collect::<String>()
            ),
        ),
        Err(e) => audit(
            db_path,
            args,
            "error",
            &format!(
                "source={} reason={}",
                args.get("source").and_then(|s| s.as_str()).unwrap_or("?"),
                e.chars().take(60).collect::<String>()
            ),
        ),
    }
    result
}

fn discover(k: &Kernel, args: &J) -> Result<J, String> {
    let o = args_obj(args)?;
    let name = name_arg(o.get("source"), "source")?;
    let tenant = tenant_arg(o)?;
    let mode = match o.get("mode") {
        None | Some(J::Null) => Mode::Schema,
        Some(J::String(m)) if m == "schema" => Mode::Schema,
        Some(J::String(m)) if m == "sample" => Mode::Sample,
        _ => return Err(format!("{INVALID}mode must be \"schema\" or \"sample\"")),
    };
    let src = find_source(&name, tenant.as_deref())?;
    let meta = introspect_source(&src, mode)?;
    let out = store::record_snapshot(k, &src, &meta)?;
    Ok(json!({
        "source": src.name,
        "changed": out.changed,
        "fingerprint": out.fingerprint,
        "previous_fingerprint": out.previous_fingerprint,
        "run_koid": out.run_koid,
        "diff": serde_json::to_value(&out.diff).map_err(|e| e.to_string())?,
        "containers": meta.containers.len(),
        "references": meta.references.len(),
        "truncated": meta.truncated,
        "version": meta.version,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use crate::sources::testkit::*;
    use crate::sources::{with_test_sources, INVALID};

    fn run<R>(name: &str, tenant: Option<&str>, db: &std::path::Path, f: impl FnOnce() -> R) -> R {
        with_test_sources(
            vec![sqlite_source(name, tenant)],
            vec![(name.to_string(), conn(db))],
            f,
        )
    }

    fn head(k: &Kernel) -> u64 {
        k.journal_head().unwrap().0
    }

    #[test]
    fn discover_records_a_snapshot_then_reports_no_change_then_the_diff() {
        let k = kernel();
        let db = crm_db("disc");
        run("crm", None, &db, || {
            let first = tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            assert_eq!(first["changed"], true);
            assert_eq!(first["containers"], 2);
            assert_eq!(first["references"], 1);
            assert_eq!(
                first["diff"]["added_containers"],
                json!(["customers", "orders"])
            );
            let before = head(&k);
            let again = tool_sources_discover(&k, &json!({"source": "crm", "mode": "sample"}), "x")
                .unwrap();
            assert_eq!(again["changed"], false);
            assert_eq!(head(&k), before);
            make(&db, "ALTER TABLE customers ADD COLUMN phone TEXT;");
            let third = tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            assert_eq!(third["changed"], true);
            assert_eq!(
                third["diff"]["changed"][0]["added_fields"],
                json!(["phone"])
            );
            assert_eq!(third["previous_fingerprint"], first["fingerprint"]);
        });
    }

    #[test]
    fn unknown_and_foreign_tenant_sources_look_identical() {
        let k = kernel();
        let db = crm_db("tenant");
        run("crm", Some("acme"), &db, || {
            let foreign =
                tool_sources_discover(&k, &json!({"source": "crm", "tenant": "globex"}), "x")
                    .unwrap_err();
            let none = tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap_err();
            let missing =
                tool_sources_discover(&k, &json!({"source": "nope", "tenant": "acme"}), "x")
                    .unwrap_err();
            assert_eq!(foreign, crate::sources::NOT_FOUND);
            assert_eq!(
                (none.as_str(), missing.as_str()),
                (crate::sources::NOT_FOUND, crate::sources::NOT_FOUND)
            );
            assert!(
                tool_sources_discover(&k, &json!({"source": "crm", "tenant": "acme"}), "x").is_ok()
            );
        });
    }

    #[test]
    fn a_failing_source_is_a_secret_free_source_error_and_records_nothing() {
        let k = kernel();
        let missing = std::env::temp_dir()
            .join(format!("very-secret-dir-{}", std::process::id()))
            .join("x.sqlite");
        run("crm", None, &missing, || {
            let before = head(&k);
            let e = tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap_err();
            assert!(e.starts_with("source crm: "), "{e}");
            assert!(!e.contains("very-secret-dir"), "{e}");
            assert_eq!(head(&k), before);
            assert_eq!(crate::sources::status_for(&e), Some(502));
        });
    }

    #[test]
    fn list_shows_only_this_tenants_sources_with_their_last_run() {
        let k = kernel();
        let db = crm_db("list");
        with_test_sources(
            vec![
                sqlite_source("crm", Some("acme")),
                sqlite_source("shared", None),
            ],
            vec![("crm".into(), conn(&db)), ("shared".into(), conn(&db))],
            || {
                let names = |tenant: Option<&str>| -> Vec<String> {
                    let args = match tenant {
                        Some(t) => json!({"tenant": t}),
                        None => json!({}),
                    };
                    tool_sources_list(&k, &args)
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| s["name"].as_str().unwrap().to_string())
                        .collect()
                };
                assert_eq!(names(Some("acme")), ["crm"]);
                assert_eq!(names(None), ["shared"]);
                assert!(names(Some("globex")).is_empty());
                let before = tool_sources_list(&k, &json!({"tenant": "acme"})).unwrap();
                assert!(before[0]["last"].is_null());
                tool_sources_discover(&k, &json!({"source": "crm", "tenant": "acme"}), "x")
                    .unwrap();
                let after = tool_sources_list(&k, &json!({"tenant": "acme"})).unwrap();
                assert_eq!(after[0]["last"]["containers"], 2);
                assert_eq!(
                    after[0]["last"]["container_names"],
                    json!(["customers", "orders"])
                );
                assert_eq!(after[0]["kind"], "sqlite");
                assert!(
                    after[0].get("url_env").is_none() && after[0].get("connection").is_none(),
                    "no connection detail is exposed"
                );
            },
        );
    }

    #[test]
    fn invalid_arguments_are_client_errors() {
        let k = kernel();
        let db = crm_db("args");
        run("crm", None, &db, || {
            for bad in [
                json!({}),
                json!({"source": 5}),
                json!({"source": ""}),
                json!({"source": "crm", "mode": "wat"}),
                json!({"source": "crm", "tenant": ""}),
                json!({"source": "crm", "tenant": 7}),
                json!([1, 2]),
                J::Null,
            ] {
                let e = tool_sources_discover(&k, &bad, "x").unwrap_err();
                assert!(e.starts_with(INVALID), "{bad} -> {e}");
                assert_eq!(crate::sources::status_for(&e), Some(400));
            }
        });
    }

    #[test]
    fn the_audit_line_has_ids_and_no_connection_detail() {
        let k = kernel();
        let db = crm_db("audit");
        let log_base =
            std::env::temp_dir().join(format!("aikoql-sources-audit-{}", std::process::id()));
        let log_base = log_base.to_str().unwrap().to_string();
        run("crm", None, &db, || {
            tool_sources_discover(&k, &json!({"source": "crm", "subject": "alice"}), &log_base)
                .unwrap();
            let _ = tool_sources_discover(
                &k,
                &json!({"source": "nope", "subject": "alice"}),
                &log_base,
            );
        });
        let path = crate::audit::audit_log_path(&log_base);
        let log = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(
            log.contains("\"tool\":\"sources\"") && log.contains("discovered"),
            "{log}"
        );
        assert!(log.contains("error"), "failures are audited too: {log}");
        assert!(
            !log.contains(db.to_str().unwrap()) && !log.contains("aikoql-sources-"),
            "{log}"
        );
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

    fn random_json(r: &mut Rng, depth: u32) -> J {
        match r.next() % if depth > 4 { 5 } else { 7 } {
            0 => J::Null,
            1 => json!(r.next().is_multiple_of(2)),
            2 => json!(r.next() as i64),
            3 => json!(
                ["", "crm", "nope", "a\u{0}b", "💥", &"x".repeat(300)][(r.next() % 6) as usize]
            ),
            4 => json!(1.5),
            5 => J::Array(
                (0..r.next() % 4)
                    .map(|_| random_json(r, depth + 1))
                    .collect(),
            ),
            _ => J::Object(
                (0..r.next() % 5)
                    .map(|_| {
                        (
                            ["source", "mode", "tenant", "sources", "merges", "x"]
                                [(r.next() % 6) as usize]
                                .to_string(),
                            random_json(r, depth + 1),
                        )
                    })
                    .collect(),
            ),
        }
    }

    #[test]
    fn argument_fuzz_never_panics_and_never_5xx_for_client_errors() {
        let iters = std::env::var("AIKOQL_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_000);
        let k = kernel();
        let db = crm_db("fuzz");
        run("crm", None, &db, || {
            for seed in 1..=iters {
                let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
                let args = random_json(&mut r, 0);
                for res in [
                    tool_sources_discover(&k, &args, "x").map(|_| ()),
                    tool_sources_list(&k, &args).map(|_| ()),
                ] {
                    if let Err(e) = res {
                        let s = crate::sources::status_for(&e);
                        assert!(
                            matches!(s, Some(400) | Some(404) | Some(502)),
                            "seed {seed}: {args} -> {e:?} -> {s:?}"
                        );
                        assert!(
                            !e.contains(db.to_str().unwrap()),
                            "seed {seed}: path leaked: {e}"
                        );
                    }
                }
            }
        });
    }
}
