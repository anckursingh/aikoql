//! Subcommand runners extracted verbatim from cli.rs (PRR-7).
//! No behavior changes.

use crate::*;

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
pub(crate) fn content_trust_extension(ct: ContentTrust) -> ExtensionMap {
    let mut ext = ExtensionMap::new();
    ext.insert(
        KnowledgeObject::EXT_CONTENT_TRUST.into(),
        Value::Text(ct.as_str().into()),
    );
    ext
}
pub(crate) fn run_ingest_dir(
    path: &str,
    db_path: &str,
    parallel: bool,
    incremental: bool,
    model_dir: Option<&str>,
) {
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
    // consumers. PRR-3: never downloads — loads the installed local model.
    #[cfg(feature = "embedding-candle")]
    let embedder = {
        let candle_dir = crate::model_store_dir(model_dir).join(
            aikoql_semantic::provider::model_slug(aikoql_semantic::provider::DEFAULT_MODEL_ID),
        );
        match aikoql_semantic::provider::CandleEmbedding::from_local(&candle_dir) {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                eprintln!(
                    "Embedding model unavailable ({e}) — run `aikoql model install` to install all-MiniLM-L6-v2 into {}; semantic recall disabled for this ingest",
                    candle_dir.display()
                );
                None
            }
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
    let kernel = match engine::open_kernel_auto(db_path) {
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
        // v0.3 K1: canonical evidence trail — page/bbox survive into the KO
        // (they used to be flattened into properties and partially dropped).
        let ev_method = if ent.evidence.extractor.contains("rust") {
            EvidenceMethod::AstExtraction
        } else {
            EvidenceMethod::DocExtraction
        };
        let mut kernel_ev = Evidence::new(
            ent.evidence
                .document_id
                .clone()
                .unwrap_or_else(|| path.to_string()),
            ev_method,
        );
        let mut loc_parts: Vec<String> = Vec::new();
        if let Some(p) = ent.evidence.page {
            loc_parts.push(format!("page {}", p));
        }
        if let Some(b) = &ent.evidence.bbox_text {
            loc_parts.push(format!("bbox {:?}", b));
        }
        if !loc_parts.is_empty() {
            kernel_ev = kernel_ev.with_location(loc_parts.join(", "));
        }
        kernel_ev = kernel_ev.with_confidence(ent.evidence.confidence);
        let mut extensions = content_trust_extension(ContentTrust::Trusted);
        extensions.insert(
            "authority".into(),
            Value::Text(Authority::for_evidence_method(ev_method).as_str().into()),
        );
        extensions.insert(
            "scope".into(),
            Value::Text(Scope::Repository.as_str().into()),
        );
        extensions.insert(
            KnowledgeObject::EXT_EVIDENCE.into(),
            KnowledgeObject::evidence_value(&[kernel_ev]),
        );
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
            extensions,
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
