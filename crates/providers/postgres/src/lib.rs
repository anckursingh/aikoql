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
    /// Postgres schema (namespace) the table lives in.
    pub schema: String,
    pub columns: Vec<ColumnInfo>,
    pub primary_keys: Vec<String>,
    pub row_count_estimate: i64,
    /// Column groups under a UNIQUE constraint (single and composite), in key order.
    pub unique_groups: Vec<Vec<String>>,
}

/// One foreign key as discovered from information_schema.
#[derive(Clone, Debug)]
pub struct ForeignKeyInfo {
    pub constraint_name: String,
    pub child_table: String,
    /// First column pair — kept for single-column callers.
    pub child_column: String,
    pub parent_table: String,
    pub parent_column: String,
    /// Every column pair of the constraint, in key order.
    pub child_columns: Vec<String>,
    pub parent_columns: Vec<String>,
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

    /// Make this session read-only (`transaction_read_only = on`): the server
    /// then refuses every write, whatever a later bug tries.
    pub fn read_only_session(&mut self) -> Result<(), String> {
        self.client
            .batch_execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
            .map_err(|e| format!("read-only session: {e}"))
    }

    /// Whether the session is read-only (`SHOW transaction_read_only`).
    pub fn is_read_only(&mut self) -> Result<bool, String> {
        let row = self
            .client
            .query_one("SHOW transaction_read_only", &[])
            .map_err(|e| format!("transaction_read_only: {e}"))?;
        Ok(row.get::<_, String>(0) == "on")
    }

    /// Introspect a table: columns, types, primary keys, exact row count.
    pub fn introspect_table(&mut self, table_name: &str) -> Result<TableSchema, String> {
        self.introspect_table_with(table_name, true)
    }

    /// With `exact_count = false` the row count is the planner's estimate
    /// (`pg_class.reltuples`; `-1` if the table was never analyzed) — no scan.
    pub fn introspect_table_with(
        &mut self,
        table_name: &str,
        exact_count: bool,
    ) -> Result<TableSchema, String> {
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

        // Row count: exact (a scan) or the planner's estimate (no scan).
        let count: i64 = if exact_count {
            self.client
                .query_one(
                    &format!("SELECT COUNT(*) FROM {}", quote_ident(table_name)),
                    &[],
                )
                .map(|r| r.get(0))
                .unwrap_or(-1)
        } else {
            self.client
                .query_one(
                    "SELECT reltuples::bigint FROM pg_class WHERE oid = to_regclass($1)",
                    &[&quote_ident(table_name)],
                )
                .map(|r| r.get(0))
                .unwrap_or(-1)
        };

        Ok(TableSchema {
            name: table_name.to_string(),
            columns,
            primary_keys: pks,
            row_count_estimate: count,
            schema: self.table_schema_name(table_name)?,
            unique_groups: self.unique_groups(table_name)?,
        })
    }

    /// The (first) non-system schema holding a table of this name.
    fn table_schema_name(&mut self, table: &str) -> Result<String, String> {
        let rows = self
            .client
            .query(
                "SELECT n.nspname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE c.relname = $1 AND c.relkind IN ('r', 'p')
                   AND n.nspname NOT IN ('pg_catalog', 'information_schema')
                 ORDER BY (n.nspname = 'public') DESC, n.nspname LIMIT 1",
                &[&table],
            )
            .map_err(|e| format!("schema of {table}: {e}"))?;
        Ok(rows.first().map(|r| r.get(0)).unwrap_or_default())
    }

    /// UNIQUE constraints of a table: columns in key order.
    fn unique_groups(&mut self, table: &str) -> Result<Vec<Vec<String>>, String> {
        let rows = self
            .client
            .query(
                "SELECT (SELECT array_agg(a.attname::text ORDER BY k.ord)
                           FROM unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord)
                           JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum)
                 FROM pg_constraint con
                 JOIN pg_class cl ON cl.oid = con.conrelid
                 JOIN pg_namespace n ON n.oid = cl.relnamespace
                 WHERE con.contype = 'u' AND cl.relname = $1
                   AND n.nspname NOT IN ('pg_catalog', 'information_schema')
                 ORDER BY con.conname",
                &[&table],
            )
            .map_err(|e| format!("unique constraints of {table}: {e}"))?;
        Ok(rows
            .iter()
            .filter_map(|r| r.get::<_, Option<Vec<String>>>(0))
            .collect())
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
        self.import_table_limited(schema, tenant, u64::MAX)
            .map(|(objects, _)| objects)
    }

    /// At most `max_rows` rows; the flag says the table had more.
    pub fn import_table_limited(
        &mut self,
        schema: &TableSchema,
        tenant: Option<&str>,
        max_rows: u64,
    ) -> Result<(Vec<KnowledgeObject>, bool), String> {
        let limit = max_rows.saturating_add(1).min(i64::MAX as u64);
        let col_list: Vec<String> = schema
            .columns
            .iter()
            .map(|c| {
                let q = quote_ident(&c.name);
                // Only types the driver decodes exactly are read natively;
                // everything else (numeric, timestamps, uuid, json, arrays,
                // enums, pgvector, ...) is read ::text and parsed by
                // pg_cell_to_value — a failed native decode must never
                // degrade a real value to NULL.
                if is_native(&c.pg_type) {
                    q
                } else {
                    format!("{q}::text")
                }
            })
            .collect();
        // Pin the text forms: the same row imports identically whatever the
        // server's or role's TimeZone / DateStyle / IntervalStyle defaults.
        self.client
            .batch_execute(
                "SET TimeZone = 'UTC'; SET DateStyle = 'ISO, YMD'; SET IntervalStyle = 'postgres'",
            )
            .map_err(|e| format!("import {}: session settings: {}", schema.name, e))?;
        let query = format!(
            "SELECT {} FROM {} LIMIT {limit}",
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
        let truncated = objects.len() as u64 > max_rows;
        objects.truncate(usize::try_from(max_rows).unwrap_or(usize::MAX));
        Ok((objects, truncated))
    }

    /// Every foreign key with ALL its column pairs, in key order.
    pub fn introspect_foreign_keys(&mut self) -> Result<Vec<ForeignKeyInfo>, String> {
        let rows = self
            .client
            .query(
                "SELECT con.conname::text, cl.relname::text, pcl.relname::text,
                        (SELECT array_agg(a.attname::text ORDER BY k.ord)
                           FROM unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord)
                           JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum),
                        (SELECT array_agg(a.attname::text ORDER BY k.ord)
                           FROM unnest(con.confkey) WITH ORDINALITY AS k(attnum, ord)
                           JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.attnum)
                 FROM pg_constraint con
                 JOIN pg_class cl ON cl.oid = con.conrelid
                 JOIN pg_namespace ns ON ns.oid = cl.relnamespace
                 JOIN pg_class pcl ON pcl.oid = con.confrelid
                 WHERE con.contype = 'f'
                   AND ns.nspname NOT IN ('pg_catalog', 'information_schema')
                 ORDER BY cl.relname, con.conname",
                &[],
            )
            .map_err(|e| format!("introspect foreign keys: {}", e))?;
        let mut seen = HashSet::new();
        let mut fks = Vec::new();
        for r in &rows {
            let child_columns: Vec<String> = r.get::<_, Option<Vec<String>>>(3).unwrap_or_default();
            let parent_columns: Vec<String> =
                r.get::<_, Option<Vec<String>>>(4).unwrap_or_default();
            let fk = ForeignKeyInfo {
                constraint_name: r.get(0),
                child_table: r.get(1),
                child_column: child_columns.first().cloned().unwrap_or_default(),
                parent_table: r.get(2),
                parent_column: parent_columns.first().cloned().unwrap_or_default(),
                child_columns,
                parent_columns,
            };
            if seen.insert((fk.child_table.clone(), fk.constraint_name.clone())) {
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

/// The bare type name: `format_type` adds typmods ("numeric(10,2)",
/// "character varying(10)") that never change how a value is read.
fn base_type(pg_type: &str) -> &str {
    pg_type.split('(').next().unwrap_or(pg_type)
}

/// Types the driver decodes exactly into the Rust type `pg_cell_to_value`
/// asks for. Array types (`integer[]`, `character varying(10)[]`) are never
/// native, whatever their element.
fn is_native(pg_type: &str) -> bool {
    !pg_type.contains("[]")
        && matches!(
            base_type(pg_type),
            "smallint"
                | "int2"
                | "integer"
                | "int"
                | "int4"
                | "bigint"
                | "int8"
                | "double precision"
                | "float8"
                | "boolean"
                | "bool"
                | "text"
                | "varchar"
                | "character varying"
                | "char"
                | "character"
                | "bpchar"
                | "name"
                | "bytea"
        )
}

/// A float written as text, exactly: non-finite forms stay text.
fn float_text(s: String) -> Value {
    match s.parse::<f64>() {
        Ok(f) if f.is_finite() => Value::Float(f),
        _ => Value::Text(s),
    }
}

/// `numeric` text -> Float while an f64 holds every digit (15 significant);
/// beyond that the exact decimal stays Text rather than silently rounding.
fn numeric_text(s: String) -> Value {
    let significant = s
        .chars()
        .filter(char::is_ascii_digit)
        .skip_while(|c| *c == '0')
        .count();
    if significant <= 15 {
        float_text(s)
    } else {
        Value::Text(s)
    }
}

/// Convert a PostgreSQL cell to an Aikoql Value. Each family reads as
/// `Option<T>` so a NULL cell is recognized by the driver itself — the old
/// try-get heuristic misread nullable non-NULL integers as NULL (item 4 red
/// debug found FK values importing as Value::Null). Columns that are not
/// `is_native` arrive as text (the import selects them `::text`) and are
/// parsed here, so no decodable value ever becomes NULL by a type mismatch.
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
    if !is_native(pg_type) {
        return opt(row, col_idx, |s: String| match base_type(pg_type) {
            "real" | "float4" => float_text(s),
            "numeric" | "decimal" => numeric_text(s),
            "vector" => parse_vector(s),
            _ => Value::Text(s),
        });
    }
    match base_type(pg_type) {
        "smallint" | "int2" => opt(row, col_idx, |v: i16| Value::Int(v as i64)),
        "integer" | "int" | "int4" => opt(row, col_idx, |v: i32| Value::Int(v as i64)),
        "bigint" | "int8" => opt(row, col_idx, Value::Int),
        "double precision" | "float8" => opt(row, col_idx, Value::Float),
        "boolean" | "bool" => opt(row, col_idx, Value::Bool),
        "bytea" => opt(row, col_idx, Value::Bytes),
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
    fn only_exactly_decodable_types_are_read_natively() {
        for t in [
            "smallint",
            "integer",
            "bigint",
            "double precision",
            "boolean",
            "text",
            "character varying(10)",
            "character(3)",
            "bytea",
        ] {
            assert!(is_native(t), "{t}");
        }
        for t in [
            "real",
            "numeric(10,2)",
            "timestamp with time zone",
            "date",
            "uuid",
            "jsonb",
            "integer[]",
            "character varying(10)[]",
            "interval",
            "inet",
            "vector(3)",
            "mood",
        ] {
            assert!(!is_native(t), "{t}");
        }
    }

    #[test]
    fn numeric_text_keeps_every_digit() {
        assert_eq!(numeric_text("12345.67".into()), Value::Float(12345.67));
        assert_eq!(numeric_text("0.0000123".into()), Value::Float(0.0000123));
        assert_eq!(numeric_text("-9.50".into()), Value::Float(-9.5));
        // 16+ significant digits would round in an f64: stay exact text
        let big = "12345678901234567890.1234567890".to_string();
        assert_eq!(numeric_text(big.clone()), Value::Text(big));
        assert_eq!(numeric_text("NaN".into()), Value::Text("NaN".into()));
        assert_eq!(
            float_text("Infinity".into()),
            Value::Text("Infinity".into())
        );
        assert_eq!(float_text("9.4".into()), Value::Float(9.4));
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
