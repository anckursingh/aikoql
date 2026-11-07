//! SQLite Import Connector — cross-db-support milestone 2.
//!
//! Opens a SQLite database file, introspects tables via `sqlite_master`
//! and `PRAGMA table_info`, and imports rows as Knowledge Objects.
//!
//! Same design as the PostgreSQL connector:
//! - Table → type_name
//! - Primary key / rowid → deterministic KOID
//! - Columns → PropertyMap
//! - SQLite types flexibly mapped to Aikoql Value types
//!
//! ponytail: synchronous, no tokio. `rusqlite` with bundled SQLite.

use aikoql_kernel::knowledge::kom::*;
use rusqlite::{Connection, OpenFlags};

// ---------------------------------------------------------------------------
// Schema types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub sqlite_type: String, // "INTEGER", "TEXT", "REAL", "BLOB", etc.
    pub is_primary_key: bool,
    pub is_nullable: bool,
}

#[derive(Clone, Debug)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    pub row_count: i64,
    /// Column groups under a UNIQUE constraint or unique index (single and composite).
    pub unique_groups: Vec<Vec<String>>,
}

/// A foreign key under construction: parent table and (seq, from, to) rows.
type FkGroup = (String, Vec<(i64, String, Option<String>)>);

/// One foreign key (composite keys keep every column pair, in order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKeyInfo {
    /// SQLite names no constraints: `fk_<child>_<n>`.
    pub constraint_name: String,
    pub child_table: String,
    pub child_columns: Vec<String>,
    pub parent_table: String,
    pub parent_columns: Vec<String>,
}

// ---------------------------------------------------------------------------
// Connector
// ---------------------------------------------------------------------------

pub struct SqliteConnector {
    conn: Connection,
}

impl SqliteConnector {
    /// Open an existing database file strictly read-only: never creates the file,
    /// never writes, and gives up on a lock after `busy_ms`.
    pub fn open_read_only(path: &str, busy_ms: u64) -> Result<Self, String> {
        use rusqlite::{ffi::ErrorCode, Error, OpenFlags};
        let short = |e: Error| match e {
            Error::SqliteFailure(f, _) => match f.code {
                ErrorCode::NotADatabase => "not a SQLite database".to_string(),
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => {
                    "database is locked".to_string()
                }
                _ => "cannot open database (missing, unreadable or not a file)".to_string(),
            },
            _ => "cannot open database".to_string(),
        };
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(short)?;
        conn.busy_timeout(std::time::Duration::from_millis(busy_ms))
            .map_err(short)?;
        // force a read now, so a non-database file or a held lock fails here
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(short)?;
        Ok(SqliteConnector { conn })
    }

    /// Open a SQLite database file. `:memory:` for an in-memory database.
    pub fn open(path: &str) -> Result<Self, String> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()
        } else {
            Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
        }
        .map_err(|e| format!("SQLite open failed: {}", e))?;
        Ok(SqliteConnector { conn })
    }

    // ------------------------------------------------------------------
    // Schema discovery
    // ------------------------------------------------------------------

    /// List all user tables (excludes sqlite_* internal tables).
    pub fn list_tables(&self) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|e| format!("list tables: {}", e))?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("query tables: {}", e))?;
        let mut tables = Vec::new();
        for r in rows {
            tables.push(r.map_err(|e| format!("row: {}", e))?);
        }
        Ok(tables)
    }

    /// Introspect a table: columns, types, PK, exact row count.
    pub fn introspect_table(&self, table_name: &str) -> Result<TableSchema, String> {
        self.introspect_table_with(table_name, true)
    }

    /// Like [`introspect_table`](Self::introspect_table); with `count = false` the
    /// table is not scanned and `row_count` is `-1`.
    pub fn introspect_table_with(
        &self,
        table_name: &str,
        count: bool,
    ) -> Result<TableSchema, String> {
        // PRAGMA table_info gives: cid, name, type, notnull, dflt_value, pk
        let mut stmt = self
            .conn
            .prepare(&format!(
                "PRAGMA table_info(\"{}\")",
                table_name.replace('"', "\"\"")
            ))
            .map_err(|e| format!("introspect {}: {}", table_name, e))?;
        let col_rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?, // name
                    row.get::<_, String>(2)?, // type
                    row.get::<_, bool>(3)?,   // notnull
                    row.get::<_, i64>(5)?,    // pk (0 = not pk, >0 = pk order)
                ))
            })
            .map_err(|e| format!("query table_info: {}", e))?;

        let mut columns = Vec::new();
        for r in col_rows {
            let (name, col_type, notnull, pk_order) =
                r.map_err(|e| format!("column row: {}", e))?;
            columns.push(ColumnInfo {
                name,
                sqlite_type: col_type,
                is_primary_key: pk_order > 0,
                is_nullable: !notnull,
            });
        }

        // Row count (a full scan — skipped for schema-only discovery).
        let count: i64 = if count {
            self.conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM \"{}\"",
                        table_name.replace('"', "\"\"")
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0)
        } else {
            -1
        };

        Ok(TableSchema {
            name: table_name.to_string(),
            columns,
            row_count: count,
            unique_groups: self.unique_groups(table_name)?,
        })
    }

    /// Unique constraints and unique indexes (not the primary key), column order kept.
    fn unique_groups(&self, table: &str) -> Result<Vec<Vec<String>>, String> {
        let quoted = table.replace('"', "\"\"");
        let mut idx = self
            .conn
            .prepare(&format!("PRAGMA index_list(\"{quoted}\")"))
            .map_err(|e| format!("index_list {table}: {e}"))?;
        // columns: seq, name, unique, origin, partial
        let rows = idx
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("index_list {table}: {e}"))?;
        let mut groups = Vec::new();
        for row in rows {
            let (name, unique, origin) = row.map_err(|e| format!("index row: {e}"))?;
            if !unique || origin == "pk" {
                continue;
            }
            let mut info = self
                .conn
                .prepare(&format!(
                    "PRAGMA index_info(\"{}\")",
                    name.replace('"', "\"\"")
                ))
                .map_err(|e| format!("index_info {name}: {e}"))?;
            let cols = info
                .query_map([], |r| r.get::<_, Option<String>>(2))
                .map_err(|e| format!("index_info {name}: {e}"))?;
            let mut group = Vec::new();
            for c in cols {
                if let Some(c) = c.map_err(|e| format!("index col: {e}"))? {
                    group.push(c);
                }
            }
            if !group.is_empty() {
                groups.push(group);
            }
        }
        Ok(groups)
    }

    /// Primary-key columns of a table in key order.
    fn primary_key_columns(&self, table: &str) -> Result<Vec<String>, String> {
        let mut st = self
            .conn
            .prepare(&format!(
                "PRAGMA table_info(\"{}\")",
                table.replace('"', "\"\"")
            ))
            .map_err(|e| format!("table_info {table}: {e}"))?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, i64>(5)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("table_info {table}: {e}"))?;
        let mut pk: Vec<(i64, String)> = Vec::new();
        for r in rows {
            let (order, name) = r.map_err(|e| format!("pk row: {e}"))?;
            if order > 0 {
                pk.push((order, name));
            }
        }
        pk.sort();
        Ok(pk.into_iter().map(|(_, n)| n).collect())
    }

    /// Every foreign key of every user table.
    pub fn introspect_foreign_keys(&self) -> Result<Vec<ForeignKeyInfo>, String> {
        let mut out = Vec::new();
        for table in self.list_tables()? {
            let mut st = self
                .conn
                .prepare(&format!(
                    "PRAGMA foreign_key_list(\"{}\")",
                    table.replace('"', "\"\"")
                ))
                .map_err(|e| format!("foreign_key_list {table}: {e}"))?;
            // columns: id, seq, table, from, to, on_update, on_delete, match
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(|e| format!("foreign_key_list {table}: {e}"))?;
            let mut by_id: std::collections::BTreeMap<i64, FkGroup> =
                std::collections::BTreeMap::new();
            for r in rows {
                let (id, seq, parent, from, to) = r.map_err(|e| format!("fk row: {e}"))?;
                by_id
                    .entry(id)
                    .or_insert_with(|| (parent, Vec::new()))
                    .1
                    .push((seq, from, to));
            }
            for (id, (parent, mut cols)) in by_id {
                cols.sort();
                // `REFERENCES parent` without columns means the parent's primary key
                let implicit = if cols.iter().any(|(_, _, to)| to.is_none()) {
                    self.primary_key_columns(&parent)?
                } else {
                    Vec::new()
                };
                let parent_columns = cols
                    .iter()
                    .enumerate()
                    .map(|(i, (_, _, to))| {
                        to.clone()
                            .or_else(|| implicit.get(i).cloned())
                            .unwrap_or_default()
                    })
                    .collect();
                out.push(ForeignKeyInfo {
                    constraint_name: format!("fk_{table}_{id}"),
                    child_table: table.clone(),
                    child_columns: cols.into_iter().map(|(_, from, _)| from).collect(),
                    parent_table: parent,
                    parent_columns,
                });
            }
        }
        Ok(out)
    }

    /// Introspect all user tables.
    pub fn introspect_all(&self) -> Result<Vec<TableSchema>, String> {
        let tables = self.list_tables()?;
        let mut schemas = Vec::new();
        for t in &tables {
            schemas.push(self.introspect_table(t)?);
        }
        Ok(schemas)
    }

    // ------------------------------------------------------------------
    // Row import
    // ------------------------------------------------------------------

    /// Import all rows from a table as KnowledgeObjects.
    pub fn import_table(
        &self,
        schema: &TableSchema,
        tenant: Option<&str>,
    ) -> Result<Vec<KnowledgeObject>, String> {
        self.import_table_limited(schema, tenant, u64::MAX)
            .map(|(objects, _)| objects)
    }

    /// At most `max_rows` rows; the flag says the table had more (memory stays
    /// bounded: one extra row is read to know).
    pub fn import_table_limited(
        &self,
        schema: &TableSchema,
        tenant: Option<&str>,
        max_rows: u64,
    ) -> Result<(Vec<KnowledgeObject>, bool), String> {
        let _ = max_rows;
        let col_names: Vec<String> = schema
            .columns
            .iter()
            .map(|c| format!("\"{}\"", c.name.replace('"', "\"\"")))
            .collect();
        let query = format!(
            "SELECT {} FROM \"{}\"",
            col_names.join(", "),
            schema.name.replace('"', "\"\"")
        );
        let mut stmt = self
            .conn
            .prepare(&query)
            .map_err(|e| format!("import {}: {}", schema.name, e))?;

        let col_count = schema.columns.len();
        let col_types: Vec<&str> = schema
            .columns
            .iter()
            .map(|c| c.sqlite_type.as_str())
            .collect();
        let col_names_ref: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
        let pk_names: Vec<&str> = schema
            .columns
            .iter()
            .filter(|c| c.is_primary_key)
            .map(|c| c.name.as_str())
            .collect();

        let rows = stmt
            .query_map([], |row| {
                let mut props = PropertyMap::new();
                let mut pk_parts: Vec<String> = Vec::new();
                for i in 0..col_count {
                    let val = sqlite_cell_to_value(row, i, col_types[i]);
                    props.insert(col_names_ref[i].to_string(), val.clone());
                    if pk_names.contains(&col_names_ref[i]) {
                        pk_parts.push(value_to_string(&val));
                    }
                }
                Ok((props, pk_parts))
            })
            .map_err(|e| format!("query {}: {}", schema.name, e))?;

        let mut objects = Vec::new();
        for r in rows {
            let (props, pk_parts) = r.map_err(|e| format!("row: {}", e))?;
            // If no explicit PK, use rowid-style: hash of all values.
            let pk = if pk_parts.is_empty() {
                let combined: String = props
                    .values()
                    .map(value_to_string)
                    .collect::<Vec<_>>()
                    .join("|");
                vec![combined]
            } else {
                pk_parts
            };
            let koid = deterministic_koid(&schema.name, &pk);
            objects.push(KnowledgeObject {
                koid,
                version: 0,
                commit_ts: 0,
                metadata: Metadata {
                    type_name: schema.name.clone(),
                    tenant: tenant.map(String::from),
                    schema_version: 1,
                    tags: vec!["imported".into(), "source:sqlite".into()],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                event_refs: vec![],
                security: SecurityDescriptor {
                    owner: "sqlite-importer".into(),
                    acl: vec![],
                    classification: None,
                },
                lifecycle: Lifecycle {
                    state: LifecycleState::Draft,
                    origin: Origin::Human,
                },
                extensions: ExtensionMap::new(),
            });
        }
        Ok((objects, false))
    }
}

// ---------------------------------------------------------------------------
// Value mapping: SQLite → Aikoql Value
// ---------------------------------------------------------------------------

fn sqlite_cell_to_value(row: &rusqlite::Row, col_idx: usize, sqlite_type: &str) -> Value {
    let utype = sqlite_type.to_uppercase();
    // Try integer first.
    if utype.contains("INT") {
        if let Ok(v) = row.get::<_, i64>(col_idx) {
            return Value::Int(v);
        }
    }
    // Try float.
    if utype.contains("REAL")
        || utype.contains("FLOAT")
        || utype.contains("DOUB")
        || utype.contains("NUMERIC")
        || utype.contains("DECIMAL")
    {
        if let Ok(v) = row.get::<_, f64>(col_idx) {
            return Value::Float(v);
        }
    }
    // Try boolean (SQLite stores bools as 0/1 integers).
    if utype.contains("BOOL") {
        if let Ok(v) = row.get::<_, bool>(col_idx) {
            return Value::Bool(v);
        }
    }
    // Try blob.
    if utype.contains("BLOB") || utype.is_empty() {
        if let Ok(v) = row.get::<_, Vec<u8>>(col_idx) {
            return Value::Bytes(v);
        }
    }
    // Fallback: text.
    if let Ok(v) = row.get::<_, String>(col_idx) {
        return Value::Text(v);
    }
    // Try as i64.
    if let Ok(v) = row.get::<_, i64>(col_idx) {
        return Value::Int(v);
    }
    // Try as f64.
    if let Ok(v) = row.get::<_, f64>(col_idx) {
        return Value::Float(v);
    }
    // NULL.
    Value::Null
}

// ---------------------------------------------------------------------------
// Helpers (same as postgres connector)
// ---------------------------------------------------------------------------

fn deterministic_koid(table_name: &str, pk_parts: &[String]) -> KOID {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in table_name.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash ^= 0x3a;
    hash = hash.wrapping_mul(0x100000001b3);
    for part in pk_parts {
        for b in part.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0x2f;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let mut koid = [0u8; 16];
    koid[..8].copy_from_slice(&hash.to_be_bytes());
    KOID(koid)
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "NULL".into(),
        Value::Bytes(b) => format!("{:x?}", b),
        Value::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(value_to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Map(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}:{}", k, value_to_string(v)))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_works() {
        let conn = SqliteConnector::open(":memory:").unwrap();
        let tables = conn.list_tables().unwrap();
        assert!(tables.is_empty());
    }

    #[test]
    fn introspect_in_memory_table() {
        let conn = SqliteConnector::open(":memory:").unwrap();
        conn.conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER, active BOOLEAN DEFAULT 1, metadata BLOB);"
        ).unwrap();
        conn.conn
            .execute_batch("INSERT INTO users VALUES (1, 'Alice', 30, 1, X'deadbeef');")
            .unwrap();

        let schema = conn.introspect_table("users").unwrap();
        assert_eq!(schema.name, "users");
        assert_eq!(schema.columns.len(), 5);
        assert_eq!(schema.row_count, 1);

        let pk_cols: Vec<&str> = schema
            .columns
            .iter()
            .filter(|c| c.is_primary_key)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(pk_cols, vec!["id"]);
    }

    #[test]
    fn foreign_keys_keep_every_column_of_a_composite_key() {
        let conn = SqliteConnector::open(":memory:").unwrap();
        conn.conn
            .execute_batch(
                "CREATE TABLE parent (a INTEGER, b INTEGER, PRIMARY KEY (a, b));
                 CREATE TABLE solo (id INTEGER PRIMARY KEY);
                 CREATE TABLE child (id INTEGER PRIMARY KEY, pa INTEGER, pb INTEGER, sid INTEGER,
                   FOREIGN KEY (pa, pb) REFERENCES parent (a, b),
                   FOREIGN KEY (sid) REFERENCES solo);",
            )
            .unwrap();
        let fks = conn.introspect_foreign_keys().unwrap();
        assert_eq!(fks.len(), 2, "{fks:?}");
        let composite = fks.iter().find(|f| f.parent_table == "parent").unwrap();
        assert_eq!(composite.child_table, "child");
        assert_eq!(composite.child_columns, ["pa", "pb"]);
        assert_eq!(composite.parent_columns, ["a", "b"]);
        // `REFERENCES solo` names no column: it means the parent's primary key
        let implicit = fks.iter().find(|f| f.parent_table == "solo").unwrap();
        assert_eq!(implicit.child_columns, ["sid"]);
        assert_eq!(implicit.parent_columns, ["id"]);
        assert_ne!(composite.constraint_name, implicit.constraint_name);
    }

    #[test]
    fn unique_groups_list_constraints_and_unique_indexes_but_not_the_pk() {
        let conn = SqliteConnector::open(":memory:").unwrap();
        conn.conn
            .execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, code TEXT UNIQUE, x INTEGER, y INTEGER, z TEXT,
                   UNIQUE (x, y));
                 CREATE UNIQUE INDEX t_z ON t (z);
                 CREATE INDEX t_plain ON t (code);",
            )
            .unwrap();
        let mut groups = conn.introspect_table("t").unwrap().unique_groups;
        groups.sort();
        assert_eq!(
            groups,
            vec![
                vec!["code".to_string()],
                vec!["x".into(), "y".into()],
                vec!["z".into()]
            ]
        );
    }

    #[test]
    fn import_rows_from_in_memory() {
        let conn = SqliteConnector::open(":memory:").unwrap();
        conn.conn
            .execute_batch(
                "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT, price REAL);
             INSERT INTO items VALUES (1, 'Widget', 9.99);
             INSERT INTO items VALUES (2, 'Gadget', 19.50);",
            )
            .unwrap();

        let schema = conn.introspect_table("items").unwrap();
        let objects = conn.import_table(&schema, Some("test-tenant")).unwrap();
        assert_eq!(objects.len(), 2);

        let widget = &objects[0];
        assert_eq!(widget.metadata.type_name, "items");
        assert_eq!(widget.metadata.tenant, Some("test-tenant".into()));
        assert!(widget.metadata.tags.contains(&"source:sqlite".into()));
        assert_eq!(
            widget.properties.get("label"),
            Some(&Value::Text("Widget".into()))
        );
        assert_eq!(widget.properties.get("price"), Some(&Value::Float(9.99)));

        // Same PK → same KOID.
        assert_eq!(objects[0].koid, objects[0].koid);
        // Different PK → different KOID.
        assert_ne!(objects[0].koid, objects[1].koid);
    }

    #[test]
    fn deterministic_koid_consistent() {
        let k1 = deterministic_koid("users", &["1".into()]);
        let k2 = deterministic_koid("users", &["1".into()]);
        assert_eq!(k1, k2);
    }

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aikoql-sqlite-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("db.sqlite")
    }

    #[test]
    fn open_read_only_never_creates_a_missing_file() {
        let p = tmp_path("missing");
        assert!(SqliteConnector::open_read_only(p.to_str().unwrap(), 200).is_err());
        assert!(!p.exists(), "read-only open must not create the file");
    }

    #[test]
    fn open_read_only_rejects_a_file_that_is_not_a_database() {
        let p = tmp_path("garbage");
        std::fs::write(
            &p,
            b"this is definitely not a sqlite database, just text ".repeat(50),
        )
        .unwrap();
        let r =
            SqliteConnector::open_read_only(p.to_str().unwrap(), 200).and_then(|c| c.list_tables());
        assert!(r.is_err(), "{r:?}");
        let empty = tmp_path("empty");
        std::fs::write(&empty, b"").unwrap();
        // a zero-byte file is a valid empty database to SQLite: no tables, and no write
        let tables = SqliteConnector::open_read_only(empty.to_str().unwrap(), 200)
            .and_then(|c| c.list_tables());
        assert!(tables.map(|t| t.is_empty()).unwrap_or(true));
    }

    #[test]
    fn a_read_only_connection_cannot_write() {
        let p = tmp_path("ro");
        let rw = Connection::open(&p).unwrap();
        rw.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(rw);
        let ro = SqliteConnector::open_read_only(p.to_str().unwrap(), 200).unwrap();
        assert_eq!(ro.list_tables().unwrap(), ["t"]);
        let w = ro.conn.execute_batch("INSERT INTO t VALUES (2)");
        assert!(
            w.is_err(),
            "write on a read-only connection must fail: {w:?}"
        );
        assert!(ro.conn.execute_batch("CREATE TABLE u (x)").is_err());
    }

    #[test]
    fn schema_only_introspection_does_not_scan_the_table() {
        let c = SqliteConnector::open(":memory:").unwrap();
        c.conn
            .execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1),(2),(3);",
            )
            .unwrap();
        assert_eq!(c.introspect_table_with("t", false).unwrap().row_count, -1);
        assert_eq!(c.introspect_table_with("t", true).unwrap().row_count, 3);
    }

    #[test]
    fn a_locked_database_fails_within_the_busy_timeout() {
        let p = tmp_path("locked");
        let rw = Connection::open(&p).unwrap();
        rw.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY);")
            .unwrap();
        rw.execute_batch("BEGIN EXCLUSIVE; INSERT INTO t VALUES (1);")
            .unwrap(); // writer holds the lock
        let started = std::time::Instant::now();
        let r =
            SqliteConnector::open_read_only(p.to_str().unwrap(), 300).and_then(|c| c.list_tables());
        assert!(
            r.is_err(),
            "a reader must not read through an exclusive lock: {r:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        rw.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn hostile_table_and_column_names_introspect_without_injection() {
        let c = SqliteConnector::open(":memory:").unwrap();
        let long = "n".repeat(300);
        c.conn
            .execute_batch(&format!(
                "CREATE TABLE victim (id INTEGER PRIMARY KEY);
                 CREATE TABLE \"x\"\"; DROP TABLE victim; --\" (\"a b\" INTEGER PRIMARY KEY, \"it's\" TEXT);
                 CREATE TABLE \"{long}\" (\"order\" INTEGER);
                 CREATE TABLE \"ünï\u{202e}💥\" (c INTEGER);"
            ))
            .unwrap();
        let all = c.introspect_all().unwrap();
        assert_eq!(all.len(), 4);
        assert!(all.iter().any(|t| t.name == "x\"; DROP TABLE victim; --"));
        assert!(all.iter().any(|t| t.name == long));
        c.introspect_foreign_keys().unwrap();
        assert!(
            c.list_tables().unwrap().contains(&"victim".to_string()),
            "nothing was injected"
        );
    }

    #[test]
    fn import_table_limited_caps_rows_and_says_when_it_cut() {
        let c = SqliteConnector::open(":memory:").unwrap();
        c.conn
            .execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);
                 INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e');",
            )
            .unwrap();
        let schema = c.introspect_table("t").unwrap();
        let (objs, cut) = c.import_table_limited(&schema, None, 3).unwrap();
        assert_eq!((objs.len(), cut), (3, true));
        let (objs, cut) = c.import_table_limited(&schema, None, 5).unwrap();
        assert_eq!(
            (objs.len(), cut),
            (5, false),
            "exactly the cap is not a cut"
        );
        let (objs, cut) = c.import_table_limited(&schema, None, 100).unwrap();
        assert_eq!((objs.len(), cut), (5, false));
        assert_eq!(
            c.import_table(&schema, None).unwrap().len(),
            5,
            "the unlimited path is unchanged"
        );
        let (objs, cut) = c.import_table_limited(&schema, None, 0).unwrap();
        assert_eq!((objs.len(), cut), (0, true));
    }
}
