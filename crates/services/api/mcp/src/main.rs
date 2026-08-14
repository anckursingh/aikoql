#![recursion_limit = "512"]
#![allow(clippy::too_many_arguments)]
//! aikoql-mcp — MCP server for the Knowledge Kernel.
//!
//! Exposes the MRFC-0011 Class A syscalls as MCP tools over the stdio
//! transport (newline-delimited JSON-RPC 2.0). `notify` is intentionally not
//! exposed (streaming; lands with durable CDC in Phase 2).
//!
//! Protocol surface: initialize, ping, tools/list, tools/call.
//! Logs go to stderr; stdout carries protocol frames only.
//! Structured tracing via `tracing` with env-filter (`RUST_LOG`).

mod api_rest;
mod error_codes;
mod graph_ui;
mod knowledge_runtime;
mod rate_limiter;
mod shell;
mod studio;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

pub(crate) struct HttpSession {
    pub username: String,
    pub roles: Vec<String>,
    pub created: Instant,
}

// Per-connection MCP session identity (MRFC-0040).
#[derive(Clone, Debug)]
struct McpSession {
    agent_id: String,
    run_id: Option<String>,
    tenant: Option<String>,
    roles: Vec<String>,
}

impl Default for McpSession {
    fn default() -> Self {
        McpSession {
            agent_id: "mcp-agent".into(),
            run_id: None,
            tenant: None,
            roles: vec![],
        }
    }
}

use aikoql_graph::*;
use aikoql_kernel::ir::*;
use aikoql_kernel::knowledge::ontology::{
    discover_ontology, OntologyDef, OntologyRegistry, ONTOLOGY_TYPE,
};
use aikoql_kernel::lifecycle::schema::SchemaRegistry;
use aikoql_kernel::*;
use aikoql_scheduler::Scheduler;
#[cfg(feature = "embedding-openai")]
use aikoql_semantic::provider::OpenAiEmbeddingProvider;
use aikoql_semantic::{EmbeddingEnricher, SemanticEngine};
use serde_json::{json, Value as J};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info, info_span, warn};

static SERVER_START: OnceLock<Instant> = OnceLock::new();
static ACTIVE_CONNECTIONS: AtomicU64 = AtomicU64::new(0);
static STREAM_ID: AtomicU64 = AtomicU64::new(0);
static MEMORY_DIR: OnceLock<String> = OnceLock::new();

const PROTOCOL_VERSION: &str = "2024-11-05";

#[allow(unused_assignments)]
fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Handle --version and --help before anything else.
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("aikoql-mcp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }

    // Subcommand routing: find first positional arg, then use args after it.
    let subcmd_idx = args.iter().skip(1).position(|a| !a.starts_with('-'));
    let subcmd = subcmd_idx.map(|i| args[i + 1].as_str());
    let arg_after = subcmd_idx.and_then(|i| args.get(i + 2)).map(String::as_str);
    let arg_after2 = subcmd_idx.and_then(|i| args.get(i + 3)).map(String::as_str);

    match subcmd {
        Some("shell") => {
            // Accept: aikoql-mcp shell [--tenant NAME] [DB_PATH]
            let mut db = "./aikoql.redb";
            let mut tenant: Option<&str> = None;
            let tail_args: Vec<&str> = args
                .iter()
                .skip(subcmd_idx.unwrap() + 2)
                .map(String::as_str)
                .collect();
            let mut ti = 0;
            while ti < tail_args.len() {
                match tail_args[ti] {
                    "--tenant" => {
                        if ti + 1 < tail_args.len() {
                            tenant = Some(tail_args[ti + 1]);
                            ti += 2;
                        } else {
                            ti += 1;
                        }
                    }
                    _ => {
                        db = tail_args[ti];
                        ti += 1;
                    }
                }
            }
            shell::run_shell(db, tenant);
            return;
        }
        Some("backup") => {
            run_backup(arg_after.unwrap_or("./aikoql.redb"));
            return;
        }
        Some("restore") => {
            let backup = arg_after.unwrap_or_else(|| {
                eprintln!("Usage: aikoql-mcp restore <BACKUP_DIR> [DB_PATH]");
                std::process::exit(1);
            });
            let target = arg_after2.unwrap_or("./aikoql.redb");
            run_restore(backup, target);
            return;
        }
        Some("audit") => {
            run_audit(arg_after.unwrap_or("./aikoql.redb"));
            return;
        }
        Some("ingest-dir") => {
            let path = arg_after.unwrap_or(".");
            let db = arg_after2.unwrap_or("./aikoql.redb");
            let mut parallel = false;
            let mut incremental = false;
            let tail_args: Vec<&str> = args
                .iter()
                .skip(subcmd_idx.unwrap() + 2)
                .map(String::as_str)
                .collect();
            for a in &tail_args {
                if *a == "--parallel" {
                    parallel = true;
                } else if *a == "--incremental" {
                    incremental = true;
                }
            }
            run_ingest_dir(path, db, parallel, incremental);
            return;
        }
        Some("report") => {
            let path = arg_after.unwrap_or(".");
            run_report(path);
            return;
        }
        Some("import") => {
            // import <source> <source-args...>
            //   import postgres <conn_str> [--tenant NAME] [--table TABLE] [DB_PATH]
            //   import sqlite <file.db> [--tenant NAME] [--table TABLE] [DB_PATH]
            let ti_args: Vec<&str> = args
                .iter()
                .skip(subcmd_idx.unwrap() + 2)
                .map(String::as_str)
                .collect();
            if ti_args.is_empty() {
                eprintln!("Usage: aikoql-mcp import <SOURCE> [ARGS...]");
                eprintln!("Sources: postgres, sqlite, mongodb, neo4j");
                eprintln!("  import postgres <CONN_STR> [--tenant NAME] [--table TABLE] [DB_PATH]");
                eprintln!("  import sqlite <FILE.db> [--tenant NAME] [--table TABLE] [DB_PATH]");
                eprintln!(
                    "  import mongodb <URI> --db <NAME> [--collection C] [--tenant T] [DB_PATH]"
                );
                eprintln!("  import neo4j <URI> [--user U] [--password P] [--label L] [--tenant T] [DB_PATH]");
                std::process::exit(1);
            }
            match ti_args[0] {
                "postgres" => {
                    let mut conn_str: Option<&str> = None;
                    let mut target_db = "./aikoql.redb";
                    let mut tenant: Option<&str> = None;
                    let mut table_filter: Option<&str> = None;
                    let mut ti = 1;
                    while ti < ti_args.len() {
                        match ti_args[ti] {
                            "--tenant" => {
                                if ti + 1 < ti_args.len() {
                                    tenant = Some(ti_args[ti + 1]);
                                    ti += 2;
                                } else {
                                    ti += 1;
                                }
                            }
                            "--table" => {
                                if ti + 1 < ti_args.len() {
                                    table_filter = Some(ti_args[ti + 1]);
                                    ti += 2;
                                } else {
                                    ti += 1;
                                }
                            }
                            _ if !ti_args[ti].starts_with("--") => {
                                if conn_str.is_none() {
                                    conn_str = Some(ti_args[ti]);
                                    ti += 1;
                                } else {
                                    target_db = ti_args[ti];
                                    ti += 1;
                                }
                            }
                            _ => {
                                ti += 1;
                            }
                        }
                    }
                    let cs = conn_str.unwrap_or_else(|| {
                        eprintln!("Usage: aikoql-mcp import postgres <CONN_STR> [--tenant NAME] [--table TABLE] [DB_PATH]");
                        std::process::exit(1);
                    });
                    run_pg_import(cs, target_db, tenant, table_filter);
                }
                "neo4j" => {
                    let mut uri: Option<&str> = None;
                    let mut user = "neo4j";
                    let mut password = "password";
                    let mut target_db = "./aikoql.redb";
                    let mut tenant: Option<&str> = None;
                    let mut label_filter: Option<&str> = None;
                    let mut ni = 1;
                    while ni < ti_args.len() {
                        match ti_args[ni] {
                            "--user" => {
                                if ni + 1 < ti_args.len() {
                                    user = ti_args[ni + 1];
                                    ni += 2;
                                } else {
                                    ni += 1;
                                }
                            }
                            "--password" => {
                                if ni + 1 < ti_args.len() {
                                    password = ti_args[ni + 1];
                                    ni += 2;
                                } else {
                                    ni += 1;
                                }
                            }
                            "--tenant" => {
                                if ni + 1 < ti_args.len() {
                                    tenant = Some(ti_args[ni + 1]);
                                    ni += 2;
                                } else {
                                    ni += 1;
                                }
                            }
                            "--label" => {
                                if ni + 1 < ti_args.len() {
                                    label_filter = Some(ti_args[ni + 1]);
                                    ni += 2;
                                } else {
                                    ni += 1;
                                }
                            }
                            _ if !ti_args[ni].starts_with("--") => {
                                if uri.is_none() {
                                    uri = Some(ti_args[ni]);
                                    ni += 1;
                                } else {
                                    target_db = ti_args[ni];
                                    ni += 1;
                                }
                            }
                            _ => {
                                ni += 1;
                            }
                        }
                    }
                    let u = uri.unwrap_or_else(|| {
                        eprintln!("Usage: aikoql-mcp import neo4j <URI> [--user U] [--password P] [--label L] [--tenant T] [DB_PATH]");
                        std::process::exit(1);
                    });
                    run_neo4j_import(u, user, password, target_db, tenant, label_filter);
                }
                "mongodb" => {
                    let mut uri: Option<&str> = None;
                    let mut database: Option<&str> = None;
                    let mut target_db = "./aikoql.redb";
                    let mut tenant: Option<&str> = None;
                    let mut coll_filter: Option<&str> = None;
                    let mut mi = 1;
                    while mi < ti_args.len() {
                        match ti_args[mi] {
                            "--db" | "--database" => {
                                if mi + 1 < ti_args.len() {
                                    database = Some(ti_args[mi + 1]);
                                    mi += 2;
                                } else {
                                    mi += 1;
                                }
                            }
                            "--tenant" => {
                                if mi + 1 < ti_args.len() {
                                    tenant = Some(ti_args[mi + 1]);
                                    mi += 2;
                                } else {
                                    mi += 1;
                                }
                            }
                            "--collection" => {
                                if mi + 1 < ti_args.len() {
                                    coll_filter = Some(ti_args[mi + 1]);
                                    mi += 2;
                                } else {
                                    mi += 1;
                                }
                            }
                            _ if !ti_args[mi].starts_with("--") => {
                                if uri.is_none() {
                                    uri = Some(ti_args[mi]);
                                    mi += 1;
                                } else if database.is_none() {
                                    database = Some(ti_args[mi]);
                                    mi += 1;
                                } else {
                                    target_db = ti_args[mi];
                                    mi += 1;
                                }
                            }
                            _ => {
                                mi += 1;
                            }
                        }
                    }
                    let u = uri.unwrap_or_else(|| {
                        eprintln!("Usage: aikoql-mcp import mongodb <URI> --db <NAME> [--collection C] [--tenant T] [DB_PATH]");
                        std::process::exit(1);
                    });
                    let db = database.unwrap_or_else(|| {
                        eprintln!("Missing --db <DATABASE_NAME>");
                        std::process::exit(1);
                    });
                    run_mongo_import(u, db, target_db, tenant, coll_filter);
                }
                "sqlite" => {
                    let mut source_file: Option<&str> = None;
                    let mut target_db = "./aikoql.redb";
                    let mut tenant: Option<&str> = None;
                    let mut table_filter: Option<&str> = None;
                    let mut si = 1;
                    while si < ti_args.len() {
                        match ti_args[si] {
                            "--tenant" => {
                                if si + 1 < ti_args.len() {
                                    tenant = Some(ti_args[si + 1]);
                                    si += 2;
                                } else {
                                    si += 1;
                                }
                            }
                            "--table" => {
                                if si + 1 < ti_args.len() {
                                    table_filter = Some(ti_args[si + 1]);
                                    si += 2;
                                } else {
                                    si += 1;
                                }
                            }
                            _ if !ti_args[si].starts_with("--") => {
                                if source_file.is_none() {
                                    source_file = Some(ti_args[si]);
                                    si += 1;
                                } else {
                                    target_db = ti_args[si];
                                    si += 1;
                                }
                            }
                            _ => {
                                si += 1;
                            }
                        }
                    }
                    let sf = source_file.unwrap_or_else(|| {
                        eprintln!("Usage: aikoql-mcp import sqlite <FILE.db> [--tenant NAME] [--table TABLE] [DB_PATH]");
                        std::process::exit(1);
                    });
                    run_sqlite_import(sf, target_db, tenant, table_filter);
                }
                other => {
                    eprintln!(
                        "Unknown import source: {}. Supported: postgres, sqlite",
                        other
                    );
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("keygen") => {
            run_keygen(arg_after.unwrap_or("./aikoql.key"));
            return;
        }
        Some("help") => {
            print_usage();
            return;
        }
        _ => {} // fall through to server mode
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut listen_addr: Option<String> = None;
    let mut metrics_addr: Option<String> = None;
    let mut db_path = "./aikoql.redb".to_string();
    let mut memory_dir = "./memory".to_string();
    let mut embedding_provider: Option<String> = None;
    #[allow(unused_assignments, unused_variables)]
    let mut embedding_base_url = String::new();
    let mut embedding_model = String::new();
    #[allow(unused_assignments, unused_variables)]
    let mut embedding_api_key: Option<String> = None;
    // `serve` is the documented server-mode subcommand; start flag parsing
    // after it so it isn't swallowed as the db path (creates a stray `serve`
    // redb file in the CWD otherwise). Bare `aikoql-mcp [DB]` still works.
    let mut i = if subcmd == Some("serve") {
        subcmd_idx.unwrap() + 2
    } else {
        1
    };
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => {
                listen_addr = Some(
                    args.get(i + 1)
                        .cloned()
                        .unwrap_or_else(|| "127.0.0.1:9090".into()),
                );
                i += 2;
            }
            "--metrics-addr" => {
                metrics_addr = Some(
                    args.get(i + 1)
                        .cloned()
                        .unwrap_or_else(|| "127.0.0.1:9091".into()),
                );
                i += 2;
            }
            "--memory-dir" => {
                memory_dir = args
                    .get(i + 1)
                    .cloned()
                    .unwrap_or_else(|| "./memory".into());
                i += 2;
            }
            "--embedding-provider" => {
                embedding_provider =
                    Some(args.get(i + 1).cloned().unwrap_or_else(|| "candle".into()));
                i += 2;
            }
            "--embedding-base-url" => {
                embedding_base_url = args
                    .get(i + 1)
                    .cloned()
                    .unwrap_or_else(|| "http://localhost:11434".into());
                i += 2;
            }
            "--embedding-model" => {
                embedding_model = args
                    .get(i + 1)
                    .cloned()
                    .unwrap_or_else(|| "nomic-embed-text".into());
                i += 2;
            }
            "--embedding-api-key" => {
                embedding_api_key = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            _ if args[i].starts_with("--") => {
                eprintln!("Unknown option: {} (run `aikoql-mcp help`)", args[i]);
                std::process::exit(1);
            }
            _ => {
                db_path = args[i].clone();
                i += 1;
            }
        }
    }
    MEMORY_DIR.set(memory_dir).ok();

    let engine = RedbEngine::open(&db_path).expect("open store");
    let kernel =
        Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xA9C9).expect("open kernel");

    #[cfg(feature = "embedding-openai")]
    let url = if embedding_base_url.is_empty() {
        "http://localhost:11434".to_string()
    } else {
        embedding_base_url.clone()
    };
    let model = if embedding_model.is_empty() {
        "nomic-embed-text".to_string()
    } else {
        embedding_model.clone()
    };

    // Build provider Arc first so we can share it with the enrichment engine.
    let emb_provider: Option<Arc<dyn EmbeddingProvider>> = match embedding_provider.as_deref() {
        Some("openai") => {
            #[cfg(feature = "embedding-openai")]
            {
                let p = OpenAiEmbeddingProvider::new(&url, &model, embedding_api_key.as_deref());
                Some(Arc::new(p))
            }
            #[cfg(not(feature = "embedding-openai"))]
            {
                info!(
                    "openai embedding requested but binary not compiled with embedding-openai feature"
                );
                None
            }
        }
        _ => {
            // Default: Candle (offline, CPU-only, ~90 MB HF model download on first use)
            #[cfg(feature = "embedding-candle")]
            {
                let p = aikoql_semantic::provider::CandleEmbedding::new()
                    .expect("load candle embedding model");
                Some(Arc::new(p))
            }
            #[cfg(not(feature = "embedding-candle"))]
            {
                info!(
                    "no embedding provider compiled in — activate embedding-candle or embedding-openai feature"
                );
                None
            }
        }
    };

    let kernel = if let Some(ref p) = emb_provider {
        kernel.with_embedding_provider(p.clone())
    } else {
        kernel
    };
    let kernel = Arc::new(kernel);

    // Start background enrichment if embedding provider is configured.
    // ponytail: synchronous scan on startup — blocks until all KOs are enriched.
    // Move to background thread when startup latency matters.
    if let Some(enrichment_provider) = emb_provider {
        let enricher = EmbeddingEnricher::new(enrichment_provider, &model);
        let engine = Arc::new(SemanticEngine::new(Arc::new(enricher)));
        let sched = Scheduler::new();
        sched.register(engine);
        if let Err(e) = sched.start_all(&kernel) {
            info!(error = %e, "background enrichment scan failed");
        }
    }

    let db_path = Arc::new(db_path);
    SERVER_START.set(Instant::now()).ok();

    // Load ontology from stored Ontology KOs (MRFC-0041).
    // Prefer manually-curated ontologies over auto-discovered ones.
    // If multiple exist, pick the latest non-discovered, fall back to discovered.
    let ontology: Arc<OntologyRegistry> = {
        let subj = Subject::with_roles("system", &["admin"]);
        match kernel.scan_by_type(&subj, ONTOLOGY_TYPE) {
            Ok(kos) if !kos.is_empty() => {
                let manual: Vec<_> = kos
                    .iter()
                    .filter(|ko| !ko.metadata.tags.contains(&"auto-discovered".to_string()))
                    .collect();
                let candidate = if !manual.is_empty() {
                    // Prefer the latest manually-created ontology.
                    manual
                        .into_iter()
                        .max_by_key(|ko| ko.commit_ts)
                        .expect("manual is non-empty")
                } else {
                    // Fall back to latest auto-discovered.
                    kos.iter()
                        .max_by_key(|ko| ko.commit_ts)
                        .expect("kos is non-empty")
                };
                match OntologyDef::from_ko(candidate) {
                    Ok(def) => {
                        let source = if candidate
                            .metadata
                            .tags
                            .contains(&"auto-discovered".to_string())
                        {
                            "auto-discovered"
                        } else {
                            "manual"
                        };
                        info!(namespace = %def.namespace, version = %def.version,
                              classes = def.classes.len(),
                              mappings = def.mappings.len(),
                              source = source,
                              "ontology loaded");
                        Arc::new(OntologyRegistry::new(def).expect("load ontology"))
                    }
                    Err(e) => {
                        info!(error = %e, "ontology KO found but failed to decode — using empty registry");
                        Arc::new(OntologyRegistry::empty())
                    }
                }
            }
            _ => {
                info!("no ontology KO found — using empty registry");
                Arc::new(OntologyRegistry::empty())
            }
        }
    };

    // Optional HTTP metrics endpoint for Prometheus + Kubernetes probes.
    if let Some(ref addr) = metrics_addr {
        let k = kernel.clone();
        let ont = ontology.clone();
        let addr_clone = addr.clone();
        let db_for_metrics = db_path.clone();
        std::thread::spawn(move || serve_metrics(k, ont, &addr_clone, &db_for_metrics));
        info!(addr = %addr, "metrics HTTP server started");
    }

    if let Some(addr) = listen_addr {
        // TCP mode: accept multiple connections, one handler thread each.
        let listener = TcpListener::bind(&addr).expect("bind TCP listener");
        info!(addr = %addr, db = %db_path, "aikoql-mcp TCP server ready");
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let k = kernel.clone();
                    let db = db_path.clone();
                    thread::spawn(move || handle_tcp_client(&k, stream, db));
                }
                Err(e) => error!("accept error: {}", e),
            }
        }
    } else {
        // Stdio mode: single connection (original behavior).
        info!(db = %db_path, protocol = PROTOCOL_VERSION, "aikoql-mcp ready");
        let stdout = Arc::new(Mutex::new(std::io::stdout()));
        let mut sub_ids: HashSet<String> = HashSet::new();
        let mut rate_limits: HashMap<String, u64> = HashMap::new();
        let mut session = McpSession::default();
        const RATE_LIMIT: u64 = 1000;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg: J = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(e) => {
                    let mut out = stdout.lock().unwrap();
                    write_frame(
                        &mut *out,
                        err_frame(&J::Null, -32700, &format!("parse error: {}", e)),
                    );
                    continue;
                }
            };
            handle_message(
                &kernel,
                &mut sub_ids,
                &stdout,
                &mut rate_limits,
                RATE_LIMIT,
                &db_path,
                &mut session,
                msg,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// CLI subcommands
// ---------------------------------------------------------------------------

fn print_usage() {
    println!(concat!(
        "aikoql — Knowledge Database Suite\n",
        "\n",
        "Usage: aikoql-mcp <COMMAND> [OPTIONS]\n",
        "\n",
        "Commands:\n",
        "  shell [DB]             Interactive aikoql shell (REPL)\n",
        "  serve [OPTIONS] [DB]   Start MCP server (default: stdio mode)\n",
        "  backup [DB]            Create a verified backup\n",
        "  restore BACKUP [DB]    Restore from a backup\n",
        "  audit [DB]             Print encryption compliance report\n",
        "  keygen [PATH]          Generate an encryption master key\n",
        "  import <SOURCE> [ARGS]  Import from DB (postgres, sqlite, mongodb)\n",
        "  ingest-dir [PATH] [DB] [--parallel] [--incremental] Ingest directory into knowledge base\n",
        "  report [PATH]          Print knowledge report for directory\n",
        "\n",
        "Server options (serve mode):\n",
        "  --listen ADDR          TCP listen address (e.g., 127.0.0.1:9090)\n",
        "  --metrics-addr ADDR    HTTP metrics + health endpoint (e.g., 127.0.0.1:9091)\n",
        "  --embedding-provider P  Embedding provider: \"openai\" (default) or \"candle\"\n",
        "  --embedding-base-url U  OpenAI-compatible base URL (default: http://localhost:11434)\n",
        "  --embedding-model M     Model name (default: nomic-embed-text)\n",
        "  --embedding-api-key K   API key for remote endpoints (omit for Ollama)\n",
        "\n",
        "Examples:\n",
        "  aikoql-mcp shell                           # Interactive shell\n",
        "  aikoql-mcp shell :memory:                  # In-memory shell\n",
        "  aikoql-mcp serve                           # Stdio MCP server\n",
        "  aikoql-mcp serve --listen :9090            # TCP MCP server\n",
        "  aikoql-mcp serve --listen :9090 --metrics-addr :9091 ./kb.redb\n",
        "  aikoql-mcp backup ./kb.redb                # Create backup\n",
        "  aikoql-mcp restore kb.redb.backup.12345    # Restore backup\n",
        "  aikoql-mcp audit                           # Compliance report\n",
        "  aikoql-mcp keygen ./master.key             # Generate key\n",
        "  aikoql-mcp import 'host=localhost db=mydb'   # Import from PostgreSQL\n",
        "  aikoql-mcp ingest-dir                        # Ingest CWD (sequential)\n",
        "  aikoql-mcp ingest-dir ~/my-project            # Ingest specific path\n",
        "  aikoql-mcp ingest-dir . --parallel            # Parallel ingestion\n",
        "  aikoql-mcp ingest-dir . --incremental         # Incremental (git-tracked)\n",
        "  aikoql-mcp report                            # Report on CWD\n",
        "  aikoql-mcp report ~/my-project                # Report on specific path\n",
    ));
}

fn run_backup(db_path: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir_name = format!("{}.backup.{}", db_path, ts);
    std::fs::create_dir_all(&dir_name).expect("create backup dir");

    // Gather metadata, then drop kernel to release file lock before copy.
    let (seq, object_count) = {
        let engine = RedbEngine::open(db_path).expect("open source db");
        let kernel =
            Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE).expect("open kernel");
        let s = kernel
            .journal_head()
            .unwrap_or_else(|e| {
                eprintln!("Error reading journal: {}", e);
                std::process::exit(1);
            })
            .0;
        let n = kernel
            .scan_heads()
            .unwrap_or_else(|e| {
                eprintln!("Error scanning heads: {}", e);
                std::process::exit(1);
            })
            .len();
        (s, n)
    }; // kernel + engine dropped → file lock released.

    let data_path = format!("{}/data.redb", dir_name);
    std::fs::copy(db_path, &data_path).expect("copy db file");

    let meta = serde_json::json!({
        "journal_seq": seq,
        "object_count": object_count,
        "backup_ts": ts,
        "source": db_path,
    });
    std::fs::write(
        format!("{}/meta.json", dir_name),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .expect("write meta");

    println!("Backup created: {}", dir_name);
    println!("  Objects: {}", object_count);
    println!("  Journal seq: {}", seq);
}

fn run_restore(backup_dir: &str, target_path: &str) {
    let data_path = format!("{}/data.redb", backup_dir);
    if !std::path::Path::new(&data_path).exists() {
        eprintln!("Error: not a valid backup — {} not found", data_path);
        std::process::exit(1);
    }
    let meta_path = format!("{}/meta.json", backup_dir);
    if let Ok(meta_str) = std::fs::read_to_string(&meta_path) {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_str) {
            println!("Restoring from: {}", backup_dir);
            println!(
                "  Original source: {}",
                meta.get("source").and_then(|s| s.as_str()).unwrap_or("?")
            );
            println!(
                "  Object count: {}",
                meta.get("object_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            );
            println!(
                "  Journal seq: {}",
                meta.get("journal_seq")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            );
        }
    }
    std::fs::copy(&data_path, target_path).expect("restore copy");
    println!("Restored to: {}", target_path);
}

fn run_audit(db_path: &str) {
    let engine = RedbEngine::open(db_path).expect("open db");
    let kernel =
        Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE).expect("open kernel");
    match kernel.compliance_report() {
        Ok(report) => {
            let json = serde_json::to_string_pretty(&serde_json::json!({
                "encryption_enabled": report.encryption_enabled,
                "policies_registered": report.policies_registered,
                "policy_types": report.policy_types,
            }))
            .unwrap_or_else(|e| e.to_string());
            println!("{}", json);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn run_report(path: &str) {
    eprintln!("Analyzing directory: {}\n", path);

    let result = match aikoql_ingestion::ingest_directory(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    let report = aikoql_ingestion::build_report(
        &result.ir,
        path,
        result.files_processed,
        result.files_skipped,
        result.dirs_skipped,
        result.binary_skipped,
    );
    println!("{}", aikoql_ingestion::format_report(&report));
}

fn run_ingest_dir(path: &str, db_path: &str, parallel: bool, incremental: bool) {
    eprintln!("Ingesting directory: {}\n", path);

    let result = if incremental {
        eprintln!("Mode: incremental");
        match aikoql_ingestion::incremental_ingest_directory(path) {
            Ok((r, full)) => {
                eprintln!("({} ingest)\n", if full { "full" } else { "incremental" });
                r
            }
            Err(e) => {
                eprintln!("Incremental skipped: {}", e);
                return;
            }
        }
    } else if parallel {
        eprintln!("Mode: parallel");
        match aikoql_ingestion::parallel_ingest_directory(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        match aikoql_ingestion::ingest_directory(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    };

    let report = aikoql_ingestion::build_report(
        &result.ir,
        path,
        result.files_processed,
        result.files_skipped,
        result.dirs_skipped,
        result.binary_skipped,
    );
    println!("{}\n", aikoql_ingestion::format_report(&report));

    // Store as a Knowledge Object in the database.
    let engine = RedbEngine::open(db_path).expect("open db");
    let kernel =
        Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE).expect("open kernel");
    let subj = Subject::with_roles("ingest-dir", &["admin"]);

    let ir_json = serde_json::to_string(&result.ir).unwrap_or_default();
    let mut props = PropertyMap::new();
    props.insert("source_path".into(), Value::Text(path.to_string()));
    props.insert(
        "entity_count".into(),
        Value::Int(result.ir.entities.len() as i64),
    );
    props.insert(
        "fact_count".into(),
        Value::Int(result.ir.facts.len() as i64),
    );
    props.insert(
        "relation_count".into(),
        Value::Int(result.ir.relations.len() as i64),
    );
    props.insert("ir_json".into(), Value::Text(ir_json));

    match kernel.remember(RememberRequest {
        context: (&subj).into(),
        koid: None,
        expected_version: Some(0),
        idempotency_key: Some(format!("ingest-dir-{}", path)),
        metadata: Metadata {
            type_name: "aikoql:ingested-directory".into(),
            tenant: None,
            schema_version: 1,
            tags: vec!["ingest-dir".into(), "auto".into()],
        },
        properties: props,
        semantic: None,
        relationships: vec![],
        security: Some(SecurityDescriptor {
            owner: "ingest-dir".into(),
            acl: vec![],
            classification: None,
        }),
        extensions: ExtensionMap::new(),
        origin: Origin::Human,
        note: Some(format!("Directory ingestion: {}", path)),
        referential_policy: ReferentialPolicy::Permissive,
    }) {
        Ok(r) => {
            println!("Stored as knowledge object:");
            println!("  KOID: {}", r.koid.to_hex());
            println!("\nQuery with: aikoql-mcp shell {} -- then:", db_path);
            println!("  compile_context \"your task\" {} 3000", r.koid.to_hex());
        }
        Err(e) => {
            eprintln!("Warning: could not store KO: {}", e);
            println!("IR generated but not persisted.");
        }
    }
}

fn run_pg_import(
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

    let engine = RedbEngine::open(target_db).expect("open target db");
    let kernel =
        Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE).expect("open kernel");
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

fn run_sqlite_import(
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

    let engine = RedbEngine::open(target_db).expect("open target db");
    let kernel =
        Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE).expect("open kernel");
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

fn run_mongo_import(
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

    let engine = RedbEngine::open(target_db).expect("open target db");
    let kernel =
        Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE).expect("open kernel");
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

fn run_neo4j_import(
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
    let rel_types = connector.list_rel_types().unwrap_or_default();
    println!(
        "Labels: {} ({}), Relationship types: {} ({})",
        labels.len(),
        labels.join(", "),
        rel_types.len(),
        rel_types.join(", ")
    );
    println!();

    let engine = RedbEngine::open(target_db).expect("open target db");
    let kernel =
        Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE).expect("open kernel");
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

fn run_keygen(path: &str) {
    use aikoql_kernel::security::crypto::{Aes256Gcm, CryptoProvider};
    let key = Aes256Gcm::new().generate_key();
    let hex: String = key.iter().map(|b| format!("{:02x}", b)).collect();
    if path == "-" {
        println!("{}", hex);
    } else {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).expect("create key dir");
            }
        }
        std::fs::write(path, &hex).expect("write key file");
        println!("Key written to: {}", path);
        println!("Set encryption.key_path in aikoql.toml to this path.");
        println!(
            "Restrict file permissions: chmod 600 {} (Linux) or equivalent.",
            path
        );
    }
}

// ---------------------------------------------------------------------------
// TCP client handler
// ---------------------------------------------------------------------------

/// Handle one TCP client connection. Each connection gets its own subscription
/// set and rate limit counters.
fn handle_tcp_client(kernel: &Arc<Kernel>, stream: TcpStream, db_path: Arc<String>) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    info!(%peer, "client connected");
    let reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let writer = Arc::new(Mutex::new(stream));
    let mut sub_ids: HashSet<String> = HashSet::new();
    let mut rate_limits: HashMap<String, u64> = HashMap::new();
    let mut session = McpSession::default();
    const RATE_LIMIT: u64 = 1000;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: J = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(e) => {
                let mut out = writer.lock().unwrap();
                write_frame(
                    &mut *out,
                    err_frame(&J::Null, -32700, &format!("parse error: {}", e)),
                );
                continue;
            }
        };
        handle_message(
            kernel,
            &mut sub_ids,
            &writer,
            &mut rate_limits,
            RATE_LIMIT,
            &db_path,
            &mut session,
            msg,
        );
    }
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
    info!(%peer, "client disconnected");
}

fn handle_message(
    k: &Kernel,
    sub_ids: &mut HashSet<String>,
    stdout: &Arc<Mutex<impl Write + Send + 'static>>,
    rate_limits: &mut HashMap<String, u64>,
    rate_limit_max: u64,
    db_path: &Arc<String>,
    session: &mut McpSession,
    msg: J,
) {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    // Rate limiting: process-local sliding-window counter.
    //
    // Scope: This limiter is per-process. In a multi-instance deployment
    // (load balancer → N instances), each instance independently allows
    // the configured limit. For global rate limiting, use a shared
    // Redis-backed `RateLimiter` impl (see rate_limiter.rs) or gateway-level
    // enforcement.
    if method == "tools/call" {
        let count = rate_limits.entry("_connection".into()).or_insert(0);
        *count += 1;
        if *count > rate_limit_max {
            warn!(count = %count, limit = %rate_limit_max, "rate limit exceeded");
            let mut out = stdout.lock().unwrap();
            if let Some(id) = id {
                write_frame(&mut *out, err_frame(&id, -32000, "rate limit exceeded"));
            }
            return;
        }
    }
    let mut out = stdout.lock().unwrap();
    match method {
        "initialize" => {
            if let Some(id) = id {
                write_frame(
                    &mut *out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": PROTOCOL_VERSION,
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "aikoql-mcp", "version": env!("CARGO_PKG_VERSION")}
                        }
                    }),
                );
            }
        }
        "ping" => {
            if let Some(id) = id {
                write_frame(&mut *out, json!({"jsonrpc":"2.0","id":id,"result":{}}));
            }
        }
        "aikoql/stream" => {
            drop(out); // release lock during query execution
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let query = params.get("query").and_then(|q| q.as_str()).unwrap_or("");
            let subject = params
                .get("subject")
                .and_then(|s| s.as_str())
                .unwrap_or("stream-user");
            let result = execute_stream_query(k, query, subject);
            let mut out = stdout.lock().unwrap();
            match result {
                Ok((chunks, stream_id)) => {
                    let total = chunks.len();
                    // Send first chunk as the JSON-RPC response.
                    if let Some(id) = id.clone() {
                        let first = if total > 0 { &chunks[0] } else { &json!([]) };
                        write_frame(
                            &mut *out,
                            json!({
                                "jsonrpc":"2.0","id":id,"result":{
                                    "stream_id": stream_id,
                                    "chunk": 0,
                                    "total_chunks": total,
                                    "results": first
                                }
                            }),
                        );
                    }
                    // Stream remaining chunks as notification frames from a background thread.
                    if total > 1 {
                        let out_arc = stdout.clone();
                        let sid = stream_id.clone();
                        let remaining: Vec<J> = chunks.into_iter().skip(1).collect();
                        std::thread::spawn(move || {
                            let n = remaining.len();
                            for (i, chunk) in remaining.into_iter().enumerate() {
                                let chunk_idx = i + 1;
                                let done = chunk_idx == n;
                                let mut w = out_arc.lock().unwrap();
                                write_frame(
                                    &mut *w,
                                    json!({
                                        "jsonrpc":"2.0",
                                        "method":"notifications/notify",
                                        "params": {
                                            "stream_id": sid,
                                            "chunk": chunk_idx,
                                            "done": done,
                                            "results": chunk
                                        }
                                    }),
                                );
                            }
                        });
                    }
                }
                Err(e) => {
                    if let Some(id) = id {
                        write_frame(&mut *out, err_frame(&id, -32603, &e));
                    }
                }
            }
        }
        "session/init" => {
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            session.agent_id = params
                .get("agent_id")
                .and_then(|v| v.as_str())
                .unwrap_or("mcp-agent")
                .into();
            session.run_id = params
                .get("run_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            session.tenant = params
                .get("tenant")
                .and_then(|v| v.as_str())
                .map(String::from);
            session.roles = params
                .get("roles")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let resp = json!({
                "session": {
                    "agent_id": session.agent_id,
                    "run_id": session.run_id,
                    "tenant": session.tenant,
                    "roles": session.roles,
                },
                "established": true,
                "note": "Session identity established. Subsequent tool calls inherit this context."
            });
            if let Some(id) = id {
                write_frame(&mut *out, json!({"jsonrpc":"2.0","id":id,"result":resp}));
            }
        }
        "tools/list" => {
            if let Some(id) = id {
                write_frame(
                    &mut *out,
                    json!({"jsonrpc":"2.0","id":id,"result":tools_list()}),
                );
            }
        }
        "tools/call" => {
            drop(out); // release lock before tool execution
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or(J::Null);
            let args = inject_session(&args, session);
            let span = info_span!("tool_call", tool = %name);
            let result = span.in_scope(|| call_tool(k, &name, &args, db_path.as_ref(), session));
            if result.is_err() {
                error!(tool = %name, "tool call failed");
            }
            // Notifications are streamed immediately by background threads;
            // no drain needed.
            let mut out = stdout.lock().unwrap();
            if let Some(id) = id {
                let frame = match result {
                    Ok(res) => json!({"jsonrpc":"2.0","id":id,"result":res}),
                    Err((code, message)) => err_frame(&id, code, &message),
                };
                write_frame(&mut *out, frame);
            }
        }
        "notifications/subscribe" => {
            drop(out); // release lock before subscription setup
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let result = notification_subscribe(k, sub_ids, stdout, &params);
            let mut out = stdout.lock().unwrap();
            if let Some(id) = id {
                let frame = match result {
                    Ok(res) => json!({"jsonrpc":"2.0","id":id,"result":res}),
                    Err((code, message)) => err_frame(&id, code, &message),
                };
                write_frame(&mut *out, frame);
            }
        }
        "notifications/unsubscribe" => {
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let result = notification_unsubscribe(k, sub_ids, &params);
            if let Some(id) = id {
                let frame = match result {
                    Ok(res) => json!({"jsonrpc":"2.0","id":id,"result":res}),
                    Err((code, message)) => err_frame(&id, code, &message),
                };
                write_frame(&mut *out, frame);
            }
        }
        "notifications/ack" => {
            let params = msg.get("params").cloned().unwrap_or(J::Null);
            let result = notification_ack(k, &params);
            if let Some(id) = id {
                let frame = match result {
                    Ok(res) => json!({"jsonrpc":"2.0","id":id,"result":res}),
                    Err((code, message)) => err_frame(&id, code, &message),
                };
                write_frame(&mut *out, frame);
            }
        }
        m if m.starts_with("notifications/") => {}
        _ => {
            if let Some(id) = id {
                write_frame(
                    &mut *out,
                    err_frame(&id, -32601, &format!("method not found: {}", method)),
                );
            }
        }
    }
}

fn notification_subscribe(
    k: &Kernel,
    sub_ids: &mut HashSet<String>,
    stdout: &Arc<Mutex<impl Write + Send + 'static>>,
    params: &J,
) -> ToolResult {
    let id = params
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or((-32602, "missing subscription id".to_string()))?
        .to_string();
    let filter = parse_event_filter(params)?;
    let rx = k
        .subscribe(id.clone(), filter)
        .map_err(|e| (-32603, e.to_string()))?;
    // Replay missed events before the subscription becomes live.
    let replayed = k.replay(&id).map_err(|e| (-32603, e.to_string()))?;
    {
        let mut out = stdout.lock().unwrap();
        for ke in &replayed {
            write_frame(
                &mut *out,
                json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/notify",
                    "params": {"id": id.clone(), "event": ke_json(ke)}
                }),
            );
        }
    }
    // Spawn a background thread that streams notifications immediately.
    let out = stdout.clone();
    let id_clone = id.clone();
    std::thread::spawn(move || {
        for ke in rx {
            let mut out = out.lock().unwrap();
            write_frame(
                &mut *out,
                json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/notify",
                    "params": {"id": id_clone.clone(), "event": ke_json(&ke)}
                }),
            );
        }
    });
    sub_ids.insert(id);
    Ok(json!({"subscribed": true, "replayed": replayed.len()}))
}

fn notification_unsubscribe(k: &Kernel, sub_ids: &mut HashSet<String>, params: &J) -> ToolResult {
    let id = params
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or((-32602, "missing subscription id".to_string()))?
        .to_string();
    k.unsubscribe(&id).map_err(|e| (-32603, e.to_string()))?;
    sub_ids.remove(&id);
    Ok(json!({}))
}

fn notification_ack(k: &Kernel, params: &J) -> ToolResult {
    let id = params
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or((-32602, "missing subscription id".to_string()))?;
    let seq = params
        .get("seq")
        .and_then(|x| x.as_u64())
        .ok_or((-32602, "missing seq".to_string()))?;
    k.ack(id, seq).map_err(|e| (-32603, e.to_string()))?;
    Ok(json!({}))
}

fn parse_event_filter(args: &J) -> Result<EventFilter, (i64, String)> {
    let koid = args
        .get("koid")
        .and_then(|x| x.as_str())
        .map(KOID::from_hex)
        .transpose()
        .map_err(|e| (-32602, format!("invalid koid: {}", e)))?;
    let kinds = args.get("kinds").and_then(|x| x.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|x| x.as_str())
            .filter_map(parse_event_kind)
            .collect::<Vec<_>>()
    });
    Ok(EventFilter { koid, kinds })
}

fn parse_event_kind(s: &str) -> Option<EventKind> {
    match s {
        "created" => Some(EventKind::Created),
        "updated" => Some(EventKind::Updated),
        "forgotten" => Some(EventKind::Forgotten),
        "lifecycle_changed" => Some(EventKind::LifecycleChanged),
        "claim_asserted" => Some(EventKind::ClaimAsserted),
        "audit" => Some(EventKind::Audit),
        _ => None,
    }
}

fn write_frame(out: &mut impl Write, frame: J) {
    writeln!(out, "{}", frame).expect("write frame");
    out.flush().expect("flush frame");
}

fn err_frame(id: &J, code: i64, message: &str) -> J {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

type ToolResult = Result<J, (i64, String)>;

// ---------------------------------------------------------------------------
// A7: Agent Gateway — audit log, capabilities, rate limiting
// ---------------------------------------------------------------------------

use std::sync::LazyLock;

static RATE_STORE: LazyLock<Mutex<HashMap<String, (Instant, u32)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Append a JSON line to the audit log.
fn audit_log(db_path: &str, agent_id: &str, tool: &str, outcome: &str, detail: &str) {
    let log_path = format!("{}.audit.log", db_path);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = json!({
        "ts": ts,
        "agent": agent_id,
        "tool": tool,
        "outcome": outcome,
        "detail": if detail.len() > 200 { &detail[..200] } else { detail },
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = writeln!(f, "{}", entry);
    }
}

fn tool_detail(name: &str, args: &J) -> String {
    let s = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "memory_search" => format!("query={}", s("query")),
        "memory_store" => format!("name={}", s("name")),
        "memory_update" => format!("name={}", s("name")),
        "memory_delete" => format!("name={}", s("name")),
        "remember" => format!("koid={}", s("koid")),
        "get" | "explain" | "trace" | "forget" | "evolve" | "verify" => {
            format!("koid={}", s("koid"))
        }
        "find_similar" => format!("query={}", s("query")),
        "compile_context" => format!("task={}", s("task")),
        "document_ingest" => format!("path={}", s("path")),
        "session_init" => format!("agent={}", s("agent_id")),
        "import" => format!("source={}", s("source")),
        "restore" => format!("backup={}", s("backup")),
        _ => String::new(),
    }
}

/// Capability grants: which roles can call which tools.
/// Empty allowed list = unrestricted (admin/superuser).
fn check_capability(roles: &[String], tool: &str) -> Result<(), (i64, String)> {
    if roles.is_empty() || roles.contains(&"admin".to_string()) {
        return Ok(()); // admin has full access
    }

    // Sensitive tools require specific roles
    let restricted: &[(&str, &[&str])] = &[
        ("backup", &["operator"]),
        ("restore", &["operator"]),
        ("deploy_program", &["developer"]),
        ("deploy_policy", &["developer"]),
        ("deploy_workflow", &["developer"]),
        ("deploy_agent", &["developer"]),
        ("deploy_connector", &["developer"]),
        ("execute_program", &["operator"]),
        ("deploy_benchmark", &["developer"]),
        ("audit_report", &["auditor"]),
        ("compliance_report", &["auditor"]),
    ];

    for (restricted_tool, allowed_roles) in restricted {
        if tool == *restricted_tool {
            if !allowed_roles.iter().any(|r| roles.contains(&r.to_string())) {
                return Err((
                    -32001,
                    format!(
                        "capability denied: tool '{}' requires one of {:?}",
                        tool, allowed_roles
                    ),
                ));
            }
            return Ok(());
        }
    }
    Ok(())
}

/// Simple per-agent rate limiter: max calls per minute.
/// Uses a sliding window — resets after 60s.
fn check_rate(agent_id: &str, roles: &[String], max_per_minute: u32) -> Result<(), (i64, String)> {
    // ponytail: unauthenticated session (no roles) = unrestricted
    if roles.is_empty() || roles.contains(&"admin".to_string()) {
        return Ok(());
    }
    let mut store = RATE_STORE.lock().unwrap();
    let now = Instant::now();
    let window = Duration::from_secs(60);

    let should_reset = match store.get(agent_id) {
        Some((start, _)) => now.duration_since(*start) > window,
        None => true,
    };

    if should_reset {
        store.insert(agent_id.to_string(), (now, 1));
    } else {
        let count = store.get(agent_id).map(|(_, c)| *c).unwrap_or(0);
        if count >= max_per_minute {
            return Err((
                -32002,
                format!("rate limit exceeded: {} calls/min", max_per_minute),
            ));
        }
        let start = store.get(agent_id).unwrap().0;
        store.insert(agent_id.to_string(), (start, count + 1));
    }
    Ok(())
}

fn call_tool(
    k: &Kernel,
    name: &str,
    args: &J,
    db_path: &str,
    session: &mut McpSession,
) -> ToolResult {
    // A7: Check capability + rate limit before dispatch
    if let Err(e) = check_capability(&session.roles, name) {
        audit_log(db_path, &session.agent_id, name, "denied:capability", &e.1);
        return Err(e);
    }
    if let Err(e) = check_rate(&session.agent_id, &session.roles, 120) {
        audit_log(db_path, &session.agent_id, name, "denied:rate", &e.1);
        return Err(e);
    }

    let res = match name {
        "remember" => tool_remember(k, args),
        "forget" => tool_forget(k, args),
        "evolve" => tool_evolve(k, args),
        "verify" => tool_verify(k, args),
        "get" => tool_get(k, args),
        "find_similar" => tool_find_similar(k, args),
        "trace" => tool_trace(k, args),
        "explain" => tool_explain(k, args),
        "prove" => tool_prove(k, args),
        "provenance" => tool_provenance(k, args),
        "relate" => tool_relate(k, args),
        "traverse" => tool_traverse(k, args),
        "eval_recall" => tool_eval_recall(k, args),
        "eval_staleness" => tool_eval_staleness(k, args),
        "eval_contradictions" => tool_eval_contradictions(k, args),
        "aikoql" => tool_aikoql(k, args),
        "backup" => tool_backup(k, db_path),
        "verify_backup" => tool_verify_backup(args),
        "restore" => tool_restore(args, db_path),
        "list_backups" => tool_list_backups(),
        "metrics" => tool_metrics(k),
        "audit_report" => tool_audit_report(k),
        "compliance_report" => tool_compliance_report(k),
        "reason" => tool_reason(k, args),
        "infer" => tool_infer(k, args),
        "predict" => tool_predict(k, args),
        "abi_version" => tool_abi_version(k),
        "deploy_program" => tool_deploy_program(k, args),
        "execute_program" => tool_execute_program(k, args),
        "list_programs" => tool_list_programs(k, args),
        "deploy_policy" => tool_deploy_policy(k, args),
        "evaluate_policies" => tool_evaluate_policies(k, args),
        "deploy_workflow" => tool_deploy_workflow(k, args),
        "deploy_trigger" => tool_deploy_trigger(k, args),
        "add_dependency" => tool_add_dependency(k, args),
        "execute_workflow" => tool_execute_workflow(k, args),
        "check_triggers" => tool_check_triggers(k),
        "program_cache_stats" => tool_program_cache_stats(),
        "deploy_agent" => tool_deploy_agent(k, args),
        "list_agents" => tool_list_agents(k, args),
        "execute_agent" => tool_execute_agent(k, args),
        "deploy_connector" => tool_deploy_connector(k, args),
        "list_connectors" => tool_list_connectors(k, args),
        "deploy_view" => tool_deploy_view(k, args),
        "list_views" => tool_list_views(k, args),
        "deploy_report" => tool_deploy_report(k, args),
        "list_reports" => tool_list_reports(k, args),
        "deploy_benchmark" => tool_deploy_benchmark(k, args),
        "list_benchmarks" => tool_list_benchmarks(k, args),
        "document_ingest" => tool_document_ingest(k, args, db_path),
        "document_list" => tool_document_list(k, args),
        "document_status" => tool_document_status(k, args),
        "document_compile" => tool_document_compile(k, args, db_path),
        "compile_context" => tool_compile_context(k, args, db_path),
        "reconcile" => tool_reconcile(k, args, db_path),
        "connector_bridge" => tool_connector_bridge(k, args),
        "filter_secrets" => tool_filter_secrets(k, args, db_path),
        "explain_component" => tool_explain_component(k, args, db_path),
        "explain_decision" => tool_explain_decision(k, args, db_path),
        "trace_requirement" => tool_trace_requirement(k, args, db_path),
        "find_conflicts" => tool_find_conflicts(k, args, db_path),
        "find_stale" => tool_find_stale(k, args, db_path),
        "validate_change" => tool_validate_change(k, args, db_path),
        "propose_update" => tool_propose_update(k, args, db_path),
        "discover_schema" => tool_discover_schema(k),
        "discover_ontology" => tool_discover_ontology(k),
        "health" => tool_health(k),
        "agent_memory" => tool_agent_memory(k, args),
        "memory_search" => tool_memory_search(args),
        "memory_store" => tool_memory_store(args),
        "memory_update" => tool_memory_update(args),
        "memory_delete" => tool_memory_delete(args),
        "batch" => tool_batch(k, args),
        "session_init" => tool_session_init(args, session),
        "decide" => tool_decide(k, args),
        _ => Err(format!("unknown tool: {}", name)),
    };
    let wrapped = error_codes::wrap_result(res);
    if wrapped["ok"] == true {
        audit_log(
            db_path,
            &session.agent_id,
            name,
            "ok",
            &tool_detail(name, args),
        );
        Ok(json!({
            "content": [{"type": "text", "text": wrapped["data"].to_string()}],
            "isError": false
        }))
    } else {
        let err_detail = wrapped["error"].as_str().unwrap_or("unknown error");
        audit_log(db_path, &session.agent_id, name, "error", err_detail);
        Ok(json!({
            "content": [{"type": "text", "text": wrapped.to_string()}],
            "isError": true
        }))
    }
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn subject_of(args: &J) -> Subject {
    let name = args
        .get("subject")
        .and_then(|s| s.as_str())
        .unwrap_or("mcp-agent");
    let roles: Vec<String> = args
        .get("roles")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Subject {
        name: name.into(),
        roles,
    }
}

/// Inject session identity into args if not overridden per-call (MRFC-0040).
fn inject_session(args: &J, session: &McpSession) -> J {
    let mut a = args.clone();
    if a.get("subject").is_none() {
        a["subject"] = json!(session.agent_id);
    }
    if a.get("roles").is_none() && !session.roles.is_empty() {
        a["roles"] = json!(session.roles);
    }
    a
}

fn koid_of(args: &J) -> Result<KOID, String> {
    let hex = args
        .get("koid")
        .and_then(|s| s.as_str())
        .ok_or("missing argument: koid")?;
    KOID::from_hex(hex).map_err(|e| e.to_string())
}

fn json_to_value(j: &J) -> Result<Value, String> {
    Ok(match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                return Err("unsupported number".into());
            }
        }
        J::String(s) => Value::Text(s.clone()),
        J::Array(xs) => Value::List(
            xs.iter()
                .map(json_to_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        J::Object(m) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in m {
                out.insert(k.clone(), json_to_value(v)?);
            }
            Value::Map(out)
        }
    })
}

fn value_to_json(v: &Value) -> J {
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => json!(i),
        Value::Float(f) => json!(f),
        Value::Text(s) => J::String(s.clone()),
        Value::Bytes(b) => json!(format!("{} bytes", b.len())),
        Value::List(xs) => J::Array(xs.iter().map(value_to_json).collect()),
        Value::Map(m) => {
            let mut out = serde_json::Map::new();
            for (k, v) in m {
                out.insert(k.clone(), value_to_json(v));
            }
            J::Object(out)
        }
    }
}

fn parse_properties(args: &J) -> Result<PropertyMap, String> {
    let mut out = PropertyMap::new();
    if let Some(J::Object(m)) = args.get("properties") {
        for (k, v) in m {
            out.insert(k.clone(), json_to_value(v)?);
        }
    }
    Ok(out)
}

fn parse_semantic(args: &J) -> Result<Option<SemanticBlock>, String> {
    let Some(s) = args.get("semantic") else {
        return Ok(None);
    };
    let embedding = match s.get("embedding") {
        Some(J::Array(xs)) => Some(
            xs.iter()
                .map(|x| {
                    x.as_f64()
                        .map(|f| f as f32)
                        .ok_or("embedding must be numbers")
                })
                .collect::<Result<Vec<f32>, _>>()?,
        ),
        _ => None,
    };
    Ok(Some(SemanticBlock {
        embedding_model: s
            .get("embedding_model")
            .and_then(|x| x.as_str())
            .map(String::from),
        embedding,
        confidence: s
            .get("confidence")
            .and_then(|x| x.as_f64())
            .map(|f| f as f32),
        source: s.get("source").and_then(|x| x.as_str()).map(String::from),
        summary: s.get("summary").and_then(|x| x.as_str()).map(String::from),
    }))
}

fn parse_origin(args: &J) -> Origin {
    match args.get("origin").and_then(|o| o.as_str()) {
        Some("system") => Origin::System,
        Some("reason") => Origin::Reason,
        Some("semantic_enrichment") => Origin::SemanticEnrichment,
        Some(other) => Origin::Agent(other.into()),
        None => Origin::Agent("mcp-agent".into()),
    }
}

fn parse_state(args: &J) -> Result<LifecycleState, String> {
    match args.get("to").and_then(|s| s.as_str()).unwrap_or("") {
        "draft" => Ok(LifecycleState::Draft),
        "active" => Ok(LifecycleState::Active),
        "verified" => Ok(LifecycleState::Verified),
        "archived" => Ok(LifecycleState::Archived),
        "deleted" => Ok(LifecycleState::Deleted),
        other => Err(format!("invalid lifecycle state: {}", other)),
    }
}

fn parse_action(args: &J) -> Result<Action, String> {
    match args.get("action").and_then(|s| s.as_str()).unwrap_or("") {
        "read" => Ok(Action::Read),
        "write" => Ok(Action::Write),
        "evolve" => Ok(Action::Evolve),
        "delete" => Ok(Action::Delete),
        "admin" => Ok(Action::Admin),
        other => Err(format!("invalid action: {}", other)),
    }
}

fn parse_fusion(args: &J) -> Fusion {
    match args.get("fusion").and_then(|f| f.as_str()).unwrap_or("rrf") {
        "vector" => Fusion::VectorOnly,
        "text" => Fusion::TextOnly,
        "weighted" => {
            let wv = args.get("wv").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            let wt = args.get("wt").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            Fusion::Weighted { wv, wt }
        }
        _ => Fusion::Rrf { k0: 60 },
    }
}

fn parse_vector(args: &J) -> Result<Option<Vec<f32>>, String> {
    match args.get("vector") {
        Some(J::Array(xs)) => Ok(Some(
            xs.iter()
                .map(|x| x.as_f64().map(|f| f as f32).ok_or("vector must be numbers"))
                .collect::<Result<Vec<f32>, _>>()?,
        )),
        _ => Ok(None),
    }
}

fn ko_json(ko: &KnowledgeObject) -> J {
    let mut props = serde_json::Map::new();
    for (k, v) in &ko.properties {
        props.insert(k.clone(), value_to_json(v));
    }
    json!({
        "koid": ko.koid.to_hex(),
        "version": ko.version,
        "commit_ts": ko.commit_ts,
        "type_name": ko.metadata.type_name,
        "state": ko.lifecycle.state.to_string(),
        "properties": J::Object(props),
        "semantic": ko.semantic.as_ref().map(|s| json!({
            "embedding_model": s.embedding_model,
            "confidence": s.confidence,
            "source": s.source,
            "summary": s.summary,
            "embedding_dims": s.embedding.as_ref().map(|e| e.len())
        })),
        "relationships": ko.relationships.iter().map(|r| json!({
            "rel_type": r.rel_type,
            "target": r.target.to_hex(),
            "direction": if r.direction == Direction::Outbound { "outbound" } else { "inbound" }
        })).collect::<Vec<_>>(),
        "event_refs": ko.event_refs.len()
    })
}

fn ke_json(ke: &KnowledgeEvent) -> J {
    json!({
        "seq": ke.seq,
        "koid": ke.koid.to_hex(),
        "version": ke.version,
        "kind": format!("{:?}", ke.kind),
        "actor": ke.actor,
        "commit_ts": ke.commit_ts,
        "note": ke.note
    })
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

fn tool_remember(k: &Kernel, args: &J) -> Result<J, String> {
    let _span = info_span!(
        "remember",
        type_name = args
            .get("type_name")
            .and_then(|t| t.as_str())
            .unwrap_or("?")
    )
    .entered();
    let subject = subject_of(args);
    let type_name = args
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: type_name")?;
    let metadata = Metadata {
        type_name: type_name.into(),
        tenant: args
            .get("tenant")
            .and_then(|t| t.as_str())
            .map(String::from),
        schema_version: args
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32,
        tags: args
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    };
    let mut req = match args.get("koid").and_then(|x| x.as_str()) {
        Some(hex) => {
            let id = KOID::from_hex(hex).map_err(|e| e.to_string())?;
            RememberRequest::update(subject, id, metadata)
        }
        None => RememberRequest::create(subject, metadata),
    };
    req.properties = parse_properties(args)?;
    req.semantic = parse_semantic(args)?;
    // Parse optional relationships array.
    if let Some(rels) = args.get("relationships").and_then(|r| r.as_array()) {
        for rel in rels {
            if let (Some(rt), Some(target_hex)) = (
                rel.get("rel_type").and_then(|v| v.as_str()),
                rel.get("target").and_then(|v| v.as_str()),
            ) {
                if let Ok(target) = KOID::from_hex(target_hex) {
                    req.relationships.push(RelationshipRef {
                        rel_type: rt.into(),
                        target,
                        direction: aikoql_kernel::knowledge::kom::Direction::Outbound,
                    });
                }
            }
        }
    }
    req.origin = parse_origin(args);
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    req.expected_version = args.get("expected_version").and_then(|v| v.as_u64());
    req.idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .map(String::from);
    let embed_requested = args.get("embed").and_then(|v| v.as_bool()).unwrap_or(false);
    let r = k.remember(req).map_err(|e| e.to_string())?;
    let mut resp = json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts
    });
    if embed_requested {
        resp["embed"] = json!({
            "requested": true,
            "status": "pending",
            "note": "SemanticEngine will enrich this KO asynchronously. Use get() to check for embeddings."
        });
    }
    Ok(resp)
}

fn tool_forget(k: &Kernel, args: &J) -> Result<J, String> {
    let mode = match args
        .get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("tombstone")
    {
        "tombstone" => ForgetMode::Tombstone,
        "erase" => ForgetMode::Erase,
        other => return Err(format!("invalid forget mode: {}", other)),
    };
    let f = k
        .forget(
            subject_of(args),
            &koid_of(args)?,
            mode,
            args.get("expected_version").and_then(|v| v.as_u64()),
            args.get("note").and_then(|n| n.as_str()).map(String::from),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": f.koid.to_hex(), "version": f.version, "commit_ts": f.commit_ts}))
}

fn tool_evolve(k: &Kernel, args: &J) -> Result<J, String> {
    let e = k
        .evolve(
            subject_of(args),
            &koid_of(args)?,
            parse_state(args)?,
            parse_origin(args),
            args.get("expected_version").and_then(|v| v.as_u64()),
            args.get("note").and_then(|n| n.as_str()).map(String::from),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": e.koid.to_hex(),
        "version": e.version,
        "commit_ts": e.commit_ts,
        "state": e.state.to_string()
    }))
}

fn tool_verify(k: &Kernel, args: &J) -> Result<J, String> {
    k.verify(subject_of(args), &koid_of(args)?, parse_action(args)?)
        .map_err(|e| e.to_string())?;
    Ok(json!({"allowed": true}))
}

fn tool_get(k: &Kernel, args: &J) -> Result<J, String> {
    let ko = k
        .get(subject_of(args), &koid_of(args)?)
        .map_err(|e| e.to_string())?;
    Ok(ko_json(&ko))
}

fn tool_find_similar(k: &Kernel, args: &J) -> Result<J, String> {
    // IR path: when type_name is explicit, compile to IR and execute via runtime.
    if let Some(type_name) = args.get("type_name").and_then(|t| t.as_str()) {
        let subject = args
            .get("subject")
            .and_then(|s| s.as_str())
            .unwrap_or("mcp-agent");
        let k_req = args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let text = args.get("text").and_then(|t| t.as_str());
        let vector = parse_vector(args)?;
        let model = args.get("embedding_model").and_then(|t| t.as_str());

        let mut ops = vec![IrOp::Scan {
            type_name: type_name.into(),
            subject: subject.into(),
        }];
        if let Some(ref v) = vector {
            ops.push(IrOp::AnnSearch {
                vector: v.clone(),
                query_text: None,
                embedding_model: model.map(String::from),
                k: k_req,
            });
        }
        if let Some(t) = text {
            ops.push(IrOp::TextSearch {
                query: t.into(),
                k: k_req,
                scoring: None,
            });
        }
        if vector.is_some() && text.is_some() {
            let mode = match args.get("fusion").and_then(|f| f.as_str()).unwrap_or("rrf") {
                "vector" => FuseMode::VectorOnly,
                "text" => FuseMode::TextOnly,
                "weighted" => FuseMode::Weighted { wv: 0.5, wt: 0.5 },
                _ => FuseMode::Rrf { k0: 60 },
            };
            ops.push(IrOp::Fuse { mode });
        }
        let raw = IrPlan::new(ops).with_description(format!("find_similar type={}", type_name));
        let plan = aikoql_compiler::planner::Planner::optimize(&raw);
        let result = aikoql_runtime::Interpreter::execute(k, &plan).map_err(|e| e.to_string())?;
        return match result {
            aikoql_runtime::RowSet::Scored(scored) => Ok(json!({
                "results": scored.iter().map(|(koid, score, tn, version)| json!({
                    "koid": koid.to_hex(),
                    "score": score,
                    "index_lag_ms": 0,
                    "type_name": tn,
                    "version": version
                })).collect::<Vec<_>>()
            })),
            _ => Err("find_similar did not produce scored results".into()),
        };
    }

    // Fallback: no type_name — use kernel's find_similar for cross-type search.
    let fusion = parse_fusion(args);
    let vector = parse_vector(args)?;
    let res = k
        .find_similar(SimilarityQuery {
            context: subject_of(args).into(),
            filter: None,
            text: args.get("text").and_then(|t| t.as_str()).map(String::from),
            vector,
            embedding_model: args
                .get("embedding_model")
                .and_then(|t| t.as_str())
                .map(String::from),
            k: args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize,
            fusion,
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "results": res.iter().map(|s| json!({
            "koid": s.ko.koid.to_hex(),
            "score": s.score,
            "index_lag_ms": s.index_lag_ms,
            "type_name": s.ko.metadata.type_name,
            "version": s.ko.version
        })).collect::<Vec<_>>()
    }))
}

fn tool_trace(k: &Kernel, args: &J) -> Result<J, String> {
    let lin = k
        .trace(subject_of(args), &koid_of(args)?)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": lin.koid.to_hex(),
        "versions": lin.versions.iter().map(|v| json!({
            "version": v.version,
            "commit_ts": v.commit_ts,
            "state": v.state.to_string()
        })).collect::<Vec<_>>(),
        "events": lin.events.iter().map(ke_json).collect::<Vec<_>>()
    }))
}

fn tool_explain(k: &Kernel, args: &J) -> Result<J, String> {
    let ex = k
        .explain(
            subject_of(args),
            &koid_of(args)?,
            args.get("version").and_then(|v| v.as_u64()),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": ex.koid.to_hex(),
        "version": ex.version,
        "origin": format!("{:?}", ex.origin),
        "source": ex.source,
        "confidence": ex.confidence,
        "verified": ex.verified,
        "evidence": ex.evidence.iter().map(|(t, id)| json!({"rel_type": t, "target": id.to_hex()})).collect::<Vec<_>>(),
        "event_refs": ex.event_refs.iter().map(|e| json!({"seq": e.seq, "commit_ts": e.commit_ts})).collect::<Vec<_>>()
    }))
}

fn tool_provenance(k: &Kernel, args: &J) -> Result<J, String> {
    let koid = koid_of(args)?;
    let ex = k
        .explain(subject_of(args), &koid, None)
        .map_err(|e| e.to_string())?;
    let trace = k
        .trace(subject_of(args), &koid)
        .map_err(|e| e.to_string())?;

    let mut md = format!("## Provenance for `{}`\n\n", koid.to_hex());
    md.push_str(&format!(
        "- **Version:** {}\n- **Origin:** {:?}\n- **Source:** {}\n- **Confidence:** {:.2}\n- **Verified:** {}\n\n",
        ex.version,
        ex.origin,
        ex.source.as_deref().unwrap_or("unknown"),
        ex.confidence.unwrap_or(0.0),
        ex.verified,
    ));

    if !ex.evidence.is_empty() {
        md.push_str("### Evidence Chain\n\n");
        for (i, (rel_type, target)) in ex.evidence.iter().enumerate() {
            md.push_str(&format!(
                "{}. `{}` → `{}`\n",
                i + 1,
                rel_type,
                target.to_hex()
            ));
        }
        md.push('\n');
    }

    if !trace.events.is_empty() {
        md.push_str("### Audit Trail\n\n");
        for evt in &trace.events {
            md.push_str(&format!(
                "- `{:?}` @ seq={} commit_ts={}\n",
                evt.kind, evt.seq, evt.commit_ts
            ));
        }
    }

    Ok(json!({"koid": koid.to_hex(), "provenance": md}))
}

fn tool_prove(k: &Kernel, args: &J) -> Result<J, String> {
    let p = k
        .prove(subject_of(args), &koid_of(args)?)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "claim": p.claim.to_hex(),
        "events": p.events,
        "chain_valid": p.chain_valid,
        "head_audit_hash": p.head_audit_hash
    }))
}

fn tool_relate(k: &Kernel, args: &J) -> Result<J, String> {
    let from = args
        .get("from")
        .and_then(|x| x.as_str())
        .ok_or("missing argument: from")?;
    let to = args
        .get("to")
        .and_then(|x| x.as_str())
        .ok_or("missing argument: to")?;
    let rel_type = args
        .get("rel_type")
        .and_then(|x| x.as_str())
        .ok_or("missing argument: rel_type")?;
    let req = RelateRequest::new(
        subject_of(args),
        KOID::from_hex(from).map_err(|e| e.to_string())?,
        KOID::from_hex(to).map_err(|e| e.to_string())?,
        rel_type,
    );
    let r = k.relate(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "commit_ts": r.commit_ts
    }))
}

fn tool_traverse(k: &Kernel, args: &J) -> Result<J, String> {
    let mut q = TraverseQuery::new(subject_of(args), koid_of(args)?);
    if let Some(rt) = args.get("rel_type").and_then(|x| x.as_str()) {
        q.rel_type = Some(rt.into());
    }
    if let Some(d) = args.get("depth").and_then(|x| x.as_u64()) {
        q.depth = d as usize;
    }
    let hits = k.traverse(q).map_err(|e| e.to_string())?;
    Ok(json!({
        "hits": hits.iter().map(|h| json!({
            "koid": h.koid.to_hex(),
            "depth": h.depth,
            "rel_type": h.rel_type,
            "direction": if h.direction == Direction::Outbound { "outbound" } else { "inbound" }
        })).collect::<Vec<_>>()
    }))
}

fn tool_eval_recall(k: &Kernel, args: &J) -> Result<J, String> {
    let expected: HashSet<KOID> = args
        .get("expected")
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .filter_map(|s| KOID::from_hex(s).ok())
                .collect()
        })
        .unwrap_or_default();
    let report = k
        .eval_recall(EvalRecallQuery {
            context: subject_of(args).into(),
            type_name: args
                .get("type_name")
                .and_then(|t| t.as_str())
                .map(String::from),
            text: args.get("text").and_then(|t| t.as_str()).map(String::from),
            vector: parse_vector(args)?,
            k: args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize,
            fusion: parse_fusion(args),
            expected,
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "k": report.k,
        "returned": report.returned,
        "expected": report.expected,
        "hits": report.hits,
        "recall": report.recall,
        "missing": report.missing.iter().map(|k| k.to_hex()).collect::<Vec<_>>(),
        "mean_lag_ms": report.mean_lag_ms,
        "max_lag_ms": report.max_lag_ms,
        "p95_lag_ms": report.p95_lag_ms,
    }))
}

fn tool_eval_staleness(k: &Kernel, args: &J) -> Result<J, String> {
    let report = k
        .eval_staleness(EvalStalenessQuery {
            context: subject_of(args).into(),
            type_name: args
                .get("type_name")
                .and_then(|t| t.as_str())
                .map(String::from),
            text: args.get("text").and_then(|t| t.as_str()).map(String::from),
            vector: parse_vector(args)?,
            k: args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize,
            fusion: parse_fusion(args),
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "results": report.results,
        "mean_lag_ms": report.mean_lag_ms,
        "max_lag_ms": report.max_lag_ms,
        "p95_lag_ms": report.p95_lag_ms,
    }))
}

fn tool_eval_contradictions(k: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: type_name")?;
    let property = args
        .get("property")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: property")?;
    let q = EvalContradictionQuery {
        context: subject_of(args).into(),
        type_name: type_name.into(),
        property: property.into(),
        similarity_threshold: args
            .get("threshold")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.9) as f32,
        max_results: args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize,
    };
    let hits = k.eval_contradictions(q).map_err(|e| e.to_string())?;
    Ok(json!({
        "contradictions": hits.iter().map(|c| json!({
            "left": c.left.to_hex(),
            "right": c.right.to_hex(),
            "score": c.score,
            "reason": c.reason,
        })).collect::<Vec<_>>()
    }))
}

fn tool_aikoql(k: &Kernel, args: &J) -> Result<J, String> {
    let source = args
        .get("query")
        .and_then(|q| q.as_str())
        .ok_or("missing argument: query")?;
    let subject = args
        .get("subject")
        .and_then(|s| s.as_str())
        .unwrap_or("query-user");
    let stmt = aikoql_compiler::parser::parse(source).map_err(|e| e.to_string())?;

    // CREATE/UPDATE/DELETE are executed directly, not via IR.
    if let aikoql_compiler::parser::ast::Statement::Create(create) = &stmt {
        let mut props = PropertyMap::new();
        for (k, v) in &create.properties {
            props.insert(k.clone(), compiler_expr_to_value(v));
        }
        let r = k
            .remember(RememberRequest {
                context: Subject::new(subject).into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: create.entity.clone(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: ExtensionMap::new(),
                origin: Origin::Human,
                note: None,
                referential_policy: ReferentialPolicy::default(),
            })
            .map_err(|e| e.to_string())?;
        return Ok(
            json!({"koid": r.koid.to_hex(), "version": r.version, "commit_ts": r.commit_ts}),
        );
    }

    let raw = aikoql_compiler::parser::compile_with_subject(source, subject)
        .map_err(|e| e.to_string())?;
    let plan = aikoql_compiler::planner::Planner::optimize(&raw);
    let result = aikoql_runtime::Interpreter::execute(k, &plan).map_err(|e| e.to_string())?;
    match result {
        aikoql_runtime::RowSet::Objects(kos) => Ok(json!({
            "results": kos.iter().map(|ko| json!({
                "koid": ko.koid.to_hex(),
                "type_name": ko.metadata.type_name,
                "version": ko.version,
                "properties": ko.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
            })).collect::<Vec<_>>()
        })),
        aikoql_runtime::RowSet::Scored(scored) => Ok(json!({
            "results": scored.iter().map(|(koid, score, tn, ver)| json!({
                "koid": koid.to_hex(),
                "score": score,
                "type_name": tn,
                "version": ver
            })).collect::<Vec<_>>()
        })),
        _ => Ok(json!({"results": []})),
    }
}

/// Execute an aikoql query and chunk the results for streaming (MRFC-0040 #5).
/// Returns (chunks, stream_id). Chunk 0 is sent as the JSON-RPC response;
/// remaining chunks are sent as notification frames.
fn execute_stream_query(
    k: &Kernel,
    query: &str,
    subject: &str,
) -> Result<(Vec<J>, String), String> {
    let raw =
        aikoql_compiler::parser::compile_with_subject(query, subject).map_err(|e| e.to_string())?;
    let plan = aikoql_compiler::planner::Planner::optimize(&raw);
    let result = aikoql_runtime::Interpreter::execute(k, &plan).map_err(|e| e.to_string())?;

    let rows: Vec<J> = match result {
        aikoql_runtime::RowSet::Objects(kos) => kos.iter().map(|ko| json!({
            "koid": ko.koid.to_hex(),
            "type_name": ko.metadata.type_name,
            "version": ko.version,
            "properties": ko.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
        })).collect(),
        aikoql_runtime::RowSet::Scored(scored) => scored.iter().map(|(koid, score, tn, ver)| json!({
            "koid": koid.to_hex(),
            "score": score,
            "type_name": tn,
            "version": ver
        })).collect(),
        _ => vec![],
    };

    const CHUNK_SIZE: usize = 100;
    let chunks: Vec<J> = rows.chunks(CHUNK_SIZE).map(|c| json!(c)).collect();
    let stream_id = format!("stream-{}", STREAM_ID.fetch_add(1, Ordering::Relaxed));
    Ok((chunks, stream_id))
}

fn compiler_expr_to_value(e: &aikoql_compiler::parser::ast::Expr) -> Value {
    match e {
        aikoql_compiler::parser::ast::Expr::String(s) => Value::Text(s.clone()),
        aikoql_compiler::parser::ast::Expr::Number(n) => Value::Float(*n),
        aikoql_compiler::parser::ast::Expr::Bool(b) => Value::Bool(*b),
        aikoql_compiler::parser::ast::Expr::Null => Value::Null,
    }
}

fn tool_backup(k: &Kernel, db_path: &str) -> Result<J, String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    use std::path::{Path, PathBuf};
    let src = Path::new(db_path);
    let backup_dir: PathBuf = {
        let mut p = src.as_os_str().to_os_string();
        p.push(format!(".backup.{}", ts));
        PathBuf::from(p)
    };
    std::fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;

    // Copy the database file — use filename from source for multi-file db support.
    let src_file = src.file_name().ok_or("invalid db path: no filename")?;
    let dest_path = backup_dir.join(src_file);
    std::fs::copy(src, &dest_path).map_err(|e| e.to_string())?;

    // Record source metadata.
    let (seq, _audit) = k.journal_head().map_err(|e| e.to_string())?;
    let obj_count = k.scan_heads().map_err(|e| e.to_string())?.len();
    let meta_path = backup_dir.join("meta.json");
    std::fs::write(
        &meta_path,
        json!({"timestamp": ts, "source": db_path, "journal_seq": seq, "object_count": obj_count})
            .to_string(),
    )
    .map_err(|e| e.to_string())?;

    // Verify: open backup in a temp kernel and check integrity.
    let dest_str = dest_path.to_string_lossy().to_string();
    let verified = verify_backup_file(&dest_str, seq, obj_count);

    Ok(
        json!({"backup": backup_dir, "timestamp": ts, "journal_seq": seq, "object_count": obj_count, "verified": verified}),
    )
}

/// Open a backup file in a throwaway kernel and check basic integrity.
fn verify_backup_file(path: &str, expected_seq: u64, expected_objects: usize) -> bool {
    let engine = match RedbEngine::open(path) {
        Ok(e) => e,
        Err(_) => return false,
    };
    let k = match Kernel::open(
        std::sync::Arc::new(engine),
        std::sync::Arc::new(SystemClock),
        0,
    ) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let (seq, _) = match k.journal_head() {
        Ok(h) => h,
        Err(_) => return false,
    };
    let count = match k.scan_heads() {
        Ok(h) => h.len(),
        Err(_) => return false,
    };
    seq == expected_seq && count == expected_objects
}

fn tool_restore(args: &J, current_db: &str) -> Result<J, String> {
    let backup = args
        .get("backup")
        .and_then(|b| b.as_str())
        .ok_or("missing argument: backup")?;
    let meta_str = std::fs::read_to_string(format!("{}/meta.json", backup))
        .map_err(|e| format!("not a valid backup: {}", e))?;
    let meta: J = serde_json::from_str(&meta_str).map_err(|e| format!("bad meta: {}", e))?;
    if !std::path::Path::new(&format!("{}/data.redb", backup)).exists() {
        return Err("backup data file missing".into());
    }
    std::fs::copy(format!("{}/data.redb", backup), current_db).map_err(|e| e.to_string())?;
    // Report PITR recovery point from backup metadata.
    let pitr_seq = meta.get("journal_seq").and_then(|v| v.as_u64());
    let pitr_ts = meta.get("timestamp").and_then(|v| v.as_u64());
    Ok(json!({
        "restored": true,
        "meta": meta,
        "recovery_point": {
            "journal_seq": pitr_seq,
            "timestamp": pitr_ts,
        }
    }))
}

fn tool_list_backups() -> Result<J, String> {
    let mut backups = Vec::new();
    if let Ok(entries) = std::fs::read_dir(".") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".backup.") {
                let meta_path = format!("{}/meta.json", name);
                if let Ok(meta_str) = std::fs::read_to_string(&meta_path) {
                    if let Ok(meta) = serde_json::from_str::<J>(&meta_str) {
                        backups.push(json!({"name": name, "meta": meta}));
                    }
                }
            }
        }
    }
    Ok(json!({"backups": backups}))
}

fn tool_audit_report(k: &Kernel) -> Result<J, String> {
    let (seq, audit) = k.journal_head().map_err(|e| e.to_string())?;
    let heads = k.scan_heads().map_err(|e| e.to_string())?;
    let total = heads.len();
    let by_state: Vec<J> = heads
        .iter()
        .map(|(koid, v, ts, state)| {
            json!({"koid": koid.to_hex(), "version": v, "commit_ts": ts, "state": state.to_string()})
        })
        .collect();
    let events = k.journal().map_err(|e| e.to_string())?;
    let event_count = events.len();
    Ok(json!({
        "audit_chain": audit.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""),
        "journal_seq": seq,
        "journal_events": event_count,
        "total_objects": total,
        "objects": by_state,
    }))
}

fn tool_compliance_report(k: &Kernel) -> Result<J, String> {
    let report = k.compliance_report().map_err(|e| e.to_string())?;
    let summary = report.field_crypto_summary.as_ref();
    let audit_counts: Vec<J> = summary
        .map(|s| {
            s.audit_events
                .iter()
                .map(|(kind, count)| json!({"kind": kind.as_str(), "count": count}))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "encryption_enabled": report.encryption_enabled,
        "policies_registered": report.policies_registered,
        "policy_types": report.policy_types,
        "field_encryption_enabled": summary.map(|s| s.field_encryption_enabled).unwrap_or(false),
        "tenant_keys": summary.map(|s| s.tenant_keys).unwrap_or(0),
        "audit_events": audit_counts,
        "compliance_grade": if report.encryption_enabled && report.policies_registered > 0 { "A" } else { "C" },
    }))
}

fn tool_reason(k: &Kernel, args: &J) -> Result<J, String> {
    let rule_type = args
        .get("type_name")
        .and_then(|v| v.as_str())
        .ok_or("missing: type_name")?;
    let rule_props = parse_properties(args)?;
    let claims = k.reason(rule_type, rule_props).map_err(|e| e.to_string())?;
    Ok(json!({
        "claims": claims.iter().map(|c| json!({
            "type_name": c.metadata.type_name,
            "property_count": c.properties.len(),
            "origin": format!("{:?}", c.lifecycle.origin),
        })).collect::<Vec<_>>(),
        "count": claims.len(),
    }))
}

fn tool_infer(k: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|v| v.as_str())
        .ok_or("missing: type_name")?;
    let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let results = k
        .infer(&subject_of(args), type_name, text)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "results": results.iter().map(|s| json!({
            "koid": s.ko.koid.to_hex(),
            "score": s.score,
            "type_name": s.ko.metadata.type_name,
        })).collect::<Vec<_>>(),
        "count": results.len(),
    }))
}

fn tool_predict(kernel: &Kernel, args: &J) -> Result<J, String> {
    let type_name = args
        .get("type_name")
        .and_then(|v| v.as_str())
        .ok_or("missing: type_name")?;
    let props = parse_properties(args)?;
    let top_k = args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
    let merged = kernel
        .predict(&subject_of(args), type_name, &props, top_k)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "predicted": merged.iter().map(|(key, val)| (key.clone(), value_to_json(val))).collect::<serde_json::Map<_,_>>(),
    }))
}

fn tool_deploy_program(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let body = args
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or("missing: body")?;
    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("aikoql");
    let r = k
        .deploy_program(name, body, language, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": r.koid.to_hex(), "version": r.version, "name": name, "language": language}))
}

fn tool_execute_program(k: &Kernel, args: &J) -> Result<J, String> {
    let hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(hex).map_err(|e| e.to_string())?;
    let params: std::collections::BTreeMap<String, Value> =
        if let Some(p) = args.get("params").and_then(|v| v.as_object()) {
            p.iter()
                .map(|(k, v)| (k.clone(), json_to_value(v).unwrap_or(Value::Null)))
                .collect()
        } else {
            std::collections::BTreeMap::new()
        };
    let subject = subject_of(args);
    // Program execution uses the caller's identity for ACL checks on the target data.
    // The program KO itself must be readable by the caller.
    let exec_subject = Subject {
        name: subject.name.clone(),
        roles: subject.roles.clone(),
    };

    // Load program KO, substitute params, compile, execute.
    let ko = k
        .get(KnowledgeContext::from(subject.clone()), &koid)
        .map_err(|e| e.to_string())?;
    if ko.metadata.type_name != "aikoql:program" {
        return Err(format!(
            "KO {} is not a program (type={})",
            hex, ko.metadata.type_name
        ));
    }
    let body = match ko.properties.get("body") {
        Some(Value::Text(s)) => s.clone(),
        _ => return Err("program has no body property".into()),
    };
    let mut query = body;
    for (key, val) in &params {
        query = query.replace(&format!("{{{{{}}}}}", key), &value_to_string(val));
    }
    let plan = aikoql_compiler::parser::compile_with_subject(&query, &exec_subject.name)
        .map_err(|e| format!("compile: {}", e))?;
    let optimized = aikoql_compiler::planner::Planner::optimize(&plan);
    let result = aikoql_runtime::Interpreter::execute(k, &optimized).map_err(|e| e.to_string())?;
    match result {
        aikoql_runtime::RowSet::Objects(kos) => Ok(json!({
            "results": kos.iter().map(|ko| json!({
                "koid": ko.koid.to_hex(), "type_name": ko.metadata.type_name, "version": ko.version,
                "properties": ko.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
            })).collect::<Vec<_>>(), "count": kos.len()
        })),
        aikoql_runtime::RowSet::Scored(scored) => Ok(json!({
            "results": scored.iter().map(|(koid, score, tn, ver)| json!({"koid": koid.to_hex(), "score": score, "type_name": tn, "version": ver})).collect::<Vec<_>>(), "count": scored.len()
        })),
        other => Ok(json!({"results": [], "debug": format!("{:?}", other)})),
    }
}

fn tool_list_programs(k: &Kernel, args: &J) -> Result<J, String> {
    let programs = k
        .list_programs(&subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "programs": programs.iter().map(|p| json!({
            "koid": p.koid.to_hex(),
            "name": p.properties.get("name").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "language": p.properties.get("language").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "version": p.properties.get("version").and_then(|v| match v { Value::Int(i) => Some(*i), _ => None }).unwrap_or(0),
            "lifecycle": p.lifecycle.state.to_string(),
        })).collect::<Vec<_>>()
    }))
}

fn tool_deploy_policy(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let effect = args
        .get("effect")
        .and_then(|v| v.as_str())
        .ok_or("missing: effect")?;
    let principal = args
        .get("principal")
        .and_then(|v| v.as_str())
        .ok_or("missing: principal")?;
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or("missing: action")?;
    let resource = args
        .get("resource_type")
        .and_then(|v| v.as_str())
        .ok_or("missing: resource_type")?;
    let condition = args.get("condition").and_then(|v| v.as_str());
    let r = k
        .deploy_policy(
            name,
            effect,
            principal,
            action,
            resource,
            condition,
            &subject_of(args),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": r.koid.to_hex(), "version": r.version, "name": name}))
}

fn tool_evaluate_policies(k: &Kernel, args: &J) -> Result<J, String> {
    let principal = args
        .get("principal")
        .and_then(|v| v.as_str())
        .ok_or("missing: principal")?;
    let action_str = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or("missing: action")?;
    let resource = args
        .get("resource_type")
        .and_then(|v| v.as_str())
        .ok_or("missing: resource_type")?;
    let action = match action_str.to_lowercase().as_str() {
        "read" => Action::Read,
        "write" => Action::Write,
        "admin" => Action::Admin,
        "evolve" => Action::Evolve,
        "delete" => Action::Delete,
        _ => return Err(format!("unknown action: {}", action_str)),
    };
    let result = k
        .evaluate_policies(principal, &action, resource, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"allowed": result.is_none(), "reason": result.unwrap_or_else(|| "allowed".into())}))
}

fn tool_deploy_workflow(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let steps = args
        .get("steps")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "[]".into());
    let r = k
        .deploy_workflow(name, &steps, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": r.koid.to_hex(), "version": r.version, "name": name}))
}

fn tool_deploy_trigger(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let event_kind = args
        .get("event_kind")
        .and_then(|v| v.as_str())
        .ok_or("missing: event_kind")?;
    let type_filter = args
        .get("type_filter")
        .and_then(|v| v.as_str())
        .unwrap_or("*");
    let program_koid = args
        .get("program_koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: program_koid")?;
    let r = k
        .deploy_trigger(
            name,
            event_kind,
            type_filter,
            program_koid,
            &subject_of(args),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": r.koid.to_hex(), "version": r.version, "name": name}))
}

fn tool_add_dependency(k: &Kernel, args: &J) -> Result<J, String> {
    let src_hex = args
        .get("source")
        .and_then(|v| v.as_str())
        .ok_or("missing: source")?;
    let tgt_hex = args
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or("missing: target")?;
    let dep_type = args
        .get("dep_type")
        .and_then(|v| v.as_str())
        .unwrap_or("uses");
    let src = KOID::from_hex(src_hex).map_err(|e| e.to_string())?;
    let tgt = KOID::from_hex(tgt_hex).map_err(|e| e.to_string())?;
    let req = RelateRequest::new(
        subject_of(args),
        src,
        tgt,
        format!("DEPENDS_ON_{}", dep_type),
    );
    let r = k.relate(req).map_err(|e| e.to_string())?;
    Ok(json!({"koid": r.koid.to_hex(), "version": r.version}))
}

// Global program cache shared across all requests.
static PROGRAM_CACHE: std::sync::LazyLock<knowledge_runtime::ProgramCache> =
    std::sync::LazyLock::new(knowledge_runtime::ProgramCache::new);

fn tool_execute_workflow(k: &Kernel, args: &J) -> Result<J, String> {
    let hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(hex).map_err(|e| e.to_string())?;
    let logs =
        knowledge_runtime::execute_workflow(k, &koid, &subject_of(args), Some(&PROGRAM_CACHE))
            .map_err(|e| e.to_string())?;
    Ok(json!({"logs": logs, "executed": true}))
}

fn tool_check_triggers(k: &Kernel) -> Result<J, String> {
    let fired = knowledge_runtime::check_and_fire_triggers(k, 0).map_err(|e| e.to_string())?;
    Ok(json!({"water_mark": fired}))
}

#[allow(dead_code)]
fn tool_execution_stats() -> Result<J, String> {
    let s = knowledge_runtime::execution_stats();
    Ok(json!({
        "programs_executed": s.programs_executed,
        "total_rows": s.total_rows_returned,
        "total_time_ms": s.total_time_ms,
        "avg_time_ms": s.total_time_ms.checked_div(s.programs_executed).unwrap_or(0),
        "cache_hits": s.cache_hits,
        "cache_misses": s.cache_misses,
        "cache_hit_rate": if s.cache_hits + s.cache_misses > 0 {
            format!("{:.1}%", 100.0 * s.cache_hits as f64 / (s.cache_hits + s.cache_misses) as f64)
        } else { "N/A".into() },
    }))
}

fn tool_program_cache_stats() -> Result<J, String> {
    Ok(json!({"cache_hits": PROGRAM_CACHE.stats()}))
}

// ---- Agent KO (MRFC-0030 Phase 7c) ---------------------------------------

fn tool_deploy_agent(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let skills = args
        .get("skills")
        .and_then(|v| v.as_array())
        .map(|a| serde_json::to_string(a).unwrap_or_default())
        .unwrap_or_else(|| "[]".into());
    let tools = args
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|a| serde_json::to_string(a).unwrap_or_default())
        .unwrap_or_else(|| "[]".into());
    let policies = args
        .get("policies")
        .and_then(|v| v.as_array())
        .map(|a| serde_json::to_string(a).unwrap_or_default())
        .unwrap_or_else(|| "[]".into());
    let r = k
        .deploy_agent(name, prompt, &skills, &tools, &policies, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": r.koid.to_hex(), "version": r.version, "name": name}))
}

fn tool_list_agents(k: &Kernel, args: &J) -> Result<J, String> {
    let agents = k
        .list_agents(&subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "agents": agents.iter().map(|a| json!({
            "koid": a.koid.to_hex(),
            "name": a.properties.get("name").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "prompt": a.properties.get("prompt").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or(""),
            "version": a.properties.get("version").and_then(|v| match v { Value::Int(i) => Some(*i), _ => None }).unwrap_or(0),
            "lifecycle": a.lifecycle.state.to_string(),
        })).collect::<Vec<_>>()
    }))
}

fn tool_execute_agent(k: &Kernel, args: &J) -> Result<J, String> {
    let koid_str = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(koid_str).map_err(|_| format!("invalid KOID: {}", koid_str))?;
    let logs = knowledge_runtime::execute_agent(k, &koid, &subject_of(args), Some(&PROGRAM_CACHE))
        .map_err(|e| e.to_string())?;
    Ok(json!({"agent_koid": koid_str, "execution_log": logs}))
}

// ---- Connector KO (MRFC-0030 Phase 7b) -----------------------------------

fn tool_deploy_connector(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let plugin = args
        .get("plugin")
        .and_then(|v| v.as_str())
        .ok_or("missing: plugin")?;
    let config = args
        .get("config")
        .and_then(|v| v.as_object())
        .map(|o| serde_json::to_string(o).unwrap_or_default())
        .unwrap_or_else(|| "{}".into());
    let mapping = args
        .get("mapping")
        .and_then(|v| v.as_array())
        .map(|a| serde_json::to_string(a).unwrap_or_default())
        .unwrap_or_else(|| "[]".into());
    let r = k
        .deploy_connector(name, plugin, &config, &mapping, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": r.koid.to_hex(), "version": r.version, "name": name}))
}

fn tool_list_connectors(k: &Kernel, args: &J) -> Result<J, String> {
    let connectors = k
        .list_connectors(&subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "connectors": connectors.iter().map(|c| json!({
            "koid": c.koid.to_hex(),
            "name": c.properties.get("name").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "plugin": c.properties.get("plugin").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "lifecycle": c.lifecycle.state.to_string(),
        })).collect::<Vec<_>>()
    }))
}

fn tool_deploy_view(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing: query")?;
    let refresh_seconds = args.get("refresh_seconds").and_then(|v| v.as_i64());
    let r = k
        .deploy_view(name, query, refresh_seconds, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"status": "deployed", "koid": r.koid.to_hex(), "type": "aikoql:view"}))
}

fn tool_list_views(k: &Kernel, args: &J) -> Result<J, String> {
    let views = k.list_views(&subject_of(args)).map_err(|e| e.to_string())?;
    Ok(json!({
        "views": views.iter().map(|v| json!({
            "koid": v.koid.to_hex(),
            "name": v.properties.get("name").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "query": v.properties.get("query").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "lifecycle": v.lifecycle.state.to_string(),
        })).collect::<Vec<_>>()
    }))
}

fn tool_deploy_report(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let template = args
        .get("template")
        .and_then(|v| v.as_str())
        .ok_or("missing: template")?;
    let format = args
        .get("format")
        .and_then(|v| v.as_str())
        .ok_or("missing: format")?;
    let parameters = args
        .get("parameters")
        .and_then(|v| v.as_array())
        .map(|a| serde_json::to_string(a).unwrap_or_default())
        .unwrap_or_else(|| "[]".into());
    let r = k
        .deploy_report(name, template, format, &parameters, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"status": "deployed", "koid": r.koid.to_hex(), "type": "aikoql:report"}))
}

fn tool_list_reports(k: &Kernel, args: &J) -> Result<J, String> {
    let reports = k
        .list_reports(&subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "reports": reports.iter().map(|r| json!({
            "koid": r.koid.to_hex(),
            "name": r.properties.get("name").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "format": r.properties.get("format").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "lifecycle": r.lifecycle.state.to_string(),
        })).collect::<Vec<_>>()
    }))
}

fn tool_deploy_benchmark(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let target_query = args
        .get("target_query")
        .and_then(|v| v.as_str())
        .ok_or("missing: target_query")?;
    let iterations = args
        .get("iterations")
        .and_then(|v| v.as_i64())
        .unwrap_or(100);
    let warmup = args.get("warmup").and_then(|v| v.as_i64());
    let r = k
        .deploy_benchmark(name, target_query, iterations, warmup, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"status": "deployed", "koid": r.koid.to_hex(), "type": "aikoql:benchmark"}))
}

fn tool_list_benchmarks(k: &Kernel, args: &J) -> Result<J, String> {
    let benchmarks = k
        .list_benchmarks(&subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "benchmarks": benchmarks.iter().map(|b| json!({
            "koid": b.koid.to_hex(),
            "name": b.properties.get("name").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "target_query": b.properties.get("target_query").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "iterations": b.properties.get("iterations").and_then(|v| match v { Value::Int(i) => Some(*i), _ => None }).unwrap_or(0),
            "lifecycle": b.lifecycle.state.to_string(),
        })).collect::<Vec<_>>()
    }))
}

// ---- Document Ingestion (MRFC-0050) -----------------------------------

fn tool_document_ingest(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let filename = args
        .get("filename")
        .and_then(|v| v.as_str())
        .ok_or("missing: filename")?;
    let content_b64 = args
        .get("content_base64")
        .and_then(|v| v.as_str())
        .ok_or("missing: content_base64")?;
    let mime_type = args
        .get("mime_type")
        .and_then(|v| v.as_str())
        .unwrap_or("application/octet-stream");

    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_b64)
        .map_err(|e| format!("base64 decode: {}", e))?;

    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    let size_bytes = bytes.len() as i64;

    // Dedup: if document with this hash already exists, return it.
    if let Ok(docs) = k.list_documents(&subject_of(args)) {
        for doc in &docs {
            if let Some(Value::Text(existing_hash)) = doc.properties.get("sha256") {
                if existing_hash == &hash {
                    return Ok(json!({
                        "koid": doc.koid.to_hex(),
                        "sha256": hash,
                        "size_bytes": size_bytes,
                        "status": "duplicate",
                        "message": "Document with this content already exists"
                    }));
                }
            }
        }
    }

    // Store artifact on disk.
    let artifact_dir = format!("{}.artifacts", db_path);
    std::fs::create_dir_all(&artifact_dir).map_err(|e| format!("artifact dir: {}", e))?;
    let artifact_path = format!("{}/{}", artifact_dir, hash);
    if !std::path::Path::new(&artifact_path).exists() {
        std::fs::write(&artifact_path, &bytes).map_err(|e| format!("write artifact: {}", e))?;
    }

    // D1/D2: Extract text from the stored artifact.
    let (page_count, char_count, status, ocr_stats) =
        match aikoql_ingestion::extract_document(&artifact_path, mime_type) {
            Ok(doc) => {
                // Store extracted text alongside the original artifact.
                let extracted_path = format!("{}/{}.extracted.txt", artifact_dir, hash);
                let extracted_text: String = doc
                    .pages
                    .iter()
                    .map(|p| format!("--- Page {} [{}] ---\n{}", p.page_number, p.source, p.text))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                std::fs::write(&extracted_path, &extracted_text).ok();

                // Derive granular status from OCR stats.
                let status = match &doc.ocr_stats {
                    Some(stats) => stats.status(),
                    None => "extracted",
                };
                let ocr_stats_json = doc.ocr_stats.as_ref().map(|s| {
                    json!({
                        "pages_ocr_attempted": s.pages_ocr_attempted,
                        "pages_ocr_succeeded": s.pages_ocr_succeeded,
                        "pages_ocr_failed": s.pages_ocr_failed,
                        "average_confidence": s.average_confidence,
                    })
                });
                (
                    doc.page_count as i64,
                    doc.total_chars as i64,
                    status,
                    ocr_stats_json,
                )
            }
            Err(e) => {
                // Unsupported format or extraction failed — still ingest the doc.
                tracing::warn!("extraction skipped for {}: {}", filename, e);
                (0i64, 0i64, "ingested", None)
            }
        };

    let r = k
        .deploy_document(
            filename,
            mime_type,
            &hash,
            size_bytes,
            page_count,
            char_count,
            status,
            &subject_of(args),
        )
        .map_err(|e| e.to_string())?;

    let mut resp = json!({
        "koid": r.koid.to_hex(),
        "sha256": hash,
        "size_bytes": size_bytes,
        "page_count": page_count,
        "char_count": char_count,
        "status": status
    });
    if let Some(ref stats) = ocr_stats {
        resp["ocr_stats"] = stats.clone();
    }
    Ok(resp)
}

fn tool_document_list(k: &Kernel, args: &J) -> Result<J, String> {
    let docs = k
        .list_documents(&subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "documents": docs.iter().map(|d| json!({
            "koid": d.koid.to_hex(),
            "filename": d.properties.get("filename").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "mime_type": d.properties.get("mime_type").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "sha256": d.properties.get("sha256").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "size_bytes": d.properties.get("size_bytes").and_then(|v| match v { Value::Int(i) => Some(*i), _ => None }).unwrap_or(0),
            "status": d.properties.get("status").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "lifecycle": d.lifecycle.state.to_string(),
        })).collect::<Vec<_>>()
    }))
}

fn tool_document_status(k: &Kernel, args: &J) -> Result<J, String> {
    let hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(hex).map_err(|e| e.to_string())?;
    let ctx = KnowledgeContext::from(subject_of(args));
    let ko = k.get(ctx, &koid).map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": ko.koid.to_hex(),
        "filename": ko.properties.get("filename").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }),
        "mime_type": ko.properties.get("mime_type").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }),
        "sha256": ko.properties.get("sha256").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }),
        "size_bytes": ko.properties.get("size_bytes").and_then(|v| match v { Value::Int(i) => Some(*i), _ => None }),
        "page_count": ko.properties.get("page_count").and_then(|v| match v { Value::Int(i) => Some(*i), _ => None }),
        "char_count": ko.properties.get("char_count").and_then(|v| match v { Value::Int(i) => Some(*i), _ => None }),
        "status": ko.properties.get("status").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }),
        "lifecycle": ko.lifecycle.state.to_string(),
        "version": ko.version,
        "commit_ts": ko.commit_ts,
    }))
}

fn tool_document_compile(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(hex).map_err(|e| e.to_string())?;
    let ctx = KnowledgeContext::from(subject_of(args));
    let ko = k.get(ctx, &koid).map_err(|e| e.to_string())?;

    let sha256 = ko
        .properties
        .get("sha256")
        .and_then(|v| match v {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        })
        .ok_or("document missing sha256 property")?;
    let mime_type = ko
        .properties
        .get("mime_type")
        .and_then(|v| match v {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "application/octet-stream".into());

    let artifact_path = format!("{}.artifacts/{}", db_path, sha256);
    if !std::path::Path::new(&artifact_path).exists() {
        return Err(format!("artifact not found: {}", artifact_path));
    }

    // Markdown: use semantic compiler
    let is_markdown =
        mime_type.contains("markdown") || mime_type == "text/md" || artifact_path.ends_with(".md");

    // Rust source: use code-to-knowledge compiler
    let is_rust = mime_type.contains("rust") || artifact_path.ends_with(".rs");

    let mut result = if is_markdown {
        let content =
            std::fs::read_to_string(&artifact_path).map_err(|e| format!("read markdown: {}", e))?;
        let ir = aikoql_ingestion::compile_markdown_string(&content, Some(hex.to_string()))
            .map_err(|e| format!("markdown compile: {}", e))?;
        let ir_json = serde_json::to_value(&ir).unwrap_or_default();
        serde_json::json!({
            "markdown_ir": ir_json,
            "method": "markdown-compiler",
            "phase_stats": {
                "d1_extract": "skipped (markdown text)",
                "d2_ocr": "skipped",
                "markdown_compile": "ok"
            },
            "entities": ir.entities.len(),
            "facts": ir.facts.len(),
            "relations": ir.relations.len(),
            "total_candidates": ir.total_candidates()
        })
    } else if is_rust {
        let ir = aikoql_ingestion::compile_rust_file(&artifact_path)
            .map_err(|e| format!("rust compile: {}", e))?;
        let ir_json = serde_json::to_value(&ir).unwrap_or_default();
        serde_json::json!({
            "code_ir": ir_json,
            "method": "rust-code-parser",
            "phase_stats": {
                "d1_extract": "skipped (rust source)",
                "d2_ocr": "skipped",
                "code_compile": "ok"
            },
            "entities": ir.entities.len(),
            "facts": ir.facts.len(),
            "relations": ir.relations.len(),
            "total_candidates": ir.total_candidates()
        })
    } else {
        let doc = aikoql_ingestion::extract_document(&artifact_path, &mime_type)
            .map_err(|e| format!("extract for compile: {}", e))?;
        let cr = aikoql_ingestion::compile_document_mock(&doc, &[]);
        serde_json::to_value(&cr).map_err(|e| format!("serialize: {}", e))?
    };

    // Attach document metadata
    if let Some(obj) = result.as_object_mut() {
        obj.insert("koid".into(), serde_json::Value::String(hex.to_string()));
        obj.insert("mime_type".into(), serde_json::Value::String(mime_type));
    }

    Ok(result)
}

// ---- Context Compiler (MRFC-0070 Phase A6) ---------------------------

fn tool_compile_context(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .ok_or("missing: task")?;
    let token_budget: usize = args
        .get("token_budget")
        .and_then(|v| v.as_u64())
        .unwrap_or(2000) as usize;

    let ir = get_ir_for_koid(k, args, db_path)?;

    // Compile context package
    let pkg = aikoql_ingestion::compile_context(task, &ir, token_budget);
    let md = aikoql_ingestion::render_context_markdown(&pkg);

    let pkg_json = serde_json::to_value(&pkg).unwrap_or_default();
    Ok(serde_json::json!({
        "context_markdown": md,
        "package": pkg_json,
        "koid": hex,
        "task": task,
        "token_budget": token_budget,
    }))
}

// A8: Change Reconciliation — git diff → affected entities → impact report.
fn tool_reconcile(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let files: Vec<String> = args
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("missing: files")?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let ir = get_ir_for_koid(k, args, db_path)?;

    let report = aikoql_ingestion::reconcile(&files, &ir);
    let report_json = serde_json::to_value(&report).unwrap_or_default();
    Ok(serde_json::json!({
        "report": report_json,
        "koid": hex,
    }))
}

// A9: Connector Bridge — convert connector metadata into KnowledgeIr.
fn tool_connector_bridge(_k: &Kernel, args: &J) -> Result<J, String> {
    let connector_type = args
        .get("connector_type")
        .and_then(|v| v.as_str())
        .ok_or("missing: connector_type")?;
    let label = args
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let raw_tables = args.get("tables").and_then(|v| v.as_array());
    let raw_refs = args.get("references").and_then(|v| v.as_array());

    let meta = if let Some(tables) = raw_tables {
        // Parse tables from JSON
        let containers: Vec<aikoql_ingestion::ContainerInfo> = tables
            .iter()
            .map(|t| {
                let name = t["name"].as_str().unwrap_or("unknown").to_string();
                let fields: Vec<aikoql_ingestion::FieldInfo> = t["fields"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|f| aikoql_ingestion::FieldInfo {
                                name: f["name"].as_str().unwrap_or("?").to_string(),
                                data_type: f["data_type"].as_str().unwrap_or("text").to_string(),
                                is_primary_key: f["is_primary_key"].as_bool().unwrap_or(false),
                                nullable: f["nullable"].as_bool().unwrap_or(true),
                                is_unique: f["is_unique"].as_bool().unwrap_or(false),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                aikoql_ingestion::ContainerInfo {
                    name,
                    fields,
                    row_count: t["row_count"].as_u64(),
                }
            })
            .collect();

        let references: Vec<aikoql_ingestion::ReferenceInfo> = raw_refs
            .map(|a| {
                a.iter()
                    .map(|r| aikoql_ingestion::ReferenceInfo {
                        from_container: r["from_container"].as_str().unwrap_or("?").to_string(),
                        from_fields: r["from_fields"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        to_container: r["to_container"].as_str().unwrap_or("?").to_string(),
                        to_fields: r["to_fields"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        name: r["name"].as_str().map(String::from),
                    })
                    .collect()
            })
            .unwrap_or_default();

        aikoql_ingestion::ConnectorMetadata {
            connector_type: connector_type.to_string(),
            label: label.to_string(),
            containers,
            references,
            version: None,
        }
    } else {
        // ponytail: empty metadata for unknown schemas — agent should call
        // connector's own introspection tool first
        aikoql_ingestion::ConnectorMetadata {
            connector_type: connector_type.to_string(),
            label: label.to_string(),
            ..Default::default()
        }
    };

    let ir = aikoql_ingestion::connector_metadata_to_ir(&meta);
    let ir_json = serde_json::to_value(&ir).unwrap_or_default();
    Ok(serde_json::json!({
        "knowledge_ir": ir_json,
        "connector_type": connector_type,
        "label": label,
        "entity_count": ir.entities.len(),
        "fact_count": ir.facts.len(),
        "relation_count": ir.relations.len(),
    }))
}

// A6: Aikoql Agent Operations — 7 semantic query tools.
// All follow the same pattern: get koid → compile KnowledgeIr → run op.

fn get_ir_for_koid(
    k: &Kernel,
    args: &J,
    db_path: &str,
) -> Result<aikoql_ingestion::KnowledgeIr, String> {
    let hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(hex).map_err(|e| e.to_string())?;
    let ctx = KnowledgeContext::from(subject_of(args));
    let ko = k.get(ctx, &koid).map_err(|e| e.to_string())?;

    // Path 1: Document KO with sha256 → read artifact → re-compile.
    if let Some(Value::Text(sha256)) = ko.properties.get("sha256") {
        let mime_type = ko
            .properties
            .get("mime_type")
            .and_then(|v| match v {
                Value::Text(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "application/octet-stream".into());
        let artifact_path = format!("{}.artifacts/{}", db_path, sha256);
        if !std::path::Path::new(&artifact_path).exists() {
            return Err(format!("artifact not found: {}", artifact_path));
        }
        return if mime_type.contains("markdown")
            || mime_type == "text/md"
            || artifact_path.ends_with(".md")
        {
            let content = std::fs::read_to_string(&artifact_path)
                .map_err(|e| format!("read markdown: {}", e))?;
            aikoql_ingestion::compile_markdown_string(&content, Some(hex.to_string()))
                .map_err(|e| format!("markdown compile: {}", e))
        } else if mime_type.contains("rust") || artifact_path.ends_with(".rs") {
            aikoql_ingestion::compile_rust_file(&artifact_path)
                .map_err(|e| format!("rust compile: {}", e))
        } else {
            let doc = aikoql_ingestion::extract_document(&artifact_path, &mime_type)
                .map_err(|e| format!("extract: {}", e))?;
            let cr = aikoql_ingestion::compile_document_mock(&doc, &[]);
            Ok(aikoql_ingestion::KnowledgeIr {
                facts: serde_json::from_value(serde_json::to_value(&cr).unwrap_or_default())
                    .unwrap_or_default(),
                ..Default::default()
            })
        };
    }

    // Path 2: Direct KO with ir_json (from remember/ingest-dir) → deserialize.
    if let Some(Value::Text(ir_json)) = ko.properties.get("ir_json") {
        return serde_json::from_str(ir_json).map_err(|e| format!("deserialize ir_json: {}", e));
    }

    Err("KO has neither sha256 (document) nor ir_json (direct knowledge) — use document_ingest or ingest-dir first".into())
}

fn tool_explain_component(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let explanation = aikoql_ingestion::explain_component(name, &ir)
        .ok_or_else(|| format!("component '{}' not found", name))?;
    Ok(serde_json::to_value(&explanation).unwrap_or_default())
}

fn tool_explain_decision(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let explanation = aikoql_ingestion::explain_decision(name, &ir)
        .ok_or_else(|| format!("decision '{}' not found", name))?;
    Ok(serde_json::to_value(&explanation).unwrap_or_default())
}

fn tool_trace_requirement(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let req_id = args
        .get("requirement")
        .and_then(|v| v.as_str())
        .ok_or("missing: requirement")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let trace = aikoql_ingestion::trace_requirement(req_id, &ir);
    Ok(serde_json::to_value(&trace).unwrap_or_default())
}

fn tool_find_conflicts(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let component = args
        .get("component")
        .and_then(|v| v.as_str())
        .ok_or("missing: component")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let conflicts = aikoql_ingestion::find_conflicts(component, &ir);
    Ok(serde_json::to_value(&conflicts).unwrap_or_default())
}

fn tool_find_stale(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let ir = get_ir_for_koid(k, args, db_path)?;
    let report = aikoql_ingestion::find_stale_documentation(&ir);
    Ok(serde_json::to_value(&report).unwrap_or_default())
}

fn tool_validate_change(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let description = args
        .get("change")
        .and_then(|v| v.as_str())
        .ok_or("missing: change")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let validation = aikoql_ingestion::validate_change(description, &ir);
    Ok(serde_json::to_value(&validation).unwrap_or_default())
}

fn tool_propose_update(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let action_str = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or("missing: action")?;
    let action = match action_str {
        "add_fact" => aikoql_ingestion::ProposalAction::AddFact,
        "remove_fact" => aikoql_ingestion::ProposalAction::RemoveFact,
        "update_entity" => aikoql_ingestion::ProposalAction::UpdateEntity,
        "add_relation" => aikoql_ingestion::ProposalAction::AddRelation,
        "remove_relation" => aikoql_ingestion::ProposalAction::RemoveRelation,
        _ => return Err(format!("unknown action: {}", action_str)),
    };
    let target = args
        .get("target_entity")
        .and_then(|v| v.as_str())
        .map(String::from);
    let new_facts: Vec<String> = args
        .get("new_facts")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let remove_facts: Vec<String> = args
        .get("remove_facts")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let new_relations: Vec<(String, String, String)> = args
        .get("new_relations")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    let arr = v.as_array()?;
                    Some((
                        arr.first()?.as_str()?.to_string(),
                        arr.get(1)?.as_str()?.to_string(),
                        arr.get(2)?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let justification = args
        .get("justification")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let ir = get_ir_for_koid(k, args, db_path)?;
    let proposal = aikoql_ingestion::propose_knowledge_update(
        action,
        target,
        new_facts,
        remove_facts,
        new_relations,
        justification,
        agent_id,
        &ir,
    );
    Ok(serde_json::to_value(&proposal).unwrap_or_default())
}

fn tool_filter_secrets(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let ir = get_ir_for_koid(k, args, db_path)?;
    let (_redacted, findings) = aikoql_ingestion::filter_secrets(&ir);
    Ok(serde_json::to_value(&findings).unwrap_or_default())
}

// ---- Agent Experience Improvements (MRFC-0040) -------------------------

fn tool_discover_schema(k: &Kernel) -> Result<J, String> {
    let types = k.list_types().map_err(|e| e.to_string())?;
    let heads = k.scan_heads().map_err(|e| e.to_string())?;
    let subject = Subject {
        name: "schema-discovery".into(),
        roles: vec!["admin".into()],
    };
    let mut type_info = serde_json::Map::new();
    let mut type_counts: HashMap<String, usize> = HashMap::new();
    for (koid, _ver, _ts, state) in &heads {
        if *state == LifecycleState::Deleted {
            continue;
        }
        if let Ok(ko) = k.get(KnowledgeContext::from(subject.clone()), koid) {
            *type_counts
                .entry(ko.metadata.type_name.clone())
                .or_insert(0) += 1;
        }
    }
    for t in &types {
        type_info.insert(
            t.clone(),
            json!({"count": type_counts.get(t).copied().unwrap_or(0)}),
        );
    }
    Ok(json!({"types": types, "type_info": type_info, "total_types": types.len()}))
}

/// Discover an ontology from all stored Knowledge Objects (MRFC-0041).
fn tool_discover_ontology(k: &Kernel) -> Result<J, String> {
    let subject = Subject {
        name: "ontology-discovery".into(),
        roles: vec!["admin".into()],
    };
    let heads = k.scan_heads().map_err(|e| e.to_string())?;
    let mut kos: Vec<KnowledgeObject> = Vec::new();
    for (koid, _ver, _ts, state) in &heads {
        if *state == LifecycleState::Deleted {
            continue;
        }
        if let Ok(ko) = k.get(KnowledgeContext::from(subject.clone()), koid) {
            kos.push(ko);
        }
    }
    let def = discover_ontology(&kos);
    // Auto-save as an Ontology KO.
    let props = def.to_property_map();
    let r = k
        .remember(RememberRequest {
            context: subject.into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: Some("auto-discovered-ontology".into()),
            metadata: Metadata {
                type_name: ONTOLOGY_TYPE.into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["auto-discovered".into()],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: "system".into(),
                acl: vec![],
                classification: None,
            }),
            extensions: ExtensionMap::new(),
            origin: Origin::System,
            note: Some("Auto-discovered from stored Knowledge Objects".into()),
            referential_policy: ReferentialPolicy::default(),
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "saved": true,
        "koid": r.koid.to_hex(),
        "version": r.version,
        "namespace": def.namespace,
        "classes": def.classes.len(),
        "relationships": def.relationships.len(),
        "mappings": def.mappings.len(),
        "types_discovered": def.classes.keys().cloned().collect::<Vec<_>>(),
    }))
}

fn tool_health(k: &Kernel) -> Result<J, String> {
    let (seq, audit) = k.journal_head().unwrap_or((0, [0u8; 32]));
    let heads = k.scan_heads().map(|h| h.len()).unwrap_or(0);
    let ready = true;
    // Single-node: journal is always current, so lag is 0.
    let journal_lag_ms: u64 = 0;
    let connections = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
    let max_connections = if connections > 0 { connections } else { 1 };
    Ok(json!({
        "status": if ready { "healthy" } else { "degraded" },
        "ready": ready,
        "journal_seq": seq,
        "journal_lag_ms": journal_lag_ms,
        "object_count": heads,
        "connection_pool": format!("{}/{}", connections, max_connections),
        "audit_hash": audit.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""),
        "uptime_seconds": SERVER_START.get().map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0),
    }))
}

fn tool_agent_memory(kernel: &Kernel, args: &J) -> Result<J, String> {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .ok_or("missing: agent_id")?;
    let key = args.get("key").and_then(|v| v.as_str());
    let value = args.get("value");
    let ttl = args.get("ttl").and_then(|v| v.as_i64()).unwrap_or(3600);

    // Write mode: store a memory.
    if let (Some(mem_key), Some(mem_val)) = (key, value) {
        let mut props = PropertyMap::new();
        props.insert("agent_id".into(), Value::Text(agent_id.to_string()));
        props.insert("key".into(), Value::Text(mem_key.to_string()));
        props.insert(
            "value".into(),
            json_to_value(mem_val).unwrap_or(Value::Null),
        );
        props.insert("ttl".into(), Value::Int(ttl));
        let r = kernel
            .remember(RememberRequest {
                context: subject_of(args).into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: Some(format!("agent-mem-{}-{}", agent_id, mem_key)),
                metadata: Metadata {
                    type_name: "aikoql:memory".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec!["agent-memory".into()],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: Some(SecurityDescriptor {
                    owner: agent_id.to_string(),
                    acl: vec![],
                    classification: None,
                }),
                extensions: ExtensionMap::new(),
                origin: Origin::Human,
                note: Some(format!("Agent memory: {}", mem_key)),
                referential_policy: ReferentialPolicy::Permissive,
            })
            .map_err(|e| e.to_string())?;
        return Ok(json!({"koid": r.koid.to_hex(), "stored": true}));
    }

    // Read mode: retrieve memories for this agent.
    let subject = subject_of(args);
    let all = kernel
        .scan_by_type(&subject, "aikoql:memory")
        .map_err(|e| e.to_string())?;
    let memories: Vec<J> = all.iter()
        .filter(|ko| ko.properties.get("agent_id") == Some(&Value::Text(agent_id.to_string())))
        .map(|ko| json!({
            "koid": ko.koid.to_hex(),
            "key": ko.properties.get("key").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }),
            "value": ko.properties.get("value").map(value_to_json),
            "ttl": ko.properties.get("ttl").and_then(|v| match v { Value::Int(i) => Some(i), _ => None }),
        }))
        .collect();
    Ok(json!({"memories": memories, "count": memories.len()}))
}

// ---- Memory Tools (MRFC-0070) ------------------------------------------

fn resolve_memory_dir(args: &J) -> String {
    args.get("memory_dir")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            MEMORY_DIR
                .get()
                .cloned()
                .unwrap_or_else(|| "./memory".into())
        })
}

fn parse_memory_frontmatter(raw: &str) -> Option<(String, String, String)> {
    // Parse YAML frontmatter between --- delimiters.
    // Returns (name, description, type) from the frontmatter.
    let body = raw.strip_prefix("---\n")?;
    let (front, _rest) = body.split_once("\n---")?;
    let mut name = String::new();
    let mut desc = String::new();
    let mut mtype = String::new();
    for line in front.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("name:") {
            name = v.trim().trim_matches('"').to_string();
        } else if let Some(v) = trimmed.strip_prefix("description:") {
            desc = v.trim().trim_matches('"').to_string();
        } else if let Some(v) = trimmed.strip_prefix("type:") {
            // may appear under metadata: block
            mtype = v.trim().trim_matches('"').to_string();
        }
    }
    if name.is_empty() {
        None
    } else {
        Some((name, desc, mtype))
    }
}

fn tool_memory_search(args: &J) -> Result<J, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing: query")?;
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;
    let dir = resolve_memory_dir(args);

    let mut results: Vec<J> = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => return Err(format!("cannot read memory dir '{}': {}", dir, e)),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let fname = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if fname == "MEMORY" {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (name, desc, _mtype) = parse_memory_frontmatter(&raw)
            .unwrap_or_else(|| (fname.to_string(), String::new(), String::new()));

        // Tokenized scoring: split both query and candidate on
        // whitespace/hyphens/underscores, score by token intersection.
        // "dogfooding e2e test" matches "e2e-dogfooding-session" because
        // tokens {dogfooding, e2e, test} ∩ {e2e, dogfooding, session} = {dogfooding, e2e}.
        fn tokenize(s: &str) -> Vec<String> {
            s.to_lowercase()
                .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        }
        let query_tokens: Vec<String> = tokenize(query);
        let token_score = |text: &str, weight: f64| -> f64 {
            let text_tokens = tokenize(text);
            if text_tokens.is_empty() || query_tokens.is_empty() {
                return 0.0;
            }
            let hits = query_tokens
                .iter()
                .filter(|qt| text_tokens.contains(qt))
                .count();
            (hits as f64) / (query_tokens.len() as f64) * weight
        };
        let name_score = token_score(&name, 3.0);
        let desc_score = token_score(&desc, 2.0);
        let body_score = token_score(&raw, 0.5);
        let score = name_score + desc_score + body_score;

        if score > 0.0 {
            // Extract snippet: first line in body that matches a query token
            let body_start = raw.find("\n---").map(|i| i + 4).unwrap_or(0);
            let body_text = &raw[body_start..];
            let snippet = body_text
                .lines()
                .find(|l| query_tokens.iter().any(|qt| l.to_lowercase().contains(qt)))
                .unwrap_or_else(|| body_text.lines().next().unwrap_or(""))
                .trim()
                .to_string();
            let snippet = if snippet.len() > 200 {
                format!("{}...", &snippet[..200])
            } else {
                snippet
            };

            results.push(json!({
                "name": name,
                "description": desc,
                "file": path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
                "snippet": snippet,
                "score": score,
            }));
        }
    }

    results.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(max_results);

    Ok(json!({"results": results, "count": results.len(), "query": query}))
}

fn tool_memory_store(args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or("missing: description")?;
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or("missing: content")?;
    let mtype = args
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("project");
    let dir = resolve_memory_dir(args);

    // Ensure directory exists
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create memory dir '{}': {}", dir, e))?;

    // Validate name is kebab-case slug
    if name.contains(char::is_whitespace) || name.contains('\\') || name.contains('/') {
        return Err(
            "name must be a kebab-case slug (no whitespace, slashes, or backslashes)".into(),
        );
    }

    let filename = format!("{}.md", name);
    let filepath = std::path::PathBuf::from(&dir).join(&filename);

    // Build frontmatter with ISO 8601 timestamp
    let now = system_time_iso8601();
    let frontmatter = format!(
        "---\nname: {}\ndescription: \"{}\"\nmetadata:\n  type: {}\n  modified: {}\n---\n\n{}\n",
        name, description, mtype, now, content
    );

    std::fs::write(&filepath, &frontmatter)
        .map_err(|e| format!("cannot write memory '{}': {}", filepath.display(), e))?;

    // Append to MEMORY.md index if not already present
    let index_path = std::path::PathBuf::from(&dir).join("MEMORY.md");
    let index_line = format!(
        "- [{}]({}) — {}\n",
        name_to_title(name),
        filename,
        description
    );
    if index_path.exists() {
        let existing = std::fs::read_to_string(&index_path)
            .map_err(|e| format!("cannot read MEMORY.md: {}", e))?;
        if !existing.contains(&filename) {
            std::fs::write(&index_path, format!("{}{}", existing, index_line))
                .map_err(|e| format!("cannot update MEMORY.md: {}", e))?;
        }
    } else {
        std::fs::write(&index_path, index_line)
            .map_err(|e| format!("cannot create MEMORY.md: {}", e))?;
    }

    Ok(json!({
        "stored": true,
        "name": name,
        "file": filename,
        "path": filepath.to_string_lossy(),
    }))
}

fn tool_memory_update(args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let dir = resolve_memory_dir(args);

    let filename = format!("{}.md", name);
    let filepath = std::path::PathBuf::from(&dir).join(&filename);
    if !filepath.exists() {
        return Err(format!(
            "memory '{}' not found at {}",
            name,
            filepath.display()
        ));
    }

    let raw = std::fs::read_to_string(&filepath)
        .map_err(|e| format!("cannot read '{}': {}", filepath.display(), e))?;
    let (cur_name, cur_desc, cur_type) = parse_memory_frontmatter(&raw)
        .unwrap_or_else(|| (name.to_string(), String::new(), "project".to_string()));

    let new_desc = args
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or(&cur_desc);
    let new_type = args
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or(&cur_type);
    let new_content = args.get("content").and_then(|v| v.as_str());

    // Rebuild the file: keep body if content not provided
    let body = if let Some(c) = new_content {
        c.to_string()
    } else {
        // Extract body after frontmatter
        raw.find("\n---")
            .and_then(|i| {
                let rest = &raw[i + 4..];
                rest.find("\n---").map(|j| rest[j + 4..].trim().to_string())
            })
            .unwrap_or_default()
    };

    let now = system_time_iso8601();
    let frontmatter = format!(
        "---\nname: {}\ndescription: \"{}\"\nmetadata:\n  type: {}\n  modified: {}\n---\n\n{}\n",
        cur_name, new_desc, new_type, now, body
    );

    std::fs::write(&filepath, &frontmatter)
        .map_err(|e| format!("cannot write '{}': {}", filepath.display(), e))?;

    Ok(json!({
        "updated": true,
        "name": cur_name,
        "file": filename,
        "path": filepath.to_string_lossy(),
    }))
}

fn tool_memory_delete(args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let dir = resolve_memory_dir(args);

    let filename = format!("{}.md", name);
    let filepath = std::path::PathBuf::from(&dir).join(&filename);
    if !filepath.exists() {
        return Err(format!(
            "memory '{}' not found at {}",
            name,
            filepath.display()
        ));
    }

    // Read before deleting to confirm what was there
    let raw = std::fs::read_to_string(&filepath)
        .map_err(|e| format!("cannot read '{}': {}", filepath.display(), e))?;
    let (_cur_name, cur_desc, _cur_type) = parse_memory_frontmatter(&raw)
        .unwrap_or_else(|| (name.to_string(), String::new(), String::new()));

    std::fs::remove_file(&filepath)
        .map_err(|e| format!("cannot delete '{}': {}", filepath.display(), e))?;

    // Remove from MEMORY.md index
    let index_path = std::path::PathBuf::from(&dir).join("MEMORY.md");
    if index_path.exists() {
        let existing = std::fs::read_to_string(&index_path)
            .map_err(|e| format!("cannot read MEMORY.md: {}", e))?;
        let cleaned: String = existing
            .lines()
            .filter(|l| !l.contains(&filename))
            .collect::<Vec<_>>()
            .join("\n");
        // Append final newline if we had content
        let cleaned = if cleaned.is_empty() {
            cleaned
        } else {
            format!("{}\n", cleaned.trim_end())
        };
        std::fs::write(&index_path, &cleaned)
            .map_err(|e| format!("cannot update MEMORY.md: {}", e))?;
    }

    Ok(json!({
        "deleted": true,
        "name": name,
        "file": filename,
        "description": cur_desc,
    }))
}

fn name_to_title(name: &str) -> String {
    name.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().to_string() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn system_time_iso8601() -> String {
    // ponytail: avoid chrono dep — format manually from UNIX epoch. Good enough for memory timestamps.
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = dur.as_secs();
    let days_since_epoch = (total_secs / 86400) as i64;
    let time_of_day = total_secs % 86400;
    let h = time_of_day / 3600;
    let mi = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    // civil_from_days algorithm (Hinnant) — all i64 arithmetic
    let z = days_since_epoch + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64; // day of era, non-negative
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, h, mi, s)
}

fn tool_batch(k: &Kernel, args: &J) -> Result<J, String> {
    let ops = args
        .get("operations")
        .and_then(|v| v.as_array())
        .ok_or("missing: operations")?;
    let mut results = Vec::new();
    let mut koids: Vec<String> = Vec::new();
    for op in ops {
        let name = op
            .get("op")
            .or_else(|| op.get("type"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                // Fallback: detect operation by presence of remember/relate/forget keys
                op.get("remember")
                    .or_else(|| op.get("relate"))
                    .or_else(|| op.get("forget"))
                    .map(|_| {
                        if op.get("remember").is_some() {
                            "remember"
                        } else if op.get("relate").is_some() {
                            "relate"
                        } else if op.get("forget").is_some() {
                            "forget"
                        } else {
                            "unknown"
                        }
                    })
            })
            .unwrap_or("unknown");
        // Substitute $N references with previously returned KOIDs.
        let op_str = op.to_string();
        let mut resolved = op_str.clone();
        for (i, koid) in koids.iter().enumerate() {
            resolved = resolved.replace(&format!("${}.koid", i + 1), koid);
        }
        let resolved_op: J = serde_json::from_str(&resolved).unwrap_or(op.clone());
        let r: Result<J, String> = match name {
            "remember" => tool_remember(k, &resolved_op),
            "relate" => tool_relate(k, &resolved_op),
            "forget" => tool_forget(k, &resolved_op),
            _ => Err(format!("unknown batch op: {}", name)),
        };
        match r {
            Ok(result) => {
                if let Some(koid) = result.get("koid").and_then(|v| v.as_str()) {
                    koids.push(koid.to_string());
                }
                results.push(json!({"op": name, "ok": true, "result": result}));
            }
            Err(e) => {
                results.push(json!({"op": name, "ok": false, "error": e}));
            }
        }
    }
    Ok(json!({"results": results, "count": results.len()}))
}

fn tool_session_init(args: &J, session: &mut McpSession) -> Result<J, String> {
    // Store session identity for subsequent tool calls (MRFC-0040).
    session.agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("mcp-agent")
        .into();
    session.run_id = args
        .get("run_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    session.tenant = args
        .get("tenant")
        .and_then(|v| v.as_str())
        .map(String::from);
    session.roles = args
        .get("roles")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "session": {
            "agent_id": session.agent_id,
            "run_id": session.run_id,
            "tenant": session.tenant,
            "roles": session.roles,
        },
        "established": true,
        "note": "Session identity established. Subsequent tool calls in this connection inherit this context."
    }))
}

fn tool_decide(k: &Kernel, args: &J) -> Result<J, String> {
    let koid_hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(koid_hex).map_err(|e| e.to_string())?;
    let decision = args
        .get("decision")
        .and_then(|v| v.as_str())
        .ok_or("missing: decision")?;
    let rationale = args.get("rationale").and_then(|v| v.as_str()).unwrap_or("");
    let confidence = args
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let subject = subject_of(args);

    // Load the target KO and record the decision as a provenance-tagged update.
    let ko = k
        .get(KnowledgeContext::from(subject.clone()), &koid)
        .map_err(|e| e.to_string())?;
    let mut props = ko.properties.clone();
    props.insert("_decision".into(), Value::Text(decision.to_string()));
    props.insert("_rationale".into(), Value::Text(rationale.to_string()));
    props.insert("_confidence".into(), Value::Float(confidence));
    props.insert("_decided_by".into(), Value::Text(subject.name.clone()));
    let r = k
        .remember(RememberRequest {
            context: subject.into(),
            koid: Some(koid),
            expected_version: Some(ko.version),
            idempotency_key: Some(format!("decide-{}-{}", koid_hex, decision)),
            metadata: ko.metadata.clone(),
            properties: props,
            semantic: None,
            relationships: ko.relationships.clone(),
            security: Some(ko.security.clone()),
            extensions: ko.extensions.clone(),
            origin: Origin::Reason,
            note: Some(format!(
                "Decision: {} (confidence: {:.2}) — {}",
                decision, confidence, rationale
            )),
            referential_policy: ReferentialPolicy::Permissive,
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "koid": r.koid.to_hex(),
        "version": r.version,
        "decision": decision,
        "confidence": confidence,
        "recorded": true,
    }))
}

fn tool_abi_version(k: &Kernel) -> Result<J, String> {
    let version = k.abi_version();
    // Also export the full audit chain for offline verification.
    let proof = k.prove_export().map_err(|e| e.to_string())?;
    Ok(json!({
        "abi_version": version,
        "journal_seq": proof.journal_seq,
        "head_audit_hash": proof.head_audit_hash.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""),
        "event_count": proof.events.len(),
        "audit_chain_exportable": true,
    }))
}

fn tool_verify_backup(args: &J) -> Result<J, String> {
    let backup = args
        .get("backup")
        .and_then(|b| b.as_str())
        .ok_or("missing argument: backup")?;
    let data_path = format!("{}/data.redb", backup);
    if !std::path::Path::new(&data_path).exists() {
        return Err(format!("backup data file not found: {}", data_path));
    }
    let meta_str = std::fs::read_to_string(format!("{}/meta.json", backup))
        .map_err(|e| format!("not a valid backup: {}", e))?;
    let meta: J = serde_json::from_str(&meta_str).map_err(|e| format!("bad meta: {}", e))?;
    let expected_seq = meta["journal_seq"].as_u64().unwrap_or(0);
    let expected_objects = meta["object_count"].as_u64().unwrap_or(0) as usize;
    let ok = verify_backup_file(&data_path, expected_seq, expected_objects);
    Ok(json!({
        "backup": backup,
        "verified": ok,
        "expected_journal_seq": expected_seq,
        "expected_objects": expected_objects,
    }))
}

// ---------------------------------------------------------------------------
// HTTP metrics server — minimal std-based HTTP/1.0 handler
// ---------------------------------------------------------------------------

fn serve_metrics(
    kernel: Arc<Kernel>,
    ontology: Arc<OntologyRegistry>,
    addr: &str,
    db_path: &Arc<String>,
) {
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            error!(%addr, %e, "metrics server bind failed");
            return;
        }
    };
    let sessions: Arc<Mutex<HashMap<String, HttpSession>>> = Arc::new(Mutex::new(HashMap::new()));
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                let k = kernel.clone();
                let sess = sessions.clone();
                let db = db_path.clone();
                let ont = ontology.clone();
                std::thread::spawn(move || handle_http(&mut s, &k, &sess, &db, &ont));
            }
            Err(e) => {
                // ponytail: don't die on transient accept errors.
                if cfg!(debug_assertions) {
                    eprintln!("metrics accept error: {}", e);
                }
            }
        }
    }
}

/// Build graph JSON: { nodes: [...], edges: [...] }.
/// Query params: ?koid=<hex> to center on a node, &detail=1 for properties.
/// Without koid, returns all heads + their outbound relationships.
fn graph_api(k: &Kernel, path: &str) -> Result<String, String> {
    let mut center_koid: Option<KOID> = None;
    let mut detail = false;
    let mut type_filter: Option<String> = None;

    // Parse query string (ponytail: manual parsing, no url crate dep).
    if let Some(qs) = path.split_once('?').map(|(_, q)| q) {
        for pair in qs.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                match k {
                    "koid" => {
                        center_koid =
                            Some(KOID::from_hex(v).map_err(|e| format!("bad koid: {}", e))?);
                    }
                    "detail" => {
                        detail = v == "1" || v == "true";
                    }
                    "type" => {
                        type_filter = Some(v.to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    let browser_ctx = KnowledgeContext::from(Subject {
        name: "graph-browser".into(),
        roles: vec!["admin".into()],
    });

    // Parse tenant filter from query string.
    let tenant_filter: Option<String> = path.split_once('?').and_then(|(_, qs)| {
        qs.split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(k, _)| *k == "tenant")
            .map(|(_, v)| v.to_string())
    });

    let heads = k.scan_heads().map_err(|e| format!("scan: {}", e))?;
    let mut nodes: Vec<J> = Vec::new();
    let mut edges: Vec<J> = Vec::new();
    let mut nodes_added: HashSet<String> = HashSet::new(); // to avoid duplicate nodes
    let mut edges_done: HashSet<(String, String)> = HashSet::new(); // to avoid duplicate edges
    let mut edge_counts: HashMap<String, usize> = HashMap::new();

    // Collect all heads (non-deleted), optionally filtered by tenant and type.
    let mut head_koids: Vec<KOID> = Vec::new();
    for (koid, _ver, _ts, state) in &heads {
        if *state == LifecycleState::Deleted {
            continue;
        }
        let ko = match k.get(browser_ctx.clone(), koid) {
            Ok(ko) => ko,
            Err(_) => continue,
        };
        if let Some(ref tf) = tenant_filter {
            if ko.metadata.tenant.as_deref() != Some(tf.as_str()) {
                continue;
            }
        }
        if let Some(ref tf) = type_filter {
            if ko.metadata.type_name != *tf {
                continue;
            }
        }
        head_koids.push(*koid);
    }

    // If a center KOID is specified, start traversal from it first.
    let start_koids: Vec<KOID> = if let Some(c) = center_koid {
        if let Ok(ko) = k.get(browser_ctx.clone(), &c) {
            if tenant_filter
                .as_ref()
                .is_none_or(|tf| ko.metadata.tenant.as_deref() == Some(tf.as_str()))
            {
                add_node_api(&mut nodes, &mut nodes_added, &ko, detail);
            }
        }
        let mut v = vec![c];
        v.extend(head_koids.iter().filter(|k| **k != c).cloned());
        v
    } else {
        head_koids
    };

    // Phase 1: add all head nodes (respecting tenant filter).
    for koid in &start_koids {
        let hex = koid.to_hex();
        if nodes_added.contains(&hex) {
            continue;
        }
        if let Ok(ko) = k.get(browser_ctx.clone(), koid) {
            add_node_api(&mut nodes, &mut nodes_added, &ko, detail);
        }
    }

    // Phase 2: traverse edges from EVERY starting node.
    for koid in &start_koids {
        traverse_edges(
            k,
            koid,
            &mut nodes,
            &mut nodes_added,
            &mut edges,
            &mut edges_done,
            detail,
            &browser_ctx,
            &mut edge_counts,
        );
    }

    // Apply edge counts to node sizes (hub nodes are bigger).
    for node in &mut nodes {
        let koid = node["koid"].as_str().unwrap_or("");
        let count = edge_counts.get(koid).copied().unwrap_or(0);
        // Size: base 18 + 4 per edge (max 42).
        let size = (18 + count * 4).min(42);
        node["size"] = json!(size);
        node["edge_count"] = json!(count);
    }

    // Collect available tenants for the filter dropdown.
    let mut tenants: Vec<String> = nodes
        .iter()
        .filter_map(|n| n["tenant"].as_str())
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    tenants.sort();

    Ok(json!({
        "nodes": nodes,
        "edges": edges,
        "tenants": tenants,
        "tenant_filter": tenant_filter,
    })
    .to_string())
}

fn add_node_api(
    nodes: &mut Vec<J>,
    nodes_added: &mut HashSet<String>,
    ko: &KnowledgeObject,
    detail: bool,
) {
    let hex = ko.koid.to_hex();
    if nodes_added.contains(&hex) {
        return;
    }
    nodes_added.insert(hex.clone());
    let c = color_for_type(&ko.metadata.type_name);
    let label = node_label(ko, 30);
    let tenant = ko.metadata.tenant.clone().unwrap_or_default();
    let key_props: Vec<J> = ko
        .properties
        .iter()
        .take(3)
        .map(|(k, v)| json!({"key": k, "value": value_to_json(v)}))
        .collect();
    let mut node = json!({
        "koid": hex,
        "type_name": ko.metadata.type_name,
        "tenant": tenant,
        "label": label,
        "color": c,
        "version": ko.version,
        "key_props": key_props,
    });
    if detail {
        let props: serde_json::Map<String, J> = ko
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect();
        node["properties"] = json!(props);
        node["lifecycle"] = json!({
            "state": ko.lifecycle.state.to_string(),
            "origin": format!("{:?}", ko.lifecycle.origin),
        });
        node["tags"] = json!(ko.metadata.tags);
        node["schema_version"] = json!(ko.metadata.schema_version);
        node["security"] = json!({
            "owner": ko.security.owner,
            "classification": ko.security.classification,
            "acl_count": ko.security.acl.len(),
        });
        node["extensions"] = json!(ko
            .extensions
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect::<serde_json::Map<_, _>>());
        node["relationships"] = json!(ko
            .relationships
            .iter()
            .map(|r| json!({
                "target": r.target.to_hex(),
                "type": r.rel_type,
                "direction": format!("{:?}", r.direction),
            }))
            .collect::<Vec<_>>());
        node["event_refs"] = json!(ko.event_refs.len());
    }
    nodes.push(node);
}

/// Traverse outbound relationships from a node, adding edges and discovering new nodes.
/// Separate from node addition so every head gets edge-traversed, even nodes discovered
/// via another node's edges.
fn traverse_edges(
    k: &Kernel,
    koid: &KOID,
    nodes: &mut Vec<J>,
    nodes_added: &mut HashSet<String>,
    edges: &mut Vec<J>,
    edges_done: &mut HashSet<(String, String)>,
    detail: bool,
    ctx: &KnowledgeContext,
    edge_counts: &mut HashMap<String, usize>,
) {
    let q = TraverseQuery {
        context: ctx.clone(),
        start: *koid,
        rel_type: None,
        depth: 1,
        direction: Some(Direction::Outbound),
    };
    let hits = match k.traverse(q) {
        Ok(h) => h,
        Err(_) => return,
    };
    let source_hex = koid.to_hex();
    for h in &hits {
        let target_hex = h.koid.to_hex();
        // Skip duplicate edges.
        let edge_key = (source_hex.clone(), target_hex.clone());
        if edges_done.contains(&edge_key) {
            continue;
        }
        edges_done.insert(edge_key);
        edges.push(json!({
            "source": source_hex,
            "target": target_hex,
            "rel_type": h.rel_type,
        }));
        *edge_counts.entry(source_hex.clone()).or_insert(0) += 1;
        *edge_counts.entry(target_hex.clone()).or_insert(0) += 1;
        // Discover target node if not already added.
        if !nodes_added.contains(&target_hex) {
            if let Ok(ko) = k.get(ctx.clone(), &h.koid) {
                add_node_api(nodes, nodes_added, &ko, detail);
            }
        }
    }
}

/// Build a human-readable label for a knowledge object.
/// Priority: "name" property → "title" → first text property → type_name → KOID prefix.
fn node_label(ko: &KnowledgeObject, max_len: usize) -> String {
    // Try named properties first.
    for key in &["name", "title", "label", "subject", "id"] {
        if let Some(v) = ko.properties.get(*key) {
            let s = value_to_string(v);
            if !s.is_empty() {
                return truncate(&s, max_len);
            }
        }
    }
    // First text property
    for v in ko.properties.values() {
        if let Value::Text(s) = v {
            if !s.is_empty() {
                return truncate(s, max_len);
            }
        }
    }
    // Fallback: type_name
    truncate(&ko.metadata.type_name, max_len)
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        Value::Bytes(_) => "(binary)".into(),
        Value::List(items) => format!("[{} items]", items.len()),
        Value::Map(m) => format!("{{{} keys}}", m.len()),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3).min(s.len())])
    }
}

fn color_for_type(type_name: &str) -> &str {
    const COLORS: &[&str] = &[
        "#8be9fd", "#ff79c6", "#50fa7b", "#ffb86c", "#bd93f9", "#ff5555", "#f1fa8c", "#6be5c1",
        "#ff92d0", "#a6e3a1", "#89b4fa", "#fab387", "#cba6f7", "#f38ba8", "#94e2d5", "#74c7ec",
    ];
    let mut h: u32 = 0;
    for b in type_name.as_bytes() {
        h = h.wrapping_mul(31).wrapping_add(*b as u32);
    }
    COLORS[(h as usize) % COLORS.len()]
}

// ---------------------------------------------------------------------------
// HTTP auth helpers
// ---------------------------------------------------------------------------

fn extract_token(path: &str, req: &str) -> Option<String> {
    // From query param: ?token=abc123
    if let Some(qs) = path.split_once('?').map(|(_, q)| q) {
        for pair in qs.split('&') {
            if let Some(("token", v)) = pair.split_once('=') {
                return Some(v.to_string());
            }
        }
    }
    // From Authorization header: Bearer abc123
    for line in req.lines() {
        if let Some(v) = line.strip_prefix("Authorization: Bearer ") {
            return Some(v.trim().to_string());
        }
    }
    None
}

fn handle_login(
    body: &str,
    sessions: &Mutex<HashMap<String, HttpSession>>,
) -> Result<String, String> {
    let creds: J = serde_json::from_str(body).map_err(|e| format!("bad JSON: {}", e))?;
    let username = creds.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let password = creds.get("password").and_then(|v| v.as_str()).unwrap_or("");

    // Default credentials (ponytail: hardcoded, config-file in prod).
    let valid = match username {
        "admin" => password == "admin",
        "user" => password == "user" || password == "readonly",
        _ => false,
    };
    if !valid {
        return Err("invalid credentials".into());
    }
    let roles: Vec<String> = if username == "admin" {
        vec!["admin".into()]
    } else {
        vec![]
    };
    // Generate a simple session token.
    // Generate session token from time + PID (ponytail: not cryptographic, fine for localhost UI).
    let token = format!(
        "{:x}{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    );
    sessions.lock().unwrap().insert(
        token.clone(),
        HttpSession {
            username: username.to_string(),
            roles,
            created: Instant::now(),
        },
    );
    Ok(token)
}

fn validate_token(
    token: Option<&str>,
    sessions: &Mutex<HashMap<String, HttpSession>>,
) -> Option<Subject> {
    let token = token?;
    let guard = sessions.lock().unwrap();
    let sess = guard.get(token)?;
    // Session expires after 24h.
    if sess.created.elapsed().as_secs() > 86400 {
        return None;
    }
    Some(Subject {
        name: sess.username.clone(),
        roles: sess.roles.clone(),
    })
}

fn aikoql_endpoint(
    k: &Kernel,
    query: &str,
    subject: &Subject,
    tenant: Option<&str>,
    ontology: &OntologyRegistry,
) -> Result<String, String> {
    if query.trim().is_empty() {
        return Err("empty query".into());
    }
    // Parse the aikoql statement.
    let stmt = aikoql_compiler::parser::parse(query).map_err(|e| e.to_string())?;

    // CREATE mutation.
    if let aikoql_compiler::parser::ast::Statement::Create(create) = &stmt {
        if !subject.roles.contains(&"admin".to_string()) {
            return Err("CREATE requires admin role".into());
        }
        let mut props = PropertyMap::new();
        for (k, v) in &create.properties {
            props.insert(k.clone(), compiler_expr_to_value(v));
        }
        let r = k
            .remember(RememberRequest {
                context: subject.clone().into(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: create.entity.clone(),
                    tenant: tenant.map(String::from),
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: ExtensionMap::new(),
                origin: Origin::Human,
                note: None,
                referential_policy: ReferentialPolicy::default(),
            })
            .map_err(|e| e.to_string())?;
        return Ok(
            json!({"created": r.koid.to_hex(), "version": r.version, "commit_ts": r.commit_ts})
                .to_string(),
        );
    }

    // Query: MATCH, TRAVERSE, etc. — ontology-aware compilation.
    let schema = SchemaRegistry::new(); // ponytail: empty registry; ontology handles resolution
    let plans = aikoql_compiler::parser::compile_with_ontology(
        query,
        &subject.name,
        &schema,
        Some(ontology),
    )
    .map_err(|e| e.to_string())?;
    // Execute all plans and merge results.
    let mut all_kos: Vec<serde_json::Value> = Vec::new();
    for plan in &plans {
        match aikoql_runtime::Interpreter::execute(k, plan).map_err(|e| e.to_string())? {
            aikoql_runtime::RowSet::Objects(kos) => {
                for ko in kos {
                    all_kos.push(json!({
                        "koid": ko.koid.to_hex(),
                        "type_name": ko.metadata.type_name,
                        "version": ko.version,
                        "properties": ko.properties.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect::<serde_json::Map<_,_>>()
                    }));
                }
            }
            aikoql_runtime::RowSet::Scored(scored) => {
                for (koid, score, tn, ver) in scored {
                    all_kos.push(json!({
                        "koid": koid.to_hex(), "score": score, "type_name": tn, "version": ver
                    }));
                }
            }
            aikoql_runtime::RowSet::Traversal(hits) => {
                for (koid, rt, depth) in hits {
                    all_kos.push(json!({
                        "koid": koid.to_hex(), "rel_type": rt, "depth": depth
                    }));
                }
            }
        }
    }
    Ok(json!({"results": all_kos}).to_string())
}

fn parse_query_param(path: &str, key: &str) -> String {
    path.split_once('?')
        .and_then(|(_, qs)| {
            qs.split('&')
                .filter_map(|p| p.split_once('='))
                .find(|(k, _)| *k == key)
                .map(|(_, v)| url_decode(v))
        })
        .unwrap_or_default()
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(hex as char);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(' ');
        } else {
            out.push(bytes[i] as char);
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Schema discovery — critical for agent schema-awareness
// ---------------------------------------------------------------------------

fn schema_endpoint(k: &Kernel) -> Result<String, String> {
    let types = k.list_types().map_err(|e| format!("{}", e))?;
    let heads = k.scan_heads().map_err(|e| format!("{}", e))?;
    let mut schema: serde_json::Map<String, J> = serde_json::Map::new();

    // Aggregate property keys per type by scanning live objects.
    let ctx = KnowledgeContext::from(Subject {
        name: "schema-browser".into(),
        roles: vec!["admin".into()],
    });
    let mut type_props: HashMap<String, HashSet<String>> = HashMap::new();
    let mut type_counts: HashMap<String, usize> = HashMap::new();
    let mut type_tenants: HashMap<String, HashSet<String>> = HashMap::new();

    for (koid, _ver, _ts, state) in &heads {
        if *state == LifecycleState::Deleted {
            continue;
        }
        if let Ok(ko) = k.get(ctx.clone(), koid) {
            let tn = ko.metadata.type_name.clone();
            *type_counts.entry(tn.clone()).or_insert(0) += 1;
            if let Some(t) = &ko.metadata.tenant {
                type_tenants
                    .entry(tn.clone())
                    .or_default()
                    .insert(t.clone());
            }
            let entry = type_props.entry(tn).or_default();
            for key in ko.properties.keys() {
                entry.insert(key.clone());
            }
        }
    }

    for t in &types {
        let info = json!({
            "count": type_counts.get(t).copied().unwrap_or(0),
            "properties": type_props.get(t).map(|s| s.iter().collect::<Vec<_>>()).unwrap_or_default(),
            "tenants": type_tenants.get(t).map(|s| s.iter().collect::<Vec<_>>()).unwrap_or_default(),
        });
        schema.insert(t.clone(), info);
    }

    Ok(json!({
        "types": types,
        "total_types": types.len(),
        "schema": schema,
    })
    .to_string())
}

// ---------------------------------------------------------------------------
// Query explain — shows the IR plan before execution
// ---------------------------------------------------------------------------

fn explain_endpoint(query: &str) -> Result<String, String> {
    let plan = aikoql_compiler::parser::compile(query).map_err(|e| e.to_string())?;
    Ok(json!({
        "query": query,
        "operators": plan.operators.iter().map(|op| format!("{:?}", op)).collect::<Vec<_>>(),
        "operator_count": plan.operators.len(),
    })
    .to_string())
}

fn handle_http(
    stream: &mut TcpStream,
    k: &Kernel,
    sessions: &Mutex<HashMap<String, HttpSession>>,
    db_path: &Arc<String>,
    ontology: &OntologyRegistry,
) {
    // ponytail: 64 KB buffer fits all practical HTTP requests. Browsers send
    // ~2-8 KB of headers; single read captures the full request.
    let mut buf = [0u8; 65536];
    let n = match stream.read(&mut buf) {
        Ok(0) => return,
        Ok(n) => n,
        Err(_) => return,
    };
    // Read body remainder if Content-Length exceeds what we already got.
    let req = String::from_utf8_lossy(&buf[..n]);
    let first_line = req.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0];
    let path = parts[1];
    let route = path.split('?').next().unwrap_or(path);
    let mut body_str = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    // If body truncated, read remainder.
    let hdr_part = req.split("\r\n\r\n").next().unwrap_or("");
    let mut content_len: Option<usize> = None;
    for line in hdr_part.lines() {
        if line.to_lowercase().starts_with("content-length:") {
            content_len = line.split(':').nth(1).and_then(|s| s.trim().parse().ok());
        }
    }
    if let Some(cl) = content_len {
        while body_str.len() < cl {
            let needed = cl - body_str.len();
            let extra = needed.max(4096);
            let mut rest = vec![0u8; extra];
            match stream.read(&mut rest) {
                Ok(0) => break,
                Ok(n) => body_str.push_str(&String::from_utf8_lossy(&rest[..n])),
                Err(_) => break,
            }
        }
    }

    // Extract token from query string or Authorization header.
    let token = extract_token(path, &req);

    // CORS preflight.
    if method == "OPTIONS" {
        let (status, ct, body) = api_rest::cors_preflight();
        let mut resp = format!(
            "HTTP/1.0 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
            status,
            ct,
            body.len()
        );
        for (k, v) in api_rest::cors_headers() {
            resp.push_str(&format!("{}: {}\r\n", k, v));
        }
        resp.push_str("\r\n");
        resp.push_str(&body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    // REST API v1 routes — handled separately, return early.
    if route.starts_with("/api/v1/") {
        let (status, ct, body) = api_rest::route_v1(
            method,
            path,
            &body_str,
            k,
            db_path.as_str(),
            sessions,
            token.clone(),
        );
        let mut resp = format!(
            "HTTP/1.0 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
            status,
            ct,
            body.len()
        );
        for (h, v) in api_rest::cors_headers() {
            resp.push_str(&format!("{}: {}\r\n", h, v));
        }
        resp.push_str("\r\n");
        resp.push_str(&body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let (status, content_type, body) = match route {
        "/ui" => (
            "200 OK",
            "text/html; charset=utf-8",
            graph_ui::GRAPH_UI_HTML.to_string(),
        ),
        "/" | "/studio" => (
            "200 OK",
            "text/html; charset=utf-8",
            studio::STUDIO_HTML.to_string(),
        ),
        "/health" => {
            let uptime = SERVER_START
                .get()
                .map(|s| s.elapsed().as_secs_f64())
                .unwrap_or(0.0);
            let body =
                json!({"status":"ok","uptime_seconds":(uptime * 10.0).round() / 10.0}).to_string();
            ("200 OK", "application/json", body)
        }
        "/metrics" => {
            let body = prometheus_metrics(k);
            ("200 OK", "text/plain; version=0.0.4", body)
        }
        "/api/login" if method == "POST" => match handle_login(&body_str, sessions) {
            Ok(token) => (
                "200 OK",
                "application/json",
                json!({"token": token}).to_string(),
            ),
            Err(e) => (
                "401 Unauthorized",
                "application/json",
                json!({"error": e}).to_string(),
            ),
        },
        _p if route.starts_with("/api/graph") => {
            let body = graph_api(k, path);
            match body {
                Ok(b) => ("200 OK", "application/json", b),
                Err(e) => ("500 Internal Server Error", "text/plain", e),
            }
        }
        _p if route.starts_with("/api/schema") => match schema_endpoint(k) {
            Ok(b) => ("200 OK", "application/json", b),
            Err(e) => (
                "500 Internal Server Error",
                "application/json",
                json!({"error": e, "code": "INTERNAL"}).to_string(),
            ),
        },
        _p if route.starts_with("/api/explain") => {
            let query = parse_query_param(path, "query");
            match explain_endpoint(&query) {
                Ok(b) => ("200 OK", "application/json", b),
                Err(e) => (
                    "400 Bad Request",
                    "application/json",
                    json!({"error": e, "code": "PARSE_ERROR"}).to_string(),
                ),
            }
        }
        _p if route.starts_with("/api/aikoql") => {
            let session = validate_token(token.as_deref(), sessions);
            if let Some(session) = session {
                let query = parse_query_param(path, "query");
                let tenant = {
                    let t = parse_query_param(path, "tenant");
                    if t.is_empty() {
                        None
                    } else {
                        Some(t)
                    }
                };
                let tenant_deref = tenant.as_deref();
                match aikoql_endpoint(k, &query, &session, tenant_deref, ontology) {
                    Ok(b) => ("200 OK", "application/json", b),
                    Err(e) => (
                        "400 Bad Request",
                        "application/json",
                        json!({"error": e}).to_string(),
                    ),
                }
            } else {
                (
                    "401 Unauthorized",
                    "application/json",
                    json!({"error": "login required"}).to_string(),
                )
            }
        }
        _ => ("404 Not Found", "text/plain", "Not Found\n".into()),
    };

    let mut resp = format!(
        "HTTP/1.0 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
        status,
        content_type,
        body.len(),
    );
    // CORS headers on all responses.
    for (k, v) in api_rest::cors_headers() {
        resp.push_str(&format!("{}: {}\r\n", k, v));
    }
    resp.push_str("\r\n");
    resp.push_str(&body);
    let _ = stream.write_all(resp.as_bytes());
}

fn prometheus_metrics(k: &Kernel) -> String {
    let (seq, _) = k.journal_head().unwrap_or((0, [0u8; 32]));
    let heads = k.scan_heads().unwrap_or_default();
    let active = heads
        .iter()
        .filter(|(_, _, _, s)| *s != LifecycleState::Deleted)
        .count();
    let uptime = SERVER_START
        .get()
        .map(|s| s.elapsed().as_secs_f64())
        .unwrap_or(0.0);

    format!(
        "# HELP aikoql_journal_seq Monotonically increasing journal sequence number.\n\
         # TYPE aikoql_journal_seq counter\n\
         aikoql_journal_seq {}\n\
         # HELP aikoql_objects_total Total committed head objects.\n\
         # TYPE aikoql_objects_total gauge\n\
         aikoql_objects_total {}\n\
         # HELP aikoql_objects_active Active (non-deleted) head objects.\n\
         # TYPE aikoql_objects_active gauge\n\
         aikoql_objects_active {}\n\
         # HELP aikoql_uptime_seconds Server uptime in seconds.\n\
         # TYPE aikoql_uptime_seconds gauge\n\
         aikoql_uptime_seconds {:.1}\n",
        seq,
        heads.len(),
        active,
        uptime
    )
}

fn tool_metrics(k: &Kernel) -> Result<J, String> {
    let (seq, _audit) = k.journal_head().map_err(|e| e.to_string())?;
    let heads = k.scan_heads().map_err(|e| e.to_string())?;
    let active = heads
        .iter()
        .filter(|(_, _, _, s)| *s != LifecycleState::Deleted)
        .count();
    let mut draft = 0u64;
    let mut active_st = 0u64;
    let mut verified = 0u64;
    let mut archived = 0u64;
    let mut deleted = 0u64;
    for (_, _, _, s) in &heads {
        match s {
            LifecycleState::Draft => draft += 1,
            LifecycleState::Active => active_st += 1,
            LifecycleState::Verified => verified += 1,
            LifecycleState::Archived => archived += 1,
            LifecycleState::Deleted => deleted += 1,
            // MRFC-0070 states: count as draft-equivalent pending
            LifecycleState::Discovered
            | LifecycleState::Extracted
            | LifecycleState::Proposed
            | LifecycleState::Validated
            | LifecycleState::Accepted
            | LifecycleState::Updated
            | LifecycleState::Superseded => draft += 1,
        }
    }
    // Type-level breakdown (ponytail: O(n) scan; add type index if slow).
    let types = k.list_types().unwrap_or_default();
    let system = Subject::with_roles("system", &["admin"]);
    let mut by_type = serde_json::Map::new();
    for t in &types {
        if let Ok(kos) = k.scan_by_type(&system, t) {
            by_type.insert(t.clone(), json!(kos.len()));
        }
    }
    let uptime_secs = SERVER_START
        .get()
        .map(|start| start.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    Ok(json!({
        "journal_seq": seq,
        "total_objects": heads.len(),
        "active_objects": active,
        "uptime_seconds": (uptime_secs * 10.0).round() / 10.0,
        "by_lifecycle": {
            "draft": draft,
            "active": active_st,
            "verified": verified,
            "archived": archived,
            "deleted": deleted,
        },
        "by_type": by_type,
    }))
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

fn tools_list() -> J {
    let subj = json!({"type": "string", "description": "calling principal (default: mcp-agent)"});
    let koid = json!({"type": "string", "description": "32-char hex KOID"});
    json!({
        "tools": [
            {"name": "remember", "description": "Commit a knowledge object (or new version) with provenance. Set embed:true for auto-embedding via SemanticEngine (MRFC-0040). Returns KOID+version.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "koid": koid, "properties": {"type": "object"}, "semantic": {"type": "object"}, "embed": {"type": "boolean", "description": "Request auto-embedding via configured AI provider (MRFC-0040)"}, "expected_version": {"type": "integer"}, "idempotency_key": {"type": "string"}, "note": {"type": "string"}}, "required": ["type_name"]}},
            {"name": "forget", "description": "Tombstone or legally erase a knowledge object (audit-preserving).", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "mode": {"type": "string", "enum": ["tombstone", "erase"]}}, "required": ["koid"]}},
            {"name": "evolve", "description": "Transition a knowledge object along its lifecycle (draft->active->verified->archived->deleted).", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "to": {"type": "string"}}, "required": ["koid", "to"]}},
            {"name": "verify", "description": "Check whether a subject may perform an action on an object.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "action": {"type": "string"}}, "required": ["koid", "action"]}},
            {"name": "get", "description": "Fetch a knowledge object by KOID.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid}, "required": ["koid"]}},
            {"name": "find_similar", "description": "Hybrid recall: vector + text + filters with RRF/weighted fusion.", "inputSchema": {"type": "object", "properties": {"subject": subj, "text": {"type": "string"}, "vector": {"type": "array"}, "embedding_model": {"type": "string", "description": "When set, only vectors from this embedding model are considered"}, "k": {"type": "integer"}, "fusion": {"type": "string"}, "type_name": {"type": "string"}}}},
            {"name": "trace", "description": "Full lineage of a fact: versions + events.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid}, "required": ["koid"]}},
            {"name": "explain", "description": "Why is this believed: provenance, source, confidence, evidence.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "version": {"type": "integer"}}, "required": ["koid"]}},
            {"name": "prove", "description": "Verify the hash-chained audit trail for a claim.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid}, "required": ["koid"]}},
            {"name": "provenance", "description": "Render the full provenance chain as markdown: source, evidence, audit trail.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid}, "required": ["koid"]}},
            {"name": "relate", "description": "Add a directed relationship edge from one KO to another.", "inputSchema": {"type": "object", "properties": {"subject": subj, "from": koid, "to": koid, "rel_type": {"type": "string"}}, "required": ["from", "to", "rel_type"]}},
            {"name": "traverse", "description": "Walk relationship edges from a starting KO up to a depth.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "rel_type": {"type": "string"}, "depth": {"type": "integer"}}, "required": ["koid"]}},
            {"name": "eval_recall", "description": "Measure recall@k against an expected KOID set.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "text": {"type": "string"}, "vector": {"type": "array"}, "k": {"type": "integer"}, "fusion": {"type": "string"}, "expected": {"type": "array", "items": {"type": "string"}}}, "required": ["expected"]}},
            {"name": "eval_staleness", "description": "Report index_lag_ms distribution for a recall query.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "text": {"type": "string"}, "vector": {"type": "array"}, "k": {"type": "integer"}, "fusion": {"type": "string"}}}},
            {"name": "eval_contradictions", "description": "Find same-type, high-similarity object pairs whose property values differ.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "property": {"type": "string"}, "threshold": {"type": "number"}, "max_results": {"type": "integer"}}, "required": ["type_name", "property"]}},
            {"name": "aikoql", "description": "Execute an aikoql query (text-based knowledge query language). Supports MATCH, WHERE, SIMILAR TO, TRAVERSE, RETURN, CREATE, UPDATE, DELETE.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string", "description": "aikoql query text"}, "subject": {"type": "string", "description": "Calling principal for ACL (default: query-user)"}}, "required": ["query"]}},
            {"name": "backup", "description": "Create a timestamped backup of the database.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "restore", "description": "Restore the database from a backup directory.", "inputSchema": {"type": "object", "properties": {"backup": {"type": "string", "description": "Backup directory name"}}, "required": ["backup"]}},
            {"name": "list_backups", "description": "List available backups in the current directory.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "verify_backup", "description": "Verify a backup by opening it in a temporary kernel and checking journal + object count integrity.", "inputSchema": {"type": "object", "properties": {"backup": {"type": "string", "description": "Backup directory name"}}, "required": ["backup"]}},
            {"name": "metrics", "description": "Return database metrics: journal sequence, object counts, uptime.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "audit_report", "description": "Generate a compliance audit report with full object inventory and audit chain hash.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "compliance_report", "description": "Generate an encryption compliance report: policies, key inventory, audit events, compliance grade (A/C).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "reason", "description": "Execute a reasoning rule: find objects matching properties and produce provenance-tagged claims.", "inputSchema": {"type": "object", "properties": {"type_name": {"type": "string"}, "properties": {"type": "object"}}, "required": ["type_name"]}},
            {"name": "infer", "description": "Infer similar knowledge: find objects textually similar to a query within a type.", "inputSchema": {"type": "object", "properties": {"type_name": {"type": "string"}, "text": {"type": "string"}}, "required": ["type_name"]}},
            {"name": "predict", "description": "Predict properties for a target object based on top-k similar objects.", "inputSchema": {"type": "object", "properties": {"type_name": {"type": "string"}, "properties": {"type": "object"}, "k": {"type": "integer"}}, "required": ["type_name"]}},
            {"name": "abi_version", "description": "Return ABI version and exportable audit chain for offline verification.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_program", "description": "Deploy an aikoql program as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "body": {"type": "string"}, "language": {"type": "string"}}, "required": ["name", "body"]}},
            {"name": "execute_program", "description": "Execute a deployed program KO by KOID with optional parameters.", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "params": {"type": "object"}}, "required": ["koid"]}},
            {"name": "list_programs", "description": "List all deployed program Knowledge Objects.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_policy", "description": "Deploy an RBAC policy as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "effect": {"type": "string"}, "principal": {"type": "string"}, "action": {"type": "string"}, "resource_type": {"type": "string"}, "condition": {"type": "string"}}, "required": ["name", "effect", "principal", "action", "resource_type"]}},
            {"name": "evaluate_policies", "description": "Evaluate all Policy KOs for a (principal, action, resource) tuple.", "inputSchema": {"type": "object", "properties": {"principal": {"type": "string"}, "action": {"type": "string"}, "resource_type": {"type": "string"}}, "required": ["principal", "action", "resource_type"]}},
            {"name": "deploy_workflow", "description": "Deploy a workflow (DAG of programs) as a versioned KO (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "steps": {"type": "array"}}, "required": ["name", "steps"]}},
            {"name": "deploy_trigger", "description": "Deploy an event-condition-action trigger as a versioned KO (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "event_kind": {"type": "string"}, "type_filter": {"type": "string"}, "program_koid": {"type": "string"}}, "required": ["name", "event_kind", "program_koid"]}},
            {"name": "add_dependency", "description": "Create a DEPENDS_ON relationship between two Active KOs (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"source": {"type": "string"}, "target": {"type": "string"}, "dep_type": {"type": "string"}}, "required": ["source", "target"]}},
            {"name": "execute_workflow", "description": "Execute a Workflow KO by KOID — runs all program steps in order (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}}, "required": ["koid"]}},
            {"name": "check_triggers", "description": "Check journal for matching Trigger KOs and fire them (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "program_cache_stats", "description": "Return ProgramCache hit stats (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_agent", "description": "Deploy an AI agent as a versioned Knowledge Object with prompt, skills, tools, policies (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "prompt": {"type": "string"}, "skills": {"type": "array"}, "tools": {"type": "array"}, "policies": {"type": "array"}}, "required": ["name"]}},
            {"name": "list_agents", "description": "List all deployed agent Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "execute_agent", "description": "Execute an Agent KO — loads the agent, resolves skills to Program KOs, executes each skill (MRFC-0030 Phase 7c).", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}}, "required": ["koid"]}},
            {"name": "deploy_connector", "description": "Deploy an external system connector as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "plugin": {"type": "string"}, "config": {"type": "object"}, "mapping": {"type": "array"}}, "required": ["name", "plugin"]}},
            {"name": "list_connectors", "description": "List all deployed connector Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_view", "description": "Deploy a materialized knowledge view as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "query": {"type": "string"}, "refresh_seconds": {"type": "integer"}}, "required": ["name", "query"]}},
            {"name": "list_views", "description": "List all deployed view Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_report", "description": "Deploy a compliance/analytics report definition as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "template": {"type": "string"}, "format": {"type": "string"}, "parameters": {"type": "array"}}, "required": ["name", "template", "format"]}},
            {"name": "list_reports", "description": "List all deployed report Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_benchmark", "description": "Deploy a versioned, replayable performance benchmark as a Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "target_query": {"type": "string"}, "iterations": {"type": "integer"}, "warmup": {"type": "integer"}}, "required": ["name", "target_query"]}},
            {"name": "list_benchmarks", "description": "List all deployed benchmark Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "document_ingest", "description": "Ingest a document: base64-encoded content → artifact store → Document KO. Returns koid + SHA-256. (MRFC-0050)", "inputSchema": {"type": "object", "properties": {"filename": {"type": "string"}, "content_base64": {"type": "string"}, "mime_type": {"type": "string"}}, "required": ["filename", "content_base64"]}},
            {"name": "document_list", "description": "List all ingested Document Knowledge Objects (MRFC-0050).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "document_status", "description": "Get processing status and metadata for an ingested document by KOID (MRFC-0050).", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}}, "required": ["koid"]}},
            {"name": "document_compile", "description": "Run the full D1-D9 document knowledge compiler pipeline on an ingested document. Returns IR entities, ontology proposals, entity resolution, commit plan, embedded chunks, and evidence trail. (MRFC-0050)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid"]}},
            {"name": "compile_context", "description": "Compile a minimum sufficient context package for an agent task from a knowledge document. Takes a task description and returns ranked entities, facts, and relationships under a token budget. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "task": {"type": "string", "description": "Natural language task description"}, "token_budget": {"type": "integer", "default": 2000, "description": "Max tokens for the context package"}, "subject": {"type": "string"}}, "required": ["koid", "task"]}},
            {"name": "reconcile", "description": "Reconcile changed files against a knowledge document. Given a list of changed file paths (e.g., from git diff), returns affected entities, potentially stale facts, and an impact report. (MRFC-0070-A8)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "files": {"type": "array", "items": {"type": "string"}, "description": "List of changed file paths (e.g., from git diff --name-only)"}, "subject": {"type": "string"}}, "required": ["koid", "files"]}},
            {"name": "connector_bridge", "description": "Convert connector schema metadata into KnowledgeIr. Provide connector_type (postgres/sqlite/mongodb/neo4j), label, and optional tables/references arrays. Each table needs name and fields (array of {name, data_type, is_primary_key, nullable, is_unique}). Each reference needs from_container, from_fields, to_container, to_fields, and optional name. (MRFC-0070-A9)", "inputSchema": {"type": "object", "properties": {"connector_type": {"type": "string"}, "label": {"type": "string"}, "tables": {"type": "array"}, "references": {"type": "array"}}, "required": ["connector_type"]}},
            {"name": "filter_secrets", "description": "Scan a knowledge document for secrets, API keys, tokens, emails, credit cards, and PII. Returns a list of findings with type, location, and redacted text. (MRFC-0070-A7)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid"]}},
            {"name": "explain_component", "description": "Explain a component: purpose, dependencies, dependents, facts, decisions, and tests. aikoql: EXPLAIN COMPONENT. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "name": {"type": "string", "description": "Component name"}, "subject": {"type": "string"}}, "required": ["koid", "name"]}},
            {"name": "explain_decision", "description": "Explain an architectural decision: context, problem, options, selected, rationale, consequences. aikoql: EXPLAIN DECISION. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "name": {"type": "string", "description": "ADR name"}, "subject": {"type": "string"}}, "required": ["koid", "name"]}},
            {"name": "trace_requirement", "description": "Trace a requirement through decisions, components, functions, to tests. aikoql: TRACE REQUIREMENT. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "requirement": {"type": "string", "description": "Requirement text or ID"}, "subject": {"type": "string"}}, "required": ["koid", "requirement"]}},
            {"name": "find_conflicts", "description": "Find contradictory claims and ambiguous facts about a component. aikoql: FIND CONFLICTS. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "component": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid", "component"]}},
            {"name": "find_stale", "description": "Find stale documentation: documentation that has diverged from code. aikoql: FIND STALE DOCUMENTATION. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid"]}},
            {"name": "validate_change", "description": "Validate a proposed change: what knowledge entities, facts, and relations would be affected? Returns risk assessment. aikoql: VALIDATE CHANGE. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "change": {"type": "string", "description": "Change description"}, "subject": {"type": "string"}}, "required": ["koid", "change"]}},
            {"name": "propose_update", "description": "Propose a knowledge update: add/remove facts, update entities, add/remove relations. Enters reconciliation workflow (PROPOSED → VALIDATED → ACCEPTED/REJECTED). aikoql: PROPOSE KNOWLEDGE UPDATE. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "action": {"type": "string", "enum": ["add_fact", "remove_fact", "update_entity", "add_relation", "remove_relation"]}, "target_entity": {"type": "string"}, "new_facts": {"type": "array", "items": {"type": "string"}}, "remove_facts": {"type": "array", "items": {"type": "string"}}, "new_relations": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}}, "justification": {"type": "string"}, "agent_id": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid", "action"]}},
            {"name": "discover_schema", "description": "Discover all types and their properties in the database (MRFC-0040 agent experience).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "discover_ontology", "description": "Auto-discover an ontology from all stored Knowledge Objects: classes, properties, relationships, and source mappings (MRFC-0041). Saves the ontology as an Ontology KO.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "health", "description": "Health check with readiness, journal seq, journal lag, object count, connection pool, uptime (MRFC-0040).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "agent_memory", "description": "Store or retrieve agent memories with TTL. Write: agent_id + key + value. Read: agent_id only. (MRFC-0040)", "inputSchema": {"type": "object", "properties": {"agent_id": {"type": "string"}, "key": {"type": "string"}, "value": {}, "ttl": {"type": "integer"}}, "required": ["agent_id"]}},
            {"name": "batch", "description": "Atomic batch of remember/relate/forget operations. Use $N.koid to reference previous results. (MRFC-0040)", "inputSchema": {"type": "object", "properties": {"operations": {"type": "array"}}, "required": ["operations"]}},
            {"name": "session_init", "description": "Establish agent session identity. Subsequent calls in this connection inherit agent_id, run_id, tenant, roles. (MRFC-0040)", "inputSchema": {"type": "object", "properties": {"agent_id": {"type": "string"}, "run_id": {"type": "string"}, "tenant": {"type": "string"}, "roles": {"type": "array", "items": {"type": "string"}}}, "required": ["agent_id"]}},
            {"name": "decide", "description": "Record an agent decision on a Knowledge Object with rationale and confidence. Creates a provenance-tagged version. (MRFC-0040)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "decision": {"type": "string"}, "rationale": {"type": "string"}, "confidence": {"type": "number"}}, "required": ["koid", "decision"]}},
            {"name": "memory_search", "description": "Search the agent memory directory for knowledge fragments. Returns ranked results with name, description, snippet, and relevance. The memory directory contains Markdown files with YAML frontmatter — each file is one memory. (MRFC-0070)", "inputSchema": {"type": "object", "properties": {"query": {"type": "string", "description": "Search query — matched against memory names, descriptions, and body content"}, "max_results": {"type": "integer", "default": 10, "description": "Maximum number of results to return"}, "memory_dir": {"type": "string", "description": "Override the memory directory path (default: server --memory-dir)"}}, "required": ["query"]}},
            {"name": "memory_store", "description": "Store a new memory as a Markdown file with YAML frontmatter in the memory directory. Auto-generates the filename from the name slug. The memory is indexed in MEMORY.md. (MRFC-0070)", "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "Short kebab-case slug for this memory (e.g. 'mrf-0070-phase-a1-complete')"}, "description": {"type": "string", "description": "One-line summary used to decide relevance during recall"}, "content": {"type": "string", "description": "Body of the memory — the fact, decision, or knowledge to persist"}, "type": {"type": "string", "enum": ["user", "feedback", "project", "reference"], "default": "project", "description": "Memory type"}, "memory_dir": {"type": "string", "description": "Override the memory directory path (default: server --memory-dir)"}}, "required": ["name", "description", "content"]}},
        {"name": "memory_update", "description": "Update an existing memory's frontmatter fields and/or body content. Only provided fields are changed — omitted fields keep their current values. Updates the modified timestamp. (MRFC-0070)", "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "Name slug of the memory to update"}, "description": {"type": "string", "description": "New one-line summary (omit to keep current)"}, "content": {"type": "string", "description": "New body content (omit to keep current)"}, "type": {"type": "string", "enum": ["user", "feedback", "project", "reference"], "description": "New memory type (omit to keep current)"}, "memory_dir": {"type": "string", "description": "Override the memory directory path"}}, "required": ["name"]}},
        {"name": "memory_delete", "description": "Delete a memory file from the memory directory and remove its entry from MEMORY.md. Returns the deleted memory's name and path. (MRFC-0070)", "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "Name slug of the memory to delete"}, "memory_dir": {"type": "string", "description": "Override the memory directory path"}}, "required": ["name"]}}
        ]
    })
}
