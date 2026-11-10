//! `POST /api/v1/sources/resolve {class}`: reconcile what several sources assert about
//! one canonical entity (docs/ONTOLOGY-IN-PRACTICE.md, Phase C).
//!
//! Rows are claims. Each container that declares an assertion for the class says which
//! column holds the entity's key and which columns assert which canonical properties;
//! rows with an exactly equal key are about the same entity. Resolution writes, under
//! reserved `aikoql:` types so it never collides with an imported container:
//! `aikoql:canonical` (one per entity), `aikoql:source-identity` (one claim per source
//! row, with a RESOLVES_TO edge) and `aikoql:contradiction` (one per property the
//! sources disagree about, with the rule that decides). Ids are deterministic, so a
//! repeat run changes nothing and the order the sources were imported in is irrelevant.

use crate::{json, Kernel, J};

pub(crate) fn tool_sources_resolve(_k: &Kernel, _args: &J, _db_path: &str) -> Result<J, String> {
    Err("unimplemented".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::import::tool_sources_import;
    use crate::sources::ontology::tool_sources_ontology;
    use crate::sources::service::tool_sources_discover;
    use crate::sources::testkit::*;
    use crate::sources::{with_test_sources, SourceConfig};
    use aikoql_kernel::{KnowledgeObject, Value};

    const ACCOUNTS: &str = "CREATE TABLE accounts (id TEXT PRIMARY KEY, status TEXT);
                            CREATE TABLE account_risk (account_id TEXT PRIMARY KEY, decision TEXT);
                            INSERT INTO accounts VALUES ('acct_77', 'active'), ('acct_78', 'active'), ('acct_79', 'closed');
                            INSERT INTO account_risk VALUES ('acct_77', 'block'), ('acct_78', 'active');";

    fn run<R>(src: SourceConfig, db: &std::path::Path, f: impl FnOnce() -> R) -> R {
        let name = src.name.clone();
        with_test_sources(vec![src], vec![(name, conn(db))], f)
    }

    fn assertions(crm_rank: u64, risk_rank: u64) -> J {
        json!([
            {"from": "crm.accounts.id", "class": "Account", "properties": {"status": "status"}, "rank": crm_rank},
            {"from": "crm.account_risk.account_id", "class": "Account", "properties": {"decision": "status"}, "rank": risk_rank},
        ])
    }

    fn publish(k: &Kernel, tenant: Option<&str>, asserts: J) {
        let mut args = json!({"sources": ["crm"], "preview": true, "assertions": asserts});
        if let Some(t) = tenant {
            args["tenant"] = json!(t);
        }
        let p = tool_sources_ontology(k, &args, "x").unwrap();
        args["preview"] = json!(false);
        args["fingerprints"] = p["fingerprints"].clone();
        let r = tool_sources_ontology(k, &args, "x").unwrap();
        assert_eq!(r["saved"], true, "{r}");
        assert!(r["warnings"].to_string().find("assertion ignored").is_none(), "{r}");
    }

    fn import(k: &Kernel, names: &[&str], tenant: Option<&str>) {
        let mut args = json!({"source": "crm", "containers": names});
        if let Some(t) = tenant {
            args["tenant"] = json!(t);
        }
        tool_sources_import(k, &args, "x").unwrap();
    }

    fn resolve(k: &Kernel, tenant: Option<&str>) -> J {
        let mut args = json!({"class": "Account"});
        if let Some(t) = tenant {
            args["tenant"] = json!(t);
        }
        tool_sources_resolve(k, &args, "x").unwrap()
    }

    fn live(k: &Kernel, ty: &str, tenant: Option<&str>) -> Vec<KnowledgeObject> {
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

    fn text(ko: &KnowledgeObject, p: &str) -> String {
        match ko.properties.get(p) {
            Some(Value::Text(t)) => t.clone(),
            other => panic!("{p}: {other:?}"),
        }
    }

    fn setup(tag: &str, tenant: Option<&str>) -> (std::sync::Arc<Kernel>, std::path::PathBuf) {
        let (k, db) = (kernel(), tmp_db(tag));
        make(&db, ACCOUNTS);
        let _ = tenant;
        (k, db)
    }

    #[test]
    fn two_sources_that_disagree_about_one_account_leave_a_contradiction_the_authority_decides() {
        let (k, db) = setup("c1", None);
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            publish(&k, None, assertions(10, 20));
            import(&k, &["accounts", "account_risk"], None);
            let r = resolve(&k, None);
            assert_eq!((r["entities"].clone(), r["claims"].clone()), (json!(3), json!(5)), "{r}");
            assert_eq!(r["contradictions"]["open"], 0, "{r}");
            assert_eq!(r["contradictions"]["resolved"], 1, "{r}");

            let cs = live(&k, "aikoql:contradiction", None);
            assert_eq!(cs.len(), 1, "only acct_77 disagrees (acct_78 agrees, acct_79 has one source)");
            let c = &cs[0];
            assert_eq!((text(c, "key").as_str(), text(c, "property").as_str(), text(c, "class").as_str()), ("acct_77", "status", "Account"));
            assert_eq!(text(c, "status"), "resolved");
            assert_eq!(text(c, "winner"), "sqlite.accounts", "the lower rank wins");
            assert!(text(c, "rule").contains("rank"), "{}", text(c, "rule"));
            let Some(Value::List(values)) = c.properties.get("values") else { panic!("values") };
            let mut seen: Vec<(String, String)> = values
                .iter()
                .map(|v| {
                    let Value::Map(m) = v else { panic!() };
                    let t = |k: &str| match m.get(k) { Some(Value::Text(t)) => t.clone(), o => panic!("{k}: {o:?}") };
                    (t("source"), t("value"))
                })
                .collect();
            seen.sort();
            assert_eq!(seen, [("sqlite.account_risk".into(), "block".into()), ("sqlite.accounts".into(), "active".into())]);

            // the canonical entity carries the winner; every claim resolves to it
            let canon = live(&k, "aikoql:canonical", None);
            assert_eq!(canon.len(), 3);
            let a77 = canon.iter().find(|e| text(e, "key") == "acct_77").unwrap();
            let Some(Value::Map(vals)) = a77.properties.get("values") else { panic!("values") };
            assert_eq!(vals.get("status"), Some(&Value::Text("active".into())));
            let claims = live(&k, "aikoql:source-identity", None);
            assert_eq!(claims.len(), 5);
            let mine: Vec<_> = claims.iter().filter(|c| text(c, "key") == "acct_77").collect();
            assert_eq!(mine.len(), 2);
            for c in mine {
                assert_eq!((text(c, "rule").as_str(), c.properties.get("confidence")), ("exact-key", Some(&Value::Float(1.0))));
                assert!(c.relationships.iter().any(|r| r.rel_type == "RESOLVES_TO" && r.target == a77.koid), "{:?}", c.relationships);
            }
        });
    }

    #[test]
    fn resolving_again_changes_nothing_and_the_import_order_does_not_matter() {
        let (k, db) = setup("c2", None);
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            publish(&k, None, assertions(10, 20));
            import(&k, &["account_risk"], None); // the risk source first this time
            import(&k, &["accounts"], None);
            let first = resolve(&k, None);
            let ids = |t: &str| {
                let mut v: Vec<_> = live(&k, t, None).into_iter().map(|ko| (ko.koid, ko.version)).collect();
                v.sort();
                v
            };
            let before: Vec<_> = ["aikoql:canonical", "aikoql:source-identity", "aikoql:contradiction"].iter().map(|t| ids(t)).collect();
            let again = resolve(&k, None);
            assert_eq!(again["written"], 0, "an identical run writes nothing: {again}");
            assert_eq!(first["written"], 9, "3 entities + 5 claims + 1 contradiction: {first}");
            let after: Vec<_> = ["aikoql:canonical", "aikoql:source-identity", "aikoql:contradiction"].iter().map(|t| ids(t)).collect();
            assert_eq!(before, after, "same objects, same versions");
            // the imported containers can still be re-imported: resolution never takes their types
            let r = tool_sources_import(&k, &json!({"source": "crm", "containers": ["accounts", "account_risk"]}), "x").unwrap();
            assert_eq!(r["skipped"], json!([]), "{r}");
        });
    }

    #[test]
    fn a_contradiction_that_the_sources_stop_having_is_retired() {
        let (k, db) = setup("c3", None);
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            publish(&k, None, assertions(10, 20));
            import(&k, &["accounts", "account_risk"], None);
            resolve(&k, None);
            assert_eq!(live(&k, "aikoql:contradiction", None).len(), 1);
            make(&db, "UPDATE account_risk SET decision = 'active' WHERE account_id = 'acct_77';");
            import(&k, &["account_risk"], None);
            let r = resolve(&k, None);
            assert_eq!(live(&k, "aikoql:contradiction", None).len(), 0, "the sources agree now: {r}");
            assert_eq!(r["pruned"]["contradictions"], 1, "{r}");
        });
    }

    #[test]
    fn equal_authority_leaves_the_disagreement_open_and_picks_no_winner() {
        let (k, db) = setup("c4", None);
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            publish(&k, None, assertions(10, 10));
            import(&k, &["accounts", "account_risk"], None);
            let r = resolve(&k, None);
            assert_eq!((r["contradictions"]["open"].clone(), r["contradictions"]["resolved"].clone()), (json!(1), json!(0)), "{r}");
            let c = &live(&k, "aikoql:contradiction", None)[0];
            assert_eq!(text(c, "status"), "open");
            assert!(c.properties.get("winner").is_none() || c.properties.get("winner") == Some(&Value::Null));
            let canon = live(&k, "aikoql:canonical", None).into_iter().find(|e| text(e, "key") == "acct_77").unwrap();
            let Some(Value::Map(vals)) = canon.properties.get("values") else { panic!() };
            assert!(vals.get("status").is_none(), "no value is claimed when nobody outranks: {vals:?}");
        });
    }

    #[test]
    fn only_an_exactly_equal_key_merges_a_near_match_is_a_proposal() {
        let (k, db) = (kernel(), tmp_db("c5"));
        make(
            &db,
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, status TEXT);
             CREATE TABLE account_risk (account_id TEXT PRIMARY KEY, decision TEXT);
             INSERT INTO accounts VALUES ('ACCT_1', 'active');
             INSERT INTO account_risk VALUES (' acct_1', 'block');",
        );
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            publish(&k, None, assertions(10, 20));
            import(&k, &["accounts", "account_risk"], None);
            let r = resolve(&k, None);
            assert_eq!(r["entities"], 2, "different spellings are different entities: {r}");
            assert_eq!(live(&k, "aikoql:contradiction", None).len(), 0, "nothing merged, so nothing contradicts");
            let p = &r["proposals"][0];
            assert_eq!(p["keys"].as_array().unwrap().len(), 2, "{r}");
            assert!(p["confidence"].as_f64().unwrap() < 1.0 && p["rule"].as_str().unwrap().contains("case"), "{p}");
            assert!(live(&k, "aikoql:source-identity", None).iter().all(|c| c.relationships.len() == 1), "each claim resolves only to its own entity");
        });
    }

    #[test]
    fn a_key_two_rows_of_one_source_share_is_ambiguous_and_never_guessed() {
        let (k, db) = (kernel(), tmp_db("c6"));
        make(
            &db,
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, status TEXT);
             CREATE TABLE account_risk (account_id TEXT, decision TEXT);
             INSERT INTO accounts VALUES ('a1', 'active');
             INSERT INTO account_risk VALUES ('a1', 'allow'), ('a1', 'block'), (NULL, 'x');",
        );
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            publish(&k, None, assertions(10, 20));
            import(&k, &["accounts", "account_risk"], None);
            let r = resolve(&k, None);
            assert_eq!((r["ambiguous"].clone(), r["no_key"].clone()), (json!(1), json!(1)), "{r}");
            assert_eq!(live(&k, "aikoql:contradiction", None).len(), 0, "the risk source says two things, so it says nothing: {r}");
            assert_eq!(live(&k, "aikoql:source-identity", None).len(), 1, "only the unambiguous claim");
        });
    }

    #[test]
    fn resolution_stays_inside_its_tenant() {
        let (k, db) = setup("c7", Some("t1"));
        run(sqlite_source("crm", Some("t1")), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm", "tenant": "t1"}), "x").unwrap();
            publish(&k, Some("t1"), assertions(10, 20));
            import(&k, &["accounts", "account_risk"], Some("t1"));
            let r = resolve(&k, Some("t1"));
            assert_eq!(r["entities"], 3, "{r}");
            assert_eq!(live(&k, "aikoql:canonical", Some("t1")).len(), 3);
            assert!(live(&k, "aikoql:canonical", None).is_empty(), "nothing leaks to the no-tenant space");
            let other = tool_sources_resolve(&k, &json!({"class": "Account", "tenant": "t2"}), "x");
            assert!(other.is_err() || other.as_ref().unwrap()["entities"] == 0, "{other:?}");
            assert!(live(&k, "aikoql:canonical", Some("t2")).is_empty());
        });
    }

    #[test]
    fn a_class_nobody_asserts_is_a_clear_refusal() {
        let (k, db) = setup("c8", None);
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            publish(&k, None, assertions(10, 20));
            let e = tool_sources_resolve(&k, &json!({"class": "Ghost"}), "x").unwrap_err();
            assert!(e.starts_with(crate::sources::INVALID) && e.contains("Ghost"), "{e}");
            let e = tool_sources_resolve(&k, &json!({}), "x").unwrap_err();
            assert!(e.starts_with(crate::sources::INVALID), "{e}");
        });
    }
}
