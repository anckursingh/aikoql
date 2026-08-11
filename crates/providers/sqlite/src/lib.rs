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
}

// ---------------------------------------------------------------------------
// Connector
// ---------------------------------------------------------------------------

pub struct SqliteConnector {
    conn: Connection,
}

impl SqliteConnector {
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

    /// Introspect a table: columns, types, PK.
    pub fn introspect_table(&self, table_name: &str) -> Result<TableSchema, String> {
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

        // Row count.
        let count: i64 = self
            .conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM \"{}\"",
                    table_name.replace('"', "\"\"")
                ),
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        Ok(TableSchema {
            name: table_name.to_string(),
            columns,
            row_count: count,
        })
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
        Ok(objects)
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
}
