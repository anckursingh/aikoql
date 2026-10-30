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

mod admin;
mod api_rest;
mod archive;
mod audit;
mod authz;
mod cli;
mod config;
mod dispatcher;
mod engine;
mod error_codes;
mod graph_ui;
mod ha;
mod helpers;
mod http;
mod imports;
mod ingest;
mod knowledge_runtime;
mod mcp_http;
mod model;
mod native;
mod oauth;
mod protocol;
mod rate_limiter;
mod replication;
mod s3;
mod session;
mod shell;
mod studio;
mod tls;
mod tool_registry;
mod tools;
mod transport;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

pub(crate) struct HttpSession {
    pub username: String,
    pub roles: Vec<String>,
    pub created: Instant,
    /// P3-M1 (§53): session TTL from [auth].session_ttl_seconds (default
    /// 24h). validate_token enforces `elapsed < ttl`.
    pub ttl_secs: u64,
}

// pub(crate) re-exports: this block is the crate prelude. Extracted modules
// (R7) import from it explicitly — every module carries its own
// `use crate::{...}` list (prelude cleanup, 2026-08-29); api_rest and
// knowledge_runtime use `use super::*` — both work because these are
// re-exports, not private uses.
pub(crate) use aikoql_graph::*;
pub(crate) use aikoql_kernel::ir::*;
pub(crate) use aikoql_kernel::knowledge::ontology::{
    discover_ontology, OntologyDef, OntologyRegistry, ONTOLOGY_TYPE,
};
pub(crate) use aikoql_kernel::lifecycle::schema::SchemaRegistry;
pub(crate) use aikoql_kernel::*;
pub(crate) use aikoql_scheduler::Scheduler;
#[cfg(feature = "embedding-openai")]
pub(crate) use aikoql_semantic::provider::OpenAiEmbeddingProvider;
pub(crate) use aikoql_semantic::{EmbeddingEnricher, SemanticEngine};
pub(crate) use aikoql_vector::{HnswVectorIndex, TantivyTextIndex};
pub(crate) use serde_json::{json, Value as J};
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::io::{BufRead, BufReader, Read, Write};
pub(crate) use std::net::{TcpListener, TcpStream, ToSocketAddrs};
pub(crate) use std::sync::atomic::{AtomicU64, Ordering};
pub(crate) use std::sync::{Arc, Mutex, OnceLock};
pub(crate) use std::thread;
pub(crate) use std::time::Instant;
pub(crate) use tracing::{error, info, info_span, warn};

pub(crate) static SERVER_START: OnceLock<Instant> = OnceLock::new();
pub(crate) static MEMORY_DIR: OnceLock<String> = OnceLock::new();

/// P5-M22 (P1-15): the production maintainer's strong owner. The kernel's
/// coordinator holds the maintainer WEAKLY (the P5-M18 Arc-cycle trap), so
/// the strong Arc must live somewhere the process owns explicitly — the
/// database context — not an accidental `OnceLock` singleton. One DB per
/// process stays the product invariant (named here, not implied).
pub(crate) struct DatabaseContext {
    pub(crate) maintainer: Mutex<Option<Arc<aikoql_scheduler::IndexMaintainer>>>,
}

pub(crate) struct ServerContext {
    pub(crate) db: DatabaseContext,
}

/// PRR-3: semantic readiness — the enrichment worker thread updates this,
/// tool_health and /health surface it.
#[derive(Clone, Debug)]
pub(crate) struct SemanticStatus {
    pub(crate) state: &'static str, // "initializing" | "ready" | "unavailable"
    pub(crate) detail: String,
}

pub(crate) static SEMANTIC_STATUS: OnceLock<Mutex<SemanticStatus>> = OnceLock::new();

pub(crate) fn set_semantic_status(state: &'static str, detail: impl Into<String>) {
    let cell = SEMANTIC_STATUS.get_or_init(|| {
        Mutex::new(SemanticStatus {
            state: "initializing",
            detail: String::new(),
        })
    });
    let mut s = cell.lock().unwrap(); // justified: Mutex poison is unrecoverable
    s.state = state;
    s.detail = detail.into();
}

pub(crate) fn semantic_status_snapshot() -> SemanticStatus {
    SEMANTIC_STATUS
        .get()
        .map(|m| m.lock().unwrap().clone()) // justified: Mutex poison is unrecoverable
        .unwrap_or(SemanticStatus {
            state: "initializing",
            detail: String::new(),
        })
}

/// PRR-3: local model store — `--model-dir` wins, else `~/.aikoql/models`.
pub(crate) fn model_store_dir(flag: Option<&str>) -> std::path::PathBuf {
    match flag {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let home = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            home.join(".aikoql").join("models")
        }
    }
}

/// The MCP versions this server speaks, oldest → newest: initialize echoes
/// a supported requested version and answers anything else with the newest
/// (the MCP negotiation rule).
pub(crate) const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] =
    ["2024-11-05", "2025-03-26", "2025-06-18"];
pub(crate) const PROTOCOL_VERSION: &str = SUPPORTED_PROTOCOL_VERSIONS[2];

/// The version initialize answers for a client that `requested` one.
pub(crate) fn negotiate_protocol(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .copied()
        .unwrap_or(PROTOCOL_VERSION)
}

// main() is the orchestrator: CLI dispatch + server bootstrap. Everything
// else lives in the modules above (R7).
use crate::cli::*;
use crate::http::*;
use crate::native::*;
use crate::session::TcpAuthTable;
use crate::transport::*;

#[allow(unused_assignments)]
/// ANSI colors render on Windows only in terminals with VT processing
/// (Windows Terminal, VS Code, mintty, CI). Legacy conhost — PowerShell 5.1's
/// default console — prints the raw escapes as `←[2m` garbage, so fall back to
/// plain logs there. (ponytail: env heuristic instead of a windows-sys dep;
/// plain logs are never broken, colors just degrade.)
fn use_ansi() -> bool {
    if !cfg!(windows) {
        return true;
    }
    std::env::var_os("WT_SESSION").is_some()
        || std::env::var_os("TERM_PROGRAM").is_some()
        || std::env::var_os("TERM").is_some_and(|t| !t.eq_ignore_ascii_case("dumb"))
        || std::env::var_os("CI").is_some()
}

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
    if dispatch(&args, subcmd, subcmd_idx) {
        return;
    }

    // PRR-4: defaults → aikoql.toml → env → CLI, validated in one place.
    let cfg = match config::load(&args, subcmd, subcmd_idx) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cfg.log_level)),
        )
        .with_writer(std::io::stderr)
        .with_ansi(use_ansi());
    if cfg.log_format == "json" {
        subscriber.json().init();
    } else {
        subscriber.init();
    }
    if let Some(path) = &cfg.config_path {
        info!(config = %path, "configuration loaded");
    }
    let listen_addr = cfg.listen_addr;
    let native_addr = cfg.native_port;
    let replicate_listen = cfg.replicate_listen;
    let metrics_addr = cfg.metrics_addr;
    let tcp_tokens = cfg.tcp_tokens;
    let db_path = cfg.db_path;
    let memory_dir = cfg.memory_dir;
    // PRR-4: [rate_limit] config — per-connection on MCP tools/call, per-token
    // on the REST surface (shared limiter below).
    let rate_enabled = cfg.rate_enabled;
    let rate_max_calls_per_minute = cfg.rate_max_calls_per_minute;
    let rest_rate_limit = Arc::new(Mutex::new(crate::rate_limiter::RateLimiter::new(
        rate_enabled,
        rate_max_calls_per_minute,
    )));
    // R5 (review round 3): ONE MCP limiter shared across all stdio/TCP
    // sessions — the dispatcher keys it by principal. (R9 deleted the
    // second, hidden limiter in authz.rs.)
    let mcp_rate_limit = Arc::new(Mutex::new(crate::rate_limiter::RateLimiter::new(
        rate_enabled,
        rate_max_calls_per_minute,
    )));
    let embedding_provider = cfg.embedding_provider;
    #[allow(unused_assignments, unused_variables)]
    let embedding_base_url = cfg.embedding_base_url;
    let embedding_model = cfg.embedding_model;
    #[allow(unused_assignments, unused_variables)]
    let embedding_api_key = cfg.embedding_api_key;
    // PRR-3: local model store override (default ~/.aikoql/models).
    let model_dir_flag = cfg.model_dir;
    let model_auto_install = cfg.model_auto_install;
    MEMORY_DIR.set(memory_dir).ok();

    // P3-M1 (§53–54): build the HTTP login resolver once — argon2id hashes
    // verified per login, bootstrap admin hashed here (the plaintext then
    // drops out of scope) — and fail closed on an armed-but-credentialless
    // remote HTTP surface (auth004) and on non-loopback metrics binds.
    let http_auth = Arc::new(crate::http::AuthResolver::new(
        cfg.auth_users,
        cfg.admin_password.as_deref(),
        cfg.auth_session_ttl_secs,
    ));
    if let Err(msg) = remote_http_requires_auth(cfg.allow_remote_http, http_auth.is_configured()) {
        eprintln!("{msg}");
        std::process::exit(2);
    }
    // TLS on the client listeners and the HTTP surface (--tls-cert/--tls-key)
    let client_tls = match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(c), Some(k)) => {
            match tls::read(c).and_then(|c| tls::read(k).and_then(|k| tls::server_config(&c, &k))) {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("--tls-cert/--tls-key: {e}");
                    std::process::exit(2);
                }
            }
        }
        (None, None) => None,
        _ => {
            eprintln!(
                "--tls-cert and --tls-key go together (or AIKOQL_TLS_CERT and AIKOQL_TLS_KEY)"
            );
            std::process::exit(2);
        }
    };
    let metrics_addr = match metrics_addr {
        Some(a) => match validate_http_listen(&a, cfg.allow_remote_http, client_tls.is_some()) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        None => None,
    };

    // MRFC-0080 R4: serving a standby promotes it to the next term (R4c:
    // before the kernel opens, so its open-time writes are the new term's);
    // a node deposed by a newer term keeps serving reads, never writes.
    // MRFC-0080 R5a: with a lease, block as a standby of its holder until
    // this node wins it (or resumes its own), then serve as the leader.
    // MRFC-0080 R3c: TLS for the replication stream. With a certificate
    // the stream is encrypted and the listener may bind any interface;
    // without one it stays loopback-only.
    let repl_tls = match (&cfg.replicate_tls_cert, &cfg.replicate_tls_key) {
        (Some(c), Some(k)) => {
            match tls::read(c).and_then(|c| tls::read(k).and_then(|k| tls::server_config(&c, &k))) {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("--replicate-tls-cert/--replicate-tls-key: {e}");
                    std::process::exit(2);
                }
            }
        }
        (None, None) => None,
        _ => {
            eprintln!("--replicate-tls-cert and --replicate-tls-key go together");
            std::process::exit(2);
        }
    };
    // with TLS the token is encrypted, so a client listener may bind any
    // interface; without it the R1 loopback-only rule holds
    let client_listen_addr = |a: &str| {
        if client_tls.is_some() {
            Ok(a.to_string())
        } else {
            validate_listen(a)
        }
    };
    let repl_trust = cfg.replicate_tls_ca.as_deref().map(|ca| {
        match tls::read(ca).and_then(|p| tls::client_config(&p)) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("--replicate-tls-ca: {e}");
                std::process::exit(2);
            }
        }
    });
    let repl_listen_addr = |a: &str| {
        if repl_tls.is_some() {
            Ok(a.to_string())
        } else {
            validate_listen(a)
        }
    };
    // MRFC-0080 standby reads: `--follow LEADER` serves a read replica —
    // never promoted by `serve`, finishing a re-bootstrap it recorded.
    if cfg.follow.is_some() && (cfg.ha_lease.is_some() || cfg.archive.is_some()) {
        eprintln!("--follow (a read replica) does not combine with --ha-lease or --archive");
        std::process::exit(2);
    }
    // a serving standby (read replica, HA standby) that exited for a
    // re-bootstrap finishes it before anything opens the directory
    if cfg.follow.is_some() || cfg.ha_lease.is_some() {
        if let Err(e) = replication::finish_rebootstrap(std::path::Path::new(&db_path)) {
            eprintln!("standby: {e}");
            std::process::exit(1);
        }
    }
    let begun = cfg.ha_lease.as_ref().map(|lease| {
        let (Some(repl), Some(node)) = (replicate_listen.as_deref(), cfg.ha_node.clone()) else {
            eprintln!("--ha-lease needs --ha-node and --replicate-listen");
            std::process::exit(2);
        };
        let repl = match repl_listen_addr(repl) {
            // the address standbys dial: the advertised one across hosts
            Ok(a) => cfg.replicate_advertise.clone().unwrap_or(a),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        };
        let Some(token) = replication_token(&tcp_tokens) else {
            eprintln!("--ha-lease needs an admin --tcp-token (or AIKOQL_REPLICATION_TOKEN) to follow with");
            std::process::exit(2);
        };
        let ttl = std::time::Duration::from_millis(cfg.ha_ttl_ms);
        let store: Arc<dyn ha::LeaseStore> = if lease.starts_with("s3://") {
            match ha::S3Lease::from_env(lease, ttl / 3) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    eprintln!("--ha-lease: {e}");
                    std::process::exit(2);
                }
            }
        } else if lease.starts_with("k8s://") {
            match ha::K8sLease::in_cluster(lease, ttl / 3) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    eprintln!("--ha-lease: {e}");
                    std::process::exit(2);
                }
            }
        } else {
            Arc::new(ha::FileLease::new(lease.into()))
        };
        let ha = ha::HaConfig {
            store,
            node,
            ttl,
            repl,
            client: cfg.advertise.clone(),
            token,
            tls: repl_trust.clone(),
            dir: std::path::PathBuf::from(&db_path),
        };
        match ha::begin(ha) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("ha: {e}");
                std::process::exit(1);
            }
        }
    });
    // standby reads: an HA standby serves reads until it wins the lease
    let (ha_slot, ha_standby) = match begun {
        Some(ha::Begun::Lead(slot)) => (Some(slot), None),
        Some(ha::Begun::Standby(c)) => (None, Some(c)),
        None => (None, None),
    };
    let serving_standby = cfg.follow.is_some() || ha_standby.is_some();
    // cascading replicas: a leader serves the stream, and so may a
    // `--follow` replica (downstream replicas follow it); an HA standby
    // does not — the lease names only the leader
    let serves_stream = replicate_listen.is_some() && ha_standby.is_none();
    let claimed = if serving_standby {
        replication::load_term(std::path::Path::new(&db_path))
    } else {
        replication::claim_leadership(std::path::Path::new(&db_path))
    };
    let (term, role) = match claimed {
        Ok(t) => t,
        Err(e) => {
            eprintln!("replication term: {e}");
            std::process::exit(1);
        }
    };
    let (kernel, admin) =
        // the log is retained for standbys and for the archive (R6)
        match engine::open_kernel(
            &db_path,
            &cfg.encryption,
            serves_stream || (cfg.archive.is_some() && !serving_standby),
            serving_standby.then(|| {
                format!(
                    "this node is a standby{}",
                    replication::leader_hint(std::path::Path::new(&db_path))
                )
            }),
        ) {
            Ok((k, admin)) => (k, admin),
            Err(e) => {
                eprintln!("open kernel: {}", e);
                std::process::exit(1);
            }
        };
    if let (Some(slot), Some(a)) = (&ha_slot, &admin) {
        let _ = slot.set(a.clone()); // the renewer can now fence writes
    }
    // R4d: after the kernel created the directory (a fresh one must be
    // empty when it opens)
    if let (replication::Role::Leader, Some(addr), false) =
        (role, cfg.advertise.as_deref(), serving_standby)
    {
        let dir = std::path::Path::new(&db_path);
        if let Err(e) = replication::store_leader(dir, addr) {
            eprintln!("record --advertise: {e}");
            std::process::exit(1);
        }
    }
    if role == replication::Role::Fenced && !serving_standby {
        let Some(a) = admin.as_ref() else {
            eprintln!("a fenced node needs the storage admin surface to refuse writes");
            std::process::exit(2);
        };
        a.fence(format!(
            "fenced at replication term {term}{}",
            replication::leader_hint(std::path::Path::new(&db_path))
        ));
        warn!(
            term,
            "deposed by a newer replication term: reads only — rejoin with `follow`"
        );
    }

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
    // PRR-3: the runtime NEVER downloads — candle loads from the local model
    // store only; a missing install degrades to lexical-only recall with a
    // clear remediation in tool_health / /health.
    let emb_provider: Option<Arc<dyn EmbeddingProvider>> = match embedding_provider.as_deref() {
        Some("openai") => {
            #[cfg(feature = "embedding-openai")]
            {
                let p = OpenAiEmbeddingProvider::new(&url, &model, embedding_api_key.as_deref());
                set_semantic_status(
                    "initializing",
                    format!("openai-compatible endpoint {url} (model {model})"),
                );
                Some(Arc::new(p))
            }
            #[cfg(not(feature = "embedding-openai"))]
            {
                set_semantic_status(
                    "unavailable",
                    "openai embedding requested but binary not compiled with embedding-openai feature",
                );
                None
            }
        }
        _ => {
            #[cfg(feature = "embedding-candle")]
            {
                let candle_dir = model_store_dir(model_dir_flag.as_deref()).join(
                    aikoql_semantic::provider::model_slug(
                        aikoql_semantic::provider::DEFAULT_MODEL_ID,
                    ),
                );
                // Model identity is explicit: a non-default --embedding-model
                // names a candle model that isn't installed, so we reject it
                // instead of silently swapping in all-MiniLM-L6-v2.
                if !embedding_model.is_empty() && embedding_model != "all-MiniLM-L6-v2" {
                    set_semantic_status(
                        "unavailable",
                        format!(
                            "model '{embedding_model}' is not installed — run `aikoql model install {embedding_model}`, or omit --embedding-model for the bundled all-MiniLM-L6-v2"
                        ),
                    );
                    None
                } else {
                    use aikoql_semantic::provider::{CandleEmbedding, DEFAULT_MODEL_ID};
                    // the one opt-in exception to PRR-3 ([embedding].auto_install /
                    // AIKOQL_MODEL_AUTO_INSTALL): a missing default model is
                    // downloaded once, at start
                    let auto_install = model_auto_install;
                    let loaded = CandleEmbedding::from_local(&candle_dir).or_else(|e| {
                        if !auto_install {
                            return Err(format!(
                                "{e} — run `aikoql model install` to install all-MiniLM-L6-v2 into {}, \
                                 or set AIKOQL_MODEL_AUTO_INSTALL=1 to download it at start",
                                candle_dir.display()
                            ));
                        }
                        info!(model = DEFAULT_MODEL_ID, dir = %candle_dir.display(),
                            "AIKOQL_MODEL_AUTO_INSTALL: installing the embedding model");
                        let store = model_store_dir(model_dir_flag.as_deref());
                        CandleEmbedding::install(DEFAULT_MODEL_ID, &store)
                            .and_then(|_| CandleEmbedding::from_local(&candle_dir))
                            .map_err(|e| format!("model auto-install failed: {e}"))
                    });
                    match loaded {
                        Ok(p) => {
                            // Test park hook (P5-M11 pattern): arm the Nth
                            // embed to hold the model lock — deterministic
                            // contention for the bounded compile_context pin.
                            let p = match std::env::var("AIKOQL_EMBED_PARK_AT")
                                .ok()
                                .and_then(|v| v.parse::<usize>().ok())
                            {
                                Some(n) => p.with_park_at(n),
                                None => p,
                            };
                            set_semantic_status(
                                "initializing",
                                "local model loaded; background enrichment running",
                            );
                            Some(Arc::new(p))
                        }
                        Err(detail) => {
                            set_semantic_status("unavailable", detail);
                            None
                        }
                    }
                }
            }
            #[cfg(not(feature = "embedding-candle"))]
            {
                set_semantic_status(
                    "unavailable",
                    "no embedding provider compiled in — activate embedding-candle or embedding-openai feature",
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
    if let Some(ha) = ha_standby {
        let Some(a) = admin.clone() else {
            eprintln!("--ha-lease needs the storage admin surface");
            std::process::exit(2);
        };
        info!("serving as an HA standby — reads only until this node wins the lease");
        replication::catch_up_loop(
            kernel.clone(),
            a.clone(),
            std::path::PathBuf::from(db_path.as_str()),
            "this node is an HA standby",
        );
        thread::spawn(move || ha::stand_by(ha, a));
    }
    if let Some(leader) = cfg.follow.clone() {
        let Some(token) = replication_token(&tcp_tokens) else {
            eprintln!(
                "--follow needs AIKOQL_REPLICATION_TOKEN (or an admin --tcp-token) to follow with"
            );
            std::process::exit(2);
        };
        let Some(a) = admin.clone() else {
            eprintln!("--follow needs the storage admin surface");
            std::process::exit(2);
        };
        info!(%leader, "serving as a read replica");
        replication::serve_replica(
            kernel.clone(),
            a,
            leader,
            token,
            repl_trust.clone(),
            std::path::PathBuf::from(db_path.as_str()),
        );
    }

    // PRR-3: enrichment runs on a worker thread — serve comes up immediately
    // and /health reports semantic readiness while the scan runs.
    if let Some(enrichment_provider) = emb_provider {
        // Record the real model: candle always loads all-MiniLM-L6-v2; the
        // --embedding-model flag only names the OpenAI-compatible endpoint.
        let enrichment_model = if embedding_provider.as_deref() == Some("openai") {
            model.clone()
        } else {
            "all-MiniLM-L6-v2".to_string()
        };
        let kernel_work = kernel.clone();
        thread::spawn(move || {
            let enricher = EmbeddingEnricher::new(enrichment_provider, &enrichment_model);
            let engine = Arc::new(SemanticEngine::new(Arc::new(enricher)));
            let sched = Scheduler::new();
            sched.register(engine);
            match sched.start_all(&kernel_work) {
                Ok(()) => set_semantic_status(
                    "ready",
                    format!("embeddings live (model {enrichment_model})"),
                ),
                Err(e) => set_semantic_status("unavailable", format!("enrichment failed: {e}")),
            }
        });
    }

    // P5-M18 (ND-14): real ANN/BM25 indexes behind the maintainer (started
    // unconditionally, off the serve path — the synchronous journal replay
    // is batch-committed). The HNSW adopts the first vector's dim (the
    // enricher emits 384-d all-MiniLM-L6-v2; a fixed default would silently
    // drop it) and is capacity-capped at 10k physical nodes (allocator hint
    // only — the coordinator ranks candidates against committed cosines).
    //
    // P5-M22 (P1-15): the maintainer resumes from `{db_path}.ckpt` when a
    // COMPLETE checkpoint exists (restart cost ∝ events after it); any load
    // failure — a torn pair, a foreign water — falls back to a fresh start
    // with a full replay.
    let server_ctx = Arc::new(ServerContext {
        db: DatabaseContext {
            maintainer: Mutex::new(None),
        },
    });
    let ckpt_dir = std::path::PathBuf::from(format!("{db_path}.ckpt"));
    // MRFC-0080 R3b: a checkpoint swaps the directory (remove + rename), so
    // the replication sender reads it only under this lock.
    let ckpt_lock = Arc::new(Mutex::new(()));
    {
        let kernel_work = kernel.clone();
        let server_work = server_ctx.clone();
        let ckpt_work = ckpt_dir.clone();
        thread::spawn(move || {
            let maintainer = match resume_or_start_maintainer(&kernel_work, &ckpt_work) {
                Ok(m) => m,
                Err(e) => {
                    warn!("index maintainer failed to start: {e}");
                    return;
                }
            };
            // M18: attach so find_similar ranks the ANN candidates (the
            // exact path stays available via search_exact).
            kernel_work.attach_indexes(maintainer.clone());
            // justified: Mutex poison is unrecoverable
            *server_work.db.maintainer.lock().unwrap() = Some(maintainer);
        });
    }

    let db_path = Arc::new(db_path);
    SERVER_START.set(Instant::now()).ok();

    // Load ontology from stored Ontology KOs (MRFC-0041) and install it in
    // the hot-reload slot — S-8 (POC-4): Auto-Discover and REST Ontology
    // remembers republish through the same path, so the query editor never
    // waits for a restart.
    let ontology = crate::http::load_ontology_registry(&kernel);
    crate::http::install_ontology(ontology.clone());

    // Optional HTTP metrics endpoint for Prometheus + Kubernetes probes.
    if let Some(ref addr) = metrics_addr {
        spawn_metrics(
            kernel.clone(),
            ontology.clone(),
            addr.clone(),
            db_path.clone(),
            rest_rate_limit.clone(),
            http_auth.clone(),
            admin.clone(),
            client_tls.clone(),
            // MCP over Streamable HTTP on the same surface: bearer = a
            // --tcp-token, or an OAuth access token when [auth] logins exist
            {
                let oauth = http_auth.is_configured().then(|| {
                    Arc::new(oauth::OAuth::new(
                        http_auth.clone(),
                        client_tls.is_some(),
                        kernel.system_store(),
                    ))
                });
                TcpAuthTable::parse(&tcp_tokens)
                    .ok()
                    .filter(|_| !tcp_tokens.is_empty() || oauth.is_some())
                    .map(|t| {
                        Arc::new(mcp_http::McpHttp::new(
                            Arc::new(t),
                            oauth,
                            cfg.request_timeout_secs,
                        ))
                    })
            },
        );
    }

    if cfg.replicate_sync_ms.is_some() && replicate_listen.is_none() {
        eprintln!("--replicate-sync-ms needs --replicate-listen");
        std::process::exit(2);
    }
    // MRFC-0080 R3c-2/R6: one ack table for the standbys and the archive
    // (the slowest bounds log release); the lag series reads it.
    let acked = Arc::new(replication::Acked::default());
    let _ = replication::LEADER.set(acked.clone());
    if let Some(url) = cfg.archive.as_deref().filter(|_| !serving_standby) {
        // R6: only a writing node archives; a fenced one serves reads
        if role == replication::Role::Fenced {
            warn!("--archive: this node is fenced — not archiving");
        } else {
            let Some(source) = admin.clone() else {
                eprintln!("--archive needs the storage admin surface");
                std::process::exit(2);
            };
            let started =
                archive::open_store(url, std::time::Duration::from_secs(60)).and_then(|store| {
                    archive::Archiver::start(
                        store,
                        source,
                        acked.clone(),
                        term,
                        std::path::PathBuf::from(format!("{db_path}.archive-tmp")),
                    )
                });
            match started {
                Ok(a) => {
                    info!(%url, term, "continuous archive started");
                    let (log_every, snap_every) = (
                        std::time::Duration::from_millis(cfg.archive_log_ms),
                        std::time::Duration::from_secs(cfg.archive_snapshot_secs),
                    );
                    let a = a.keeping(cfg.archive_keep);
                    thread::spawn(move || a.run(log_every, snap_every));
                }
                Err(e) => {
                    eprintln!("--archive: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
    // an HA standby serves no stream (its promoted, re-executed process
    // does); a `--follow` replica may — downstream replicas cascade off it
    if let Some(addr) = replicate_listen.filter(|_| serves_stream) {
        // MRFC-0080 R3a: the replication stream rides beside any serve mode
        // (stdio included) under the TCP gates — tokens required, loopback
        // bind only (the stream is the whole database, in plaintext).
        if tcp_tokens.is_empty() {
            eprintln!("--replicate-listen requires a --tcp-token with the admin role for standbys");
            std::process::exit(2);
        }
        let auth = match TcpAuthTable::parse(&tcp_tokens) {
            Ok(t) => Arc::new(t),
            Err(e) => {
                eprintln!("invalid --tcp-token: {e}");
                std::process::exit(2);
            }
        };
        let addr = match repl_listen_addr(&addr) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        };
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("bind replication listener {}: {}", addr, e);
                std::process::exit(1);
            }
        };
        let Some(source) = admin.clone() else {
            eprintln!("--replicate-listen needs the storage admin surface");
            std::process::exit(2);
        };
        let bound = listener
            .local_addr()
            .map_or(addr.clone(), |a| a.to_string());
        info!(addr = %bound, tls = repl_tls.is_some(), "replication stream listening");
        let shipped = replication::ShippedCheckpoint {
            dir: ckpt_dir.clone(),
            lock: ckpt_lock.clone(),
        };
        let repl_dir = std::path::PathBuf::from(db_path.as_str());
        if let Some(ms) = cfg.replicate_sync_ms {
            source.set_commit_gate(replication::sync_gate(
                acked.clone(),
                std::time::Duration::from_millis(ms),
            ));
        }
        thread::spawn(move || {
            replication::serve_replication(
                listener,
                source,
                auth,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Some(shipped),
                repl_dir,
                acked,
                repl_tls,
            )
        });
        // R3b: a leader that crashes leaves no shutdown checkpoint — take
        // one on a timer so standbys always hold a recent one.
        let (k, ctx, dir, lock) = (
            kernel.clone(),
            server_ctx.clone(),
            ckpt_dir.clone(),
            ckpt_lock.clone(),
        );
        let every = std::time::Duration::from_secs(cfg.index_checkpoint_secs);
        thread::spawn(move || loop {
            thread::sleep(every);
            // justified: Mutex poison is unrecoverable
            let m = ctx.db.maintainer.lock().unwrap().clone();
            if let Some(m) = m {
                let _g = lock.lock().unwrap();
                if let Err(e) = m.checkpoint(&k, &dir) {
                    warn!("periodic index checkpoint: {e}");
                }
            }
        });
    }

    if listen_addr.is_some() || native_addr.is_some() {
        // PRR-2: TCP requires token auth (fail-closed). Stdio keeps the
        // process-boundary trust model and needs no token. D-15: the native
        // listener carries the same bearer token, so it inherits the gate.
        if tcp_tokens.is_empty() {
            eprintln!(
                "TCP mode requires at least one --tcp-token TOKEN[:TENANT[:ROLE1,ROLE2]] — refusing to serve without authentication (stdio mode needs no token)"
            );
            std::process::exit(2);
        }
        let auth = match TcpAuthTable::parse(&tcp_tokens) {
            Ok(t) => Arc::new(t),
            Err(e) => {
                eprintln!("invalid --tcp-token: {e}");
                std::process::exit(2);
            }
        };
        if let Some(addr) = listen_addr {
            // R1 (review round 3): plaintext TCP is loopback-only — a
            // non-loopback bind would put the bearer token on the wire
            // unencrypted, so it is rejected fail-closed unless TLS is on.
            let addr = match client_listen_addr(&addr) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            let listener = match TcpListener::bind(&addr) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("bind TCP listener {}: {}", addr, e);
                    std::process::exit(1);
                }
            };
            // D-20 (§30): one server serves both transports. With
            // --native-port set, the TCP loop moves to its own thread so the
            // main thread reaches the native loop below; otherwise it blocks
            // main as before. The clones keep the originals for that loop.
            let has_native = native_addr.is_some();
            let (k, a, d, rl, adm, t) = (
                kernel.clone(),
                auth.clone(),
                db_path.clone(),
                mcp_rate_limit.clone(),
                admin.clone(),
                client_tls.clone(),
            );
            let serve = move || {
                run_tcp_listener(
                    k,
                    listener,
                    a,
                    d,
                    rl,
                    adm,
                    cfg.request_timeout_secs,
                    cfg.max_connections,
                    t,
                )
            };
            if has_native {
                thread::spawn(serve);
            } else {
                serve();
            }
        }
        if let Some(addr) = native_addr {
            // D-15: the same R1 rule — the framed protocol carries the same
            // bearer token, so plaintext stays loopback-only.
            let addr = match client_listen_addr(&addr) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            let listener = match TcpListener::bind(&addr) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("bind native listener {}: {}", addr, e);
                    std::process::exit(1);
                }
            };
            run_native_listener(
                kernel,
                listener,
                auth,
                db_path,
                mcp_rate_limit,
                admin,
                cfg.request_timeout_secs,
                cfg.max_connections,
                client_tls,
            );
        }
    } else if metrics_addr.is_some() {
        // S-12 (POC-4): metrics-only serve must not read stdin — in a
        // background/service context stdin hits EOF immediately, which the
        // stdio loop treats as shutdown, taking the HTTP surface down with
        // a clean exit. Park main forever (the metrics thread keeps
        // serving); like TCP mode there is no shutdown-time checkpoint
        // (honest ledger).
        loop {
            std::thread::park();
        }
    } else {
        run_stdio(
            &kernel,
            &db_path,
            mcp_rate_limit,
            admin,
            cfg.request_timeout_secs,
        );
        // P5-M22 (P1-15): stdio mode returns at stdin EOF — checkpoint the
        // maintainer before the process exits. TCP mode never returns, so
        // it has no shutdown-time checkpoint yet (honest ledger).
        //
        // T-59 (P3-010): the maintainer is attached from a spawned thread
        // (see the serve setup above) — a close->rapid-EOF shutdown can
        // read the slot before the attach lands and skip the checkpoint.
        // Wait for the attach (bounded) so the hand-off is deterministic.
        let maintainer = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let m = server_ctx.db.maintainer.lock().unwrap().clone();
                if m.is_some() {
                    break m;
                }
                if std::time::Instant::now() >= deadline {
                    warn!("shutdown: maintainer not attached within 5s — skipping checkpoint");
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        if let Some(m) = maintainer {
            let t0 = std::time::Instant::now();
            // justified: Mutex poison is unrecoverable
            let _g = ckpt_lock.lock().unwrap();
            if let Err(e) = m.checkpoint(&kernel, &ckpt_dir) {
                warn!("maintainer checkpoint failed: {e}");
            }
            info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                "shutdown maintainer checkpoint done"
            );
        }
    }
}

/// P5-M22 (P1-15): resume from `ckpt_dir` when a COMPLETE checkpoint is
/// there — restart cost ∝ the events after it — and start fresh with a full
/// replay otherwise. ANY load failure (a torn pair, P1-14; a foreign water,
/// P1-15) falls back to a fresh start, never to an unavailable index.
fn resume_or_start_maintainer(
    kernel: &Kernel,
    ckpt_dir: &std::path::Path,
) -> KResult<Arc<aikoql_scheduler::IndexMaintainer>> {
    if let Some(water) = aikoql_scheduler::IndexMaintainer::checkpoint_water(ckpt_dir)? {
        // the reason rides the warning — an operator must see WHY a
        // promotion fell back to a full replay
        let resumed = HnswVectorIndex::load(&ckpt_dir.join("vectors"))
            .map_err(|e| format!("vectors: {e}"))
            .and_then(|v| {
                let t = TantivyTextIndex::load(&ckpt_dir.join("text"))
                    .map_err(|e| format!("text: {e}"))?;
                let vectors: Arc<dyn VectorIndex> = Arc::new(v);
                let text: Arc<dyn TextIndex> = Arc::new(t);
                aikoql_scheduler::IndexMaintainer::start_at(
                    kernel,
                    vectors,
                    text,
                    Some(water),
                    Some(ckpt_dir),
                )
                .map_err(|e| format!("resume: {e}"))
            });
        match resumed {
            Ok(m) => {
                let (head, _) = kernel.journal_head()?;
                info!(
                    water,
                    replayed = head.saturating_sub(water),
                    "index maintainer resumed from checkpoint"
                );
                return Ok(m);
            }
            Err(why) => warn!(
                "checkpoint at {} unusable ({why}) — starting fresh with a full replay",
                ckpt_dir.display()
            ),
        }
    }
    let m = aikoql_scheduler::IndexMaintainer::start(
        kernel,
        Arc::new(HnswVectorIndex::new(0, 10_000)),
        Arc::new(TantivyTextIndex::new()?),
    )?;
    let (head, _) = kernel.journal_head()?;
    info!(replayed = head, "index maintainer full replay");
    Ok(m)
}

/// PRR-2 + R1 (review round 3): `--listen :9090` (empty host) binds
/// loopback only. A non-loopback bind is REJECTED fail-closed — without TLS
/// the bearer token would travel in plaintext, so plaintext TCP is
/// loopback-only in MVP. Hostnames resolve; every resolved address must be
/// loopback.
/// The token a standby presents to its leader: the operator's
/// AIKOQL_REPLICATION_TOKEN, else this node's first admin --tcp-token.
fn replication_token(tcp_tokens: &[String]) -> Option<String> {
    std::env::var("AIKOQL_REPLICATION_TOKEN").ok().or_else(|| {
        tcp_tokens
            .iter()
            .find(|t| {
                t.split(':')
                    .nth(2)
                    .is_some_and(|r| r.split(',').any(|r| r == "admin"))
            })
            .and_then(|t| t.split(':').next().map(str::to_string))
    })
}

fn validate_listen(addr: &str) -> Result<String, String> {
    let expanded = match addr.rsplit_once(':') {
        Some(("", port)) => format!("127.0.0.1:{port}"),
        _ => addr.to_string(),
    };
    let resolved: Vec<std::net::SocketAddr> = expanded
        .to_socket_addrs()
        .map_err(|e| format!("invalid --listen address {expanded}: {e}"))?
        .collect();
    if resolved.is_empty() {
        return Err(format!(
            "--listen {expanded} did not resolve to any address"
        ));
    }
    if !resolved.iter().all(|a| a.ip().is_loopback()) {
        return Err(format!(
            "--listen {expanded} would accept plaintext TCP on a non-loopback interface; \
             bearer tokens must not travel unencrypted — bind 127.0.0.1 (or ::1) and \
             terminate TLS at a reverse proxy, or use stdio mode"
        ));
    }
    Ok(expanded)
}

/// P3-M1 (auth004): the HTTP/metrics listener carries login + the knowledge
/// API — loopback-only unless allow_remote_http is armed (same fail-closed
/// reasoning as validate_listen; arming itself is gated on credentials by
/// remote_http_requires_auth).
fn validate_http_listen(addr: &str, allow_remote: bool, tls: bool) -> Result<String, String> {
    let expanded = match addr.rsplit_once(':') {
        Some(("", port)) => format!("127.0.0.1:{port}"),
        _ => addr.to_string(),
    };
    let resolved: Vec<std::net::SocketAddr> = expanded
        .to_socket_addrs()
        .map_err(|e| format!("invalid --metrics-addr address {expanded}: {e}"))?
        .collect();
    if resolved.is_empty() {
        return Err(format!(
            "--metrics-addr {expanded} did not resolve to any address"
        ));
    }
    if resolved.iter().all(|a| a.ip().is_loopback()) {
        return Ok(expanded);
    }
    if !allow_remote {
        return Err(format!(
            "--metrics-addr {expanded} would serve the HTTP surface (login + knowledge API) \
             on a non-loopback interface; bind 127.0.0.1 (or ::1), or set \
             allow_remote_http = true together with [auth] credentials and --tls-cert/--tls-key"
        ));
    }
    // logins and session tokens must not cross the network in plaintext
    if !tls {
        return Err(format!(
            "--metrics-addr {expanded} would serve logins and session tokens over plaintext \
             HTTP on a non-loopback interface; add --tls-cert/--tls-key (or AIKOQL_TLS_CERT/_KEY) \
             to serve HTTPS, or bind 127.0.0.1"
        ));
    }
    Ok(expanded)
}

/// P3-M1 (auth004): a remote HTTP surface without configured credentials is
/// an unauthenticated network API — refused at serve, never defaulted.
fn remote_http_requires_auth(allow_remote: bool, auth_configured: bool) -> Result<(), String> {
    if allow_remote && !auth_configured {
        return Err(
            "allow_remote_http = true requires HTTP credentials ([auth].users in aikoql.toml \
             or AIKOQL_ADMIN_PASSWORD) — refusing to serve an unauthenticated HTTP surface \
             on non-loopback interfaces"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
