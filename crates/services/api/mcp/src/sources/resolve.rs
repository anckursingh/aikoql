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

use super::import::key_text;
use super::{args_obj, audit, tenant_arg, INVALID};
use crate::derived_ids::{identity_koid, live_koid};
use crate::imports::{fresh_run_id, ImportSink};
use crate::{
    json, ExtensionMap, Kernel, Lifecycle, LifecycleState, Metadata, Origin, RelationshipRef,
    SecurityDescriptor, Subject, J,
};
use aikoql_kernel::{
    AssertDef, Direction, KnowledgeObject, MappingEntry, OntologyDef, Value, KOID,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const T_CANONICAL: &str = "aikoql:canonical";
const T_CLAIM: &str = "aikoql:source-identity";
const T_CONTRADICTION: &str = "aikoql:contradiction";
/// More rows than this in one asserting container: refuse rather than hold them all.
const MAX_ROWS: usize = 200_000;

/// One source row's assertions about one entity.
struct Claim {
    /// `<source kind>.<container>`
    label: String,
    kind: String,
    container: String,
    object: KOID,
    rank: u32,
    key: String,
    /// canonical property -> asserted value (nulls omitted)
    values: BTreeMap<String, Value>,
}

/// The `g`th generation of a derived object's id (0 = the first; later ones only after a tombstone).
fn gen_koid(ty: &str, scope: &str, parts: &[String], g: u32) -> KOID {
    let mut p = parts.to_vec();
    if g > 0 {
        p.push(format!("generation {g}"));
    }
    identity_koid(ty, scope, &p)
}

/// `POST /api/v1/sources/resolve {class, tenant?}`.
pub(crate) fn tool_sources_resolve(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let result = run(k, args);
    audit(
        db_path,
        args,
        if result.is_ok() { "resolved" } else { "error" },
        &match &result {
            Ok(r) => format!(
                "class={} entities={}",
                r["class"].as_str().unwrap_or(""),
                r["entities"]
            ),
            Err(e) => format!("resolve reason={}", e.chars().take(60).collect::<String>()),
        },
    );
    result
}

fn mappings_with(k: &Kernel, tenant: Option<&str>, class: &str) -> Vec<(MappingEntry, AssertDef)> {
    let Some((ko, _)) = crate::http::active_ontology_for(k, tenant) else {
        return Vec::new();
    };
    let Ok(def) = OntologyDef::from_property_map(&ko.properties) else {
        return Vec::new();
    };
    def.mappings
        .into_iter()
        .flat_map(|m| {
            let hits: Vec<AssertDef> = m
                .asserts
                .iter()
                .filter(|a| a.class == class)
                .cloned()
                .collect();
            hits.into_iter().map(move |a| (m.clone(), a))
        })
        .collect()
}

fn run(k: &Kernel, args: &J) -> Result<J, String> {
    let o = args_obj(args)?;
    let class = match o.get("class") {
        Some(J::String(c)) if !c.trim().is_empty() && c.chars().count() <= 128 => c.clone(),
        _ => {
            return Err(format!(
                "{INVALID}class must be a string of 1-128 characters"
            ))
        }
    };
    let tenant = tenant_arg(o)?;
    let asserting = mappings_with(k, tenant.as_deref(), &class);
    if asserting.is_empty() {
        let shown: String = class
            .chars()
            .take(40)
            .flat_map(char::escape_debug)
            .collect();
        return Err(format!(
            "{INVALID}no mapping in the active ontology asserts properties of class '{shown}' - publish assertions first"
        ));
    }
    let scan = Subject::with_roles("system", &["admin"]);
    let scan = match &tenant {
        Some(t) => scan.in_tenant(t),
        None => scan,
    };
    // ---- gather the claims -----------------------------------------------------------
    let (mut claims, mut ambiguous, mut no_key) = (Vec::<Claim>::new(), 0u64, 0u64);
    for (m, a) in &asserting {
        let rows: Vec<KnowledgeObject> = k
            .scan_by_type(&scan, &m.physical_type)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|ko| ko.metadata.tenant.as_deref() == tenant.as_deref())
            .collect();
        if rows.len() > MAX_ROWS {
            return Err(format!(
                "{} holds more than {MAX_ROWS} rows; resolve a smaller set (import fewer rows)",
                m.physical_type
            ));
        }
        let mut per_key: HashMap<String, Vec<Claim>> = HashMap::new();
        for ko in rows {
            let Some(key) = ko
                .properties
                .get(&a.key_column)
                .filter(|v| !matches!(v, Value::Null))
            else {
                no_key += 1;
                continue;
            };
            let key = key_text(key);
            let values = a
                .properties
                .iter()
                .filter_map(|(col, canon)| {
                    ko.properties
                        .get(col)
                        .filter(|v| !matches!(v, Value::Null))
                        .map(|v| (canon.clone(), v.clone()))
                })
                .collect();
            per_key.entry(key.clone()).or_default().push(Claim {
                label: format!("{}.{}", m.source, m.physical_type),
                kind: m.source.clone(),
                container: m.physical_type.clone(),
                object: ko.koid,
                rank: a.rank,
                key,
                values,
            });
        }
        for (_, mut group) in per_key {
            // a source that gives one key several rows is saying several things: it says nothing
            if group.len() > 1 {
                ambiguous += 1;
            } else {
                claims.append(&mut group);
            }
        }
    }
    let mut by_key: BTreeMap<String, Vec<&Claim>> = BTreeMap::new();
    for c in &claims {
        by_key.entry(c.key.clone()).or_default().push(c);
    }

    // ---- build what the claims say ---------------------------------------------------
    let mut subject = Subject::new("resolver");
    if let Some(t) = &tenant {
        subject = subject.in_tenant(t);
    }
    let sink = ImportSink::new(k, fresh_run_id(), subject.clone(), vec![]);
    // ids are tenant-scoped: two tenants with identical data must not share an object
    let scoped = |scope: &str| match &tenant {
        Some(t) => format!("{t}\u{1f}{scope}"),
        None => scope.to_string(),
    };
    let build = |ty: &str,
                 koid: KOID,
                 props: BTreeMap<String, Value>,
                 rels: Vec<RelationshipRef>| KnowledgeObject {
        koid,
        version: 0,
        commit_ts: 0,
        metadata: Metadata {
            type_name: ty.into(),
            tenant: tenant.clone(),
            schema_version: 1,
            tags: vec!["resolution".into()],
        },
        properties: props,
        semantic: None,
        relationships: rels,
        event_refs: vec![],
        security: SecurityDescriptor {
            owner: "resolver".into(),
            acl: vec![],
            classification: None,
        },
        lifecycle: Lifecycle {
            state: LifecycleState::Draft,
            origin: Origin::Human,
        },
        extensions: ExtensionMap::new(),
    };
    let text = |s: &str| Value::Text(s.to_string());
    let (mut present, mut written) = ([HashSet::new(), HashSet::new(), HashSet::new()], 0u64);
    let (mut open, mut resolved) = (0u64, 0u64);
    for (key, group) in &by_key {
        let canonical = live_koid(k, |g| {
            gen_koid(T_CANONICAL, &scoped(&class), std::slice::from_ref(key), g)
        });
        // per canonical property, who says what
        let mut said: BTreeMap<&str, Vec<(&Claim, &Value)>> = BTreeMap::new();
        for c in group {
            for (p, v) in &c.values {
                said.entry(p.as_str()).or_default().push((c, v));
            }
        }
        let mut values = BTreeMap::new();
        for (prop, says) in &said {
            let distinct: BTreeSet<String> = says.iter().map(|(_, v)| key_text(v)).collect();
            let best = says.iter().map(|(c, _)| c.rank).min().unwrap_or(0);
            let top: Vec<_> = says.iter().filter(|(c, _)| c.rank == best).collect();
            let top_values: BTreeSet<String> = top.iter().map(|(_, v)| key_text(v)).collect();
            // the value nobody outranks, if the top rank agrees with itself
            let winner = (top_values.len() == 1).then(|| top[0]);
            if let Some((_, v)) = winner {
                values.insert((*prop).to_string(), (*v).clone());
            }
            if distinct.len() < 2 {
                continue;
            }
            let koid = live_koid(k, |g| {
                gen_koid(
                    T_CONTRADICTION,
                    &scoped(&class),
                    &[key.clone(), (*prop).to_string()],
                    g,
                )
            });
            let rule = match winner {
                Some((c, _)) => format!(
                    "authority rank: {} (rank {best}) outranks the other sources",
                    c.label
                ),
                None => format!(
                    "authority rank: sources of equal rank {best} disagree; nothing outranks"
                ),
            };
            let listed = says
                .iter()
                .map(|(c, v)| {
                    Value::Map(BTreeMap::from([
                        ("source".to_string(), text(&c.label)),
                        ("value".to_string(), text(&key_text(v))),
                        ("rank".to_string(), Value::Int(i64::from(c.rank))),
                        ("object".to_string(), text(&c.object.to_hex())),
                    ]))
                })
                .collect();
            let mut props = BTreeMap::from([
                ("class".to_string(), text(&class)),
                ("key".to_string(), text(key)),
                ("property".to_string(), text(prop)),
                (
                    "status".to_string(),
                    text(if winner.is_some() { "resolved" } else { "open" }),
                ),
                ("rule".to_string(), text(&rule)),
                ("values".to_string(), Value::List(listed)),
            ]);
            if let Some((c, _)) = winner {
                props.insert("winner".into(), text(&c.label));
                resolved += 1;
            } else {
                open += 1;
            }
            let about = RelationshipRef {
                rel_type: "ABOUT".into(),
                target: canonical,
                direction: Direction::Outbound,
            };
            present[2].insert(koid);
            written += u64::from(
                sink.put(
                    build(T_CONTRADICTION, koid, props, vec![about]),
                    "resolver",
                    "contradiction",
                )
                .map_err(|e| e.to_string())?,
            );
        }
        let sources: Vec<Value> = group.iter().map(|c| text(&c.label)).collect();
        let props = BTreeMap::from([
            ("class".to_string(), text(&class)),
            ("key".to_string(), text(key)),
            ("values".to_string(), Value::Map(values)),
            ("sources".to_string(), Value::List(sources)),
        ]);
        present[0].insert(canonical);
        written += u64::from(
            sink.put(
                build(T_CANONICAL, canonical, props, vec![]),
                "resolver",
                "canonical entity",
            )
            .map_err(|e| e.to_string())?,
        );
        for c in group {
            let koid = live_koid(k, |g| {
                gen_koid(T_CLAIM, &scoped(&c.label), &[class.clone(), key.clone()], g)
            });
            let props = BTreeMap::from([
                ("class".to_string(), text(&class)),
                ("key".to_string(), text(key)),
                ("source".to_string(), text(&c.kind)),
                ("container".to_string(), text(&c.container)),
                ("source_object".to_string(), text(&c.object.to_hex())),
                ("canonical".to_string(), text(&canonical.to_hex())),
                ("rule".to_string(), text("exact-key")),
                ("confidence".to_string(), Value::Float(1.0)),
            ]);
            let edge = RelationshipRef {
                rel_type: "RESOLVES_TO".into(),
                target: canonical,
                direction: Direction::Outbound,
            };
            present[1].insert(koid);
            written += u64::from(
                sink.put(
                    build(T_CLAIM, koid, props, vec![edge]),
                    "resolver",
                    "source identity claim",
                )
                .map_err(|e| e.to_string())?,
            );
        }
    }

    // ---- retire what the sources no longer support (this tenant, this class only) -----
    let mine = |ko: &KnowledgeObject| {
        ko.metadata.tenant.as_deref() == tenant.as_deref()
            && matches!(ko.properties.get("class"), Some(Value::Text(c)) if *c == class)
    };
    let mut pruned = [0usize; 3];
    for (n, ty) in [T_CANONICAL, T_CLAIM, T_CONTRADICTION].iter().enumerate() {
        pruned[n] = sink
            .prune_missing_where(ty, &present[n], mine)
            .map_err(|e| e.to_string())?;
    }

    // ---- near matches: proposals, never merges --------------------------------------
    let mut spellings: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for key in by_key.keys() {
        spellings
            .entry(key.trim().to_lowercase())
            .or_default()
            .insert(key);
    }
    let proposals: Vec<J> = spellings
        .values()
        .filter(|v| v.len() > 1)
        .take(100)
        .map(|v| json!({"class": class, "keys": v, "rule": "case- and whitespace-insensitive key", "confidence": 0.5}))
        .collect();

    Ok(json!({
        "class": class,
        "entities": by_key.len(),
        "claims": claims.len(),
        "contradictions": {"open": open, "resolved": resolved},
        "written": written,
        "pruned": {"entities": pruned[0], "claims": pruned[1], "contradictions": pruned[2]},
        "ambiguous": ambiguous,
        "no_key": no_key,
        "proposals": proposals,
    }))
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
        assert!(
            r["warnings"]
                .to_string()
                .find("assertion ignored")
                .is_none(),
            "{r}"
        );
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
            assert_eq!(
                (r["entities"].clone(), r["claims"].clone()),
                (json!(3), json!(5)),
                "{r}"
            );
            assert_eq!(r["contradictions"]["open"], 0, "{r}");
            assert_eq!(r["contradictions"]["resolved"], 1, "{r}");

            let cs = live(&k, "aikoql:contradiction", None);
            assert_eq!(
                cs.len(),
                1,
                "only acct_77 disagrees (acct_78 agrees, acct_79 has one source)"
            );
            let c = &cs[0];
            assert_eq!(
                (
                    text(c, "key").as_str(),
                    text(c, "property").as_str(),
                    text(c, "class").as_str()
                ),
                ("acct_77", "status", "Account")
            );
            assert_eq!(text(c, "status"), "resolved");
            assert_eq!(text(c, "winner"), "sqlite.accounts", "the lower rank wins");
            assert!(text(c, "rule").contains("rank"), "{}", text(c, "rule"));
            let Some(Value::List(values)) = c.properties.get("values") else {
                panic!("values")
            };
            let mut seen: Vec<(String, String)> = values
                .iter()
                .map(|v| {
                    let Value::Map(m) = v else { panic!() };
                    let t = |k: &str| match m.get(k) {
                        Some(Value::Text(t)) => t.clone(),
                        o => panic!("{k}: {o:?}"),
                    };
                    (t("source"), t("value"))
                })
                .collect();
            seen.sort();
            assert_eq!(
                seen,
                [
                    ("sqlite.account_risk".into(), "block".into()),
                    ("sqlite.accounts".into(), "active".into())
                ]
            );

            // the canonical entity carries the winner; every claim resolves to it
            let canon = live(&k, "aikoql:canonical", None);
            assert_eq!(canon.len(), 3);
            let a77 = canon.iter().find(|e| text(e, "key") == "acct_77").unwrap();
            let Some(Value::Map(vals)) = a77.properties.get("values") else {
                panic!("values")
            };
            assert_eq!(vals.get("status"), Some(&Value::Text("active".into())));
            let claims = live(&k, "aikoql:source-identity", None);
            assert_eq!(claims.len(), 5);
            let mine: Vec<_> = claims
                .iter()
                .filter(|c| text(c, "key") == "acct_77")
                .collect();
            assert_eq!(mine.len(), 2);
            for c in mine {
                assert_eq!(
                    (text(c, "rule").as_str(), c.properties.get("confidence")),
                    ("exact-key", Some(&Value::Float(1.0)))
                );
                assert!(
                    c.relationships
                        .iter()
                        .any(|r| r.rel_type == "RESOLVES_TO" && r.target == a77.koid),
                    "{:?}",
                    c.relationships
                );
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
                let mut v: Vec<_> = live(&k, t, None)
                    .into_iter()
                    .map(|ko| (ko.koid, ko.version))
                    .collect();
                v.sort();
                v
            };
            let before: Vec<_> = [
                "aikoql:canonical",
                "aikoql:source-identity",
                "aikoql:contradiction",
            ]
            .iter()
            .map(|t| ids(t))
            .collect();
            let again = resolve(&k, None);
            assert_eq!(
                again["written"], 0,
                "an identical run writes nothing: {again}"
            );
            assert_eq!(
                first["written"], 9,
                "3 entities + 5 claims + 1 contradiction: {first}"
            );
            let after: Vec<_> = [
                "aikoql:canonical",
                "aikoql:source-identity",
                "aikoql:contradiction",
            ]
            .iter()
            .map(|t| ids(t))
            .collect();
            assert_eq!(before, after, "same objects, same versions");
            // the imported containers can still be re-imported: resolution never takes their types
            let r = tool_sources_import(
                &k,
                &json!({"source": "crm", "containers": ["accounts", "account_risk"]}),
                "x",
            )
            .unwrap();
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
            make(
                &db,
                "UPDATE account_risk SET decision = 'active' WHERE account_id = 'acct_77';",
            );
            import(&k, &["account_risk"], None);
            let r = resolve(&k, None);
            assert_eq!(
                live(&k, "aikoql:contradiction", None).len(),
                0,
                "the sources agree now: {r}"
            );
            assert_eq!(r["pruned"]["contradictions"], 1, "{r}");
            // and it comes back when they disagree again (same deterministic id, once tombstoned)
            make(
                &db,
                "UPDATE account_risk SET decision = 'block' WHERE account_id = 'acct_77';",
            );
            import(&k, &["account_risk"], None);
            let r = resolve(&k, None);
            assert_eq!(
                live(&k, "aikoql:contradiction", None).len(),
                1,
                "the disagreement is back: {r}"
            );
            assert_eq!(r["contradictions"]["resolved"], 1, "{r}");
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
            assert_eq!(
                (
                    r["contradictions"]["open"].clone(),
                    r["contradictions"]["resolved"].clone()
                ),
                (json!(1), json!(0)),
                "{r}"
            );
            let c = &live(&k, "aikoql:contradiction", None)[0];
            assert_eq!(text(c, "status"), "open");
            assert!(matches!(
                c.properties.get("winner"),
                None | Some(Value::Null)
            ));
            let canon = live(&k, "aikoql:canonical", None)
                .into_iter()
                .find(|e| text(e, "key") == "acct_77")
                .unwrap();
            let Some(Value::Map(vals)) = canon.properties.get("values") else {
                panic!()
            };
            assert!(
                vals.get("status").is_none(),
                "no value is claimed when nobody outranks: {vals:?}"
            );
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
            assert_eq!(
                r["entities"], 2,
                "different spellings are different entities: {r}"
            );
            assert_eq!(
                live(&k, "aikoql:contradiction", None).len(),
                0,
                "nothing merged, so nothing contradicts"
            );
            let p = &r["proposals"][0];
            assert_eq!(p["keys"].as_array().unwrap().len(), 2, "{r}");
            assert!(
                p["confidence"].as_f64().unwrap() < 1.0
                    && p["rule"].as_str().unwrap().contains("case"),
                "{p}"
            );
            assert!(
                live(&k, "aikoql:source-identity", None)
                    .iter()
                    .all(|c| c.relationships.len() == 1),
                "each claim resolves only to its own entity"
            );
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
            assert_eq!(
                (r["ambiguous"].clone(), r["no_key"].clone()),
                (json!(1), json!(1)),
                "{r}"
            );
            assert_eq!(
                live(&k, "aikoql:contradiction", None).len(),
                0,
                "the risk source says two things, so it says nothing: {r}"
            );
            assert_eq!(
                live(&k, "aikoql:source-identity", None).len(),
                1,
                "only the unambiguous claim"
            );
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
            assert!(
                live(&k, "aikoql:canonical", None).is_empty(),
                "nothing leaks to the no-tenant space"
            );
            let other = tool_sources_resolve(&k, &json!({"class": "Account", "tenant": "t2"}), "x");
            assert!(
                other.is_err() || other.as_ref().unwrap()["entities"] == 0,
                "{other:?}"
            );
            assert!(live(&k, "aikoql:canonical", Some("t2")).is_empty());
        });
    }

    #[test]
    fn a_preview_after_publishing_reports_what_is_already_published() {
        let (k, db) = setup("c9", None);
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let body = |preview: bool, fp: &J| {
                json!({"sources": ["crm"], "preview": preview, "fingerprints": fp,
                       "identities": {"crm.accounts": ["id"]}, "assertions": assertions(10, 20),
                       "relationships": [{"name": "scored_by", "from": "crm.account_risk.account_id", "to": "crm.accounts.id"}]})
            };
            let p = tool_sources_ontology(&k, &body(true, &json!({})), "x").unwrap();
            assert_eq!(
                p["published"]["assertions"],
                json!([]),
                "nothing is published before the first publish: {p}"
            );
            tool_sources_ontology(&k, &body(false, &p["fingerprints"]), "x").unwrap();
            // a fresh preview that declares nothing still knows what the ontology holds
            let q = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            assert_eq!(
                q["published"]["identities"]["crm.accounts"],
                json!(["id"]),
                "{q}"
            );
            let a = q["published"]["assertions"].as_array().unwrap();
            assert_eq!(a.len(), 2, "{q}");
            assert!(
                a.iter().any(|x| x["from"] == "crm.accounts.id"
                    && x["class"] == "Account"
                    && x["rank"] == 10),
                "{a:?}"
            );
            assert_eq!(q["published"]["links"][0]["name"], "scored_by", "{q}");
            assert_eq!(q["published"]["links"][0]["to"], "crm.accounts.id", "{q}");
            // published links are not proposed again; published keys are marked
            assert!(
                q["links"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|l| l["name"] != "scored_by"),
                "{q}"
            );
            let key = q["identities"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["container"] == "crm.accounts")
                .unwrap();
            assert_eq!(key["published"], true, "{key}");
        });
    }

    #[test]
    fn two_tenants_with_identical_data_get_independent_objects() {
        let (k, db) = setup("c10", None);
        with_test_sources(
            vec![
                sqlite_source("crm", Some("t1")),
                sqlite_source("crm", Some("t2")),
            ],
            vec![("crm".to_string(), conn(&db))],
            || {
                for t in ["t1", "t2"] {
                    tool_sources_discover(&k, &json!({"source": "crm", "tenant": t}), "x").unwrap();
                    publish(&k, Some(t), assertions(10, 20));
                    import(&k, &["accounts", "account_risk"], Some(t));
                }
                for t in ["t1", "t2"] {
                    let r = resolve(&k, Some(t));
                    assert_eq!(
                        (r["entities"].clone(), r["written"].clone()),
                        (json!(3), json!(9)),
                        "{t}: {r}"
                    );
                }
                for t in ["t1", "t2"] {
                    assert_eq!(
                        live(&k, "aikoql:canonical", Some(t)).len(),
                        3,
                        "{t} keeps its own entities"
                    );
                    assert_eq!(live(&k, "aikoql:contradiction", Some(t)).len(), 1, "{t}");
                    assert_eq!(
                        live(&k, "accounts", Some(t)).len(),
                        3,
                        "{t} keeps its own imported rows"
                    );
                }
                let ids = |t: &str| -> std::collections::BTreeSet<_> {
                    live(&k, "aikoql:canonical", Some(t))
                        .into_iter()
                        .map(|e| e.koid)
                        .collect()
                };
                assert!(
                    ids("t1").is_disjoint(&ids("t2")),
                    "no object is shared between tenants"
                );
            },
        );
    }

    #[test]
    fn a_class_nobody_asserts_is_a_clear_refusal() {
        let (k, db) = setup("c8", None);
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            publish(&k, None, assertions(10, 20));
            let e = tool_sources_resolve(&k, &json!({"class": "Ghost"}), "x").unwrap_err();
            assert!(
                e.starts_with(crate::sources::INVALID) && e.contains("Ghost"),
                "{e}"
            );
            let e = tool_sources_resolve(&k, &json!({}), "x").unwrap_err();
            assert!(e.starts_with(crate::sources::INVALID), "{e}");
        });
    }
}
