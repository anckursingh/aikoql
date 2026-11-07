//! Turn discovered source schemas into a publishable ontology definition:
//! one class per container, typed properties, relationships from references,
//! and a `MappingEntry` per physical container so `MATCH <Class>` reaches every
//! mapped source. Same-named classes from different sources are never merged
//! automatically; only an explicit [`ClassMerge`] does that.

use crate::connector_bridge::ConnectorMetadata;
use serde_json::{json, Value};

pub struct SourceInput<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub meta: &'a ConnectorMetadata,
}

/// Two `"<source>.<container>"` references a human decided are the same concept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassMerge {
    pub a: String,
    pub b: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassStatus {
    New,
    PossibleDuplicate { with: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftClass {
    pub name: String,
    /// `"<source>.<container>"` references feeding this class.
    pub sources: Vec<String>,
    pub status: ClassStatus,
}

#[derive(Clone, Debug)]
pub struct DefinitionDraft {
    /// `{namespace, version, classes, relationships, property_defs, mappings}`,
    /// exactly what the ontology save path accepts.
    pub definition: Value,
    pub classes: Vec<DraftClass>,
    pub warnings: Vec<String>,
}

pub fn definition_from_sources(
    _sources: &[SourceInput],
    _merges: &[ClassMerge],
) -> DefinitionDraft {
    DefinitionDraft {
        definition: Value::Null,
        classes: Vec::new(),
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector_bridge::{discover_connector_schema, FieldProfile, Provenance};

    type Col = (&'static str, &'static str, bool, bool, bool);

    fn meta(
        kind: &str,
        label: &str,
        tables: &[(&str, &[Col])],
        fks: &[FkRow],
    ) -> ConnectorMetadata {
        discover_connector_schema(kind, label, tables, fks)
    }
    type FkRow<'a> = (
        &'a str,
        &'a [&'a str],
        &'a str,
        &'a [&'a str],
        Option<&'a str>,
    );

    const ID: Col = ("id", "integer", true, false, true);

    fn names(d: &DefinitionDraft) -> Vec<String> {
        let mut n: Vec<String> = d.classes.iter().map(|c| c.name.clone()).collect();
        n.sort();
        n
    }

    fn list<'a>(d: &'a DefinitionDraft, key: &str) -> &'a Vec<Value> {
        d.definition[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} missing: {}", d.definition))
    }

    #[test]
    fn three_tables_three_classes_with_mappings() {
        let m = meta(
            "postgres",
            "shop",
            &[
                ("customers", &[ID, ("email", "text", false, false, false)]),
                ("orders", &[ID]),
                ("products", &[ID]),
            ],
            &[],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "shop",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        assert_eq!(names(&d), ["Customer", "Order", "Product"]);
        let mappings = list(&d, "mappings");
        assert!(mappings.iter().any(|m| m["source"] == "postgres"
            && m["physical_type"] == "customers"
            && m["class"] == "Customer"));
        let customer = list(&d, "classes")
            .iter()
            .find(|c| c["name"] == "Customer")
            .unwrap();
        assert!(customer["properties"]
            .as_array()
            .unwrap()
            .contains(&json!("email")));
    }

    #[test]
    fn fk_becomes_relationship_with_domain_and_range() {
        let m = meta(
            "postgres",
            "shop",
            &[
                ("customers", &[ID]),
                (
                    "orders",
                    &[ID, ("customer_id", "integer", false, true, false)],
                ),
            ],
            &[(
                "orders",
                &["customer_id"],
                "customers",
                &["id"],
                Some("orders_customer_fk"),
            )],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "shop",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        let rel = list(&d, "relationships")
            .iter()
            .find(|r| r["name"] == "orders_customer_fk")
            .expect("relationship");
        assert_eq!(
            (rel["domain"].as_str(), rel["range"].as_str()),
            (Some("Order"), Some("Customer"))
        );
    }

    #[test]
    fn same_name_in_two_sources_is_possible_duplicate_not_merged() {
        let a = meta("postgres", "crm", &[("customers", &[ID])], &[]);
        let b = meta("sqlite", "shop", &[("customers", &[ID])], &[]);
        let d = definition_from_sources(
            &[
                SourceInput {
                    name: "crm",
                    kind: "postgres",
                    meta: &a,
                },
                SourceInput {
                    name: "shop",
                    kind: "sqlite",
                    meta: &b,
                },
            ],
            &[],
        );
        assert_eq!(names(&d), ["CrmCustomer", "ShopCustomer"]);
        assert!(d
            .classes
            .iter()
            .all(|c| matches!(c.status, ClassStatus::PossibleDuplicate { .. })));
    }

    #[test]
    fn explicit_merge_yields_one_class_two_mappings() {
        let a = meta(
            "postgres",
            "crm",
            &[("customers", &[ID, ("email", "text", false, false, false)])],
            &[],
        );
        let b = meta(
            "sqlite",
            "shop",
            &[(
                "clients",
                &[ID, ("email_address", "text", false, false, false)],
            )],
            &[],
        );
        let d = definition_from_sources(
            &[
                SourceInput {
                    name: "crm",
                    kind: "postgres",
                    meta: &a,
                },
                SourceInput {
                    name: "shop",
                    kind: "sqlite",
                    meta: &b,
                },
            ],
            &[ClassMerge {
                a: "crm.customers".into(),
                b: "shop.clients".into(),
            }],
        );
        assert_eq!(names(&d), ["Customer"]);
        let maps: Vec<&Value> = list(&d, "mappings")
            .iter()
            .filter(|m| m["class"] == "Customer")
            .collect();
        assert_eq!(maps.len(), 2, "{}", d.definition);
        let shop = maps
            .iter()
            .find(|m| m["physical_type"] == "clients")
            .unwrap();
        assert_eq!(shop["property_map"]["email_address"], "email_address");
        assert!(d.classes[0].sources.contains(&"shop.clients".to_string()));
    }

    #[test]
    fn unknown_merge_reference_warns_and_is_ignored() {
        let a = meta("postgres", "crm", &[("customers", &[ID])], &[]);
        let d = definition_from_sources(
            &[SourceInput {
                name: "crm",
                kind: "postgres",
                meta: &a,
            }],
            &[ClassMerge {
                a: "crm.customers".into(),
                b: "nope.nothing".into(),
            }],
        );
        assert_eq!(names(&d), ["Customer"]);
        assert!(
            d.warnings.iter().any(|w| w.contains("nope.nothing")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn conflicting_property_types_widen_to_text_with_warning() {
        let a = meta(
            "postgres",
            "a",
            &[("things", &[("code", "integer", false, false, false)])],
            &[],
        );
        let b = meta(
            "sqlite",
            "b",
            &[("items", &[("code", "text", false, false, false)])],
            &[],
        );
        let d = definition_from_sources(
            &[
                SourceInput {
                    name: "a",
                    kind: "postgres",
                    meta: &a,
                },
                SourceInput {
                    name: "b",
                    kind: "sqlite",
                    meta: &b,
                },
            ],
            &[],
        );
        let code = list(&d, "property_defs")
            .iter()
            .find(|p| p["name"] == "code")
            .unwrap();
        assert_eq!(code["type"], "Text");
        assert!(
            d.warnings.iter().any(|w| w.contains("code")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn odd_container_names_yield_unique_valid_class_names() {
        let m = meta(
            "postgres",
            "x",
            &[
                ("order", &[ID]),
                ("Order", &[ID]),
                ("user data", &[ID]),
                ("2024_sales", &[ID]),
                ("users", &[ID]),
                ("user", &[ID]),
            ],
            &[],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "x",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        let n = names(&d);
        let mut unique = n.clone();
        unique.dedup();
        assert_eq!(n.len(), 6);
        assert_eq!(n, unique, "class names must be unique: {n:?}");
        assert!(n.iter().all(|c| c.chars().next().unwrap().is_alphabetic()));
        assert!(n.contains(&"UserData".to_string()) && n.contains(&"T2024Sale".to_string()));
    }

    #[test]
    fn canonical_property_names_are_snake_case_and_mapped() {
        let m = meta(
            "mongodb",
            "crm",
            &[("people", &[("customerId", "bson", false, true, false)])],
            &[],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "crm",
                kind: "mongodb",
                meta: &m,
            }],
            &[],
        );
        assert!(list(&d, "property_defs")
            .iter()
            .any(|p| p["name"] == "customer_id"));
        assert_eq!(
            list(&d, "mappings")[0]["property_map"]["customerId"],
            "customer_id"
        );
    }

    #[test]
    fn observed_fields_warn_and_use_the_observed_type() {
        let mut m = meta(
            "mongodb",
            "crm",
            &[("people", &[("age", "bson", false, true, false)])],
            &[],
        );
        m.containers[0].fields[0].provenance = Provenance::Observed;
        m.containers[0].fields[0].profile = Some(FieldProfile {
            types: vec!["Int32".into(), "Null".into()],
            presence_ratio: 0.9,
            null_ratio: 0.1,
            samples: 100,
        });
        let d = definition_from_sources(
            &[SourceInput {
                name: "crm",
                kind: "mongodb",
                meta: &m,
            }],
            &[],
        );
        let age = list(&d, "property_defs")
            .iter()
            .find(|p| p["name"] == "age")
            .unwrap();
        assert_eq!(age["type"], "Int");
        assert!(
            d.warnings.iter().any(|w| w.contains("observed")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn composite_foreign_key_warns() {
        let m = meta(
            "postgres",
            "x",
            &[("a", &[ID]), ("b", &[ID])],
            &[("b", &["x", "y"], "a", &["p", "q"], Some("b_a_fk"))],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "x",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        assert!(
            d.warnings.iter().any(|w| w.contains("b_a_fk")),
            "{:?}",
            d.warnings
        );
        assert!(list(&d, "relationships")
            .iter()
            .any(|r| r["name"] == "b_a_fk"));
    }

    #[test]
    fn same_physical_type_in_two_sources_of_one_kind_warns() {
        let a = meta("postgres", "a", &[("customers", &[ID])], &[]);
        let b = meta("postgres", "b", &[("customers", &[ID])], &[]);
        let d = definition_from_sources(
            &[
                SourceInput {
                    name: "a",
                    kind: "postgres",
                    meta: &a,
                },
                SourceInput {
                    name: "b",
                    kind: "postgres",
                    meta: &b,
                },
            ],
            &[],
        );
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("customers") && w.contains("already")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn empty_sources_is_an_empty_draft_not_an_error() {
        let d = definition_from_sources(&[], &[]);
        assert!(d.classes.is_empty() && d.warnings.is_empty());
        assert!(list(&d, "classes").is_empty() && list(&d, "mappings").is_empty());
    }
}
