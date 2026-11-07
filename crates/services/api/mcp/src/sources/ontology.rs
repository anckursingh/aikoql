//! `POST /api/v1/sources/ontology`: preview and publish the ontology drafted
//! from recorded source schemas, merged into the tenant's manual ontology.

use crate::{Kernel, J};

pub(crate) fn tool_sources_ontology(_k: &Kernel, _args: &J, _db_path: &str) -> Result<J, String> {
    Err("todo".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use crate::sources::service::tool_sources_discover;
    use crate::sources::testkit::*;
    use crate::sources::{with_test_sources, STALE};

    fn two_sources<R>(crm: &std::path::Path, shop: &std::path::Path, f: impl FnOnce() -> R) -> R {
        with_test_sources(
            vec![sqlite_source("crm", None), sqlite_source("shop", None)],
            vec![("crm".into(), conn(crm)), ("shop".into(), conn(shop))],
            f,
        )
    }

    fn discover(k: &Kernel, name: &str) -> J {
        tool_sources_discover(k, &json!({"source": name}), "x").unwrap()
    }

    fn head(k: &Kernel) -> u64 {
        k.journal_head().unwrap().0
    }

    fn manual(k: &Kernel) -> Option<(aikoql_kernel::KnowledgeObject, aikoql_kernel::OntologyDef)> {
        let (ko, source) = crate::http::active_ontology_for(k, None)?;
        assert_eq!(source, "manual");
        let def = aikoql_kernel::OntologyDef::from_ko(&ko).unwrap();
        Some((ko, def))
    }

    fn apply_args(preview: &J, extra: J) -> J {
        let mut a =
            json!({"sources": ["crm"], "preview": false, "fingerprints": preview["fingerprints"]});
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        a
    }

    #[test]
    fn preview_writes_nothing_and_describes_the_draft() {
        let (k, crm, shop) = (kernel(), crm_db("p1"), shop_db("p1"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            discover(&k, "shop");
            let before = head(&k);
            let p = tool_sources_ontology(
                &k,
                &json!({"sources": ["crm", "shop"], "preview": true}),
                "x",
            )
            .unwrap();
            assert_eq!(head(&k), before, "a preview writes nothing");
            let classes: Vec<&str> = p["classes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["name"].as_str().unwrap())
                .collect();
            assert_eq!(classes, ["Customer", "Order", "Client"]);
            assert_eq!(p["classes"][0]["status"], "new");
            assert_eq!(p["adds"], json!(["Customer", "Order", "Client"]));
            assert_eq!(p["fingerprints"].as_object().unwrap().len(), 2);
            assert!(p["mappings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["physical_type"] == "customers" && m["class"] == "Customer"));
            assert!(p["active_version"].is_null());
        });
    }

    #[test]
    fn publishing_requires_the_fingerprint_the_reviewer_saw() {
        let (k, crm, shop) = (kernel(), crm_db("p2"), shop_db("p2"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            make(&crm, "ALTER TABLE customers ADD COLUMN phone TEXT;");
            discover(&k, "crm"); // the schema moved after the preview
            let e = tool_sources_ontology(&k, &apply_args(&p, json!({})), "x").unwrap_err();
            assert_eq!(e, STALE);
            assert!(manual(&k).is_none(), "nothing was published");
            let e = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": false}), "x")
                .unwrap_err();
            assert!(
                e.starts_with(crate::sources::INVALID),
                "apply without fingerprints: {e}"
            );
        });
    }

    #[test]
    fn publishing_creates_the_ontology_and_the_class_maps_to_the_physical_tables() {
        let (k, crm, shop) = (kernel(), crm_db("p3"), shop_db("p3"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            let r = tool_sources_ontology(&k, &apply_args(&p, json!({})), "x").unwrap();
            assert_eq!(r["saved"], true);
            let (_, def) = manual(&k).expect("a manual ontology exists");
            assert!(def.classes.contains_key("Customer") && def.classes.contains_key("Order"));
            assert!(def.mappings.iter().any(|m| m.class == "Customer"
                && m.physical_type == "customers"
                && m.source == "sqlite"));
            assert!(def
                .relationships
                .values()
                .any(|r| r.domain.as_deref() == Some("Order")
                    && r.range.as_deref() == Some("Customer")));
        });
    }

    #[test]
    fn publishing_never_touches_what_a_human_wrote() {
        let (k, crm, shop) = (kernel(), crm_db("p4"), shop_db("p4"));
        two_sources(&crm, &shop, || {
            crate::tools::tool_save_ontology(
                &k,
                &json!({"definition": {
                    "namespace": "acme", "version": "2.0",
                    "classes": [{"name": "Widget", "description": "hand written", "properties": ["sku"]},
                                {"name": "Customer", "description": "our own Customer", "properties": ["tier"]}],
                    "relationships": [{"name": "made_of", "domain": "Widget", "range": "Widget"}],
                    "property_defs": [{"name": "sku", "type": "Text", "required": true}, {"name": "tier", "type": "Int"}],
                    "mappings": [],
                }}),
            )
            .unwrap();
            let (ko, before) = manual(&k).unwrap();
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            assert_eq!(p["active_version"], ko.version);
            assert!(p["untouched"]
                .as_array()
                .unwrap()
                .contains(&json!("Widget")));
            assert!(
                p["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|w| w.as_str().unwrap().contains("Customer")),
                "{}",
                p["warnings"]
            );
            let e = tool_sources_ontology(&k, &apply_args(&p, json!({})), "x").unwrap_err();
            assert!(
                e.starts_with("expected_version required"),
                "a CAS version is mandatory over an existing ontology: {e}"
            );
            tool_sources_ontology(
                &k,
                &apply_args(&p, json!({"expected_version": ko.version})),
                "x",
            )
            .unwrap();
            let (_, after) = manual(&k).unwrap();
            assert_eq!(after.namespace, "acme");
            assert_eq!(
                after.classes["Widget"], before.classes["Widget"],
                "hand-written class untouched"
            );
            assert_eq!(
                after.classes["Customer"], before.classes["Customer"],
                "a same-named human class wins"
            );
            assert_eq!(after.property_defs["sku"], before.property_defs["sku"]);
            assert!(
                after.classes.contains_key("Order"),
                "discovered classes are added"
            );
            assert!(after.relationships.contains_key("made_of"));
        });
    }

    #[test]
    fn a_stale_expected_version_is_a_conflict() {
        let (k, crm, shop) = (kernel(), crm_db("p5"), shop_db("p5"));
        two_sources(&crm, &shop, || {
            crate::tools::tool_save_ontology(&k, &json!({"definition": {"namespace": "n", "version": "1", "classes": [{"name": "Widget"}]}})).unwrap();
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            let e =
                tool_sources_ontology(&k, &apply_args(&p, json!({"expected_version": 99})), "x")
                    .unwrap_err();
            assert!(
                e.contains("VERSION_CONFLICT") || e.starts_with("expected_version conflict"),
                "{e}"
            );
        });
    }

    #[test]
    fn applying_twice_is_idempotent_and_reapplying_refreshes_only_discovered_classes() {
        let (k, crm, shop) = (kernel(), crm_db("p6"), shop_db("p6"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            tool_sources_ontology(&k, &apply_args(&p, json!({})), "x").unwrap();
            let (ko, def1) = manual(&k).unwrap();
            let before = head(&k);
            let again = tool_sources_ontology(
                &k,
                &apply_args(&p, json!({"expected_version": ko.version})),
                "x",
            )
            .unwrap();
            assert_eq!(again["saved"], false, "identical content writes nothing");
            assert_eq!(head(&k), before);
            // schema drift: new column, nothing changes until a human re-approves
            make(&crm, "ALTER TABLE customers ADD COLUMN phone TEXT;");
            let d = discover(&k, "crm");
            assert_eq!(d["diff"]["changed"][0]["added_fields"], json!(["phone"]));
            let (ko2, _) = manual(&k).unwrap();
            assert_eq!(
                ko2.version, ko.version,
                "drift alone never rewrites the ontology"
            );
            let p2 = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            assert!(
                p2["refreshed"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("Customer")),
                "{}",
                p2["refreshed"]
            );
            tool_sources_ontology(
                &k,
                &apply_args(&p2, json!({"expected_version": ko2.version})),
                "x",
            )
            .unwrap();
            let (_, def2) = manual(&k).unwrap();
            assert!(!def1.classes["Customer"]
                .properties
                .contains(&"phone".to_string()));
            assert!(def2.classes["Customer"]
                .properties
                .contains(&"phone".to_string()));
            assert!(def2.property_defs.contains_key("phone"));
        });
    }

    #[test]
    fn an_explicit_merge_makes_one_class_with_two_mappings() {
        let (k, crm, shop) = (kernel(), crm_db("p7"), shop_db("p7"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            discover(&k, "shop");
            let merges = json!([["crm.customers", "shop.clients"]]);
            let p = tool_sources_ontology(
                &k,
                &json!({"sources": ["crm", "shop"], "preview": true, "merges": merges}),
                "x",
            )
            .unwrap();
            let mut a = apply_args(&p, json!({"sources": ["crm", "shop"], "merges": merges}));
            a["fingerprints"] = p["fingerprints"].clone();
            tool_sources_ontology(&k, &a, "x").unwrap();
            let (_, def) = manual(&k).unwrap();
            assert!(def.classes.contains_key("Customer") && !def.classes.contains_key("Client"));
            let maps: Vec<&str> = def
                .mappings
                .iter()
                .filter(|m| m.class == "Customer")
                .map(|m| m.physical_type.as_str())
                .collect();
            assert_eq!(maps.len(), 2, "{maps:?}");
            assert!(maps.contains(&"customers") && maps.contains(&"clients"));
        });
    }

    #[test]
    fn nothing_to_publish_is_a_clean_client_error() {
        let k = kernel();
        let empty = tmp_db("empty");
        make(&empty, "CREATE TABLE sqlite_dummy_check (x);");
        let db = tmp_db("noneyet");
        make(&db, "SELECT 1;");
        with_test_sources(
            vec![sqlite_source("crm", None)],
            vec![("crm".into(), conn(&db))],
            || {
                let e =
                    tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                        .unwrap_err();
                assert!(
                    e.contains(crate::sources::NO_SCHEMA),
                    "not discovered yet: {e}"
                );
                let db_empty = tmp_db("zero");
                make(&db_empty, "SELECT 1;");
                drop(db_empty);
            },
        );
        let zero = tmp_db("zero2");
        make(&zero, "SELECT 1;");
        with_test_sources(
            vec![sqlite_source("z", None)],
            vec![("z".into(), conn(&zero))],
            || {
                tool_sources_discover(&k, &json!({"source": "z"}), "x").unwrap();
                let e = tool_sources_ontology(&k, &json!({"sources": ["z"], "preview": true}), "x")
                    .unwrap_err();
                assert!(e.starts_with(crate::sources::NOTHING), "{e}");
                assert_eq!(crate::sources::status_for(&e), Some(400));
            },
        );
    }

    #[test]
    fn bad_arguments_are_client_errors() {
        let (k, crm, shop) = (kernel(), crm_db("p9"), shop_db("p9"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            for bad in [
                json!({}),
                json!({"sources": []}),
                json!({"sources": "crm"}),
                json!({"sources": [1]}),
                json!({"sources": ["crm"], "merges": "x"}),
                json!({"sources": ["crm"], "merges": [["only-one"]]}),
                json!({"sources": ["crm"], "merges": [[1, 2]]}),
                json!({"sources": ["crm"], "preview": "yes"}),
                json!({"sources": ["crm"], "fingerprints": [1]}),
                json!({"sources": (0..60).map(|i| format!("s{i}")).collect::<Vec<_>>()}),
            ] {
                let e = tool_sources_ontology(&k, &bad, "x").unwrap_err();
                assert_eq!(crate::sources::status_for(&e), Some(400), "{bad} -> {e}");
            }
            assert_eq!(
                tool_sources_ontology(&k, &json!({"sources": ["nope"], "preview": true}), "x")
                    .unwrap_err(),
                crate::sources::NOT_FOUND
            );
        });
    }

    #[test]
    fn publish_fuzz_random_merges_always_save_a_valid_ontology() {
        let iters = std::env::var("AIKOQL_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_000)
            / 20;
        for seed in 1..=iters.max(10) {
            let (k, crm, shop) = (
                kernel(),
                crm_db(&format!("f{seed}")),
                shop_db(&format!("f{seed}")),
            );
            let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut rnd = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            two_sources(&crm, &shop, || {
                discover(&k, "crm");
                discover(&k, "shop");
                let refs = ["crm.customers", "crm.orders", "shop.clients", "nope.x"];
                let merges: Vec<J> = (0..rnd() % 3)
                    .map(|_| json!([refs[(rnd() % 4) as usize], refs[(rnd() % 4) as usize]]))
                    .collect();
                let base = json!({"sources": ["crm", "shop"], "merges": merges});
                let mut pv = base.clone();
                pv["preview"] = json!(true);
                let p = tool_sources_ontology(&k, &pv, "x")
                    .unwrap_or_else(|e| panic!("seed {seed}: preview {e}"));
                let mut ap = base.clone();
                ap["preview"] = json!(false);
                ap["fingerprints"] = p["fingerprints"].clone();
                tool_sources_ontology(&k, &ap, "x")
                    .unwrap_or_else(|e| panic!("seed {seed}: apply {e}"));
                let (_, def) = manual(&k).unwrap_or_else(|| panic!("seed {seed}: nothing saved"));
                assert!(!def.classes.is_empty(), "seed {seed}");
                assert!(
                    def.mappings
                        .iter()
                        .all(|m| def.classes.contains_key(&m.class)),
                    "seed {seed}: mapping to a missing class"
                );
            });
        }
    }
}
