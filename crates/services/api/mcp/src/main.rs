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
mod audit;
mod authz;
mod cli;
mod error_codes;
mod graph_ui;
mod helpers;
mod http;
mod knowledge_runtime;
mod rate_limiter;
mod server;
mod session;
mod shell;
mod studio;
mod tools;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

pub(crate) struct HttpSession {
    pub username: String,
    pub roles: Vec<String>,
    pub created: Instant,
}

// pub(crate) re-exports: this block is the crate prelude. Extracted modules
// (R7) rely on `use crate::*` to see these; api_rest/knowledge_runtime rely on
// `use super::*` — both work because these are re-exports, not private uses.
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
pub(crate) use serde_json::{json, Value as J};
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::io::{BufRead, BufReader, Read, Write};
pub(crate) use std::net::{TcpListener, TcpStream};
pub(crate) use std::sync::atomic::{AtomicU64, Ordering};
pub(crate) use std::sync::{Arc, Mutex, OnceLock};
pub(crate) use std::thread;
pub(crate) use std::time::{Duration, Instant};
pub(crate) use tracing::{error, info, info_span, warn};

pub(crate) static SERVER_START: OnceLock<Instant> = OnceLock::new();
pub(crate) static MEMORY_DIR: OnceLock<String> = OnceLock::new();

pub(crate) const PROTOCOL_VERSION: &str = "2024-11-05";

// main() is the orchestrator: CLI dispatch + server bootstrap. Everything
// else lives in the modules above (R7).
use crate::cli::*;
use crate::http::*;
use crate::server::*;

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
    if dispatch(&args, subcmd, subcmd_idx) {
        return;
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
        let Some(idx) = subcmd_idx else {
            eprintln!("Usage: aikoql-mcp serve [OPTIONS] [DB]");
            std::process::exit(2);
        };
        idx + 2
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
                // justified: missing flag value → empty (no API key)
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

    let engine = match RedbEngine::open(&db_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("open store: {}", e);
            std::process::exit(1);
        }
    };
    let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xA9C9) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };

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
                match aikoql_semantic::provider::CandleEmbedding::new() {
                    Ok(p) => Some(Arc::new(p)),
                    // A cold machine or a transient HF failure must not kill
                    // the server (CI hit this: first tools/call got EOF because
                    // serve panicked before reading stdin). Degrade to
                    // lexical-only recall; a later restart retries the download.
                    Err(e) => {
                        info!(error = %e, "candle model unavailable — serving without semantic embeddings");
                        None
                    }
                }
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
        // Record the real model: candle always loads all-MiniLM-L6-v2; the
        // --embedding-model flag only names the OpenAI-compatible endpoint.
        let enrichment_model = if embedding_provider.as_deref() == Some("openai") {
            model.clone()
        } else {
            "all-MiniLM-L6-v2".to_string()
        };
        let enricher = EmbeddingEnricher::new(enrichment_provider, &enrichment_model);
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
                        .expect("manual is non-empty") // justified: guarded by !manual.is_empty() above
                } else {
                    // Fall back to latest auto-discovered.
                    kos.iter()
                        .max_by_key(|ko| ko.commit_ts)
                        .expect("kos is non-empty") // justified: guarded by the outer Ok(kos) if !kos.is_empty() arm
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
                        match OntologyRegistry::new(def) {
                            Ok(r) => Arc::new(r),
                            Err(e) => {
                                info!(error = %e, "ontology registry failed to initialize — using empty registry");
                                Arc::new(OntologyRegistry::empty())
                            }
                        }
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
        spawn_metrics(
            kernel.clone(),
            ontology.clone(),
            addr.clone(),
            db_path.clone(),
        );
    }

    if let Some(addr) = listen_addr {
        run_tcp_listener(kernel.clone(), &addr, db_path.clone());
    } else {
        run_stdio(&kernel, &db_path);
    }
}

#[cfg(test)]
mod tests {
    use crate::http::truncate;

    #[test]
    fn truncate_never_splits_multibyte_chars() {
        // 25 x 'a' + '—' (bytes 25..28) + 'zzzz' = 32 bytes. max 30 → end 27
        // lands inside the em dash and must back off to a char boundary.
        let s = "aaaaaaaaaaaaaaaaaaaaaaaaa—zzzz";
        let t = truncate(s, 30);
        assert!(t.ends_with("..."));
        assert_eq!(&t[t.len() - 4..], "a...");
    }

    #[test]
    fn truncate_passthrough_short_strings() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn enrich_file_contains_adds_file_entities_and_relations() {
        use aikoql_ingestion::{EntityCandidate, Evidence, KnowledgeIr};
        let mut ir = KnowledgeIr {
            entities: vec![
                EntityCandidate {
                    name: "graph_api".into(),
                    type_hint: Some("Function".into()),
                    mentions: vec![],
                    confidence: 0.8,
                    evidence: Evidence {
                        document_id: Some("src/main.rs".into()),
                        ..Default::default()
                    },
                },
                EntityCandidate {
                    name: "retry_loop".into(),
                    type_hint: Some("Function".into()),
                    mentions: vec![],
                    confidence: 0.8,
                    evidence: Evidence {
                        document_id: Some("src/main.rs".into()),
                        ..Default::default()
                    },
                },
                // doc == name fallback path entity: no duplicate File entity,
                // no self-contains relation.
                EntityCandidate {
                    name: "src/lib.rs".into(),
                    type_hint: Some("file".into()),
                    mentions: vec![],
                    confidence: 0.8,
                    evidence: Evidence {
                        document_id: Some("src/lib.rs".into()),
                        ..Default::default()
                    },
                },
            ],
            ..Default::default()
        };
        crate::cli::enrich_file_contains(&mut ir);
        let files: Vec<&str> = ir
            .entities
            .iter()
            .filter(|e| e.type_hint.as_deref() == Some("file"))
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(files, vec!["src/lib.rs", "src/main.rs"]);
        let contains: Vec<(&str, &str, &str)> = ir
            .relations
            .iter()
            .map(|r| (r.subject.as_str(), r.predicate.as_str(), r.object.as_str()))
            .collect();
        assert_eq!(contains.len(), 2);
        assert!(contains.contains(&("src/main.rs", "contains", "graph_api")));
        assert!(contains.contains(&("src/main.rs", "contains", "retry_loop")));
    }

    #[test]
    fn semantic_scores_parses_caches_and_scores() {
        // Regression check for the EMB_CACHE self-deadlock: the cache-insert
        // branch used to re-lock the mutex it already held via a match
        // scrutinee temporary, wedging the first request (and every request
        // after it) forever. This test walks both branches: parse+insert,
        // then cache-hit.
        let db = std::env::temp_dir().join(format!("mnemo-sem-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let engine = super::RedbEngine::open(db.to_str().unwrap()).expect("open store");
        let k = super::Kernel::open(
            std::sync::Arc::new(engine),
            std::sync::Arc::new(super::SystemClock),
            0,
        )
        .expect("open kernel");

        let mut props = super::PropertyMap::new();
        props.insert(
            "entity_embeddings".into(),
            super::Value::Text(r#"{"a::b":[1.0,0.0]}"#.into()),
        );
        let r = k
            .remember(super::RememberRequest {
                context: super::KnowledgeContext::from(&super::Subject::with_roles(
                    "test",
                    &["admin"],
                )),
                koid: None,
                expected_version: Some(0),
                idempotency_key: Some("sem-scores-test".into()),
                metadata: super::Metadata {
                    type_name: "aikoql:ingested-directory".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: super::ExtensionMap::new(),
                origin: super::Origin::Human,
                note: None,
                referential_policy: super::ReferentialPolicy::Permissive,
            })
            .expect("remember");
        let args =
            serde_json::json!({"koid": r.koid.to_hex(), "subject": "test", "roles": ["admin"]});

        let scores = crate::tools::semantic_scores(&k, &args, &[1.0, 0.0]).expect("scores");
        assert!((scores["a::b"] - 1.0).abs() < 1e-6);
        let cached = crate::tools::semantic_scores(&k, &args, &[1.0, 0.0]).expect("cached hit");
        assert_eq!(cached.len(), 1);

        drop(k);
        let _ = std::fs::remove_file(&db);
    }
}
