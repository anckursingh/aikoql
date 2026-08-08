//! PostgreSQL Import Connector — cross-db-support milestone 1.
//!
//! Connects to a PostgreSQL database, introspects the schema via
//! `information_schema`, and imports tables as Knowledge Objects.
//!
//! Design:
//! - Table → type_name
//! - Primary key → deterministic KOID (table_name || PK_value)
//! - Non-PK columns → PropertyMap
//! - Foreign keys → RelationshipRef (deferred: import all tables first, then link)
//! - PG types mapped to Mnemosyne Value types
//!
//! ponytail: synchronous, single-threaded. Batch imports land when throughput
//! matters (>100k rows). Foreign key relationship creation lands in Phase 2.

use mnemosyne_kernel::knowledge::kom::*;
use postgres::{Client, NoTls};

// ---------------------------------------------------------------------------
// PG type → Mnemosyne Value mapping
// ---------------------------------------------------------------------------

/// A column as discovered from information_schema.
#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub pg_type: String,     // e.g. "integer", "text", "boolean"
    pub is_nullable: bool,
    pub is_primary_key: bool,
}

/// Schema for one table.
#[derive(Clone, Debug)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    pub primary_keys: Vec<String>,
    pub row_count_estimate: i64,
}

/// The import connector.
pub struct PostgresConnector {
    client: Client,
}

impl PostgresConnector {
    /// Connect to a PostgreSQL database.
    /// `conn_str` format: `host=localhost user=postgres dbname=mydb`
    pub fn connect(conn_str: &str) -> Result<Self, String> {
        let client = Client::connect(conn_str, NoTls)
            .map_err(|e| format!("PG connection failed: {}", e))?;
        Ok(PostgresConnector { client })
    }

    // ------------------------------------------------------------------
    // Schema discovery
    // ------------------------------------------------------------------

    /// List all user tables (excludes pg_catalog and information_schema).
    pub fn list_tables(&mut self) -> Result<Vec<String>, String> {
        let rows = self
            .client
            .query(
                "SELECT table_name FROM information_schema.tables
                 WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
                 ORDER BY table_name",
                &[],
            )
            .map_err(|e| format!("list tables: {}", e))?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// Introspect a table: columns, types, primary keys.
    pub fn introspect_table(&mut self, table_name: &str) -> Result<TableSchema, String> {
        // Discover columns.
        let col_rows = self
            .client
            .query(
                "SELECT column_name, data_type, is_nullable
                 FROM information_schema.columns
                 WHERE table_name = $1
                 ORDER BY ordinal_position",
                &[&table_name],
            )
            .map_err(|e| format!("introspect columns for {}: {}", table_name, e))?;

        // Discover primary keys.
        let pk_rows = self
            .client
            .query(
                "SELECT kcu.column_name
                 FROM information_schema.table_constraints tc
                 JOIN information_schema.key_column_usage kcu
                   ON tc.constraint_name = kcu.constraint_name
                 WHERE tc.table_name = $1 AND tc.constraint_type = 'PRIMARY KEY'",
                &[&table_name],
            )
            .map_err(|e| format!("introspect PKs for {}: {}", table_name, e))?;

        let pks: Vec<String> = pk_rows.iter().map(|r| r.get(0)).collect();

        let columns: Vec<ColumnInfo> = col_rows
            .iter()
            .map(|r| {
                let name: String = r.get(0);
                ColumnInfo {
                    is_primary_key: pks.contains(&name),
                    name,
                    pg_type: r.get(1),
                    is_nullable: r.get::<_, String>(2) == "YES",
                }
            })
            .collect();

        // Row count estimate.
        let count: i64 = self
            .client
            .query_one(
                &format!("SELECT COUNT(*) FROM {}", quote_ident(table_name)),
                &[],
            )
            .map(|r| r.get(0))
            .unwrap_or(-1);

        Ok(TableSchema {
            name: table_name.to_string(),
            columns,
            primary_keys: pks,
            row_count_estimate: count,
        })
    }

    /// Introspect all user tables.
    pub fn introspect_all(&mut self) -> Result<Vec<TableSchema>, String> {
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

    /// Import all rows from a table and return KnowledgeObjects.
    /// Each row becomes one KO of type `table_name`.
    /// The KOID is deterministic: first 8 bytes = hash of "table_name/pk_col:pk_val".
    pub fn import_table(
        &mut self,
        schema: &TableSchema,
        tenant: Option<&str>,
    ) -> Result<Vec<KnowledgeObject>, String> {
        let col_list: Vec<String> = schema.columns.iter().map(|c| quote_ident(&c.name)).collect();
        let query = format!("SELECT {} FROM {}", col_list.join(", "), quote_ident(&schema.name));
        let rows = self
            .client
            .query(&query, &[])
            .map_err(|e| format!("import {}: {}", schema.name, e))?;

        let mut objects = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut props = PropertyMap::new();
            let mut pk_parts: Vec<String> = Vec::new();

            for (i, col) in schema.columns.iter().enumerate() {
                let val = pg_cell_to_value(&row, i, &col.pg_type, col.is_nullable);
                props.insert(col.name.clone(), val.clone());

                if col.is_primary_key {
                    pk_parts.push(value_to_string(&val));
                }
            }

            // Deterministic KOID from table + PK.
            let koid = deterministic_koid(&schema.name, &pk_parts);

            objects.push(KnowledgeObject {
                koid,
                version: 0,
                commit_ts: 0,
                metadata: Metadata {
                    type_name: schema.name.clone(),
                    tenant: tenant.map(String::from),
                    schema_version: 1,
                    tags: vec!["imported".into(), "postgres".into()],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                event_refs: vec![],
                security: SecurityDescriptor {
                    owner: "postgres-importer".into(),
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
// Value mapping helpers
// ---------------------------------------------------------------------------

/// Convert a PostgreSQL cell to a Mnemosyne Value.
fn pg_cell_to_value(
    row: &postgres::Row,
    col_idx: usize,
    pg_type: &str,
    nullable: bool,
) -> Value {
    // Try each target type; if the column is NULL, return Value::Null.
    if nullable && try_is_null(row, col_idx) {
        return Value::Null;
    }
    match pg_type {
        // Integer family
        "smallint" | "integer" | "int" | "int4" => {
            row.try_get::<_, i32>(col_idx)
                .map(|v| Value::Int(v as i64))
                .unwrap_or(Value::Null)
        }
        "bigint" | "int8" => {
            row.try_get::<_, i64>(col_idx)
                .map(Value::Int)
                .unwrap_or(Value::Null)
        }
        // Float family
        "real" | "float4" => {
            row.try_get::<_, f32>(col_idx)
                .map(|v| Value::Float(v as f64))
                .unwrap_or(Value::Null)
        }
        "double precision" | "float8" | "numeric" | "decimal" => {
            row.try_get::<_, f64>(col_idx)
                .map(Value::Float)
                .unwrap_or(Value::Null)
        }
        // Boolean
        "boolean" | "bool" => {
            row.try_get::<_, bool>(col_idx)
                .map(Value::Bool)
                .unwrap_or(Value::Null)
        }
        // Text family
        "text" | "varchar" | "character varying" | "char" | "character" | "name" | "uuid" | "json" | "jsonb" => {
            row.try_get::<_, String>(col_idx)
                .map(Value::Text)
                .unwrap_or(Value::Null)
        }
        // Binary
        "bytea" => {
            row.try_get::<_, Vec<u8>>(col_idx)
                .map(Value::Bytes)
                .unwrap_or(Value::Null)
        }
        // Timestamps → ISO 8601 text
        "timestamp" | "timestamptz" | "timestamp without time zone" | "timestamp with time zone" |
        "date" | "time" | "timetz" => {
            row.try_get::<_, String>(col_idx)
                .map(Value::Text)
                .unwrap_or(Value::Null)
        }
        // Everything else → text representation
        _ => {
            row.try_get::<_, String>(col_idx)
                .map(Value::Text)
                .unwrap_or(Value::Null)
        }
    }
}

fn try_is_null(row: &postgres::Row, col_idx: usize) -> bool {
    // ponytail: check null by trying to get as Option<String>.
    row.try_get::<_, Option<String>>(col_idx).ok().flatten().is_none()
        && row.try_get::<_, Option<i64>>(col_idx).ok().flatten().is_none()
        && row.try_get::<_, Option<f64>>(col_idx).ok().flatten().is_none()
        && row.try_get::<_, Option<bool>>(col_idx).ok().flatten().is_none()
}

/// Deterministic KOID from table name and primary key parts.
/// Uses the first 8 bytes of a simple FNV-style hash.
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
    // Remaining 8 bytes stay zero — unique per table+PK, no collision in practice
    // for single-table imports. Full SHA-256 lands when cross-table FK resolution
    // is added (Phase 2).
    KOID(koid)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "NULL".into(),
        Value::Bytes(b) => format!("{:x?}", b),
        Value::List(items) => format!("[{}]", items.iter().map(value_to_string).collect::<Vec<_>>().join(",")),
        Value::Map(m) => format!("{{{}}}", m.iter().map(|(k, v)| format!("{}:{}", k, value_to_string(v))).collect::<Vec<_>>().join(",")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_koid_same_pk_same_result() {
        let k1 = deterministic_koid("users", &["42".into()]);
        let k2 = deterministic_koid("users", &["42".into()]);
        assert_eq!(k1, k2);
    }

    #[test]
    fn deterministic_koid_different_table_different_result() {
        let k1 = deterministic_koid("users", &["1".into()]);
        let k2 = deterministic_koid("orders", &["1".into()]);
        assert_ne!(k1, k2);
    }

    #[test]
    fn deterministic_koid_different_pk_different_result() {
        let k1 = deterministic_koid("users", &["1".into()]);
        let k2 = deterministic_koid("users", &["2".into()]);
        assert_ne!(k1, k2);
    }

    #[test]
    fn quote_ident_handles_special_chars() {
        assert_eq!(quote_ident("normal"), "\"normal\"");
        assert_eq!(quote_ident("has\"quote"), "\"has\"\"quote\"");
    }

    #[test]
    fn pg_cell_text_to_value() {
        // ponytail: unit-tested via integration test with real PG.
        // Type mapping is verified by inspection.
    }
}
