//! Shared live-connector harness (MVP-QA-001 Suite D/E, GATE-04).
#![allow(dead_code)] // harness pieces are consumed as TDD items 2..13 land
//!
//! Env-gate convention (same as real_model_bench.rs): each `Live::*`
//! constructor returns `None` with a `[SKIP]` notice when its `AIKOQL_TEST_*`
//! variable is unset; when the variable IS set the constructor probes the
//! live database and panics on failure — env set means the operator wants
//! the live test, and a dead database must fail loudly, never silently skip.
//!
//! ```text
//! docker compose --profile full up -d
//! $env:AIKOQL_TEST_PG_DSN      = "host=localhost port=5433 user=aikoql password=aikoql-dev-only dbname=knowledge"
//! $env:AIKOQL_TEST_PGVECTOR_DSN = $env:AIKOQL_TEST_PG_DSN
//! $env:AIKOQL_TEST_MONGO_URI   = "mongodb://localhost:27017"
//! $env:AIKOQL_TEST_MONGO_DB    = "knowledge"
//! $env:AIKOQL_TEST_NEO4J_URI   = "http://localhost:7474"
//! $env:AIKOQL_TEST_NEO4J_USER  = "neo4j"
//! $env:AIKOQL_TEST_NEO4J_PASSWORD = "password-dev-only"
//! cargo test -p aikoql-mcp --test connector_certification -- --nocapture
//! ```

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// One live source, probed at construction (connect + trivial query).
#[derive(Debug, Clone)]
pub struct Live {
    /// Source label used in messages: "pg", "pgvector", "mongo", "neo4j".
    pub kind: &'static str,
    /// PostgreSQL/PGVector conn string (`host=... user=... dbname=...`).
    pub dsn: String,
    /// MongoDB URI.
    pub mongo_uri: String,
    /// MongoDB database name.
    pub mongo_db: String,
    /// Neo4j base URL.
    pub neo4j_uri: String,
    pub neo4j_user: String,
    pub neo4j_password: String,
}

impl Live {
    fn env_opt(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    }

    fn skip(kind: &str, vars: &[&str]) -> Option<Live> {
        eprintln!("[SKIP] no live {kind} — unset: {}", vars.join(", "));
        None
    }

    pub fn pg() -> Option<Live> {
        let dsn = match Self::env_opt("AIKOQL_TEST_PG_DSN") {
            Some(d) => d,
            None => return Self::skip("postgres", &["AIKOQL_TEST_PG_DSN"]),
        };
        let mut conn = aikoql_postgres::PostgresConnector::connect(&dsn)
            .unwrap_or_else(|e| panic!("live pg probe failed: {e}"));
        conn.list_tables()
            .unwrap_or_else(|e| panic!("live pg probe failed: {e}"));
        Some(Live {
            kind: "pg",
            dsn,
            ..Live::blank()
        })
    }

    pub fn pgvector() -> Option<Live> {
        let dsn = match Self::env_opt("AIKOQL_TEST_PGVECTOR_DSN") {
            Some(d) => d,
            None => return Self::skip("pgvector", &["AIKOQL_TEST_PGVECTOR_DSN"]),
        };
        let mut conn = aikoql_postgres::PostgresConnector::connect(&dsn)
            .unwrap_or_else(|e| panic!("live pgvector probe failed: {e}"));
        conn.list_tables()
            .unwrap_or_else(|e| panic!("live pgvector probe failed: {e}"));
        Some(Live {
            kind: "pgvector",
            dsn,
            ..Live::blank()
        })
    }

    pub fn mongo() -> Option<Live> {
        let (uri, db) = match (
            Self::env_opt("AIKOQL_TEST_MONGO_URI"),
            Self::env_opt("AIKOQL_TEST_MONGO_DB"),
        ) {
            (Some(u), Some(d)) => (u, d),
            _ => {
                return Self::skip(
                    "mongodb",
                    &["AIKOQL_TEST_MONGO_URI", "AIKOQL_TEST_MONGO_DB"],
                )
            }
        };
        let conn = aikoql_mongodb::MongoConnector::connect(&uri, &db)
            .unwrap_or_else(|e| panic!("live mongo probe failed: {e}"));
        conn.list_collections()
            .unwrap_or_else(|e| panic!("live mongo probe failed: {e}"));
        Some(Live {
            kind: "mongo",
            mongo_uri: uri,
            mongo_db: db,
            ..Live::blank()
        })
    }

    pub fn neo4j() -> Option<Live> {
        let uri = match Self::env_opt("AIKOQL_TEST_NEO4J_URI") {
            Some(u) => u,
            None => {
                return Self::skip(
                    "neo4j",
                    &[
                        "AIKOQL_TEST_NEO4J_URI",
                        "AIKOQL_TEST_NEO4J_USER",
                        "AIKOQL_TEST_NEO4J_PASSWORD",
                    ],
                )
            }
        };
        let user = Self::env_opt("AIKOQL_TEST_NEO4J_USER").unwrap_or_else(|| "neo4j".into());
        let password =
            Self::env_opt("AIKOQL_TEST_NEO4J_PASSWORD").unwrap_or_else(|| "password".into());
        let conn = aikoql_neo4j::Neo4jConnector::connect(&uri, &user, &password)
            .unwrap_or_else(|e| panic!("live neo4j probe failed: {e}"));
        conn.list_labels()
            .unwrap_or_else(|e| panic!("live neo4j probe failed: {e}"));
        Some(Live {
            kind: "neo4j",
            neo4j_uri: uri,
            neo4j_user: user,
            neo4j_password: password,
            ..Live::blank()
        })
    }

    fn blank() -> Live {
        Live {
            kind: "",
            dsn: String::new(),
            mongo_uri: String::new(),
            mongo_db: String::new(),
            neo4j_uri: String::new(),
            neo4j_user: String::new(),
            neo4j_password: String::new(),
        }
    }
}

/// Fresh temp db path for this process (deleted if it exists), same pattern
/// as mcp_real_world.rs.
pub fn temp_db(suffix: &str) -> String {
    let path = std::env::temp_dir().join(format!("mcp-{suffix}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path.to_string_lossy().into_owned()
}

/// Freshest-built aikoql-mcp binary (same resolution as McpClient::start in
/// mcp_real_world.rs — a stale release binary would silently test old code).
pub fn binary_path() -> PathBuf {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let exe = if cfg!(windows) {
        "aikoql-mcp.exe"
    } else {
        "aikoql-mcp"
    };
    let release_bin = workspace_root.join("target/release").join(exe);
    let debug_bin = workspace_root.join("target/debug").join(exe);
    let newest = |a: &std::path::Path, b: &std::path::Path| -> bool {
        let m = |p: &std::path::Path| {
            p.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH)
        };
        m(a) >= m(b)
    };
    let bin = match (release_bin.exists(), debug_bin.exists()) {
        (true, true) if newest(&debug_bin, &release_bin) => debug_bin,
        (true, false) => release_bin,
        _ => debug_bin,
    };
    assert!(
        bin.exists(),
        "aikoql-mcp binary not built: {}",
        bin.display()
    );
    eprintln!("Using binary: {}", bin.display());
    bin
}

pub struct ImportOut {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Run `aikoql-mcp <args...>` (args start with the subcommand, e.g.
/// `["import","postgres",conn_str,db]`); captures stdout+stderr.
pub fn run_import(args: &[&str]) -> ImportOut {
    let bin = binary_path();
    let out = Command::new(&bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
    ImportOut {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Panic with the captured output — makes red-test failures self-explanatory.
pub fn assert_import_ok(out: &ImportOut, what: &str) {
    assert!(
        out.status.success(),
        "import {what} failed ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        out.stdout,
        out.stderr
    );
}
