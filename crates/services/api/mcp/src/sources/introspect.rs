//! Production adapters: a configured source -> `ConnectorMetadata`.
//!
//! Read-only by construction (SQLite opened `READ_ONLY`, Postgres session set
//! read-only, ClickHouse `readonly=1`), bounded (`max_containers`, per-call
//! timeout), and secret-free in every error.

use super::{SourceConfig, SourceKind};
use aikoql_ingestion::ConnectorMetadata;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Catalog metadata only; schemaless kinds (Mongo, Neo4j) infer names and
    /// types from a small sample and keep no ratios.
    Schema,
    /// Schemaless kinds also keep a value profile (types, presence, nulls)
    /// over `sample_limit` records. SQL kinds read the catalog either way.
    Sample,
}

/// Passwords hiding in a connection string: `password=…` pairs and the
/// `user:password@` part of a URL, raw and percent-decoded.
fn secrets_in(conn: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let lower = conn.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("password=") {
        let start = from + i + "password=".len();
        let rest = &conn[start..];
        let value: String = match rest.strip_prefix('\'') {
            Some(q) => q.split('\'').next().unwrap_or("").to_string(),
            None => rest.split_whitespace().next().unwrap_or("").to_string(),
        };
        if !value.is_empty() {
            out.push(value);
        }
        from = start;
    }
    if let Some((_, after)) = conn.split_once("://") {
        let authority = after.split(['/', '?']).next().unwrap_or("");
        if let Some((userinfo, _)) = authority.rsplit_once('@') {
            if let Some((_, pw)) = userinfo.split_once(':') {
                out.push(pw.to_string());
                out.push(percent_decode(pw));
            }
        }
    }
    out.retain(|s| !s.is_empty());
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Bounds blocking source calls. A driver that blocks forever on a silent
/// server cannot be interrupted, so the call runs on its own thread and the
/// request gives up at the deadline; abandoned threads are counted, and when
/// too many are still stuck new calls are refused instead of piling up.
pub(crate) struct Gate {
    stuck: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max: usize,
}

/// Process-wide: at most this many abandoned calls may be stuck at once.
pub(crate) static GATE: std::sync::LazyLock<Gate> = std::sync::LazyLock::new(|| Gate::new(8));

impl Gate {
    pub(crate) fn new(max: usize) -> Gate {
        Gate {
            stuck: Default::default(),
            max,
        }
    }

    pub(crate) fn stuck(&self) -> usize {
        self.stuck.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn run<T: Send + 'static>(
        &self,
        limit: std::time::Duration,
        f: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        use std::sync::atomic::{AtomicU8, Ordering::SeqCst};
        use std::sync::{mpsc, Arc};
        if self.stuck() >= self.max {
            return Err(
                "too many calls are still waiting on unresponsive sources; try again later".into(),
            );
        }
        // 0 = running, 1 = finished, 2 = abandoned by the waiter
        let state = Arc::new(AtomicU8::new(0));
        let (tx, rx) = mpsc::channel();
        let (st, stuck) = (state.clone(), self.stuck.clone());
        std::thread::Builder::new()
            .name("source-call".into())
            .spawn(move || {
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
                if st.swap(1, SeqCst) == 2 {
                    stuck.fetch_sub(1, SeqCst); // a straggler came home
                }
                let _ = tx.send(out);
            })
            .map_err(|e| format!("could not start the source call: {e}"))?;
        let failed = || "the source call failed unexpectedly".to_string();
        match rx.recv_timeout(limit) {
            Ok(Ok(v)) => v,
            Ok(Err(_panic)) => Err(failed()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // count first, then claim: a finish racing in between must find the
                // count already there to undo
                self.stuck.fetch_add(1, SeqCst);
                if state.compare_exchange(0, 2, SeqCst, SeqCst).is_ok() {
                    Err(format!("timed out after {}s", limit.as_secs().max(1)))
                } else {
                    self.stuck.fetch_sub(1, SeqCst);
                    match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                        Ok(Ok(v)) => v,
                        _ => Err(failed()),
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(failed()),
        }
    }
}

/// Remove the connection string and any password from an error message.
pub(crate) fn scrub(msg: &str, conn: &str) -> String {
    let mut out = msg.to_string();
    if !conn.is_empty() {
        out = out.replace(conn, "***");
    }
    for secret in secrets_in(conn) {
        out = out.replace(&secret, "***");
    }
    out
}

use aikoql_ingestion::{ContainerInfo, FieldInfo, FieldProfile, Provenance, ReferenceInfo};
use std::collections::HashSet;

fn non_null(types: &[String]) -> Vec<&String> {
    types.iter().filter(|t| t.as_str() != "Null").collect()
}

fn map_mongo(name: &str, stats: &[aikoql_mongodb::FieldStats], mode: Mode) -> ContainerInfo {
    ContainerInfo {
        name: name.to_string(),
        fields: stats
            .iter()
            .map(|f| {
                let types = non_null(&f.types);
                FieldInfo {
                    name: f.name.clone(),
                    data_type: if types.is_empty() {
                        "Null".to_string()
                    } else {
                        types
                            .iter()
                            .map(|t| t.as_str())
                            .collect::<Vec<_>>()
                            .join("|")
                    },
                    is_primary_key: f.name == "_id",
                    nullable: f.name != "_id" && (f.nulls > 0 || f.present < f.samples),
                    is_unique: false,
                    provenance: Provenance::Observed,
                    profile: (mode == Mode::Sample).then(|| FieldProfile {
                        types: f.types.clone(),
                        presence_ratio: f.presence_ratio(),
                        null_ratio: f.null_ratio(),
                        samples: f.samples,
                    }),
                }
            })
            .collect(),
        ..Default::default()
    }
}

fn map_neo4j(label: &str, props: &[aikoql_neo4j::LabelProp], mode: Mode) -> ContainerInfo {
    ContainerInfo {
        name: label.to_string(),
        fields: props
            .iter()
            .map(|p| FieldInfo {
                name: p.name.clone(),
                data_type: if p.types.is_empty() {
                    "Any".to_string()
                } else {
                    p.types.join("|")
                },
                nullable: !p.mandatory,
                provenance: Provenance::Observed,
                // the schema procedure reports types and "mandatory", not ratios: samples = 0
                profile: (mode == Mode::Sample).then(|| FieldProfile {
                    types: p.types.clone(),
                    presence_ratio: if p.mandatory { 1.0 } else { 0.0 },
                    null_ratio: 0.0,
                    samples: 0,
                }),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// Attach a label's indexes and constraints. A uniqueness / node-key
/// constraint is a guarantee (unique group, unique field); an existence or
/// node-key constraint makes its properties mandatory; a plain index is not.
fn apply_neo4j_schema(
    container: &mut ContainerInfo,
    indexes: &[aikoql_neo4j::SchemaIndex],
    constraints: &[aikoql_neo4j::SchemaConstraint],
) {
    let label = container.name.clone();
    container.indexes = indexes
        .iter()
        .filter(|i| i.labels.contains(&label) && !i.properties.is_empty())
        .map(|i| aikoql_ingestion::IndexInfo {
            name: i.name.clone(),
            columns: i.properties.clone(),
            unique: i.unique,
            method: Some(i.kind.to_lowercase()),
            predicate: None,
        })
        .collect();
    for k in constraints
        .iter()
        .filter(|k| k.labels.contains(&label) && !k.properties.is_empty())
    {
        let unique = matches!(k.kind.as_str(), "UNIQUENESS" | "NODE_KEY");
        let mandatory = matches!(k.kind.as_str(), "NODE_KEY" | "NODE_PROPERTY_EXISTENCE");
        if unique && !container.unique_groups.contains(&k.properties) {
            container.unique_groups.push(k.properties.clone());
        }
        for f in container
            .fields
            .iter_mut()
            .filter(|f| k.properties.contains(&f.name))
        {
            f.is_unique |= unique && k.properties.len() == 1;
            f.nullable &= !mandatory;
        }
    }
}

fn map_clickhouse(t: &aikoql_clickhouse::TableSchema) -> ContainerInfo {
    ContainerInfo {
        name: t.name.clone(),
        fields: t
            .columns
            .iter()
            .map(|c| FieldInfo {
                name: c.name.clone(),
                data_type: c.ch_type.clone(),
                is_primary_key: c.in_primary_key,
                nullable: c.is_nullable,
                ..Default::default()
            })
            .collect(),
        row_count: Some(t.row_count_estimate),
        kind: match t.engine.as_str() {
            "MaterializedView" => Some("materialized_view".to_string()),
            "View" | "LiveView" | "WindowView" => Some("view".to_string()),
            _ => None,
        },
        properties: [
            ("engine", t.engine.clone()),
            ("sorting_key", t.sorting_key.join(", ")),
            ("primary_key", t.primary_key.join(", ")),
            ("partition_key", t.partition_key.clone()),
            ("as_select", t.as_select.chars().take(4_000).collect()),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        indexes: t
            .skip_indexes
            .iter()
            .map(|i| aikoql_ingestion::IndexInfo {
                name: i.name.clone(),
                columns: vec![i.expr.clone()],
                unique: false,
                method: Some(i.kind.clone()),
                predicate: None,
            })
            .collect(),
        ..Default::default()
    }
}

/// Views point at the tables they read: `view -derived_from-> table`.
fn clickhouse_lineage(tables: &[aikoql_clickhouse::TableSchema]) -> Vec<ReferenceInfo> {
    let known: HashSet<&str> = tables.iter().map(|t| t.name.as_str()).collect();
    let mut out: Vec<ReferenceInfo> = tables
        .iter()
        .flat_map(|source| {
            source
                .dependents
                .iter()
                .filter(|d| known.contains(d.as_str()))
                .map(|view| ReferenceInfo {
                    from_container: view.clone(),
                    from_fields: Vec::new(),
                    to_container: source.name.clone(),
                    to_fields: Vec::new(),
                    name: Some("derived_from".to_string()),
                })
        })
        .collect();
    out.sort_by(|a, b| {
        (&a.from_container, &a.to_container).cmp(&(&b.from_container, &b.to_container))
    });
    out.dedup_by(|a, b| a.from_container == b.from_container && a.to_container == b.to_container);
    out
}

fn map_postgres(t: &aikoql_postgres::TableSchema) -> ContainerInfo {
    ContainerInfo {
        name: t.name.clone(),
        fields: t
            .columns
            .iter()
            .map(|c| FieldInfo {
                name: c.name.clone(),
                data_type: c.pg_type.clone(),
                is_primary_key: c.is_primary_key,
                nullable: c.is_nullable,
                is_unique: t
                    .unique_groups
                    .iter()
                    .any(|g| g.len() == 1 && g[0] == c.name),
                ..Default::default()
            })
            .collect(),
        row_count: u64::try_from(t.row_count_estimate).ok(),
        schema: (!t.schema.is_empty()).then(|| t.schema.clone()),
        unique_groups: t.unique_groups.clone(),
        indexes: t
            .indexes
            .iter()
            .map(|i| aikoql_ingestion::IndexInfo {
                name: i.name.clone(),
                columns: i.columns.clone(),
                unique: i.unique,
                method: Some(i.method.clone()),
                predicate: i.predicate.clone(),
            })
            .collect(),
        checks: t
            .checks
            .iter()
            .map(|k| aikoql_ingestion::CheckInfo {
                name: k.name.clone(),
                expression: k.expression.clone(),
            })
            .collect(),
        ..Default::default()
    }
}

fn map_pg_fks(
    fks: &[aikoql_postgres::ForeignKeyInfo],
    included: &HashSet<String>,
) -> Vec<ReferenceInfo> {
    fks.iter()
        .filter(|f| included.contains(&f.child_table) && included.contains(&f.parent_table))
        .map(|f| ReferenceInfo {
            from_container: f.child_table.clone(),
            from_fields: if f.child_columns.is_empty() {
                vec![f.child_column.clone()]
            } else {
                f.child_columns.clone()
            },
            to_container: f.parent_table.clone(),
            to_fields: if f.parent_columns.is_empty() {
                vec![f.parent_column.clone()]
            } else {
                f.parent_columns.clone()
            },
            name: Some(f.constraint_name.clone()),
        })
        .collect()
}

pub(crate) fn introspect_source(
    src: &SourceConfig,
    mode: Mode,
) -> Result<ConnectorMetadata, String> {
    introspect_with(src, &src.connection()?, mode)
}

/// Introspect with an already-resolved connection string (what tests use).
pub(crate) fn introspect_with(
    src: &SourceConfig,
    conn: &str,
    mode: Mode,
) -> Result<ConnectorMetadata, String> {
    let ms = src.timeout_secs.saturating_mul(1000);
    let max = src.max_containers as usize;
    let sample = match mode {
        Mode::Schema => src.sample_limit.min(20),
        Mode::Sample => src.sample_limit,
    };
    let (kind, conn_owned, database) = (
        src.kind,
        conn.to_string(),
        src.database.clone().unwrap_or_default(),
    );
    // the connector ends its own work just inside the gate's limit, so the thread returns
    let own = std::time::Duration::from_millis(ms + 1_000);
    let result = GATE.run(
        std::time::Duration::from_millis(ms + 1_500),
        move || match kind {
            SourceKind::Sqlite => sqlite_meta(&conn_owned, ms, max, own),
            SourceKind::Postgres => postgres_meta(&conn_owned, ms, max),
            SourceKind::Mongodb => mongo_meta(&conn_owned, &database, ms, max, sample, mode, own),
            SourceKind::Neo4j => neo4j_meta(&conn_owned, ms, max, sample, mode),
            SourceKind::Clickhouse => clickhouse_meta(&conn_owned, &database, ms, max),
        },
    );
    let (containers, references, truncated, version) =
        result.map_err(|e| format!("source {}: {}", src.name, scrub(&e, conn)))?;
    Ok(ConnectorMetadata {
        connector_type: src.kind.as_str().to_string(),
        label: src.name.clone(),
        containers,
        references,
        version,
        truncated,
    })
}

type Parts = (Vec<ContainerInfo>, Vec<ReferenceInfo>, bool, Option<String>);

fn cap(mut names: Vec<String>, max: usize) -> (Vec<String>, bool) {
    names.sort();
    names.dedup();
    let truncated = names.len() > max;
    names.truncate(max);
    (names, truncated)
}

fn sqlite_meta(path: &str, ms: u64, max: usize, own: std::time::Duration) -> Result<Parts, String> {
    let started = std::time::Instant::now();
    let c = aikoql_sqlite::SqliteConnector::open_read_only(path, ms)?
        .with_deadline(own.saturating_sub(started.elapsed()));
    let (names, truncated) = cap(c.list_tables()?, max);
    let included: HashSet<String> = names.iter().cloned().collect();
    let mut containers = Vec::with_capacity(names.len());
    for n in &names {
        let t = c.introspect_table_with(n, false)?;
        containers.push(ContainerInfo {
            name: t.name.clone(),
            fields: t
                .columns
                .iter()
                .map(|col| FieldInfo {
                    name: col.name.clone(),
                    data_type: col.sqlite_type.clone(),
                    is_primary_key: col.is_primary_key,
                    nullable: col.is_nullable && !col.is_primary_key,
                    is_unique: t
                        .unique_groups
                        .iter()
                        .any(|g| g.len() == 1 && g[0] == col.name),
                    ..Default::default()
                })
                .collect(),
            row_count: None,
            schema: None,
            unique_groups: t.unique_groups,
            ..Default::default()
        });
    }
    let references = c
        .introspect_foreign_keys()?
        .into_iter()
        .filter(|f| included.contains(&f.child_table) && included.contains(&f.parent_table))
        .map(|f| ReferenceInfo {
            from_container: f.child_table,
            from_fields: f.child_columns,
            to_container: f.parent_table,
            to_fields: f.parent_columns,
            name: Some(f.constraint_name),
        })
        .collect();
    Ok((containers, references, truncated, None))
}

fn postgres_meta(conn: &str, ms: u64, max: usize) -> Result<Parts, String> {
    let mut c = aikoql_postgres::PostgresConnector::connect_with_timeout(conn, Some(ms))?;
    c.read_only_session()?;
    if !c.is_read_only()? {
        return Err("could not make the session read-only".into());
    }
    let (names, truncated) = cap(c.list_tables()?, max);
    let included: HashSet<String> = names.iter().cloned().collect();
    let mut containers = Vec::with_capacity(names.len());
    for n in &names {
        containers.push(map_postgres(&c.introspect_table_with(n, false)?));
    }
    let references = map_pg_fks(&c.introspect_foreign_keys()?, &included);
    Ok((containers, references, truncated, None))
}

fn mongo_meta(
    uri: &str,
    database: &str,
    ms: u64,
    max: usize,
    sample: u32,
    mode: Mode,
    own: std::time::Duration,
) -> Result<Parts, String> {
    let started = std::time::Instant::now();
    let c = aikoql_mongodb::MongoConnector::connect_with_timeout(uri, database, Some(ms))?
        .with_deadline(own.saturating_sub(started.elapsed()));
    let names: Vec<String> = c
        .list_collections()?
        .into_iter()
        .filter(|n| !n.starts_with("system."))
        .collect();
    let (names, truncated) = cap(names, max);
    let mut containers = Vec::with_capacity(names.len());
    for n in &names {
        containers.push(map_mongo(
            n,
            &c.introspect_collection_sampled(n, sample)?,
            mode,
        ));
    }
    Ok((containers, Vec::new(), truncated, None))
}

fn neo4j_meta(conn: &str, ms: u64, max: usize, sample: u32, mode: Mode) -> Result<Parts, String> {
    let (_, rest) = conn.split_once("://").ok_or("invalid Neo4j URL")?;
    let scheme = &conn[..conn.len() - rest.len()];
    let (userinfo, host) = rest
        .rsplit_once('@')
        .ok_or("Neo4j URL needs user:password@host")?;
    let (user, pass) = userinfo.split_once(':').unwrap_or((userinfo, ""));
    let c = aikoql_neo4j::Neo4jConnector::connect_with_timeout(
        &format!("{scheme}{host}"),
        &percent_decode(user),
        &percent_decode(pass),
        Some(ms),
    )?;
    let (labels, truncated) = cap(c.list_labels()?, max);
    let included: HashSet<String> = labels.iter().cloned().collect();
    let mut containers = Vec::with_capacity(labels.len());
    for l in &labels {
        containers.push(map_neo4j(l, &c.introspect_label_schema(l, sample)?, mode));
    }
    // best effort: SHOW needs Neo4j 4.3+ and the privilege to run it, and a
    // source that worked before must not start failing for lacking either
    let indexes = c.introspect_indexes().unwrap_or_default();
    let constraints = c.introspect_constraints().unwrap_or_default();
    for container in &mut containers {
        apply_neo4j_schema(container, &indexes, &constraints);
    }
    let references = c
        .introspect_rel_patterns(1_000)?
        .into_iter()
        .filter(|(_, from, to)| included.contains(from) && included.contains(to))
        .map(|(rel, from, to)| ReferenceInfo {
            from_container: from,
            from_fields: Vec::new(),
            to_container: to,
            to_fields: Vec::new(),
            name: Some(rel),
        })
        .collect();
    Ok((containers, references, truncated, None))
}

fn clickhouse_meta(url: &str, database: &str, ms: u64, max: usize) -> Result<Parts, String> {
    let c = aikoql_clickhouse::ClickHouseConnector::connect_with_timeout(url, database, Some(ms))?;
    let mut tables = c.introspect_all(max)?;
    let truncated = tables.len() > max;
    tables.truncate(max);
    let version = Some(c.version().to_string()).filter(|v| !v.is_empty());
    Ok((
        tables.iter().map(map_clickhouse).collect(),
        clickhouse_lineage(&tables),
        truncated,
        version,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    fn src(name: &str, kind: SourceKind) -> SourceConfig {
        SourceConfig {
            name: name.into(),
            kind,
            url_env: "PATH".into(),
            database: None,
            tenant: None,
            sample_limit: 100,
            max_containers: 500,
            timeout_secs: 3,
            import_max_rows: 100_000,
        }
    }

    fn tmp_db(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aikoql-introspect-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("db.sqlite")
    }

    fn make(path: &Path, sql: &str) {
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute_batch(sql).unwrap();
    }

    fn sqlite(path: &Path, mode: Mode) -> Result<ConnectorMetadata, String> {
        introspect_with(
            &src("crm", SourceKind::Sqlite),
            path.to_str().unwrap(),
            mode,
        )
    }

    #[test]
    fn sqlite_tables_keys_references_and_uniques_are_reported() {
        let p = tmp_db("basic");
        make(
            &p,
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, note TEXT);
             CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), total REAL);
             INSERT INTO customers VALUES (1, 'a@x', NULL);",
        );
        let m = sqlite(&p, Mode::Schema).unwrap();
        assert_eq!(
            (m.connector_type.as_str(), m.label.as_str()),
            ("sqlite", "crm")
        );
        assert!(!m.truncated);
        let names: Vec<&str> = m.containers.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["customers", "orders"]);
        let cust = &m.containers[0];
        let f = |n: &str| cust.fields.iter().find(|f| f.name == n).unwrap();
        assert!(f("id").is_primary_key && !f("id").nullable);
        assert!(f("email").is_unique && !f("email").nullable && f("note").nullable);
        assert_eq!(cust.unique_groups, vec![vec!["email".to_string()]]);
        assert!(cust
            .fields
            .iter()
            .all(|f| f.provenance == aikoql_ingestion::Provenance::Declared));
        assert_eq!(
            cust.row_count, None,
            "schema discovery does not scan tables"
        );
        assert_eq!(m.references.len(), 1);
        let r = &m.references[0];
        assert_eq!(
            (r.from_container.as_str(), r.to_container.as_str()),
            ("orders", "customers")
        );
        assert_eq!(
            (r.from_fields.as_slice(), r.to_fields.as_slice()),
            (&["customer_id".to_string()][..], &["id".to_string()][..])
        );
    }

    #[test]
    fn max_containers_truncates_deterministically_and_flags_it() {
        let p = tmp_db("trunc");
        make(
            &p,
            "CREATE TABLE c (x); CREATE TABLE a (x); CREATE TABLE b (x);",
        );
        let mut s = src("crm", SourceKind::Sqlite);
        s.max_containers = 2;
        let m = introspect_with(&s, p.to_str().unwrap(), Mode::Schema).unwrap();
        assert!(m.truncated);
        assert_eq!(
            m.containers
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        s.max_containers = 3;
        assert!(
            !introspect_with(&s, p.to_str().unwrap(), Mode::Schema)
                .unwrap()
                .truncated
        );
    }

    #[test]
    fn unusable_sources_fail_short_and_leak_neither_path_nor_secret() {
        let secret_dir = format!("very-secret-dir-{}", std::process::id());
        let missing = std::env::temp_dir().join(&secret_dir).join("nope.sqlite");
        let e = sqlite(&missing, Mode::Schema).unwrap_err();
        assert!(
            !e.contains(&secret_dir) && e.starts_with("source crm:"),
            "{e}"
        );
        assert!(
            !missing.exists() && !missing.parent().unwrap().exists(),
            "nothing created"
        );

        let garbage = tmp_db("garbage");
        std::fs::write(&garbage, b"not a database ".repeat(100)).unwrap();
        let e = sqlite(&garbage, Mode::Schema).unwrap_err();
        assert!(e.starts_with("source crm:") && e.len() < 200, "{e}");

        let dir = tmp_db("dir").parent().unwrap().to_path_buf();
        assert!(
            sqlite(&dir, Mode::Schema).is_err(),
            "a directory is not a database"
        );
    }

    #[test]
    fn discovery_leaves_the_source_file_byte_identical() {
        let p = tmp_db("ro");
        make(&p, "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT); INSERT INTO t VALUES (1,'x'),(2,'y');");
        let before = std::fs::read(&p).unwrap();
        for mode in [Mode::Schema, Mode::Sample] {
            sqlite(&p, mode).unwrap();
        }
        assert_eq!(
            std::fs::read(&p).unwrap(),
            before,
            "introspection must not modify the source"
        );
        assert!(
            !p.with_extension("sqlite-journal").exists()
                && !p.with_extension("sqlite-wal").exists()
        );
    }

    #[test]
    fn five_thousand_tables_truncate_fast() {
        let p = tmp_db("many");
        let mut sql = String::from("BEGIN;");
        for i in 0..5_000 {
            sql.push_str(&format!(
                "CREATE TABLE t{i:05} (id INTEGER PRIMARY KEY, v TEXT);"
            ));
        }
        sql.push_str("COMMIT;");
        make(&p, &sql);
        let started = Instant::now();
        let m = sqlite(&p, Mode::Schema).unwrap();
        assert!(
            m.truncated && m.containers.len() == 500,
            "{} containers",
            m.containers.len()
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_2000_column_table_is_fully_reported() {
        let p = tmp_db("wide");
        let cols: Vec<String> = (0..2_000).map(|i| format!("c{i} INTEGER")).collect();
        make(&p, &format!("CREATE TABLE wide ({});", cols.join(", ")));
        let m = sqlite(&p, Mode::Schema).unwrap();
        assert_eq!(m.containers[0].fields.len(), 2_000);
    }

    #[test]
    fn hostile_names_round_trip_exactly() {
        let p = tmp_db("hostile");
        let long = "L".repeat(300);
        make(
            &p,
            &format!(
                "CREATE TABLE \"a\"\"b\" (\"x y\" INTEGER PRIMARY KEY, \"it's\" TEXT);
                 CREATE TABLE \"order\" (id INTEGER);
                 CREATE TABLE \"{long}\" (id INTEGER);
                 CREATE TABLE \"ünï\u{202e}💥\" (id INTEGER);"
            ),
        );
        let m = sqlite(&p, Mode::Schema).unwrap();
        let names: Vec<&str> = m.containers.iter().map(|c| c.name.as_str()).collect();
        for want in ["a\"b", "order", long.as_str(), "ünï\u{202e}💥"] {
            assert!(names.contains(&want), "{want:?} missing from {names:?}");
        }
        let ab = m.containers.iter().find(|c| c.name == "a\"b").unwrap();
        assert!(
            ab.fields.iter().any(|f| f.name == "x y") && ab.fields.iter().any(|f| f.name == "it's")
        );
    }

    #[test]
    fn a_locked_source_fails_within_the_timeout_not_forever() {
        let p = tmp_db("locked");
        make(&p, "CREATE TABLE t (id INTEGER PRIMARY KEY);");
        let writer = rusqlite::Connection::open(&p).unwrap();
        writer
            .execute_batch("BEGIN EXCLUSIVE; INSERT INTO t VALUES (1);")
            .unwrap();
        let mut s = src("crm", SourceKind::Sqlite);
        s.timeout_secs = 1;
        let started = Instant::now();
        let e = introspect_with(&s, p.to_str().unwrap(), Mode::Schema).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        assert!(e.starts_with("source crm:"), "{e}");
        writer.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn scrub_removes_the_connection_string_and_every_password_form() {
        let cases = [
            (
                "host=db user=app password=s3cr3t dbname=x",
                "failed: host=db user=app password=s3cr3t dbname=x refused",
            ),
            (
                "host=db user=app password=s3cr3t dbname=x",
                "bad value s3cr3t in config",
            ),
            (
                "http://neo4j:pw-123@h:7474",
                "request to http://neo4j:pw-123@h:7474 failed: pw-123",
            ),
            (
                "mongodb://u:p%40ss@h/db",
                "error for mongodb://u:p%40ss@h/db (p@ss)",
            ),
        ];
        for (conn, msg) in cases {
            let out = scrub(msg, conn);
            for secret in ["s3cr3t", "pw-123", "p%40ss", "p@ss"] {
                assert!(!out.contains(secret), "{secret:?} survived in {out:?}");
            }
            assert!(!out.contains(conn), "{out}");
        }
        assert_eq!(scrub("plain error", "host=h"), "plain error");
    }

    #[test]
    fn sources_that_are_not_wired_report_a_clear_error() {
        // a kind whose server is absent must fail like any unreachable source
        let mut s = src("pg", SourceKind::Postgres);
        s.timeout_secs = 1;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let started = Instant::now();
        let e = introspect_with(
            &s,
            &format!("host=127.0.0.1 port={port} user=u password=topsecret dbname=d"),
            Mode::Schema,
        )
        .unwrap_err();
        assert!(
            !e.contains("topsecret") && e.starts_with("source pg:"),
            "{e}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "{:?}",
            started.elapsed()
        );
    }

    // ---- pure mappers: provider structs -> ConnectorMetadata pieces ----

    #[test]
    fn mongo_fields_are_observed_and_keep_a_profile_only_in_sample_mode() {
        let stats = vec![
            aikoql_mongodb::FieldStats {
                name: "_id".into(),
                types: vec!["ObjectId".into()],
                present: 10,
                nulls: 0,
                samples: 10,
            },
            aikoql_mongodb::FieldStats {
                name: "age".into(),
                types: vec!["Int32".into(), "Null".into()],
                present: 9,
                nulls: 2,
                samples: 10,
            },
            aikoql_mongodb::FieldStats {
                name: "addr.city".into(),
                types: vec!["String".into()],
                present: 5,
                nulls: 0,
                samples: 10,
            },
        ];
        let sample = map_mongo("people", &stats, Mode::Sample);
        let f =
            |c: &ContainerInfo, n: &str| c.fields.iter().find(|f| f.name == n).cloned().unwrap();
        assert!(f(&sample, "_id").is_primary_key && !f(&sample, "_id").nullable);
        assert!(f(&sample, "age").nullable, "a null was seen");
        assert!(
            f(&sample, "addr.city").nullable,
            "absent from half the documents"
        );
        assert!(sample
            .fields
            .iter()
            .all(|f| f.provenance == aikoql_ingestion::Provenance::Observed));
        let p = f(&sample, "age").profile.unwrap();
        assert_eq!(p.types, ["Int32", "Null"]);
        assert!((p.presence_ratio - 0.9).abs() < 1e-9 && p.samples == 10);
        assert_eq!(
            f(&sample, "age").data_type,
            "Int32",
            "declared type shows the non-null observed type"
        );
        let schema = map_mongo("people", &stats, Mode::Schema);
        assert!(
            schema.fields.iter().all(|f| f.profile.is_none()),
            "Schema mode keeps no ratios"
        );
        assert!(map_mongo("empty", &[], Mode::Sample).fields.is_empty());
    }

    #[test]
    fn neo4j_properties_map_types_and_mandatory() {
        let props = vec![
            aikoql_neo4j::LabelProp {
                name: "name".into(),
                types: vec!["String".into()],
                mandatory: true,
            },
            aikoql_neo4j::LabelProp {
                name: "age".into(),
                types: vec!["Long".into(), "String".into()],
                mandatory: false,
            },
        ];
        let c = map_neo4j("Person", &props, Mode::Sample);
        assert_eq!(c.name, "Person");
        let name = c.fields.iter().find(|f| f.name == "name").unwrap();
        let age = c.fields.iter().find(|f| f.name == "age").unwrap();
        assert!(!name.nullable && age.nullable);
        assert_eq!(age.profile.as_ref().unwrap().types, ["Long", "String"]);
        assert!(map_neo4j("Person", &props, Mode::Schema)
            .fields
            .iter()
            .all(|f| f.profile.is_none()));
    }

    fn ch_table(name: &str, engine: &str, dependents: &[&str]) -> aikoql_clickhouse::TableSchema {
        aikoql_clickhouse::TableSchema {
            name: name.into(),
            engine: engine.into(),
            columns: Vec::new(),
            sorting_key: vec!["a".into(), "b".into()],
            primary_key: vec!["a".into()],
            partition_key: String::new(),
            row_count_estimate: 0,
            as_select: if engine.ends_with("View") {
                "SELECT 1".into()
            } else {
                String::new()
            },
            dependents: dependents.iter().map(|d| d.to_string()).collect(),
            skip_indexes: Vec::new(),
        }
    }

    #[test]
    fn clickhouse_views_storage_properties_and_lineage_map() {
        let events = ch_table("events", "MergeTree", &["daily", "recent", "gone"]);
        let daily = ch_table("daily", "MaterializedView", &[]);
        let recent = ch_table("recent", "View", &[]);
        let c = map_clickhouse(&events);
        assert_eq!(c.kind, None);
        assert_eq!(c.properties["engine"], "MergeTree");
        assert_eq!(c.properties["sorting_key"], "a, b");
        assert!(
            !c.properties.contains_key("partition_key"),
            "empty facts are not recorded"
        );
        assert!(!c.properties.contains_key("as_select"));
        assert_eq!(
            map_clickhouse(&daily).kind.as_deref(),
            Some("materialized_view")
        );
        assert_eq!(map_clickhouse(&recent).kind.as_deref(), Some("view"));
        assert_eq!(map_clickhouse(&daily).properties["as_select"], "SELECT 1");
        // lineage: a view points at the table it reads; a dependent outside the list is dropped
        let refs = clickhouse_lineage(&[events, daily, recent]);
        let pairs: Vec<(&str, &str)> = refs
            .iter()
            .map(|r| (r.from_container.as_str(), r.to_container.as_str()))
            .collect();
        assert_eq!(pairs, [("daily", "events"), ("recent", "events")]);
        assert!(refs
            .iter()
            .all(|r| r.name.as_deref() == Some("derived_from")));
        // a very long defining query is bounded
        let mut long = ch_table("v", "View", &[]);
        long.as_select = "x".repeat(50_000);
        assert_eq!(map_clickhouse(&long).properties["as_select"].len(), 4_000);
    }

    #[test]
    fn clickhouse_tables_map_keys_and_nullability() {
        let t = aikoql_clickhouse::TableSchema {
            name: "events".into(),
            engine: "MergeTree".into(),
            columns: vec![
                aikoql_clickhouse::ColumnInfo {
                    name: "ts".into(),
                    ch_type: "DateTime".into(),
                    is_nullable: false,
                    is_low_cardinality: false,
                    in_primary_key: true,
                },
                aikoql_clickhouse::ColumnInfo {
                    name: "note".into(),
                    ch_type: "String".into(),
                    is_nullable: true,
                    is_low_cardinality: false,
                    in_primary_key: false,
                },
            ],
            sorting_key: vec!["ts".into()],
            primary_key: vec!["ts".into()],
            partition_key: String::new(),
            row_count_estimate: 42,
            as_select: String::new(),
            dependents: Vec::new(),
            skip_indexes: Vec::new(),
        };
        let c = map_clickhouse(&t);
        assert_eq!(
            (c.name.as_str(), c.row_count),
            ("events", Some(42)),
            "a catalog estimate is an approximate count"
        );
        let ts = c.fields.iter().find(|f| f.name == "ts").unwrap();
        assert!(ts.is_primary_key && !ts.nullable && ts.data_type == "DateTime");
        assert!(c.fields.iter().find(|f| f.name == "note").unwrap().nullable);
    }

    #[test]
    fn neo4j_constraints_make_properties_unique_or_mandatory_and_other_labels_are_ignored() {
        let field = |n: &str| FieldInfo {
            name: n.into(),
            nullable: true,
            ..Default::default()
        };
        let mut c = ContainerInfo {
            name: "Person".into(),
            fields: vec![
                field("email"),
                field("ssn"),
                field("region"),
                field("tag"),
                field("other"),
            ],
            ..Default::default()
        };
        let k =
            |name: &str, kind: &str, label: &str, props: &[&str]| aikoql_neo4j::SchemaConstraint {
                name: name.into(),
                kind: kind.into(),
                labels: vec![label.into()],
                properties: props.iter().map(|p| p.to_string()).collect(),
            };
        let idx =
            |name: &str, label: &str, props: &[&str], unique: bool| aikoql_neo4j::SchemaIndex {
                name: name.into(),
                kind: "RANGE".into(),
                labels: vec![label.into()],
                properties: props.iter().map(|p| p.to_string()).collect(),
                unique,
            };
        apply_neo4j_schema(
            &mut c,
            &[
                idx("p_email", "Person", &["email"], true),
                idx("p_region_tag", "Person", &["region", "tag"], false),
                idx("elsewhere", "Company", &["other"], false),
                idx("token_like", "Person", &[], false),
            ],
            &[
                k("u_email", "UNIQUENESS", "Person", &["email"]),
                k("key_pair", "NODE_KEY", "Person", &["region", "tag"]),
                k("ex_ssn", "NODE_PROPERTY_EXISTENCE", "Person", &["ssn"]),
                k("u_other", "UNIQUENESS", "Company", &["other"]),
            ],
        );
        let f = |n: &str| c.fields.iter().find(|f| f.name == n).unwrap();
        assert!(
            f("email").is_unique && f("email").nullable,
            "unique, not mandatory"
        );
        assert!(
            !f("ssn").nullable && !f("ssn").is_unique,
            "existence makes it mandatory only"
        );
        assert!(
            !f("region").nullable && !f("tag").nullable,
            "a node key is mandatory"
        );
        assert!(
            !f("region").is_unique,
            "a composite key member is not unique alone"
        );
        assert!(
            f("other").nullable && !f("other").is_unique,
            "another label's constraint"
        );
        assert_eq!(
            c.unique_groups,
            [
                vec!["email".to_string()],
                vec!["region".to_string(), "tag".to_string()]
            ]
        );
        let names: Vec<&str> = c.indexes.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            ["p_email", "p_region_tag"],
            "other labels and property-less indexes are dropped"
        );
        assert_eq!(c.indexes[0].method.as_deref(), Some("range"));
        assert!(c.indexes[0].unique && !c.indexes[1].unique);
    }

    #[test]
    fn postgres_tables_map_schema_uniques_and_composite_fks() {
        let t = aikoql_postgres::TableSchema {
            name: "child".into(),
            schema: "hr".into(),
            columns: vec![
                aikoql_postgres::ColumnInfo {
                    name: "id".into(),
                    pg_type: "integer".into(),
                    is_nullable: false,
                    is_primary_key: true,
                },
                aikoql_postgres::ColumnInfo {
                    name: "code".into(),
                    pg_type: "text".into(),
                    is_nullable: true,
                    is_primary_key: false,
                },
                aikoql_postgres::ColumnInfo {
                    name: "x".into(),
                    pg_type: "integer".into(),
                    is_nullable: true,
                    is_primary_key: false,
                },
            ],
            primary_keys: vec!["id".into()],
            row_count_estimate: -1,
            unique_groups: vec![vec!["code".into()], vec!["x".into(), "id".into()]],
            indexes: vec![aikoql_postgres::IndexDef {
                name: "child_code_part".into(),
                columns: vec!["code".into()],
                unique: true,
                method: "btree".into(),
                predicate: Some("(code IS NOT NULL)".into()),
                plain: true,
            }],
            checks: vec![aikoql_postgres::CheckDef {
                name: "x_pos".into(),
                expression: "x > 0".into(),
            }],
        };
        let c = map_postgres(&t);
        assert_eq!(
            c.indexes,
            [aikoql_ingestion::IndexInfo {
                name: "child_code_part".into(),
                columns: vec!["code".into()],
                unique: true,
                method: Some("btree".into()),
                predicate: Some("(code IS NOT NULL)".into()),
            }]
        );
        assert_eq!(
            c.checks,
            [aikoql_ingestion::CheckInfo {
                name: "x_pos".into(),
                expression: "x > 0".into()
            }]
        );
        assert_eq!(c.schema.as_deref(), Some("hr"));
        assert_eq!(
            c.row_count, None,
            "-1 (never analyzed) is unknown, not a count"
        );
        assert!(
            c.fields
                .iter()
                .find(|f| f.name == "code")
                .unwrap()
                .is_unique,
            "single-column unique"
        );
        assert!(
            !c.fields.iter().find(|f| f.name == "x").unwrap().is_unique,
            "a member of a composite group is not unique alone"
        );
        assert_eq!(c.unique_groups.len(), 2);
        let fks = vec![
            aikoql_postgres::ForeignKeyInfo {
                constraint_name: "child_parent_fk".into(),
                child_table: "child".into(),
                child_column: "pa".into(),
                parent_table: "parent".into(),
                parent_column: "a".into(),
                child_columns: vec!["pa".into(), "pb".into()],
                parent_columns: vec!["a".into(), "b".into()],
            },
            aikoql_postgres::ForeignKeyInfo {
                constraint_name: "to_elsewhere".into(),
                child_table: "child".into(),
                child_column: "z".into(),
                parent_table: "not_included".into(),
                parent_column: "id".into(),
                child_columns: vec!["z".into()],
                parent_columns: vec!["id".into()],
            },
        ];
        let included: HashSet<String> = ["child", "parent"].iter().map(|s| s.to_string()).collect();
        let refs = map_pg_fks(&fks, &included);
        assert_eq!(
            refs.len(),
            1,
            "a reference outside the included containers is dropped"
        );
        assert_eq!(refs[0].from_fields, ["pa", "pb"]);
        assert_eq!(refs[0].to_fields, ["a", "b"]);
        assert_eq!(refs[0].name.as_deref(), Some("child_parent_fk"));
    }

    // ---- live (env-gated): seeded here, introspected through the production path ----

    fn live(var: &str) -> Option<String> {
        std::env::var(var).ok().filter(|v| !v.trim().is_empty())
    }

    #[test]
    fn live_postgres_reports_composite_keys_uniques_schema_and_is_read_only() {
        let Some(dsn) = live("AIKOQL_TEST_PG_DSN") else {
            return;
        };
        let admin = dsn.clone();
        let mut c = postgres::Client::connect(&admin, postgres::NoTls).unwrap();
        c.batch_execute("DROP DATABASE IF EXISTS src_t7_pg")
            .unwrap();
        c.batch_execute("CREATE DATABASE src_t7_pg").unwrap();
        let private = admin.replace("dbname=knowledge", "dbname=src_t7_pg");
        let mut p = postgres::Client::connect(&private, postgres::NoTls).unwrap();
        p.batch_execute(
            "CREATE TABLE parent (a INT, b INT, PRIMARY KEY (a, b));
             CREATE TABLE child (id INT PRIMARY KEY, pa INT, pb INT, code TEXT UNIQUE,
               FOREIGN KEY (pa, pb) REFERENCES parent (a, b));
             CREATE SCHEMA hr; CREATE TABLE hr.staff (id INT PRIMARY KEY);
             INSERT INTO parent VALUES (1,1);",
        )
        .unwrap();
        let m = introspect_with(&src("pg", SourceKind::Postgres), &private, Mode::Schema).unwrap();
        let child = m.containers.iter().find(|c| c.name == "child").unwrap();
        assert_eq!(child.schema.as_deref(), Some("public"));
        assert!(
            child
                .fields
                .iter()
                .find(|f| f.name == "code")
                .unwrap()
                .is_unique
        );
        assert_eq!(
            m.containers
                .iter()
                .find(|c| c.name == "staff")
                .unwrap()
                .schema
                .as_deref(),
            Some("hr")
        );
        let fk = m
            .references
            .iter()
            .find(|r| r.from_container == "child")
            .unwrap();
        assert_eq!(fk.from_fields, ["pa", "pb"]);
        // the data was not touched
        let rows = p.query("SELECT count(*) FROM parent", &[]).unwrap();
        assert_eq!(rows[0].get::<_, i64>(0), 1);
    }

    #[test]
    fn live_clickhouse_reports_keys_and_wrapped_types() {
        let (Some(url), Some(db)) = (
            live("AIKOQL_TEST_CLICKHOUSE_URL"),
            live("AIKOQL_TEST_CLICKHOUSE_DB"),
        ) else {
            return;
        };
        let (scheme, rest) = url.split_once("://").unwrap();
        let (ui, host) = rest.rsplit_once('@').unwrap();
        let (u, pw) = ui.split_once(':').unwrap();
        for sql in [
            "DROP TABLE IF EXISTS t7_events",
            "CREATE TABLE t7_events (ts DateTime, note Nullable(String)) ENGINE = MergeTree ORDER BY ts",
        ] {
            ureq::post(&format!("{scheme}://{host}/")).query("database", &db).set("X-ClickHouse-User", u).set("X-ClickHouse-Key", pw).send_string(sql).unwrap();
        }
        let mut s = src("ch", SourceKind::Clickhouse);
        s.database = Some(db);
        let m = introspect_with(&s, &url, Mode::Schema).unwrap();
        let ev = m.containers.iter().find(|c| c.name == "t7_events").unwrap();
        assert!(
            ev.fields
                .iter()
                .find(|f| f.name == "note")
                .unwrap()
                .nullable
        );
        assert!(
            ev.fields
                .iter()
                .find(|f| f.name == "ts")
                .unwrap()
                .is_primary_key
        );
    }

    #[test]
    fn live_neo4j_reports_label_properties_and_relationship_patterns() {
        let (Some(uri), Some(user), Some(pass)) = (
            live("AIKOQL_TEST_NEO4J_URI"),
            live("AIKOQL_TEST_NEO4J_USER"),
            live("AIKOQL_TEST_NEO4J_PASSWORD"),
        ) else {
            return;
        };
        let auth = format!(
            "Basic {}",
            aikoql_neo4j::base64_encode(&format!("{user}:{pass}"))
        );
        let body = serde_json::json!({"statements": [
            {"statement": "MATCH (n:T7Person) DETACH DELETE n"},
            {"statement": "CREATE (:T7Person {name:'a'})-[:T7_KNOWS]->(:T7Person {name:'b', age:3})"},
        ]});
        ureq::post(&format!("{}/db/neo4j/tx/commit", uri.trim_end_matches('/')))
            .set("Authorization", &auth)
            .send_json(&body)
            .unwrap();
        let conn = uri.replacen("://", &format!("://{user}:{pass}@"), 1);
        let m = introspect_with(&src("g", SourceKind::Neo4j), &conn, Mode::Sample).unwrap();
        let person = m.containers.iter().find(|c| c.name == "T7Person").unwrap();
        assert!(
            !person
                .fields
                .iter()
                .find(|f| f.name == "name")
                .unwrap()
                .nullable
        );
        assert!(
            person
                .fields
                .iter()
                .find(|f| f.name == "age")
                .unwrap()
                .nullable
        );
        assert!(m.references.iter().any(|r| r.from_container == "T7Person"
            && r.to_container == "T7Person"
            && r.name.as_deref() == Some("T7_KNOWS")));
    }

    // ---- Gate: a call that never returns must not hang the request ----

    #[test]
    fn gate_returns_a_fast_result_and_a_fast_error_untouched() {
        let g = Gate::new(4);
        assert_eq!(g.run(Duration::from_secs(2), || Ok(41 + 1)), Ok(42));
        assert_eq!(
            g.run(Duration::from_secs(2), || -> Result<u8, String> {
                Err("boom".into())
            }),
            Err("boom".to_string())
        );
        assert_eq!(g.stuck(), 0);
    }

    #[test]
    fn gate_gives_up_on_a_stuck_call_counts_it_and_uncounts_it_when_it_finally_returns() {
        let g = std::sync::Arc::new(Gate::new(4));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let started = Instant::now();
        let r = g.run(Duration::from_millis(200), move || {
            let _ = release_rx.recv(); // blocks until released: a silent server
            Ok(())
        });
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(r.unwrap_err().contains("timed out"));
        assert_eq!(g.stuck(), 1);
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while g.stuck() != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(g.stuck(), 0, "a finished straggler is no longer counted");
    }

    #[test]
    fn gate_refuses_new_calls_while_too_many_are_stuck() {
        let g = Gate::new(2);
        let mut releases = Vec::new();
        for _ in 0..2 {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            releases.push(tx);
            assert!(g
                .run(Duration::from_millis(100), move || {
                    let _ = rx.recv();
                    Ok(())
                })
                .is_err());
        }
        assert_eq!(g.stuck(), 2);
        let started = Instant::now();
        let e = g.run(Duration::from_secs(5), || Ok(())).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "refusal must be immediate"
        );
        assert!(e.contains("unresponsive"), "{e}");
        for tx in releases {
            tx.send(()).unwrap();
        }
    }

    #[test]
    fn gate_survives_a_panicking_call() {
        let g = Gate::new(4);
        // generous on purpose: with RUST_BACKTRACE=1 (CI) Windows spent ~48s resolving the
        // panic's backtrace, and a short limit would report "timed out" instead of the panic
        let e = g
            .run(Duration::from_secs(600), || -> Result<u8, String> {
                panic!("driver bug")
            })
            .unwrap_err();
        assert!(e.contains("failed"), "{e}");
        assert_eq!(g.stuck(), 0);
    }

    #[test]
    fn gate_finishing_just_at_the_deadline_never_leaks_a_count() {
        let g = std::sync::Arc::new(Gate::new(64));
        for i in 0..40u64 {
            let _ = g.run(Duration::from_millis(5), move || {
                std::thread::sleep(Duration::from_millis(3 + i % 6));
                Ok(())
            });
        }
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(g.stuck(), 0, "racing finishes must not strand the counter");
    }
}
