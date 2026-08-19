//! Subcommand runners extracted verbatim from cli.rs (PRR-7).
//! No behavior changes.

use crate::*;

pub(crate) fn run_pg_import(
    conn_str: &str,
    target_db: &str,
    tenant: Option<&str>,
    table_filter: Option<&str>,
) {
    use aikoql_postgres::PostgresConnector;

    println!("Connecting to PostgreSQL...");
    let mut connector = match PostgresConnector::connect(conn_str) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Connection failed: {}", e);
            std::process::exit(1);
        }
    };

    println!("Discovering schema...");
    let schemas = match connector.introspect_all() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Schema discovery failed: {}", e);
            std::process::exit(1);
        }
    };

    if schemas.is_empty() {
        println!("No user tables found in the database.");
        return;
    }

    println!("Found {} table(s):", schemas.len());
    for s in &schemas {
        println!(
            "  {} ({} cols, ~{} rows)",
            s.name,
            s.columns.len(),
            s.row_count_estimate
        );
    }
    println!();

    let kernel = match engine::open_kernel_auto(target_db) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
    let mut total_imported = 0usize;

    for schema in &schemas {
        if let Some(tf) = table_filter {
            if schema.name != tf {
                continue;
            }
        }
        println!("Importing {}...", schema.name);
        match connector.import_table(schema, tenant) {
            Ok(objects) => {
                let count = objects.len();
                for ko in objects {
                    match kernel.remember(RememberRequest {
                        context: Subject::new("pg-importer").into(),
                        koid: Some(ko.koid),
                        expected_version: Some(0),
                        idempotency_key: Some(format!(
                            "pg-import-{}-{}",
                            schema.name,
                            ko.koid.to_hex()
                        )),
                        metadata: ko.metadata,
                        properties: ko.properties,
                        semantic: None,
                        relationships: vec![],
                        security: Some(ko.security),
                        extensions: ko.extensions,
                        origin: Origin::Human,
                        note: Some("imported from PostgreSQL".into()),
                        referential_policy: ReferentialPolicy::Permissive,
                    }) {
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("  Warning: failed to commit row: {}", e);
                        }
                    }
                }
                total_imported += count;
                println!("  {} rows imported", count);
            }
            Err(e) => {
                eprintln!("  Error importing {}: {}", schema.name, e);
            }
        }
    }

    println!();
    println!(
        "Import complete. {} total objects imported into {}",
        total_imported, target_db
    );
}
pub(crate) fn run_sqlite_import(
    source_file: &str,
    target_db: &str,
    tenant: Option<&str>,
    table_filter: Option<&str>,
) {
    use aikoql_sqlite::SqliteConnector;

    println!("Opening SQLite: {}", source_file);
    let connector = match SqliteConnector::open(source_file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Open failed: {}", e);
            std::process::exit(1);
        }
    };

    println!("Discovering schema...");
    let schemas = match connector.introspect_all() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Schema discovery failed: {}", e);
            std::process::exit(1);
        }
    };

    if schemas.is_empty() {
        println!("No user tables found.");
        return;
    }

    println!("Found {} table(s):", schemas.len());
    for s in &schemas {
        println!(
            "  {} ({} cols, {} rows)",
            s.name,
            s.columns.len(),
            s.row_count
        );
    }
    println!();

    let kernel = match engine::open_kernel_auto(target_db) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
    let mut total_imported = 0usize;

    for schema in &schemas {
        if let Some(tf) = table_filter {
            if schema.name != tf {
                continue;
            }
        }
        println!("Importing {}...", schema.name);
        match connector.import_table(schema, tenant) {
            Ok(objects) => {
                let count = objects.len();
                for ko in objects {
                    let idem_key = format!("sqlite-import-{}-{}", schema.name, ko.koid.to_hex());
                    match kernel.remember(RememberRequest {
                        context: Subject::new("sqlite-importer").into(),
                        koid: Some(ko.koid),
                        expected_version: Some(0),
                        idempotency_key: Some(idem_key),
                        metadata: ko.metadata,
                        properties: ko.properties,
                        semantic: None,
                        relationships: vec![],
                        security: Some(ko.security),
                        extensions: ko.extensions,
                        origin: Origin::Human,
                        note: Some("imported from SQLite".into()),
                        referential_policy: ReferentialPolicy::Permissive,
                    }) {
                        Ok(_) => {}
                        Err(e) => eprintln!("  Warning: failed to commit row: {}", e),
                    }
                }
                total_imported += count;
                println!("  {} rows imported", count);
            }
            Err(e) => {
                eprintln!("  Error importing {}: {}", schema.name, e);
            }
        }
    }

    println!();
    println!(
        "Import complete. {} total objects imported into {}",
        total_imported, target_db
    );
}
pub(crate) fn run_mongo_import(
    uri: &str,
    database: &str,
    target_db: &str,
    tenant: Option<&str>,
    coll_filter: Option<&str>,
) {
    use aikoql_mongodb::MongoConnector;

    println!("Connecting to MongoDB: {}", uri);
    let connector = match MongoConnector::connect(uri, database) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Connection failed: {}", e);
            std::process::exit(1);
        }
    };

    println!("Discovering collections in '{}'...", database);
    let schemas = match connector.introspect_all() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Discovery failed: {}", e);
            std::process::exit(1);
        }
    };

    if schemas.is_empty() {
        println!("No collections found.");
        return;
    }

    println!("Found {} collection(s):", schemas.len());
    for s in &schemas {
        println!(
            "  {} ({} docs, {} properties)",
            s.name,
            s.document_count,
            s.properties.len()
        );
        if s.properties.len() <= 15 {
            println!("    props: {}", s.properties.join(", "));
        }
    }
    println!();

    let kernel = match engine::open_kernel_auto(target_db) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
    let mut total_imported = 0usize;

    for schema in &schemas {
        if let Some(cf) = coll_filter {
            if schema.name != cf {
                continue;
            }
        }
        println!("Importing {}...", schema.name);
        match connector.import_collection(schema, tenant) {
            Ok(objects) => {
                let count = objects.len();
                for ko in objects {
                    let idem_key = format!("mongo-import-{}-{}", schema.name, ko.koid.to_hex());
                    match kernel.remember(RememberRequest {
                        context: Subject::new("mongo-importer").into(),
                        koid: Some(ko.koid),
                        expected_version: Some(0),
                        idempotency_key: Some(idem_key),
                        metadata: ko.metadata,
                        properties: ko.properties,
                        semantic: None,
                        relationships: vec![],
                        security: Some(ko.security),
                        extensions: ko.extensions,
                        origin: Origin::Human,
                        note: Some("imported from MongoDB".into()),
                        referential_policy: ReferentialPolicy::Permissive,
                    }) {
                        Ok(_) => {}
                        Err(e) => eprintln!("  Warning: failed to commit doc: {}", e),
                    }
                }
                total_imported += count;
                println!("  {} documents imported", count);
            }
            Err(e) => {
                eprintln!("  Error importing {}: {}", schema.name, e);
            }
        }
    }

    println!();
    println!(
        "Import complete. {} total documents imported into {}",
        total_imported, target_db
    );
}
pub(crate) fn run_neo4j_import(
    uri: &str,
    user: &str,
    password: &str,
    target_db: &str,
    tenant: Option<&str>,
    label_filter: Option<&str>,
) {
    use aikoql_neo4j::Neo4jConnector;

    println!("Connecting to Neo4j: {}", uri);
    let connector = match Neo4jConnector::connect(uri, user, password) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Connection failed: {}", e);
            std::process::exit(1);
        }
    };

    println!("Discovering graph schema...");
    let labels = match connector.list_labels() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to list labels: {}", e);
            std::process::exit(1);
        }
    };
    let rel_types = match connector.list_rel_types() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to list relationship types: {}", e);
            std::process::exit(1);
        }
    };
    println!(
        "Labels: {} ({}), Relationship types: {} ({})",
        labels.len(),
        labels.join(", "),
        rel_types.len(),
        rel_types.join(", ")
    );
    println!();

    let kernel = match engine::open_kernel_auto(target_db) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
    let mut total_nodes = 0usize;
    let mut total_rels = 0usize;

    // Phase 1: import nodes, build elementId → KOID map.
    let mut global_id_map: HashMap<String, KOID> = HashMap::new();
    let filtered_labels: Vec<&str> = if let Some(lf) = label_filter {
        labels
            .iter()
            .filter(|l| l.as_str() == lf)
            .map(String::as_str)
            .collect()
    } else {
        labels.iter().map(String::as_str).collect()
    };

    for label in &filtered_labels {
        println!("Importing nodes with label '{}'...", label);
        match connector.import_nodes(label, tenant) {
            Ok((objects, id_map)) => {
                let count = objects.len();
                for (elem_id, koid) in &id_map {
                    global_id_map.insert(elem_id.clone(), *koid);
                }
                for ko in objects {
                    let idem_key = format!("neo4j-node-{}-{}", label, ko.koid.to_hex());
                    match kernel.remember(RememberRequest {
                        context: Subject::new("neo4j-importer").into(),
                        koid: Some(ko.koid),
                        expected_version: Some(0),
                        idempotency_key: Some(idem_key),
                        metadata: ko.metadata,
                        properties: ko.properties,
                        semantic: None,
                        relationships: vec![],
                        security: Some(ko.security),
                        extensions: ko.extensions,
                        origin: Origin::Human,
                        note: Some("imported from Neo4j".into()),
                        referential_policy: ReferentialPolicy::Permissive,
                    }) {
                        Ok(_) => {}
                        Err(e) => eprintln!("  Warning: commit failed: {}", e),
                    }
                }
                total_nodes += count;
                println!("  {} nodes imported", count);
            }
            Err(e) => eprintln!("  Error: {}", e),
        }
    }

    // Phase 2: import relationships (only if we have nodes mapped).
    if !global_id_map.is_empty() {
        for rt in &rel_types {
            println!("Importing relationships [{}]...", rt);
            match connector.import_relationships(rt, &global_id_map) {
                Ok(rels) => {
                    let count = rels.len();
                    // Update source nodes to include these relationships.
                    let mut node_rels: HashMap<KOID, Vec<RelationshipRef>> = HashMap::new();
                    for (rel, src_koid, _tgt_koid) in &rels {
                        node_rels.entry(*src_koid).or_default().push(rel.clone());
                    }
                    for (koid, rels) in &node_rels {
                        // Re-remember the source node with relationships attached.
                        if let Ok(ko) =
                            kernel.get(KnowledgeContext::from(Subject::new("neo4j-importer")), koid)
                        {
                            let mut updated = ko.clone();
                            updated.relationships = rels.clone();
                            let idem_key = format!("neo4j-rel-update-{}", koid.to_hex());
                            let _ = kernel.remember(RememberRequest {
                                context: Subject::new("neo4j-importer").into(),
                                koid: Some(*koid),
                                expected_version: Some(ko.version),
                                idempotency_key: Some(idem_key),
                                metadata: updated.metadata,
                                properties: updated.properties,
                                semantic: None,
                                relationships: updated.relationships,
                                security: Some(updated.security),
                                extensions: updated.extensions,
                                origin: Origin::Human,
                                note: Some("Neo4j relationships attached".into()),
                                referential_policy: ReferentialPolicy::Permissive,
                            });
                        }
                    }
                    total_rels += count;
                    println!("  {} relationships imported", count);
                }
                Err(e) => eprintln!("  Error: {}", e),
            }
        }
    }

    println!();
    println!(
        "Import complete. {} nodes, {} relationships imported into {}",
        total_nodes, total_rels, target_db
    );
}
