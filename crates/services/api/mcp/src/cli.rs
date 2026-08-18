//! CLI subcommands: backup/restore/audit/report/ingest-dir/import/keygen.
//! Extracted from main.rs (R7 modularization). No behavior changes.

use crate::*;
pub(crate) fn print_usage() {
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
        "  --embedding-provider P  Embedding provider: \"candle\" (default) or \"openai\"\n",
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

pub(crate) fn run_backup(db_path: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir_name = format!("{}.backup.{}", db_path, ts);
    if let Err(e) = std::fs::create_dir_all(&dir_name) {
        eprintln!("create backup dir: {}", e);
        std::process::exit(1);
    }

    // Gather metadata, then drop kernel to release file lock before copy.
    let (seq, object_count) = {
        let engine = match RedbEngine::open(db_path) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("open source db: {}", e);
                std::process::exit(1);
            }
        };
        let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("open kernel: {}", e);
                std::process::exit(1);
            }
        };
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
    if let Err(e) = std::fs::copy(db_path, &data_path) {
        eprintln!("copy db file: {}", e);
        std::process::exit(1);
    }

    let meta = serde_json::json!({
        "journal_seq": seq,
        "object_count": object_count,
        "backup_ts": ts,
        "source": db_path,
    });
    let meta_json = serde_json::to_string_pretty(&meta).unwrap_or_else(|e| {
        eprintln!("write backup meta: {}", e);
        std::process::exit(1);
    });
    if let Err(e) = std::fs::write(format!("{}/meta.json", dir_name), meta_json) {
        eprintln!("write backup meta: {}", e);
        std::process::exit(1);
    }

    println!("Backup created: {}", dir_name);
    println!("  Objects: {}", object_count);
    println!("  Journal seq: {}", seq);
}

pub(crate) fn run_restore(backup_dir: &str, target_path: &str) {
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
    if let Err(e) = std::fs::copy(&data_path, target_path) {
        eprintln!("restore copy: {}", e);
        std::process::exit(1);
    }
    println!("Restored to: {}", target_path);
}

pub(crate) fn run_audit(db_path: &str) {
    let engine = match RedbEngine::open(db_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("open db: {}", e);
            std::process::exit(1);
        }
    };
    let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
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

pub(crate) fn run_report(path: &str) {
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

/// Enrich the merged IR before the snapshot write: one `file` entity per
/// source file referenced by entity evidence, plus `file contains entity`
/// relations. compile_context's relation-aware boost follows these edges, so
/// a ranked entity's containing file enters the fold at low token budgets.
/// ponytail: kernel-side File KOs/edges are already written by Phase 0/1b —
/// this only feeds the ir_json snapshot, so no kernel graph changes (and no
/// duplicate File KOs on re-ingest). File entities get no embeddings (added
/// after the embedding pass): paths tokenize as subword junk that would only
/// feed the gibberish cosine band the semantic gate exists for.
pub(crate) fn enrich_file_contains(ir: &mut aikoql_ingestion::KnowledgeIr) {
    use std::collections::HashSet;
    let existing: HashSet<String> = ir.entities.iter().map(|e| e.name.clone()).collect();
    let pairs: Vec<(String, String)> = ir
        .entities
        .iter()
        .filter(|e| {
            e.evidence
                .document_id
                .as_deref()
                .is_some_and(|doc| doc != e.name)
        })
        .map(|e| {
            (
                // justified: entity without provenance → no document link
                e.evidence.document_id.clone().unwrap_or_default(),
                e.name.clone(),
            )
        })
        .collect();
    let mut added: HashSet<String> = HashSet::new();
    for (doc, name) in pairs {
        if !existing.contains(&doc) && added.insert(doc.clone()) {
            ir.entities.push(aikoql_ingestion::EntityCandidate {
                name: doc.clone(),
                type_hint: Some("file".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: aikoql_ingestion::Evidence {
                    document_id: Some(doc.clone()),
                    ..Default::default()
                },
            });
        }
        ir.relations.push(aikoql_ingestion::RelationCandidate {
            subject: doc,
            predicate: "contains".into(),
            object: name,
            confidence: 0.9,
            evidence: aikoql_ingestion::Evidence::default(),
        });
    }
}

/// R8: stamp ingested KOs with their content trust level. The IR carries the
/// value for compile_context; the KO extension makes it queryable/auditable
/// in the kernel graph.
pub(crate) fn content_trust_extension(ct: ContentTrust) -> ExtensionMap {
    let mut ext = ExtensionMap::new();
    ext.insert(
        KnowledgeObject::EXT_CONTENT_TRUST.into(),
        Value::Text(ct.as_str().into()),
    );
    ext
}

pub(crate) fn run_ingest_dir(path: &str, db_path: &str, parallel: bool, incremental: bool) {
    // Idempotency keys embed the path verbatim — normalize separators so
    // `E:\x` and `E:/x` update the same KOs instead of duplicating them
    // (every KO here keys on this string; Windows APIs accept both forms).
    let path = &path.replace('\\', "/");
    eprintln!("Ingesting directory: {}\n", path);

    let mut result = if incremental {
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

    // R8: a local repo checkout is human-authored, reviewed content —
    // tag the whole IR Trusted so the context compiler keeps its
    // instruction facts (uploads go the other way: deploy_document
    // stamps Untrusted).
    result.ir.content_trust = Some(ContentTrust::Trusted);

    let report = aikoql_ingestion::build_report(
        &result.ir,
        path,
        result.files_processed,
        result.files_skipped,
        result.dirs_skipped,
        result.binary_skipped,
    );
    println!("{}\n", aikoql_ingestion::format_report(&report));

    // Semantic recall pass: embed each entity (name + first mention) once so
    // compile_context can match symptom-described tasks ("fix the endpoint
    // resolution bug") that share no keywords with entity names. Stored as a
    // second property on the snapshot KO — ir_json stays lean for its other
    // consumers. ponytail: CLI loads its own candle model (one-time ~90MB
    // download); no OpenAI provider here, add a flag when it's needed.
    #[cfg(feature = "embedding-candle")]
    let embedder = match aikoql_semantic::provider::CandleEmbedding::new() {
        Ok(p) => Some(Arc::new(p)),
        Err(e) => {
            eprintln!(
                "Embedding model unavailable ({}), semantic recall disabled for this ingest",
                e
            );
            None
        }
    };
    #[cfg(not(feature = "embedding-candle"))]
    let embedder: Option<Arc<dyn EmbeddingProvider>> = None;

    let mut entity_embeddings: HashMap<String, Vec<f32>> = HashMap::new();
    if let Some(p) = embedder {
        let t0 = Instant::now();
        let (mut ok, mut skip) = (0usize, 0usize);
        for ent in &result.ir.entities {
            let mention: String = ent
                .mentions
                .first()
                .map(|m| m.chars().take(256).collect())
                // justified: entity without mentions → empty mention text
                .unwrap_or_default();
            let text = format!("{} {}", ent.name, mention);
            match p.embed(&text, None) {
                Ok(v) if !v.is_empty() => {
                    let key = format!(
                        "{}::{}",
                        // justified: no provenance → empty document id
                        ent.evidence.document_id.as_deref().unwrap_or_default(),
                        ent.name
                    );
                    entity_embeddings.insert(key, v);
                    ok += 1;
                }
                _ => skip += 1,
            }
        }
        eprintln!(
            "Embedded {}/{} entities for semantic recall in {:?} ({} skipped)",
            ok,
            ok + skip,
            t0.elapsed(),
            skip
        );
    }

    // Store the IR as production knowledge: one KO per entity with kernel
    // relationships between them. The summary KO below remains only as the
    // compile_context IR snapshot (tool_compile_context reads ir_json).
    let engine = match RedbEngine::open(db_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("open db: {}", e);
            std::process::exit(1);
        }
    };
    let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
    let subj = Subject::with_roles("ingest-dir", &["admin"]);

    // Facts attach to every entity they reference.
    let mut facts_by_entity: HashMap<&str, Vec<String>> = HashMap::new();
    for fact in &result.ir.facts {
        for name in &fact.entities {
            facts_by_entity
                .entry(name.as_str())
                .or_default()
                .push(fact.statement.clone());
        }
    }

    // Phase 1: one KO per entity. The idempotency key includes the entity's
    // document so same-named entities from different files (each file's `mod
    // tests`, `fn main`) stay distinct KOs instead of collapsing into one.
    let mut ent_kos: Vec<(&str, KOID, Option<String>)> = Vec::new();
    let mut entity_failures = 0usize;
    for ent in &result.ir.entities {
        // A path-named fallback entity (unparseable/text file) is its own
        // File KO — type it as such instead of its extractor hint.
        let type_name = if ent.evidence.document_id.as_deref() == Some(ent.name.as_str()) {
            "File".into()
        } else {
            entity_type_name(ent.type_hint.as_deref())
        };
        let mut ent_props = PropertyMap::new();
        ent_props.insert("name".into(), Value::Text(ent.name.clone()));
        if let Some(hint) = &ent.type_hint {
            ent_props.insert("type_hint".into(), Value::Text(hint.clone()));
        }
        ent_props.insert(
            "mentions".into(),
            Value::List(ent.mentions.iter().cloned().map(Value::Text).collect()),
        );
        ent_props.insert("confidence".into(), Value::Float(ent.confidence as f64));
        if let Some(doc) = &ent.evidence.document_id {
            ent_props.insert("evidence_document".into(), Value::Text(doc.clone()));
        }
        ent_props.insert(
            "evidence_extractor".into(),
            Value::Text(ent.evidence.extractor.clone()),
        );
        if let Some(model) = &ent.evidence.model {
            ent_props.insert("evidence_model".into(), Value::Text(model.clone()));
        }
        if let Some(facts) = facts_by_entity.get(ent.name.as_str()) {
            ent_props.insert(
                "facts".into(),
                Value::List(facts.iter().cloned().map(Value::Text).collect()),
            );
        }
        // Exact-once replay means a re-ingest would silently keep the stale
        // entity content (e.g. mention text from an older parser). Resolve
        // the idempotency key: existing → true update, new → guarded create.
        let idem = format!(
            "ingest-entity:{}:{}:{}",
            path,
            // justified: no provenance → empty document id
            ent.evidence.document_id.as_deref().unwrap_or_default(),
            ent.name
        );
        let mut req = RememberRequest {
            context: (&subj).into(),
            koid: None,
            expected_version: None,
            idempotency_key: None,
            metadata: Metadata {
                type_name,
                tenant: None,
                schema_version: 1,
                tags: vec!["ingest-dir".into(), "auto".into()],
            },
            properties: ent_props,
            semantic: None,
            relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: "ingest-dir".into(),
                acl: vec![],
                classification: None,
            }),
            extensions: content_trust_extension(ContentTrust::Trusted),
            origin: Origin::Agent("ingest-dir".into()),
            note: Some(format!("entity from ingest-dir {}", path)),
            referential_policy: ReferentialPolicy::Permissive,
        };
        match kernel.resolve_idempotency(&idem) {
            Ok(Some((koid, _, _))) => {
                req.koid = Some(koid);
                // Carry the semantic block forward when the entity's content
                // is unchanged — an update with semantic:None resets it, and
                // serve's startup catch-up then re-embeds every KO (~4-6 min
                // per re-ingest). Changed/new entities stay None and get
                // enriched at serve start (only those, not the whole corpus).
                if let Ok(old) = kernel.get(KnowledgeContext::from(&subj), &koid) {
                    if old.properties == req.properties {
                        req.semantic = old.semantic.clone();
                    }
                }
            }
            _ => {
                req.idempotency_key = Some(idem);
                req.expected_version = Some(0);
            }
        }
        match kernel.remember(req) {
            Ok(r) => {
                ent_kos.push((ent.name.as_str(), r.koid, ent.evidence.document_id.clone()));
            }
            Err(e) => {
                entity_failures += 1;
                eprintln!("entity {}: {}", ent.name, e);
            }
        }
    }

    // Phase 0: one File KO per walked source file, independent of whether any
    // of its entities survived merging — a fully name-deduped, macro-only, or
    // empty file must still be a node in the graph. Fallback path entities
    // (doc == name) are stored by Phase 1 and reused below, so skip those.
    let mut file_koids: HashMap<String, KOID> = HashMap::new();
    let mut outbound: HashMap<KOID, Vec<RelationshipRef>> = HashMap::new();
    let mut orphan_refs: Vec<RelationshipRef> = Vec::new();
    let fallback_docs: std::collections::HashSet<&str> = result
        .ir
        .entities
        .iter()
        .filter(|e| e.evidence.document_id.as_deref() == Some(e.name.as_str()))
        .map(|e| e.name.as_str())
        .collect();
    let mut paths = Vec::new();
    let mut path_stats = aikoql_ingestion::IngestStats::default();
    if let Err(e) = aikoql_ingestion::collect_file_paths(
        std::path::Path::new(path),
        &mut paths,
        &mut path_stats,
    ) {
        eprintln!("file walk for containment: {}", e);
    }
    for p in &paths {
        let doc = p.to_string_lossy().to_string();
        if fallback_docs.contains(doc.as_str()) || file_koids.contains_key(&doc) {
            continue;
        }
        let mut fprops = PropertyMap::new();
        fprops.insert("name".into(), Value::Text(doc.clone()));
        match kernel.remember(RememberRequest {
            context: (&subj).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: Some(format!("ingest-file:{}:{}", path, doc)),
            metadata: Metadata {
                type_name: "File".into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["ingest-dir".into(), "auto".into()],
            },
            properties: fprops,
            semantic: None,
            relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: "ingest-dir".into(),
                acl: vec![],
                classification: None,
            }),
            extensions: content_trust_extension(ContentTrust::Trusted),
            origin: Origin::Agent("ingest-dir".into()),
            note: Some(format!("source file from ingest-dir {}", path)),
            referential_policy: ReferentialPolicy::Permissive,
        }) {
            Ok(r) => {
                file_koids.insert(doc, r.koid);
            }
            Err(e) => eprintln!("file KO {}: {}", doc, e),
        }
    }

    // Phase 1b: containment — each file `contains` its entities and the
    // directory KO (below) `contains` every file. Fallback path entities
    // (doc == name) become their own File KO.
    for &(name, koid, ref doc_opt) in &ent_kos {
        let Some(doc) = doc_opt.as_deref() else {
            // No provenance — hook directly to the directory KO.
            orphan_refs.push(RelationshipRef {
                rel_type: kom::CONTAINS.to_string(),
                target: koid,
                direction: Direction::Outbound,
            });
            continue;
        };
        if doc == name {
            file_koids.insert(doc.to_string(), koid);
            continue;
        }
        let file_koid = match file_koids.get(doc) {
            Some(&k) => k,
            None => {
                let mut fprops = PropertyMap::new();
                fprops.insert("name".into(), Value::Text(doc.to_string()));
                let k = match kernel.remember(RememberRequest {
                    context: (&subj).into(),
                    koid: None,
                    expected_version: Some(0),
                    idempotency_key: Some(format!("ingest-file:{}:{}", path, doc)),
                    metadata: Metadata {
                        type_name: "File".into(),
                        tenant: None,
                        schema_version: 1,
                        tags: vec!["ingest-dir".into(), "auto".into()],
                    },
                    properties: fprops,
                    semantic: None,
                    relationships: vec![],
                    security: Some(SecurityDescriptor {
                        owner: "ingest-dir".into(),
                        acl: vec![],
                        classification: None,
                    }),
                    extensions: ExtensionMap::new(),
                    origin: Origin::Agent("ingest-dir".into()),
                    note: Some(format!("source file from ingest-dir {}", path)),
                    referential_policy: ReferentialPolicy::Permissive,
                }) {
                    Ok(r) => r.koid,
                    Err(e) => {
                        eprintln!("file KO {}: {}", doc, e);
                        orphan_refs.push(RelationshipRef {
                            rel_type: kom::CONTAINS.to_string(),
                            target: koid,
                            direction: Direction::Outbound,
                        });
                        continue;
                    }
                };
                file_koids.insert(doc.to_string(), k);
                k
            }
        };
        outbound
            .entry(file_koid)
            .or_default()
            .push(RelationshipRef {
                rel_type: kom::CONTAINS.to_string(),
                target: koid,
                direction: Direction::Outbound,
            });
    }

    // Phase 2: relationships, outbound from the subject entity's KO. Grouped
    // per subject so each KO gets one update (replace semantics keep
    // re-ingest idempotent). Relation endpoints resolve doc-locally first
    // (TESTED_BY "tests" must hit this file's tests module, not another
    // file's), then case-folded unique name, then last `::` segment
    // (use-paths / trait paths like `aikoql_kernel::KError`). Anything else —
    // std/external refs, `crate`, wikilink targets outside the corpus — is
    // out-of-corpus and skipped.
    let mut local_lower: HashMap<(String, String), KOID> = HashMap::new();
    let mut by_lower: HashMap<String, KOID> = HashMap::new();
    let mut by_last_seg: HashMap<String, KOID> = HashMap::new();
    let mut ambiguous: std::collections::HashSet<String> = std::collections::HashSet::new();
    for &(name, koid, ref doc) in &ent_kos {
        local_lower.insert(
            (
                // justified: no provenance → empty document id
                doc.as_deref().unwrap_or_default().to_string(),
                name.to_lowercase(),
            ),
            koid,
        );
        let lower = name.to_lowercase();
        if by_lower.insert(lower.clone(), koid).is_some() {
            ambiguous.insert(lower);
        }
        if let Some(seg) = name.rsplit("::").next() {
            let key = seg.to_lowercase();
            if by_last_seg.insert(key.clone(), koid).is_some() {
                ambiguous.insert(key);
            }
        }
    }
    let resolve = |name: &str, doc: Option<&str>| -> Option<KOID> {
        let lower = name.to_lowercase();
        // justified: no provenance → empty document id
        if let Some(&k) = local_lower.get(&(doc.unwrap_or_default().to_string(), lower.clone())) {
            return Some(k);
        }
        if !ambiguous.contains(&lower) {
            if let Some(&k) = by_lower.get(&lower) {
                return Some(k);
            }
        }
        if let Some(seg) = name.rsplit("::").next() {
            let key = seg.to_lowercase();
            if !ambiguous.contains(&key) {
                if let Some(&k) = by_last_seg.get(&key) {
                    return Some(k);
                }
            }
        }
        None
    };
    let mut rel_skipped = 0usize;
    for rel in &result.ir.relations {
        // The parser anchors per-file roots as "crate"; resolve to the File KO
        // via the relation's provenance.
        let crate_koid = |name: &str| {
            (name == "crate")
                .then(|| {
                    rel.evidence
                        .document_id
                        .as_deref()
                        .and_then(|d| file_koids.get(d))
                        .copied()
                })
                .flatten()
        };
        // `impl X for Y` relations carry the whole impl header as subject.
        let Some(subject_koid) = resolve(&rel.subject, rel.evidence.document_id.as_deref())
            .or_else(|| {
                resolve(
                    rel.subject.rsplit(" for ").next().unwrap_or(&rel.subject),
                    rel.evidence.document_id.as_deref(),
                )
            })
            .or_else(|| crate_koid(&rel.subject))
        else {
            rel_skipped += 1;
            if rel_skipped <= 5 {
                eprintln!(
                    "  unresolved: {} {} {} (out-of-corpus)",
                    rel.subject, rel.predicate, rel.object
                );
            }
            continue;
        };
        // `use a::{B, C}` relations join targets with commas — one edge each.
        let mut targets: Vec<KOID> = Vec::new();
        for target in rel.object.split(',') {
            let t = target.trim();
            if let Some(k) =
                resolve(t, rel.evidence.document_id.as_deref()).or_else(|| crate_koid(t))
            {
                if !targets.contains(&k) {
                    targets.push(k);
                }
            }
        }
        if targets.is_empty() {
            rel_skipped += 1;
            continue;
        }
        let entry = outbound.entry(subject_koid).or_default();
        for target in targets {
            entry.push(RelationshipRef {
                rel_type: rel.predicate.clone(),
                target,
                direction: Direction::Outbound,
            });
        }
    }
    let mut rel_written = 0usize;
    for (koid, refs) in &outbound {
        let ctx = KnowledgeContext::from(&subj);
        match kernel.get(ctx, koid) {
            Ok(ko) => {
                let mut req = RememberRequest::update(
                    KnowledgeContext::from(&subj),
                    *koid,
                    ko.metadata.clone(),
                );
                req.properties = ko.properties.clone();
                // Same carry-forward as Phase 1 — this update touches only
                // relationships, so preserve whatever semantic Phase 1 left
                // (carried forward or None for changed entities).
                req.semantic = ko.semantic.clone();
                req.relationships = refs.clone();
                match kernel.remember(req) {
                    Ok(_) => rel_written += refs.len(),
                    Err(e) => eprintln!("relationships for {}: {}", koid.to_hex(), e),
                }
            }
            Err(e) => eprintln!("relationships for {}: {}", koid.to_hex(), e),
        }
    }
    println!(
        "Stored {} entity KOs, {} file KOs and {} relationships{}",
        ent_kos.len(),
        file_koids.len(),
        rel_written,
        if entity_failures > 0 {
            format!(" ({} entities failed)", entity_failures)
        } else if rel_skipped > 0 {
            format!(" ({} out-of-corpus refs skipped)", rel_skipped)
        } else {
            String::new()
        }
    );

    // Snapshot KO — ir_json's only consumer is tool_compile_context.
    enrich_file_contains(&mut result.ir);
    let ir_json = serde_json::to_string(&result.ir).unwrap_or_else(|e| {
        eprintln!("serialize ir: {}", e);
        std::process::exit(1);
    });
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
    let emb_json = serde_json::to_string(&entity_embeddings).unwrap_or_else(|e| {
        eprintln!("serialize entity_embeddings: {}", e);
        std::process::exit(1);
    });
    props.insert("ir_json".into(), Value::Text(ir_json));
    props.insert("entity_embeddings".into(), Value::Text(emb_json));

    // Same exact-once-replay trap as entity KOs: without resolving the key,
    // a re-ingest would keep the stale ir_json/entity_embeddings forever.
    let idem = format!("ingest-dir-{}", path);
    let mut req = RememberRequest {
        context: (&subj).into(),
        koid: None,
        expected_version: None,
        idempotency_key: None,
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
        extensions: content_trust_extension(ContentTrust::Trusted),
        origin: Origin::Human,
        note: Some(format!(
            "Directory ingestion IR snapshot (compile_context source): {}",
            path
        )),
        referential_policy: ReferentialPolicy::Permissive,
    };
    match kernel.resolve_idempotency(&idem) {
        Ok(Some((koid, _, _))) => req.koid = Some(koid),
        _ => {
            req.idempotency_key = Some(idem);
            req.expected_version = Some(0);
        }
    }
    match kernel.remember(req) {
        Ok(r) => {
            // The directory KO is the graph root: it `contains` every File KO
            // and any entity without file provenance. Update (REPLACE) keeps
            // re-ingest idempotent.
            let mut dir_refs: Vec<RelationshipRef> = file_koids
                .values()
                .map(|&k| RelationshipRef {
                    rel_type: kom::CONTAINS.to_string(),
                    target: k,
                    direction: Direction::Outbound,
                })
                .collect();
            dir_refs.extend(orphan_refs);
            let ctx = KnowledgeContext::from(&subj);
            if let Ok(ko) = kernel.get(ctx, &r.koid) {
                let mut req = RememberRequest::update(
                    KnowledgeContext::from(&subj),
                    r.koid,
                    ko.metadata.clone(),
                );
                req.properties = ko.properties.clone();
                req.relationships = dir_refs;
                if let Err(e) = kernel.remember(req) {
                    eprintln!("Warning: directory containment edges: {}", e);
                }
            }
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

/// Map an IR type hint to a kernel type name. Hints are extractor-produced
/// tokens like "file", "module", "test"; anything unusable falls back to the
/// generic entity type.
pub(crate) fn entity_type_name(hint: Option<&str>) -> String {
    match hint {
        Some(h) if !h.trim().is_empty() => {
            let sanitized: String = h
                .trim()
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            if sanitized.is_empty() {
                "aikoql:entity".into()
            } else {
                sanitized
            }
        }
        _ => "aikoql:entity".into(),
    }
}

pub(crate) fn run_pg_import(
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

    let engine = match RedbEngine::open(target_db) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("open target db: {}", e);
            std::process::exit(1);
        }
    };
    let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
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

pub(crate) fn run_sqlite_import(
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

    let engine = match RedbEngine::open(target_db) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("open target db: {}", e);
            std::process::exit(1);
        }
    };
    let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
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

pub(crate) fn run_mongo_import(
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

    let engine = match RedbEngine::open(target_db) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("open target db: {}", e);
            std::process::exit(1);
        }
    };
    let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
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

pub(crate) fn run_neo4j_import(
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
    let rel_types = match connector.list_rel_types() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to list relationship types: {}", e);
            std::process::exit(1);
        }
    };
    println!(
        "Labels: {} ({}), Relationship types: {} ({})",
        labels.len(),
        labels.join(", "),
        rel_types.len(),
        rel_types.join(", ")
    );
    println!();

    let engine = match RedbEngine::open(target_db) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("open target db: {}", e);
            std::process::exit(1);
        }
    };
    let kernel = match Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xCAFE) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("open kernel: {}", e);
            std::process::exit(1);
        }
    };
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

pub(crate) fn run_keygen(path: &str) {
    use aikoql_kernel::security::crypto::{Aes256Gcm, CryptoProvider};
    let key = Aes256Gcm::new().generate_key();
    let hex: String = key.iter().map(|b| format!("{:02x}", b)).collect();
    if path == "-" {
        println!("{}", hex);
    } else {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!("create key dir: {}", e);
                    std::process::exit(1);
                }
            }
        }
        if let Err(e) = std::fs::write(path, &hex) {
            eprintln!("write key file: {}", e);
            std::process::exit(1);
        }
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

/// CLI subcommand dispatch (R7: moved verbatim from main()). Returns true
/// when a subcommand ran; false falls through to server mode.
pub(crate) fn dispatch(args: &[String], subcmd: Option<&str>, subcmd_idx: Option<usize>) -> bool {
    let arg_after = subcmd_idx.and_then(|i| args.get(i + 2)).map(String::as_str);
    let arg_after2 = subcmd_idx.and_then(|i| args.get(i + 3)).map(String::as_str);

    match subcmd {
        Some("shell") => {
            // Accept: aikoql-mcp shell [--tenant NAME] [DB_PATH]
            let Some(idx) = subcmd_idx else {
                eprintln!("Usage: aikoql-mcp shell [--tenant NAME] [DB_PATH]");
                std::process::exit(2);
            };
            let mut db = "./aikoql.redb";
            let mut tenant: Option<&str> = None;
            let tail_args: Vec<&str> = args.iter().skip(idx + 2).map(String::as_str).collect();
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
            crate::shell::run_shell(db, tenant);
            true
        }
        Some("backup") => {
            run_backup(arg_after.unwrap_or("./aikoql.redb"));
            true
        }
        Some("restore") => {
            let backup = arg_after.unwrap_or_else(|| {
                eprintln!("Usage: aikoql-mcp restore <BACKUP_DIR> [DB_PATH]");
                std::process::exit(1);
            });
            let target = arg_after2.unwrap_or("./aikoql.redb");
            run_restore(backup, target);
            true
        }
        Some("audit") => {
            run_audit(arg_after.unwrap_or("./aikoql.redb"));
            true
        }
        Some("ingest-dir") => {
            let Some(idx) = subcmd_idx else {
                eprintln!("Usage: aikoql-mcp ingest-dir [PATH] [DB] [--parallel] [--incremental]");
                std::process::exit(2);
            };
            let path = arg_after.unwrap_or(".");
            let db = arg_after2.unwrap_or("./aikoql.redb");
            let mut parallel = false;
            let mut incremental = false;
            let tail_args: Vec<&str> = args.iter().skip(idx + 2).map(String::as_str).collect();
            for a in &tail_args {
                if *a == "--parallel" {
                    parallel = true;
                } else if *a == "--incremental" {
                    incremental = true;
                }
            }
            run_ingest_dir(path, db, parallel, incremental);
            true
        }
        Some("report") => {
            let path = arg_after.unwrap_or(".");
            run_report(path);
            true
        }
        Some("import") => {
            // import <source> <source-args...>
            //   import postgres <conn_str> [--tenant NAME] [--table TABLE] [DB_PATH]
            //   import sqlite <file.db> [--tenant NAME] [--table TABLE] [DB_PATH]
            let Some(idx) = subcmd_idx else {
                eprintln!("Usage: aikoql-mcp import <SOURCE> [ARGS...]");
                std::process::exit(2);
            };
            let ti_args: Vec<&str> = args.iter().skip(idx + 2).map(String::as_str).collect();
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
            true
        }
        Some("keygen") => {
            run_keygen(arg_after.unwrap_or("./aikoql.key"));
            true
        }
        Some("help") => {
            print_usage();
            true
        }
        _ => false,
    }
}
