//! Source schema discovery: configured external databases, their schema as
//! knowledge, and the ontology drafted from it.

#[cfg(test)]
mod draft_tests {
    use aikoql_ingestion::{definition_from_sources, discover_connector_schema, SourceInput};

    /// The draft is exactly what `tool_save_ontology` decodes and validates.
    #[test]
    fn draft_definition_round_trips_through_ontologydef() {
        let m = discover_connector_schema(
            "postgres",
            "shop",
            &[
                (
                    "customers",
                    &[
                        ("id", "integer", true, false, true),
                        ("email", "text", false, false, false),
                    ],
                ),
                (
                    "orders",
                    &[
                        ("id", "integer", true, false, true),
                        ("customer_id", "integer", false, true, false),
                    ],
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
        let draft = definition_from_sources(
            &[SourceInput {
                name: "shop",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        let props = crate::helpers::parse_properties(
            &serde_json::json!({ "properties": draft.definition }),
        )
        .expect("draft JSON parses as properties");
        let def = aikoql_kernel::OntologyDef::from_property_map(&props)
            .expect("draft decodes and validates as an OntologyDef");
        assert_eq!(def.classes.len(), 2);
        assert_eq!(def.mappings.len(), 2);
        assert!(def.relationships.contains_key("orders_customer_fk"));
    }
}
