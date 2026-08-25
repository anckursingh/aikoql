//! Subcommand runners (PRR-7) + the ImportSink commit path (connector TDD
//! items 2+). The old runners used constant idempotency keys and warned
//! through commit failures — a failed row still exited 0 and a changed row
//! could never update (remember replays the original commit on a key hit).

use crate::*;

/// Fresh default for `--run-id`: unique per invocation. Deliberately
/// non-cryptographic — the key only scopes idempotency within one database.
pub(crate) fn fresh_run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), nanos)
}

/// Mask credentials in a URI for printing: everything between "://" and the
/// first "@" becomes "***" (mongodb://user:pass@host → mongodb://***@host).
fn redact_uri(uri: &str) -> String {
    match (uri.find("://"), uri.find('@')) {
        (Some(s), Some(a)) if a > s => format!("{}***{}", &uri[..s + 3], &uri[a..]),
        _ => uri.to_string(),
    }
}

/// Replace every occurrence of each secret with [REDACTED], longest first so
/// overlapping secrets mask fully. Full-substring matching (CON-006): a
/// provider error echoing the URI loses its credential part even when the
/// whole URI isn't a listed secret.
fn redact_secrets(msg: &str, secrets: &[String]) -> String {
    let mut out = msg.to_string();
    let mut sorted: Vec<&String> = secrets.iter().filter(|s| !s.is_empty()).collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for s in sorted {
        out = out.replace(s.as_str(), "[REDACTED]");
    }
    out
}

/// Per-run commit path for connector imports.
///
/// Idempotency keys include `run_id`: a fresh run (default `--run-id`) may
/// update existing KOs, while retrying the same `--run-id` replays the
/// original commits cleanly. Identical heads are skipped before `remember`
/// so unchanged re-imports churn no versions. `secrets` (conn strings,
/// passwords) are scrubbed from every failure message before it is printed
/// or stored.
struct ImportSink<'k> {
    kernel: &'k Kernel,
    run_id: String,
    subject: Subject,
    secrets: Vec<String>,
}

impl<'k> ImportSink<'k> {
    fn new(kernel: &'k Kernel, run_id: String, subject: Subject, secrets: Vec<String>) -> Self {
        ImportSink {
            kernel,
            run_id,
            subject,
            secrets,
        }
    }

    /// Commit one imported KO. `Ok(false)` = head already identical (skipped).
    /// Errors are loud on purpose — a failed put means a failed run.
    fn put(&self, ko: KnowledgeObject, source: &str, note: &str) -> KResult<bool> {
        let ctx = KnowledgeContext::from(self.subject.clone());
        let head = self.kernel.get(ctx, &ko.koid).ok();
        if let Some(h) = &head {
            // ponytail: compares properties+relationships only — connector
            // metadata derives from the source schema and is stable per type.
            if h.properties == ko.properties && h.relationships == ko.relationships {
                return Ok(false);
            }
        }
        let expected_version = Some(head.map(|h| h.version).unwrap_or(0));
        let idem = format!(
            "{source}-{}-{}-{}",
            self.run_id,
            ko.metadata.type_name,
            ko.koid.to_hex()
        );
        self.kernel.remember(RememberRequest {
            context: self.subject.clone().into(),
            koid: Some(ko.koid),
            expected_version,
            idempotency_key: Some(idem),
            metadata: ko.metadata,
            properties: ko.properties,
            semantic: None,
            relationships: ko.relationships,
            security: Some(ko.security),
            extensions: ko.extensions,
            origin: Origin::Human,
            note: Some(note.into()),
            referential_policy: ReferentialPolicy::Permissive,
        })?;
        Ok(true)
    }

    /// Tombstone head KOs of `type_name` whose KOIDs the source no longer
    /// contains (CON-001 delete reconcile). Callers must invoke this only on
    /// the all-success path — a partial run must not be mistaken for a full
    /// picture of the source (CON-007).
    fn prune_missing(&self, type_name: &str, present: &HashSet<KOID>) -> KResult<usize> {
        let mut pruned = 0;
        for ko in self.kernel.scan_by_type(&self.subject, type_name)? {
            if !present.contains(&ko.koid) {
                self.kernel.forget(
                    KnowledgeContext::from(self.subject.clone()),
                    &ko.koid,
                    ForgetMode::Tombstone,
                    None,
                    Some("row deleted at source".into()),
                )?;
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// CON-005: a failed run is "explicitly marked", not silently rolled
    /// back — a `connector_run` marker KO with status "incomplete".
    /// Deterministic KOID per (source, run_id): retried failures land on
    /// the same KO and put()'s skip-if-identical means no version churn.
    /// ponytail: markers are audit records and are NOT removed by a later
    /// successful run of the same run-id.
    fn mark_incomplete(&self, source: &str, error: &str) {
        let mut koid = [0u8; 16];
        koid[..8].copy_from_slice(
            &fnv1a64(format!("connector_run:{source}:{}", self.run_id).as_bytes()).to_be_bytes(),
        );
        let mut props: std::collections::BTreeMap<String, Value> =
            std::collections::BTreeMap::new();
        props.insert("source".to_string(), Value::Text(source.to_string()));
        props.insert("status".to_string(), Value::Text("incomplete".to_string()));
        props.insert("error".to_string(), Value::Text(error.to_string()));
        let ko = KnowledgeObject {
            koid: KOID(koid),
            version: 0,
            commit_ts: 0,
            metadata: Metadata {
                type_name: "connector_run".to_string(),
                tenant: None,
                schema_version: 1,
                tags: vec!["connector".to_string(), "incomplete".to_string()],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            event_refs: vec![],
            security: SecurityDescriptor {
                owner: self.subject.name.clone(),
                acl: vec![],
                classification: None,
            },
            lifecycle: Lifecycle {
                state: LifecycleState::Draft,
                origin: Origin::Human,
            },
            extensions: ExtensionMap::new(),
        };
        // Best-effort audit write on an already-failing path — the original
        // error is the loud one and a marker failure must not mask it.
        let _ = self.put(ko, "connector-run-marker", "incomplete connector run");
    }

    /// CON-005: print the failure, record the incomplete marker, exit
    /// non-zero. Every runner error path after kernel open funnels here.
    /// The message is scrubbed (CON-006) before print or store — provider
    /// error text can echo the connection string.
    fn abort(&self, source: &str, msg: &str) -> ! {
        let msg = redact_secrets(msg, &self.secrets);
        eprintln!("{msg}");
        self.mark_incomplete(source, &msg);
        std::process::exit(1);
    }
}

pub(crate) fn run_pg_import(
    conn_str: &str,
    target_db: &str,
    tenant: Option<&str>,
    table_filter: Option<&str>,
    run_id: &str,
    timeout_ms: Option<u64>,
) {
    use aikoql_postgres::PostgresConnector;

    // Kernel first: every failure path below marks the run incomplete
    // (CON-005), so the sink must exist before any source I/O.
    let kernel = match engine::open_kernel_auto(target_db) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
    let sink = ImportSink::new(
        &kernel,
        run_id.to_string(),
        Subject::new("pg-importer"),
        vec![conn_str.to_string()],
    );

    println!("Connecting to PostgreSQL...");
    let mut connector = match PostgresConnector::connect_with_timeout(conn_str, timeout_ms) {
        Ok(c) => c,
        Err(e) => sink.abort("postgres", &format!("Connection failed: {e}")),
    };

    println!("Discovering schema...");
    let schemas = match connector.introspect_all() {
        Ok(s) => s,
        Err(e) => sink.abort("postgres", &format!("Schema discovery failed: {e}")),
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

    let filtered: Vec<&aikoql_postgres::TableSchema> = schemas
        .iter()
        .filter(|s| table_filter.is_none_or(|tf| s.name == tf))
        .collect();

    // Phase A: read every filtered table. No commits yet — FK linking
    // (Phase B) resolves parent KOIDs across this run's objects.
    let mut all_objects: Vec<KnowledgeObject> = Vec::new();
    for schema in &filtered {
        println!("Importing {}...", schema.name);
        let objects = match connector.import_table(schema, tenant) {
            Ok(o) => o,
            Err(e) => {
                sink.abort(
                    "postgres",
                    &format!("  Error importing {}: {e}", schema.name),
                );
            }
        };
        println!("  {} rows read", objects.len());
        all_objects.extend(objects);
    }

    // Phase B: foreign keys → RelationshipRef on the child rows (links only
    // tables imported in this run).
    if !all_objects.is_empty() {
        match connector.link_relationships(&filtered, &mut all_objects) {
            Ok(0) => {}
            Ok(n) => println!("  {n} foreign-key relationship(s) linked"),
            Err(e) => sink.abort("postgres", &format!("  FK linking failed: {e}")),
        }
    }

    // Phase C: commit. Any failure exits before the prune pass (CON-007).
    let mut present_by_table: HashMap<String, HashSet<KOID>> = HashMap::new();
    let mut committed_by_table: HashMap<String, usize> = HashMap::new();
    let mut total_imported = 0usize;
    for ko in all_objects {
        let table = ko.metadata.type_name.clone();
        present_by_table
            .entry(table.clone())
            .or_default()
            .insert(ko.koid);
        match sink.put(ko, "pg-import", "imported from PostgreSQL") {
            Ok(true) => {
                *committed_by_table.entry(table).or_default() += 1;
                total_imported += 1;
            }
            Ok(false) => {}
            Err(e) => {
                // A failed put = failed run: partial imports must not
                // look successful (CON-007 prune gate keys off this).
                sink.abort(
                    "postgres",
                    &format!("  Failed to commit row from {table}: {e}"),
                );
            }
        }
    }
    for schema in &filtered {
        println!(
            "  {} rows committed in {}",
            committed_by_table.get(&schema.name).copied().unwrap_or(0),
            schema.name
        );
    }

    // All tables committed without a failure — only now reconcile deletions
    // (any failure above exited instead of reaching this point).
    for (table, present) in &present_by_table {
        match sink.prune_missing(table, present) {
            Ok(0) => {}
            Ok(n) => println!("  {n} stale row(s) tombstoned in {table}"),
            Err(e) => sink.abort("postgres", &format!("  prune {table}: {e}")),
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
    run_id: &str,
    timeout_ms: Option<u64>,
) {
    use aikoql_mongodb::MongoConnector;

    // Kernel first: every failure path below marks the run incomplete
    // (CON-005), so the sink must exist before any source I/O.
    let kernel = match engine::open_kernel_auto(target_db) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
    let sink = ImportSink::new(
        &kernel,
        run_id.to_string(),
        Subject::new("mongo-importer"),
        vec![uri.to_string()],
    );

    println!("Connecting to MongoDB: {}", redact_uri(uri));
    let connector = match MongoConnector::connect_with_timeout(uri, database, timeout_ms) {
        Ok(c) => c,
        Err(e) => sink.abort("mongodb", &format!("Connection failed: {e}")),
    };

    println!("Discovering collections in '{}'...", database);
    let schemas = match connector.introspect_all() {
        Ok(s) => s,
        Err(e) => sink.abort("mongodb", &format!("Discovery failed: {e}")),
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

    let filtered: Vec<&aikoql_mongodb::CollectionSchema> = schemas
        .iter()
        .filter(|s| coll_filter.is_none_or(|cf| s.name == cf))
        .collect();

    // Phase A: read every filtered collection (no commits yet).
    let mut all_objects: Vec<KnowledgeObject> = Vec::new();
    for schema in &filtered {
        println!("Importing {}...", schema.name);
        let objects = match connector.import_collection(schema, tenant) {
            Ok(o) => o,
            Err(e) => {
                sink.abort(
                    "mongodb",
                    &format!("  Error importing {}: {e}", schema.name),
                );
            }
        };
        println!("  {} documents read", objects.len());
        all_objects.extend(objects);
    }

    // Phase B: commit. Any failure exits before the prune pass (CON-007).
    let mut present_by_table: HashMap<String, HashSet<KOID>> = HashMap::new();
    let mut total_imported = 0usize;
    for ko in all_objects {
        let coll = ko.metadata.type_name.clone();
        present_by_table
            .entry(coll.clone())
            .or_default()
            .insert(ko.koid);
        match sink.put(ko, "mongo-import", "imported from MongoDB") {
            Ok(true) => total_imported += 1,
            Ok(false) => {}
            Err(e) => {
                sink.abort(
                    "mongodb",
                    &format!("  Failed to commit doc from {coll}: {e}"),
                );
            }
        }
    }

    // Phase C: reconcile deletions on the all-success path only.
    for (coll, present) in &present_by_table {
        match sink.prune_missing(coll, present) {
            Ok(0) => {}
            Ok(n) => println!("  {n} stale doc(s) tombstoned in {coll}"),
            Err(e) => sink.abort("mongodb", &format!("  prune {coll}: {e}")),
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
    run_id: &str,
    timeout_ms: Option<u64>,
) {
    use aikoql_neo4j::Neo4jConnector;

    // Kernel first: every failure path below marks the run incomplete
    // (CON-005), so the sink must exist before any source I/O.
    let kernel = match engine::open_kernel_auto(target_db) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
    let sink = ImportSink::new(
        &kernel,
        run_id.to_string(),
        Subject::new("neo4j-importer"),
        vec![uri.to_string(), user.to_string(), password.to_string()],
    );

    println!("Connecting to Neo4j: {}", redact_uri(uri));
    let connector = match Neo4jConnector::connect_with_timeout(uri, user, password, timeout_ms) {
        Ok(c) => c,
        Err(e) => sink.abort("neo4j", &format!("Connection failed: {e}")),
    };

    println!("Discovering graph schema...");
    let mut labels = match connector.list_labels() {
        Ok(l) => l,
        Err(e) => sink.abort("neo4j", &format!("Failed to list labels: {e}")),
    };
    let mut rel_types = match connector.list_rel_types() {
        Ok(l) => l,
        Err(e) => sink.abort("neo4j", &format!("Failed to list relationship types: {e}")),
    };
    println!(
        "Labels: {} ({}), Relationship types: {} ({})",
        labels.len(),
        labels.join(", "),
        rel_types.len(),
        rel_types.join(", ")
    );
    println!();

    // Deterministic iteration order: the first sorted label becomes the
    // type_name of multi-label nodes; the KOID is label-independent, so the
    // later label passes see the same KO and skip it.
    labels.sort();
    let filtered: Vec<&str> = labels
        .iter()
        .filter(|l| label_filter.is_none_or(|lf| l.as_str() == lf))
        .map(String::as_str)
        .collect();

    // Phase A: nodes — one KO per elementId (multi-label dedupe by KOID).
    let mut global_id_map: HashMap<String, KOID> = HashMap::new();
    let mut all_objects: Vec<KnowledgeObject> = Vec::new();
    let mut seen: HashSet<KOID> = HashSet::new();
    for label in &filtered {
        println!("Importing nodes with label '{}'...", label);
        let (objects, id_map) = match connector.import_nodes(label, tenant) {
            Ok(o) => o,
            Err(e) => {
                sink.abort("neo4j", &format!("  Error importing label {label}: {e}"));
            }
        };
        for (elem_id, koid) in id_map {
            global_id_map.entry(elem_id).or_insert(koid);
        }
        let count = objects.len();
        for ko in objects {
            if seen.insert(ko.koid) {
                all_objects.push(ko);
            }
        }
        println!("  {count} nodes read");
    }

    // Phase B: relationships — append per type. Rel properties fold into the
    // source node's properties["rel:<TYPE>"] list (RelationshipRef has no
    // props field), each map tagged with the target KOID. Rel types sorted so
    // the result is deterministic across re-imports.
    rel_types.sort();
    let mut node_rels: HashMap<KOID, Vec<RelationshipRef>> = HashMap::new();
    let mut node_rel_props: HashMap<KOID, HashMap<String, Vec<Value>>> = HashMap::new();
    for rt in &rel_types {
        println!("Importing relationships [{}]...", rt);
        let rels = match connector.import_relationships(rt, &global_id_map) {
            Ok(r) => r,
            Err(e) => {
                sink.abort(
                    "neo4j",
                    &format!("  Error importing relationships {rt}: {e}"),
                );
            }
        };
        println!("  {} relationships read", rels.len());
        for (rel, src_koid, tgt_koid, rprops) in &rels {
            node_rels.entry(*src_koid).or_default().push(rel.clone());
            let mut m = rprops.clone();
            m.insert("target".to_string(), Value::Text(tgt_koid.to_hex()));
            node_rel_props
                .entry(*src_koid)
                .or_default()
                .entry(format!("rel:{rt}"))
                .or_default()
                .push(Value::Map(m));
        }
    }

    // Phase C: commit. Any failure exits before the prune pass (CON-007).
    let mut present_by_label: HashMap<String, HashSet<KOID>> = HashMap::new();
    let mut total_nodes = 0usize;
    for mut ko in all_objects {
        let label = ko.metadata.type_name.clone();
        let koid = ko.koid;
        ko.relationships = node_rels.remove(&koid).unwrap_or_default();
        if let Some(rel_props) = node_rel_props.remove(&koid) {
            for (key, list) in rel_props {
                ko.properties.insert(key, Value::List(list));
            }
        }
        present_by_label
            .entry(label.clone())
            .or_default()
            .insert(koid);
        match sink.put(ko, "neo4j-import", "imported from Neo4j") {
            Ok(true) => total_nodes += 1,
            Ok(false) => {}
            Err(e) => {
                sink.abort(
                    "neo4j",
                    &format!("  Failed to commit node from {label}: {e}"),
                );
            }
        }
    }

    // Phase D: reconcile deletions on the all-success path only.
    // ponytail: a node that loses its PRIMARY label while keeping another is
    // tombstoned here even though the source still has the node — the KO's
    // identity is its primary label; upgrade to label-set tracking if real
    // graphs hit this.
    for (label, present) in &present_by_label {
        match sink.prune_missing(label, present) {
            Ok(0) => {}
            Ok(n) => println!("  {n} stale node(s) tombstoned in {label}"),
            Err(e) => sink.abort("neo4j", &format!("  prune {label}: {e}")),
        }
    }

    println!();
    println!(
        "Import complete. {} nodes imported into {}",
        total_nodes, target_db
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_uri_masks_credentials() {
        assert_eq!(
            redact_uri("mongodb://ancku:SECRET@127.0.0.1:1"),
            "mongodb://***@127.0.0.1:1"
        );
        // No credentials → unchanged.
        assert_eq!(redact_uri("http://localhost:7474"), "http://localhost:7474");
    }

    #[test]
    fn redact_secrets_scrubs_error_text() {
        let secrets = vec![
            "mongodb://ancku:SECRET@127.0.0.1:1".to_string(),
            "SECRET".to_string(),
        ];
        let out = redact_secrets(
            "failed: SECRET in mongodb://ancku:SECRET@127.0.0.1:1",
            &secrets,
        );
        assert!(!out.contains("SECRET"), "secret survived: {out}");
        assert!(out.contains("[REDACTED]"), "no redaction marker: {out}");
    }
}
