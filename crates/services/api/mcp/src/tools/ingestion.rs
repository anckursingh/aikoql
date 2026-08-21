//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::*;

use crate::session::*;
pub(crate) fn tool_document_ingest(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
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
    let asset_dir = format!("{}.assets", artifact_path);
    let (page_count, char_count, status, ocr_stats) =
        match aikoql_ingestion::extract_document(&artifact_path, mime_type, Some(&asset_dir)) {
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

pub(crate) fn tool_document_list(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_document_status(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_document_compile(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
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
        let ir_json =
            serde_json::to_value(&ir).map_err(|e| format!("serialize markdown_ir: {}", e))?;
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
        let ir_json = serde_json::to_value(&ir).map_err(|e| format!("serialize code_ir: {}", e))?;
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
        let asset_dir = format!("{}.assets", artifact_path);
        let doc = aikoql_ingestion::extract_document(&artifact_path, &mime_type, Some(&asset_dir))
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
