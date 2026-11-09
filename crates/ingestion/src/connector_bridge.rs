//! MRFC-0070 Phase A9: Connector Bridge.
//!
//! Converts connector metadata into KnowledgeIr so database schemas,
//! graph models, and document collections participate in the
//! knowledge graph alongside code and documentation.
//!
//! Strategy: each connector produces a flat metadata struct, then
//! `connector_metadata_to_ir()` normalizes it into KnowledgeIr.

use crate::ir::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate};

/// Generic connector metadata — what any connector can report about itself.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ConnectorMetadata {
    /// Connector type: "postgres", "neo4j", "mongodb", "sqlite", etc.
    pub connector_type: String,
    /// Connection label (e.g. database name, cluster name).
    pub label: String,
    /// Tables, collections, or node labels.
    pub containers: Vec<ContainerInfo>,
    /// Foreign keys, graph relationships, or document references.
    pub references: Vec<ReferenceInfo>,
    /// Server version, if known.
    pub version: Option<String>,
    /// The container list was cut at a configured maximum.
    #[serde(default)]
    pub truncated: bool,
}

/// A table, collection, or node label.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ContainerInfo {
    /// Container name (table name, collection name, node label).
    pub name: String,
    /// Columns, fields, or properties.
    pub fields: Vec<FieldInfo>,
    /// Approximate row count, if known.
    pub row_count: Option<u64>,
    /// Namespace inside the source (Postgres schema), if it has one.
    #[serde(default)]
    pub schema: Option<String>,
    /// Column groups with a unique constraint (single and composite).
    #[serde(default)]
    pub unique_groups: Vec<Vec<String>>,
    /// Secondary indexes (and unique indexes), in key-column order.
    #[serde(default)]
    pub indexes: Vec<IndexInfo>,
    /// CHECK constraints, as the source reports their definition.
    #[serde(default)]
    pub checks: Vec<CheckInfo>,
    /// `None` for a plain table / collection / label; `"view"`,
    /// `"materialized_view"` otherwise.
    #[serde(default)]
    pub kind: Option<String>,
    /// Source-specific storage facts (ClickHouse engine, sorting / partition /
    /// primary key, a view's defining query). Declared by the source, so part
    /// of the schema's identity.
    #[serde(default)]
    pub properties: std::collections::BTreeMap<String, String>,
}

/// A secondary index. `unique` is a guarantee the source enforces.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    /// btree / hash / gin / range / text / vector, as the source names it.
    #[serde(default)]
    pub method: Option<String>,
}

/// A CHECK constraint.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CheckInfo {
    pub name: String,
    pub expression: String,
}

/// Whether a field's type was declared by the source or only observed in a sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Provenance {
    #[default]
    Declared,
    Observed,
}

/// What a sample of values showed about a field (observed schemas only).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FieldProfile {
    pub types: Vec<String>,
    pub presence_ratio: f64,
    pub null_ratio: f64,
    pub samples: u64,
}

/// A column, field, or property within a container.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct FieldInfo {
    pub name: String,
    /// SQL type, GraphQL type, BSON type, etc.
    pub data_type: String,
    /// Whether this field is part of a primary key.
    pub is_primary_key: bool,
    /// Whether this field can be null.
    pub nullable: bool,
    /// Whether this field has a unique constraint.
    pub is_unique: bool,
    #[serde(default)]
    pub provenance: Provenance,
    #[serde(default)]
    pub profile: Option<FieldProfile>,
}

/// A foreign key, graph relationship, or document reference.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReferenceInfo {
    /// Source container.
    pub from_container: String,
    /// Source field(s).
    pub from_fields: Vec<String>,
    /// Target container.
    pub to_container: String,
    /// Target field(s).
    pub to_fields: Vec<String>,
    /// Relationship name (FK constraint name, edge label, etc.).
    pub name: Option<String>,
}

/// The domain class a container proposes: singular PascalCase.
pub fn class_name_for_container(container: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in container.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            words.push(std::mem::take(&mut cur));
        }
        cur.push(c);
        prev_lower = c.is_lowercase();
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let Some(last) = words.last_mut() else {
        return "Thing".into();
    };
    *last = singular(last);
    let name: String = words
        .iter()
        .map(|w| {
            let mut cs = w.chars();
            cs.next()
                .map(|f| f.to_uppercase().chain(cs).collect::<String>())
                .unwrap_or_default()
        })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("T{name}")
    } else {
        name
    }
}

/// Heuristic singular of a table-ish word: `ies→y`, `sses→ss`, a lone
/// trailing `s` (not `ss`/`us`/`is`). Irregular plurals are left alone.
fn singular(w: &str) -> String {
    let lower = w.to_ascii_lowercase();
    let n = w.len();
    if lower.ends_with("ies") && n > 4 {
        format!("{}y", &w[..n - 3])
    } else if lower.ends_with("sses") {
        w[..n - 2].to_string()
    } else if lower.ends_with('s')
        && !lower.ends_with("ss")
        && !lower.ends_with("us")
        && !lower.ends_with("is")
        && n > 3
    {
        w[..n - 1].to_string()
    } else {
        w.to_string()
    }
}

/// Convert connector metadata into KnowledgeIr.
///
/// - Containers → Entities (type = connector_type + "Table"/"Collection"/"NodeLabel")
/// - Fields → Facts (claims about column names and types)
/// - References → Relations (foreign keys, graph edges)
/// - Primary keys → Facts (claims about identity columns)
pub fn connector_metadata_to_ir(meta: &ConnectorMetadata) -> KnowledgeIr {
    let source_id = format!("connector://{}/{}", meta.connector_type, meta.label);
    let container_type = container_type_label(&meta.connector_type);

    let mut entities = Vec::new();
    let mut facts = Vec::new();
    let mut relations = Vec::new();

    for container in &meta.containers {
        // Container → Entity
        let entity_name = format!("{}.{}", meta.label, container.name);
        let evidence = connector_evidence(&source_id, &meta.connector_type);

        entities.push(EntityCandidate {
            name: entity_name.clone(),
            type_hint: Some(class_name_for_container(&container.name)),
            mentions: vec![format!(
                "{} '{}' with {} fields (approx {} rows)",
                container_type,
                container.name,
                container.fields.len(),
                container.row_count.unwrap_or(0)
            )],
            confidence: 1.0,
            evidence,
        });

        // Primary key columns → Facts
        for field in &container.fields {
            if field.is_primary_key {
                facts.push(FactCandidate {
                    snippet: None,
                    statement: format!(
                        "{}.{}.{} is the primary key (type: {}, nullable: {})",
                        meta.label, container.name, field.name, field.data_type, field.nullable
                    ),
                    entities: vec![entity_name.clone()],
                    confidence: 1.0,
                    evidence: connector_evidence(&source_id, &meta.connector_type),
                });
            }
            if field.is_unique && !field.is_primary_key {
                facts.push(FactCandidate {
                    snippet: None,
                    statement: format!(
                        "{}.{}.{} has a unique constraint (type: {})",
                        meta.label, container.name, field.name, field.data_type
                    ),
                    entities: vec![entity_name.clone()],
                    confidence: 1.0,
                    evidence: connector_evidence(&source_id, &meta.connector_type),
                });
            }
            // Schema facts: every column
            facts.push(FactCandidate {
                snippet: None,
                statement: format!(
                    "{}.{}.{} : {} {} {}",
                    meta.label,
                    container.name,
                    field.name,
                    field.data_type,
                    if field.nullable {
                        "nullable"
                    } else {
                        "NOT NULL"
                    },
                    if field.is_primary_key { "PK" } else { "" },
                ),
                entities: vec![entity_name.clone()],
                confidence: 1.0,
                evidence: connector_evidence(&source_id, &meta.connector_type),
            });
            if let Some(p) = &field.profile {
                facts.push(FactCandidate {
                    snippet: None,
                    statement: format!(
                        "{}.{}.{} observed: types=[{}] presence={:.2} null={:.2} samples={}",
                        meta.label,
                        container.name,
                        field.name,
                        p.types.join(", "),
                        p.presence_ratio,
                        p.null_ratio,
                        p.samples
                    ),
                    entities: vec![entity_name.clone()],
                    confidence: 0.8,
                    evidence: connector_evidence(&source_id, &meta.connector_type),
                });
            }
        }
    }

    // Container-level schema facts: indexes, CHECK constraints, view kind and
    // storage properties, each a statement about the container's entity.
    for container in &meta.containers {
        let entity_name = format!("{}.{}", meta.label, container.name);
        let mut statements: Vec<String> = Vec::new();
        for i in &container.indexes {
            statements.push(format!(
                "{entity_name} has {}index {} on ({}){}",
                if i.unique { "unique " } else { "" },
                i.name,
                i.columns.join(", "),
                i.method
                    .as_ref()
                    .map_or(String::new(), |m| format!(" [{m}]"))
            ));
        }
        for k in &container.checks {
            statements.push(format!(
                "{entity_name} has check constraint {}: {}",
                k.name, k.expression
            ));
        }
        if let Some(kind) = &container.kind {
            statements.push(format!("{entity_name} is a {}", kind.replace('_', " ")));
        }
        for (key, value) in &container.properties {
            statements.push(format!("{entity_name} {key} = {value}"));
        }
        for statement in statements {
            facts.push(FactCandidate {
                snippet: None,
                statement,
                entities: vec![entity_name.clone()],
                confidence: 1.0,
                evidence: connector_evidence(&source_id, &meta.connector_type),
            });
        }
    }

    // References → Relations
    for r in &meta.references {
        let subj = format!("{}.{}", meta.label, r.from_container);
        let obj = format!("{}.{}", meta.label, r.to_container);
        let pred = r
            .name
            .clone()
            .unwrap_or_else(|| format!("references_{}", r.to_container));

        relations.push(RelationCandidate {
            subject: subj,
            predicate: pred,
            object: obj,
            confidence: 1.0,
            evidence: connector_evidence(&source_id, &meta.connector_type),
        });
    }

    KnowledgeIr {
        document_id: Some(source_id.clone()),
        entities,
        facts,
        relations,
        extractor: format!("connector-bridge/{}", meta.connector_type),
        ..Default::default()
    }
}

/// Discover connector metadata from a live database schema.
///
/// This is a stub for when a connector is active and we can introspect.
/// In practice, this would use the connector's schema discovery API.
pub fn discover_connector_schema(
    connector_type: &str,
    label: &str,
    tables: &[(&str, &[(&str, &str, bool, bool, bool)])],
    foreign_keys: &[(&str, &[&str], &str, &[&str], Option<&str>)],
) -> ConnectorMetadata {
    let containers: Vec<ContainerInfo> = tables
        .iter()
        .map(|(name, cols)| ContainerInfo {
            name: name.to_string(),
            fields: cols
                .iter()
                .map(|(col_name, col_type, pk, nullable, unique)| FieldInfo {
                    name: col_name.to_string(),
                    data_type: col_type.to_string(),
                    is_primary_key: *pk,
                    nullable: *nullable,
                    is_unique: *unique,
                    ..Default::default()
                })
                .collect(),
            row_count: None,
            ..Default::default()
        })
        .collect();

    let references: Vec<ReferenceInfo> = foreign_keys
        .iter()
        .map(
            |(from_container, from_fields, to_container, to_fields, name)| ReferenceInfo {
                from_container: from_container.to_string(),
                from_fields: from_fields.iter().map(|s| s.to_string()).collect(),
                to_container: to_container.to_string(),
                to_fields: to_fields.iter().map(|s| s.to_string()).collect(),
                name: name.map(|n| n.to_string()),
            },
        )
        .collect();

    ConnectorMetadata {
        connector_type: connector_type.to_string(),
        label: label.to_string(),
        containers,
        references,
        version: None,
        ..Default::default()
    }
}

fn container_type_label(connector_type: &str) -> &str {
    match connector_type {
        "postgres" | "sqlite" | "mysql" => "Table",
        "mongodb" => "Collection",
        "neo4j" => "NodeLabel",
        _ => "Container",
    }
}

fn connector_evidence(source_id: &str, connector_type: &str) -> Evidence {
    Evidence {
        document_id: Some(source_id.to_string()),
        extractor: format!("connector-bridge/{}", connector_type),
        confidence: 1.0,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_schema_to_ir() {
        let meta = discover_connector_schema(
            "postgres",
            "mydb",
            &[
                (
                    "users",
                    &[
                        ("id", "integer", true, false, true),
                        ("email", "varchar", false, false, true),
                        ("name", "varchar", false, true, false),
                    ],
                ),
                (
                    "orders",
                    &[
                        ("id", "integer", true, false, true),
                        ("user_id", "integer", false, false, false),
                        ("total", "numeric", false, false, false),
                    ],
                ),
            ],
            &[(
                "orders",
                &["user_id"],
                "users",
                &["id"],
                Some("fk_orders_users"),
            )],
        );

        let ir = connector_metadata_to_ir(&meta);

        // 2 tables → 2 entities
        assert_eq!(ir.entities.len(), 2);
        assert!(ir.entities.iter().any(|e| e.name == "mydb.users"));
        assert!(ir.entities.iter().any(|e| e.name == "mydb.orders"));

        // 6 columns → 6 column facts + 2 PK facts + 1 unique fact = 9 facts
        assert!(ir.facts.len() >= 6);

        // 1 foreign key → 1 relation
        assert_eq!(ir.relations.len(), 1);
        let rel = &ir.relations[0];
        assert_eq!(rel.subject, "mydb.orders");
        assert_eq!(rel.object, "mydb.users");
        assert_eq!(rel.predicate, "fk_orders_users");
    }

    #[test]
    fn mongodb_collections_to_ir() {
        let meta = discover_connector_schema(
            "mongodb",
            "analytics",
            &[("events", &[("_id", "objectid", true, false, true)])],
            &[],
        );

        let ir = connector_metadata_to_ir(&meta);
        assert_eq!(ir.entities.len(), 1);
        assert_eq!(ir.entities[0].name, "analytics.events");
        assert_eq!(ir.entities[0].type_hint.as_deref(), Some("Event"));
    }

    #[test]
    fn class_name_for_container_is_singular_pascal_case() {
        for (input, want) in [
            ("customers", "Customer"),
            ("order_items", "OrderItem"),
            ("OrderItems", "OrderItem"),
            ("Categories", "Category"),
            ("addresses", "Address"),
            ("address", "Address"),
            ("status", "Status"),
            ("people", "People"),
            ("2024_sales", "T2024Sale"),
            ("user data", "UserData"),
            ("a-b", "AB"),
            ("cert_ont001_people", "CertOnt001People"),
            ("P_Ont001", "POnt001"),
            ("", "Thing"),
            ("___", "Thing"),
        ] {
            assert_eq!(class_name_for_container(input), want, "input {input:?}");
        }
    }

    #[test]
    fn each_container_gets_its_own_class() {
        let meta = discover_connector_schema(
            "postgres",
            "shop",
            &[
                ("customers", &[("id", "int", true, false, true)]),
                ("orders", &[("id", "int", true, false, true)]),
                ("products", &[("id", "int", true, false, true)]),
            ],
            &[],
        );
        let proposal = crate::discover_ontology_from_ir(&connector_metadata_to_ir(&meta));
        let mut names: Vec<&str> = proposal.classes.iter().map(|c| c.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["Customer", "Order", "Product"]);
        assert!(
            names
                .iter()
                .all(|n| !n.contains("Table") && !n.contains("postgres")),
            "the source kind is not a class: {names:?}"
        );
    }

    #[test]
    fn observed_fields_add_a_profile_fact_and_no_property_noise() {
        let mut meta = discover_connector_schema(
            "mongodb",
            "crm",
            &[("people", &[("age", "bson", false, true, false)])],
            &[],
        );
        let plain = crate::discover_ontology_from_ir(&connector_metadata_to_ir(&meta));
        meta.containers[0].fields[0].provenance = Provenance::Observed;
        meta.containers[0].fields[0].profile = Some(FieldProfile {
            types: vec!["Int".into(), "Null".into()],
            presence_ratio: 0.91,
            null_ratio: 0.04,
            samples: 1000,
        });
        let ir = connector_metadata_to_ir(&meta);
        let fact = ir
            .facts
            .iter()
            .find(|f| f.statement.contains("observed"))
            .expect("an observed-profile fact");
        assert!(fact.statement.contains("Int, Null") && fact.statement.contains("0.91"));
        assert_eq!(
            crate::discover_ontology_from_ir(&ir).properties.len(),
            plain.properties.len(),
            "the profile fact must not invent properties"
        );
    }

    #[test]
    fn physical_container_stays_in_the_entity_mention() {
        let meta = discover_connector_schema(
            "postgres",
            "shop",
            &[("customers", &[("id", "int", true, false, true)])],
            &[],
        );
        let ir = connector_metadata_to_ir(&meta);
        assert_eq!(ir.entities[0].name, "shop.customers");
        assert!(ir.entities[0].mentions[0].contains("Table 'customers'"));
    }

    #[test]
    fn empty_schema_produces_empty_ir() {
        let meta = ConnectorMetadata::default();
        let ir = connector_metadata_to_ir(&meta);
        assert!(ir.entities.is_empty());
        assert!(ir.facts.is_empty());
        assert!(ir.relations.is_empty());
    }

    #[test]
    fn pk_facts_include_column_type() {
        let meta = discover_connector_schema(
            "postgres",
            "db",
            &[("items", &[("id", "uuid", true, false, true)])],
            &[],
        );

        let ir = connector_metadata_to_ir(&meta);
        let pk_fact = ir
            .facts
            .iter()
            .find(|f| f.statement.contains("primary key"))
            .unwrap();
        assert!(pk_fact.statement.contains("uuid"));
        assert!(pk_fact.statement.contains("db.items.id"));
    }

    #[test]
    fn indexes_checks_kinds_and_storage_properties_become_facts() {
        let meta = ConnectorMetadata {
            connector_type: "clickhouse".into(),
            label: "analytics".into(),
            containers: vec![ContainerInfo {
                name: "orders".into(),
                fields: vec![FieldInfo {
                    name: "id".into(),
                    data_type: "UInt64".into(),
                    ..Default::default()
                }],
                indexes: vec![
                    IndexInfo {
                        name: "orders_customer_idx".into(),
                        columns: vec!["customer_id".into()],
                        unique: false,
                        method: Some("btree".into()),
                    },
                    IndexInfo {
                        name: "orders_email_key".into(),
                        columns: vec!["email".into(), "tenant".into()],
                        unique: true,
                        method: None,
                    },
                ],
                checks: vec![CheckInfo {
                    name: "pos".into(),
                    expression: "total > 0".into(),
                }],
                kind: Some("materialized_view".into()),
                properties: [("engine".to_string(), "ReplacingMergeTree".to_string())].into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let ir = connector_metadata_to_ir(&meta);
        let facts: Vec<&str> = ir.facts.iter().map(|f| f.statement.as_str()).collect();
        let has = |needle: &str| facts.iter().any(|f| f.contains(needle));
        assert!(
            has("analytics.orders has index orders_customer_idx on (customer_id) [btree]"),
            "{facts:?}"
        );
        assert!(
            has("analytics.orders has unique index orders_email_key on (email, tenant)"),
            "{facts:?}"
        );
        assert!(
            has("analytics.orders has check constraint pos: total > 0"),
            "{facts:?}"
        );
        assert!(has("analytics.orders is a materialized view"), "{facts:?}");
        assert!(
            has("analytics.orders engine = ReplacingMergeTree"),
            "{facts:?}"
        );
        // all of it hangs off the container's entity
        for f in ir
            .facts
            .iter()
            .filter(|f| f.statement.contains("index") || f.statement.contains("check"))
        {
            assert_eq!(f.entities, vec!["analytics.orders".to_string()]);
        }
        // a plain table says none of it
        let plain = connector_metadata_to_ir(&ConnectorMetadata {
            containers: vec![ContainerInfo {
                name: "t".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(plain.facts.iter().all(|f| !f.statement.contains("index")
            && !f.statement.contains("check")
            && !f.statement.contains(" is a ")));
    }
}
