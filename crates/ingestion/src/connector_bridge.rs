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
}

/// A table, collection, or node label.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ContainerInfo {
    /// Container name (table name, collection name, node label).
    pub name: String,
    /// Columns, fields, or properties.
    pub fields: Vec<FieldInfo>,
    /// Approximate row count, if known.
    pub row_count: Option<u64>,
}

/// A column, field, or property within a container.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
            type_hint: Some(format!("{}{}", meta.connector_type, container_type)),
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
                })
                .collect(),
            row_count: None,
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
        assert_eq!(
            ir.entities[0].type_hint.as_deref(),
            Some("mongodbCollection")
        );
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
}
