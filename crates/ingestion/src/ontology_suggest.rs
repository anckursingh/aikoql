//! Proposals, never decisions: which containers of different sources look like
//! the same concept, and which of their fields look like the same property.
//!
//! Deterministic evidence only (names, a small synonym table, key roles,
//! value types). A suggestion carries its confidence and the evidence behind
//! it; a human applies it as an explicit [`crate::ClassMerge`] or ignores it.

use crate::ontology_draft::SourceInput;

/// Two fields that look like one property.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyMatch {
    pub a_field: String,
    pub b_field: String,
    pub confidence: f32,
    pub evidence: Vec<String>,
}

/// Two `"<source>.<container>"` references that look like one concept.
#[derive(Clone, Debug, PartialEq)]
pub struct MergeSuggestion {
    pub a: String,
    pub b: String,
    pub confidence: f32,
    pub evidence: Vec<String>,
    /// Only pairs whose canonical names differ: the ones an alignment would change.
    pub properties: Vec<PropertyMatch>,
}

pub fn suggest_merges(_sources: &[SourceInput]) -> Vec<MergeSuggestion> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector_bridge::{discover_connector_schema, ConnectorMetadata};

    type Col = (&'static str, &'static str, bool, bool, bool);
    const ID: Col = ("id", "integer", true, false, true);

    fn meta(kind: &str, label: &str, tables: &[(&str, &[Col])]) -> ConnectorMetadata {
        discover_connector_schema(kind, label, tables, &[])
    }

    fn suggest(a: &ConnectorMetadata, b: &ConnectorMetadata) -> Vec<MergeSuggestion> {
        suggest_merges(&[
            SourceInput {
                name: "crm",
                kind: "postgres",
                meta: a,
            },
            SourceInput {
                name: "shop",
                kind: "mongodb",
                meta: b,
            },
        ])
    }

    fn crm() -> ConnectorMetadata {
        meta(
            "postgres",
            "crm",
            &[
                (
                    "customers",
                    &[
                        ID,
                        ("email", "text", false, false, true),
                        ("tier", "text", false, true, false),
                        ("created_at", "timestamptz", false, true, false),
                    ],
                ),
                (
                    "orders",
                    &[
                        ID,
                        ("customer_id", "integer", false, false, false),
                        ("total", "numeric", false, false, false),
                    ],
                ),
            ],
        )
    }

    #[test]
    fn synonym_names_with_aligned_fields_are_suggested_with_evidence() {
        let shop = meta(
            "mongodb",
            "shop",
            &[(
                "clients",
                &[
                    ("_id", "ObjectId", true, false, true),
                    ("email_address", "String", false, false, false),
                    ("age", "Int32", false, true, false),
                ],
            )],
        );
        let s = suggest(&crm(), &shop);
        assert_eq!(s.len(), 1, "{s:?}");
        let m = &s[0];
        assert_eq!(
            (m.a.as_str(), m.b.as_str()),
            ("crm.customers", "shop.clients")
        );
        assert!(
            m.confidence >= 0.5 && m.confidence < 1.0,
            "{}",
            m.confidence
        );
        assert!(
            m.evidence
                .iter()
                .any(|e| e.contains("customer") && e.contains("client")),
            "{:?}",
            m.evidence
        );
        // the email pair needs an alignment; id/_id already share a canonical name
        assert_eq!(m.properties.len(), 1, "{:?}", m.properties);
        assert_eq!(
            (
                m.properties[0].a_field.as_str(),
                m.properties[0].b_field.as_str()
            ),
            ("email", "email_address")
        );
        assert!(!m.properties[0].evidence.is_empty());
    }

    #[test]
    fn identical_class_names_with_identical_fields_are_a_confident_suggestion() {
        let other = meta(
            "mongodb",
            "shop",
            &[(
                "customers",
                &[
                    ID,
                    ("email", "text", false, false, true),
                    ("tier", "text", false, true, false),
                ],
            )],
        );
        let s = suggest(&crm(), &other);
        assert_eq!(s.len(), 1);
        assert!(s[0].confidence >= 0.8, "{}", s[0].confidence);
        assert!(
            s[0].properties.is_empty(),
            "nothing to align: names already agree"
        );
    }

    #[test]
    fn unrelated_containers_are_not_suggested() {
        let shop = meta(
            "mongodb",
            "shop",
            &[
                (
                    "products",
                    &[
                        ID,
                        ("sku", "text", false, false, true),
                        ("price", "numeric", false, false, false),
                    ],
                ),
                ("sessions", &[ID, ("token", "text", false, false, false)]),
            ],
        );
        assert!(suggest(&crm(), &shop).is_empty());
    }

    #[test]
    fn a_shared_primary_key_name_alone_is_not_evidence() {
        // every table has an `id`: orders/clients must not be suggested for that
        let shop = meta(
            "mongodb",
            "shop",
            &[(
                "clients",
                &[
                    ("_id", "ObjectId", true, false, true),
                    ("note", "String", false, true, false),
                ],
            )],
        );
        let only_orders = meta(
            "postgres",
            "crm",
            &[("orders", &[ID, ("total", "numeric", false, false, false)])],
        );
        assert!(suggest(&only_orders, &shop).is_empty());
    }

    #[test]
    fn containers_of_one_source_are_never_paired() {
        let m = meta(
            "postgres",
            "crm",
            &[
                ("customers", &[ID, ("email", "text", false, false, true)]),
                (
                    "clients",
                    &[ID, ("email_address", "text", false, false, true)],
                ),
            ],
        );
        let s = suggest_merges(&[SourceInput {
            name: "crm",
            kind: "postgres",
            meta: &m,
        }]);
        assert!(s.is_empty(), "{s:?}");
    }

    #[test]
    fn id_is_not_matched_to_a_foreign_key_that_merely_contains_it() {
        // customer_id (a foreign key in orders) is not the identity of clients
        let a = meta(
            "postgres",
            "crm",
            &[(
                "orders",
                &[ID, ("customer_id", "integer", false, false, false)],
            )],
        );
        let b = meta(
            "mongodb",
            "shop",
            &[(
                "clients",
                &[
                    ("_id", "ObjectId", true, false, true),
                    ("customer_ref", "String", false, true, false),
                ],
            )],
        );
        let s = suggest(&a, &b);
        assert!(
            s.iter().all(|m| m
                .properties
                .iter()
                .all(|p| p.a_field != "customer_id" || p.b_field != "_id")),
            "{s:?}"
        );
    }

    #[test]
    fn suggestions_are_deterministic_ordered_and_bounded() {
        let a = crm();
        let b = meta(
            "mongodb",
            "shop",
            &[
                ("customers", &[ID, ("email", "text", false, false, true)]),
                (
                    "clients",
                    &[
                        ID,
                        ("email_address", "text", false, false, true),
                        ("tier", "text", false, true, false),
                    ],
                ),
            ],
        );
        let first = suggest(&a, &b);
        for _ in 0..5 {
            assert_eq!(suggest(&a, &b), first);
        }
        assert!(first.windows(2).all(|w| w[0].confidence >= w[1].confidence));
        assert!(first.len() >= 2);
    }

    #[test]
    fn mismatched_value_types_lower_the_property_confidence() {
        let a = meta(
            "postgres",
            "crm",
            &[("customers", &[ID, ("age", "integer", false, true, false)])],
        );
        let same = meta(
            "mongodb",
            "shop",
            &[("customers", &[ID, ("age", "integer", false, true, false)])],
        );
        let diff = meta(
            "mongodb",
            "shop",
            &[("customers", &[ID, ("age", "text", false, true, false)])],
        );
        let (s1, s2) = (suggest(&a, &same), suggest(&a, &diff));
        assert!(
            s1[0].confidence > s2[0].confidence,
            "{} vs {}",
            s1[0].confidence,
            s2[0].confidence
        );
    }
}
