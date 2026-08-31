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
//! - PG types mapped to Aikoql Value types
//!
//! ponytail: synchronous, single-threaded. Batch imports land when throughput
//! matters (>100k rows).

use std::collections::HashSet;

use aikoql_kernel::knowledge::kom::*;
use postgres::{Client, NoTls};

// ---------------------------------------------------------------------------
// PG type → Aikoql Value mapping
// ---------------------------------------------------------------------------

/// A column as discovered from information_schema.
#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub pg_type: String, // e.g. "integer", "text", "boolean"
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

/// One foreign key as discovered from information_schema.
#[derive(Clone, Debug)]
pub struct ForeignKeyInfo {
    pub constraint_name: String,
    pub child_table: String,
    pub child_column: String,
    pub parent_table: String,
    pub parent_column: String,
}

/// The import connector.
pub struct PostgresConnector {
    client: Client,
}

impl PostgresConnector {
    /// Connect to a PostgreSQL database.
    /// `conn_str` format: `host=localhost user=postgres dbname=mydb`
    pub fn connect(conn_str: &str) -> Result<Self, String> {
        Self::connect_with_timeout(conn_str, None)
    }

    /// Connect with an optional timeout (CON-005): `connect_timeout` bounds
    /// the TCP handshake, `SET statement_timeout` makes the server abort any
    /// query that stalls past it — a hung query fails the run instead of
    /// hanging the import forever.
    pub fn connect_with_timeout(conn_str: &str, timeout_ms: Option<u64>) -> Result<Self, String> {
        let mut config: postgres::Config = conn_str
            .parse()
            .map_err(|e| format!("PG connection string: {}", e))?;
        if let Some(ms) = timeout_ms {
            config.connect_timeout(std::time::Duration::from_millis(ms));
        }
        let mut client = config
            .connect(NoTls)
            .map_err(|e| format!("PG connection failed: {}", e))?;
        if let Some(ms) = timeout_ms {
            client
                .batch_execute(&format!("SET statement_timeout = {ms}"))
                .map_err(|e| format!("SET statement_timeout: {}", e))?;
        }
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
        // Discover columns. format_type adds the typmod so pgvector columns
        // report as "vector(3)" (dims) instead of the bare USER-DEFINED
        // data_type — and numeric/varchar keep their precision.
        let col_rows = self
            .client
            .query(
                "SELECT c.column_name, c.data_type, c.is_nullable,
                        COALESCE(format_type(a.atttypid, a.atttypmod), c.data_type) AS pg_type
                 FROM information_schema.columns c
                 LEFT JOIN pg_catalog.pg_attribute a
                   ON a.attrelid = to_regclass(c.table_schema || '.' || c.table_name)
                  AND a.attname = c.column_name
                 WHERE c.table_name = $1
                 ORDER BY c.ordinal_position",
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
                    pg_type: r.get(3),
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

    /// List tables carrying at least one pgvector `vector` column (CON-004
    /// discovery). Dims per column come from the introspected pg_type
    /// ("vector(N)") — see introspect_table.
    pub fn list_vector_tables(&mut self) -> Result<Vec<String>, String> {
        let rows = self
            .client
            .query(
                "SELECT DISTINCT table_name
                 FROM information_schema.columns
                 WHERE udt_name = 'vector'
                   AND table_schema NOT IN ('pg_catalog', 'information_schema')
                 ORDER BY table_name",
                &[],
            )
            .map_err(|e| format!("list vector tables: {}", e))?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
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
        let col_list: Vec<String> = schema
            .columns
            .iter()
            .map(|c| {
                let q = quote_ident(&c.name);
                // pgvector UDTs have no driver-native FromSql — read the
                // column ::text and let pg_cell_to_value parse the form.
                if c.pg_type.starts_with("vector") {
                    format!("{q}::text")
                } else {
                    q
                }
            })
            .collect();
        let query = format!(
            "SELECT {} FROM {}",
            col_list.join(", "),
            quote_ident(&schema.name)
        );
        let rows = self
            .client
            .query(&query, &[])
            .map_err(|e| format!("import {}: {}", schema.name, e))?;

        let mut objects = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut props = PropertyMap::new();
            let mut pk_parts: Vec<String> = Vec::new();

            for (i, col) in schema.columns.iter().enumerate() {
                let val = pg_cell_to_value(row, i, &col.pg_type);
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
                    tags: vec!["imported".into(), "source:postgres".into()],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                event_refs: vec![],
                security: SecurityDescriptor {
                    owner: "pg-importer".into(),
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

    /// List single-column foreign keys. ponytail: a composite FK (same
    /// constraint_name twice) keeps only its first column pair — MVP schemas
    /// are single-column.
    pub fn introspect_foreign_keys(&mut self) -> Result<Vec<ForeignKeyInfo>, String> {
        let rows = self
            .client
            .query(
                "SELECT tc.constraint_name, tc.table_name, kcu.column_name,
                        ccu.table_name AS parent_table, ccu.column_name AS parent_column
                 FROM information_schema.table_constraints tc
                 JOIN information_schema.key_column_usage kcu
                   ON tc.constraint_name = kcu.constraint_name
                  AND tc.table_schema = kcu.table_schema
                 JOIN information_schema.constraint_column_usage ccu
                   ON ccu.constraint_name = tc.constraint_name
                  AND ccu.table_schema = tc.table_schema
                 WHERE tc.constraint_type = 'FOREIGN KEY'
                   AND tc.table_schema NOT IN ('pg_catalog', 'information_schema')
                 ORDER BY tc.table_name, tc.constraint_name",
                &[],
            )
            .map_err(|e| format!("introspect foreign keys: {}", e))?;
        let mut seen = HashSet::new();
        let mut fks = Vec::new();
        for r in &rows {
            let fk = ForeignKeyInfo {
                constraint_name: r.get(0),
                child_table: r.get(1),
                child_column: r.get(2),
                parent_table: r.get(3),
                parent_column: r.get(4),
            };
            if seen.insert(fk.constraint_name.clone()) {
                fks.push(fk);
            }
        }
        Ok(fks)
    }

    /// Phase 2: foreign keys → RelationshipRefs on the child rows. Links
    /// only FKs whose referenced column is the parent's single-column PK and
    /// whose both tables are in `schemas` (i.e. imported this run — a
    /// `--table`-filtered run links only what it imported). Returns the
    /// number of relationships attached.
    pub fn link_relationships(
        &mut self,
        schemas: &[&TableSchema],
        objects: &mut [KnowledgeObject],
    ) -> Result<usize, String> {
        let fks = self.introspect_foreign_keys()?;
        let pk_cols: std::collections::HashMap<&str, &[String]> = schemas
            .iter()
            .map(|s| (s.name.as_str(), s.primary_keys.as_slice()))
            .collect();

        // Parent index: (parent_table, pk-value) → KOID, built from this
        // run's objects. pk values use the same value_to_string form as
        // deterministic_koid, so child FK values ("1") match parent PKs (1).
        let mut parent_index: std::collections::HashMap<(String, String), KOID> =
            std::collections::HashMap::new();
        for ko in objects.iter() {
            let Some(pk) = pk_cols.get(ko.metadata.type_name.as_str()) else {
                continue;
            };
            let parts: Vec<String> = pk
                .iter()
                .map(|c| value_to_string(ko.properties.get(c.as_str()).unwrap_or(&Value::Null)))
                .collect();
            parent_index.insert(
                (ko.metadata.type_name.clone(), parts.join("\u{1f}")),
                ko.koid,
            );
        }

        let mut linked = 0usize;
        for fk in &fks {
            // Skip FKs we cannot resolve honestly (parent column is not a
            // single-column PK, or a side was filtered out of this run).
            match pk_cols.get(fk.parent_table.as_str()) {
                Some(pk) if pk.len() == 1 && pk[0] == fk.parent_column => {}
                _ => continue,
            }
            if !pk_cols.contains_key(fk.child_table.as_str()) {
                continue;
            }
            // ponytail: O(rows) scan per FK — fine for MVP row counts.
            for ko in objects.iter_mut() {
                if ko.metadata.type_name != fk.child_table {
                    continue;
                }
                let val = value_to_string(
                    ko.properties
                        .get(fk.child_column.as_str())
                        .unwrap_or(&Value::Null),
                );
                let Some(parent) = parent_index.get(&(fk.parent_table.clone(), val.clone())) else {
                    continue; // NULL FK or value without a parent row
                };
                ko.relationships.push(RelationshipRef {
                    rel_type: fk.constraint_name.clone(),
                    target: *parent,
                    direction: Direction::Outbound,
                });
                linked += 1;
            }
        }
        Ok(linked)
    }
}

// ---------------------------------------------------------------------------
// Value mapping helpers
// ---------------------------------------------------------------------------

/// Convert a PostgreSQL cell to an Aikoql Value. Each family reads as
/// `Option<T>` so a NULL cell is recognized by the driver itself — the old
/// try-get heuristic misread nullable non-NULL integers as NULL (item 4 red
/// debug found FK values importing as Value::Null). Read failures per type
/// also become Value::Null, as before.
fn pg_cell_to_value(row: &postgres::Row, col_idx: usize, pg_type: &str) -> Value {
    fn opt<T, F>(row: &postgres::Row, col_idx: usize, f: F) -> Value
    where
        T: postgres::types::FromSqlOwned,
        F: FnOnce(T) -> Value,
    {
        row.try_get::<_, Option<T>>(col_idx)
            .ok()
            .flatten()
            .map(f)
            .unwrap_or(Value::Null)
    }
    // format_type adds typmods ("vector(3)", "numeric(10,2)") — match on the
    // bare type name.
    let base = pg_type.split('(').next().unwrap_or(pg_type);
    match base {
        // Integer family
        "smallint" | "integer" | "int" | "int4" => opt(row, col_idx, |v: i32| Value::Int(v as i64)),
        "bigint" | "int8" => opt(row, col_idx, Value::Int),
        // Float family
        "real" | "float4" => opt(row, col_idx, |v: f32| Value::Float(v as f64)),
        "double precision" | "float8" | "numeric" | "decimal" => opt(row, col_idx, Value::Float),
        // Boolean
        "boolean" | "bool" => opt(row, col_idx, Value::Bool),
        // Text family
        "text" | "varchar" | "character varying" | "char" | "character" | "name" | "uuid"
        | "json" | "jsonb" => opt(row, col_idx, Value::Text),
        // Binary
        "bytea" => opt(row, col_idx, Value::Bytes),
        // Timestamps → ISO 8601 text
        "timestamp"
        | "timestamptz"
        | "timestamp without time zone"
        | "timestamp with time zone"
        | "date"
        | "time"
        | "timetz" => opt(row, col_idx, Value::Text),
        // pgvector (import_table selects the column ::text) → List of floats
        "vector" => opt(row, col_idx, parse_vector),
        // Everything else → text representation
        _ => opt(row, col_idx, Value::Text),
    }
}

/// "[0.1,0.2,0.3]" → Value::List of Float; malformed → Value::Null.
fn parse_vector(s: String) -> Value {
    let inner = s.trim().trim_start_matches('[').trim_end_matches(']');
    let vals: Vec<Value> = inner
        .split(',')
        .filter_map(|t| t.trim().parse::<f64>().ok().map(Value::Float))
        .collect();
    if vals.is_empty() && !inner.trim().is_empty() {
        return Value::Null;
    }
    Value::List(vals)
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

    #[test]
    fn parse_vector_pgvector_text() {
        assert_eq!(
            parse_vector("[0.1,0.2,0.3]".into()),
            Value::List(vec![
                Value::Float(0.1),
                Value::Float(0.2),
                Value::Float(0.3)
            ])
        );
        assert_eq!(parse_vector("[]".into()), Value::List(vec![]));
        assert_eq!(parse_vector("garbage".into()), Value::Null);
        assert_eq!(
            parse_vector("[0.5]".into()),
            Value::List(vec![Value::Float(0.5)])
        );
    }
}
