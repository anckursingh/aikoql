//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::*;
use std::sync::LazyLock;

use crate::session::*;
pub(crate) fn tool_compile_context(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
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

    // Semantic scores: embed the task and score every stored entity
    // embedding against it. Falls back to lexical-only when no provider
    // is wired or the snapshot predates embedding support. semantic_ran
    // makes the fallback detectable in the response (§36: a disabled
    // index must not be silently absorbed).
    let (semantic, semantic_ran) = match k.embed_text(task, None) {
        Ok(task_emb) if !task_emb.is_empty() => match semantic_scores(k, args, &task_emb) {
            Some(scores) => (Some(scores), true),
            None => (None, false),
        },
        _ => (None, false),
    };

    // Compile context package — cached per (task, budget, knowledge hash,
    // semantic fingerprint) so re-asked contexts are served without
    // recompiling (5 min TTL).
    let pkg = aikoql_ingestion::compile_context_cached_semantic(
        task,
        &ir,
        token_budget,
        300,
        semantic.as_ref(),
    );
    let mut md = aikoql_ingestion::render_context_markdown(&pkg);

    // v0.3 K5: append matched agent experiences — prior runs the caller is
    // allowed to read, gated by reuse conditions and ranked by confidence.
    // Bounded by `limit`; the IR budget governs the core package.
    let experiences = k
        .match_experiences(subject_of(args), task, 5)
        .map_err(|e| e.to_string())?;
    let mut experience_json = Vec::new();
    if !experiences.is_empty() {
        let mut section = String::from("\n## Previous Agent Experience\n\n");
        for (ko, score) in &experiences {
            let txt = |key: &str| match ko.properties.get(key) {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let confidence = ko.confidence_context().map(|c| c.score).unwrap_or(0.0);
            section.push_str(&format!(
                "- **{}** (confidence {:.2}, reuse score {:.2})\n  - Goal: {}\n  - Action: {}\n  - Outcome: {}\n  - Lesson: {}\n",
                txt("actor"),
                confidence,
                score,
                txt("goal"),
                txt("action"),
                txt("outcome"),
                txt("lesson"),
            ));
            experience_json.push(json!({
                "koid": ko.koid.to_hex(),
                "score": score,
                "actor": txt("actor"),
                "goal": txt("goal"),
                "action": txt("action"),
                "outcome": txt("outcome"),
                "lesson": txt("lesson"),
                "confidence": confidence
            }));
        }
        md.push_str(&section);
    }

    let pkg_json = serde_json::to_value(&pkg).map_err(|e| format!("serialize package: {}", e))?;
    Ok(serde_json::json!({
        "context_markdown": md,
        "package": pkg_json,
        "koid": hex,
        "task": task,
        "token_budget": token_budget,
        "semantic": semantic_ran,
        "experiences": experience_json,
    }))
}

/// Cosine similarity of every stored entity embedding against the task
/// embedding, keyed "document_id::name" for compile_context_semantic.
/// The snapshot's `entity_embeddings` property is read once per directory
/// KO and cached in-process (parsing a ~10MB JSON string per call would
/// dominate the request).
pub(crate) type EmbeddingMap = HashMap<String, Vec<f32>>;

pub(crate) fn semantic_scores(
    k: &Kernel,
    args: &J,
    task_emb: &[f32],
) -> Option<HashMap<String, f32>> {
    static EMB_CACHE: LazyLock<Mutex<HashMap<String, Arc<EmbeddingMap>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    let hex = args.get("koid")?.as_str()?;
    // Cache hit: score directly. The guard must never span the kernel call
    // or the insert below — std Mutex is not reentrant, and a nested lock
    // on the same mutex self-deadlocks, wedging every later request.
    // justified: Mutex poison is unrecoverable
    if let Some(map) = EMB_CACHE.lock().unwrap().get(hex).cloned() {
        return Some(score_map(map, task_emb));
    }
    let koid = KOID::from_hex(hex).ok()?;
    let ctx = KnowledgeContext::from(subject_of(args));
    let ko = k.get(ctx, &koid).ok()?;
    let txt = match ko.properties.get("entity_embeddings") {
        Some(Value::Text(t)) => t,
        _ => return None, // snapshot predates semantic ingest → lexical-only
    };
    let parsed: HashMap<String, Vec<f32>> = serde_json::from_str(txt).ok()?;
    let arc = Arc::new(parsed);
    EMB_CACHE
        .lock()
        // justified: Mutex poison is unrecoverable
        .unwrap()
        .insert(hex.to_string(), arc.clone());
    Some(score_map(arc, task_emb))
}

pub(crate) fn score_map(map: Arc<EmbeddingMap>, task_emb: &[f32]) -> HashMap<String, f32> {
    map.iter()
        .map(|(key, emb)| {
            (
                key.clone(),
                aikoql_ingestion::cosine_similarity(task_emb, emb),
            )
        })
        .collect()
}

// A8: Change Reconciliation — git diff → affected entities → impact report.
pub(crate) fn tool_reconcile(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
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
    let report_json =
        serde_json::to_value(&report).map_err(|e| format!("serialize report: {}", e))?;
    Ok(serde_json::json!({
        "report": report_json,
        "koid": hex,
    }))
}

// A9: Connector Bridge — convert connector metadata into KnowledgeIr.
pub(crate) fn tool_connector_bridge(_k: &Kernel, args: &J) -> Result<J, String> {
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
                    // justified: absent fields array → empty field list
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
                            // justified: absent from_fields array → empty field list
                            .unwrap_or_default(),
                        to_container: r["to_container"].as_str().unwrap_or("?").to_string(),
                        to_fields: r["to_fields"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            // justified: absent to_fields array → empty field list
                            .unwrap_or_default(),
                        name: r["name"].as_str().map(String::from),
                    })
                    .collect()
            })
            // justified: absent references array → empty list
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
    let ir_json =
        serde_json::to_value(&ir).map_err(|e| format!("serialize knowledge_ir: {}", e))?;
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

pub(crate) fn get_ir_for_koid(
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
        let mut ir = if mime_type.contains("markdown")
            || mime_type == "text/md"
            || artifact_path.ends_with(".md")
        {
            let content = std::fs::read_to_string(&artifact_path)
                .map_err(|e| format!("read markdown: {}", e))?;
            aikoql_ingestion::compile_markdown_string(&content, Some(hex.to_string()))
                .map_err(|e| format!("markdown compile: {}", e))?
        } else if mime_type.contains("rust") || artifact_path.ends_with(".rs") {
            aikoql_ingestion::compile_rust_file(&artifact_path)
                .map_err(|e| format!("rust compile: {}", e))?
        } else {
            let asset_dir = format!("{}.assets", artifact_path);
            let doc =
                aikoql_ingestion::extract_document(&artifact_path, &mime_type, Some(&asset_dir))
                    .map_err(|e| format!("extract: {}", e))?;
            let cr =
                aikoql_ingestion::compile_document_mock_with_assets(&doc, &[], Some(&asset_dir));
            let cr_v = serde_json::to_value(&cr).map_err(|e| format!("serialize facts: {}", e))?;
            aikoql_ingestion::KnowledgeIr {
                facts: serde_json::from_value(cr_v).map_err(|e| format!("decode facts: {}", e))?,
                ..Default::default()
            }
        };
        // R8: the re-compiled IR inherits the document KO's trust level —
        // deploy_document stamps uploads Untrusted, so untrusted content
        // stays untrusted through the context pipeline.
        ir.content_trust = Some(ko.content_trust());
        return Ok(ir);
    }

    // Path 2: Direct KO with ir_json (from remember/ingest-dir) → deserialize.
    if let Some(Value::Text(ir_json)) = ko.properties.get("ir_json") {
        return serde_json::from_str(ir_json).map_err(|e| format!("deserialize ir_json: {}", e));
    }

    Err("KO has neither sha256 (document) nor ir_json (direct knowledge) — use document_ingest or ingest-dir first".into())
}

pub(crate) fn tool_explain_component(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let explanation = aikoql_ingestion::explain_component(name, &ir)
        .ok_or_else(|| format!("component '{}' not found", name))?;
    serde_json::to_value(&explanation).map_err(|e| format!("serialize explanation: {}", e))
}

pub(crate) fn tool_explain_decision(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let explanation = aikoql_ingestion::explain_decision(name, &ir)
        .ok_or_else(|| format!("decision '{}' not found", name))?;
    serde_json::to_value(&explanation).map_err(|e| format!("serialize explanation: {}", e))
}

pub(crate) fn tool_trace_requirement(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let req_id = args
        .get("requirement")
        .and_then(|v| v.as_str())
        .ok_or("missing: requirement")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let trace = aikoql_ingestion::trace_requirement(req_id, &ir);
    serde_json::to_value(&trace).map_err(|e| format!("serialize trace: {}", e))
}

pub(crate) fn tool_find_conflicts(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let component = args
        .get("component")
        .and_then(|v| v.as_str())
        .ok_or("missing: component")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let conflicts = aikoql_ingestion::find_conflicts(component, &ir);
    serde_json::to_value(&conflicts).map_err(|e| format!("serialize conflicts: {}", e))
}

pub(crate) fn tool_find_stale(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let ir = get_ir_for_koid(k, args, db_path)?;
    let report = aikoql_ingestion::find_stale_documentation(&ir);
    serde_json::to_value(&report).map_err(|e| format!("serialize report: {}", e))
}

pub(crate) fn tool_validate_change(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let description = args
        .get("change")
        .and_then(|v| v.as_str())
        .ok_or("missing: change")?;
    let ir = get_ir_for_koid(k, args, db_path)?;
    let validation = aikoql_ingestion::validate_change(description, &ir);
    serde_json::to_value(&validation).map_err(|e| format!("serialize validation: {}", e))
}

pub(crate) fn tool_propose_update(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
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
        // justified: absent new_facts param → empty list
        .unwrap_or_default();
    let remove_facts: Vec<String> = args
        .get("remove_facts")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        // justified: absent remove_facts param → empty list
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
        // justified: absent new_relations param → empty list
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
    serde_json::to_value(&proposal).map_err(|e| format!("serialize proposal: {}", e))
}

pub(crate) fn tool_filter_secrets(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let ir = get_ir_for_koid(k, args, db_path)?;
    let (_redacted, findings) = aikoql_ingestion::filter_secrets(&ir);
    serde_json::to_value(&findings).map_err(|e| format!("serialize findings: {}", e))
}

// ---- Agent Experience Improvements (MRFC-0040) -------------------------

pub(crate) fn tool_discover_schema(k: &Kernel) -> Result<J, String> {
    let types = k.list_types().map_err(|e| e.to_string())?;
    let heads = k.scan_heads().map_err(|e| e.to_string())?;
    let subject = Subject {
        name: "schema-discovery".into(),
        roles: vec!["admin".into()],
        tenant: None, // unscoped admin — full visibility
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
pub(crate) fn tool_discover_ontology(k: &Kernel) -> Result<J, String> {
    let subject = Subject {
        name: "ontology-discovery".into(),
        roles: vec!["admin".into()],
        tenant: None, // unscoped admin — full visibility
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
