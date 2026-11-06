//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::helpers::commit_ts_millis;
use crate::session::*;
use crate::{json, Kernel, KnowledgeContext, Value, J, KOID};
/// HLD §54/§55 ingestion limits, set once at startup from `[ingestion]`
/// (the CORE_PROFILE pattern); defaults when unset (tests, subcommands).
pub(crate) static INGEST_POLICY: std::sync::OnceLock<aikoql_ingestion::ProcessingPolicy> =
    std::sync::OnceLock::new();

fn ingest_policy() -> aikoql_ingestion::ProcessingPolicy {
    INGEST_POLICY.get().cloned().unwrap_or_default()
}

pub(crate) fn tool_document_ingest(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    document_ingest_with(k, args, db_path, &ingest_policy())
}

// ---- Over-limit documents: batch by batch, in the background -----------
//
// A document over `max_pages` is stored at once with status `processing`
// and read by a worker thread in batches of at most `max_pages` pages (the
// per-batch limits apply per batch). Progress counters live on the
// document KO; each batch's result is a line in `{hash}.batches.json` and
// its pages in `{hash}.batch-{first}-{last}.json` (compile merges those, so
// it never re-runs OCR and never hits the page limit again).

/// The prefix of the compile refusal while batches run (REST maps it to 409).
pub(crate) const STILL_PROCESSING: &str = "document still processing";
/// The prefix of the compile refusal after a restart cut processing short.
pub(crate) const INTERRUPTED: &str = "document processing was interrupted";

/// Artifacts with a worker running in this process (no second worker);
/// keyed by path, as KOIDs repeat across databases.
static RUNNING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// A worker's entry in RUNNING, released when dropped (a panic included).
struct Running(String);
impl Drop for Running {
    fn drop(&mut self) {
        // justified: Mutex poison is unrecoverable
        RUNNING.lock().unwrap().retain(|x| *x != self.0);
    }
}

fn ptext<'a>(p: &'a std::collections::BTreeMap<String, Value>, key: &str) -> &'a str {
    match p.get(key) {
        Some(Value::Text(s)) => s,
        _ => "",
    }
}

fn pint(p: &std::collections::BTreeMap<String, Value>, key: &str) -> i64 {
    match p.get(key) {
        Some(Value::Int(i)) => *i,
        _ => 0,
    }
}

fn batches_path(artifact_dir: &str, hash: &str) -> String {
    format!("{artifact_dir}/{hash}.batches.json")
}

fn batch_path(artifact_dir: &str, hash: &str, first: u64, last: u64) -> String {
    format!("{artifact_dir}/{hash}.batch-{first}-{last}.json")
}

/// The finished batches' results, in page order (empty when none yet).
fn read_batches(artifact_dir: &str, hash: &str) -> Vec<J> {
    std::fs::read(batches_path(artifact_dir, hash))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Pages and starting batch plan when a limit refused a multi-page document
/// (a batch still over a limit is split further by the worker).
fn batch_plan(
    ex: &Extracted,
    artifact_path: &str,
    mime_type: &str,
    policy: &aikoql_ingestion::ProcessingPolicy,
) -> Option<(usize, Vec<(u32, u32)>)> {
    if ex.status != "limit_exceeded" {
        return None;
    }
    let pages = aikoql_ingestion::document_page_count(artifact_path, mime_type).ok()?;
    (pages > 1).then(|| {
        (
            pages,
            aikoql_ingestion::plan_batches(pages, policy.max_pages),
        )
    })
}

/// What a document's status means, in words (`None` = nothing to add).
fn batch_message(p: &std::collections::BTreeMap<String, Value>, batches: &[J]) -> Option<String> {
    let (total, done) = (pint(p, "batches_total"), pint(p, "batches_done"));
    let pages = pint(p, "page_count");
    match ptext(p, "status") {
        "processing" => Some(format!(
            "This document is large ({pages} pages). It is being processed in {total} batches \
             of up to {} pages: {done} of {total} done ({} of {pages} pages). Please wait until \
             it finishes before compiling (a batch over a limit is split, so the count can grow).",
            pint(p, "batch_pages"),
            pint(p, "pages_done")
        )),
        "interrupted" => Some(format!(
            "Processing was interrupted by a server restart{}. Upload the file again to \
             finish it.",
            if total > 0 {
                format!(" after {done} of {total} batches")
            } else {
                String::new()
            }
        )),
        _ if total == 0 => None,
        "partial" => {
            let failed: Vec<String> = batches
                .iter()
                .filter(|b| b["status"] == "failed")
                .map(|b| format!("pages {}-{}", b["first_page"], b["last_page"]))
                .collect();
            Some(format!(
                "{} of {total} batches failed ({}); the other pages were read and compile \
                 covers them. Upload the file again to retry.",
                failed.len(),
                failed.join(", ")
            ))
        }
        _ => Some(format!("Processed in {total} batches ({pages} pages).")),
    }
}

/// Counters + message for a batched document, merged into `out`.
fn add_batch_fields(out: &mut J, p: &std::collections::BTreeMap<String, Value>, batches: &[J]) {
    if pint(p, "batches_total") > 0 {
        out["batches_total"] = json!(pint(p, "batches_total"));
        out["batches_done"] = json!(pint(p, "batches_done"));
        out["pages_done"] = json!(pint(p, "pages_done"));
    }
    if let Some(m) = batch_message(p, batches) {
        out["message"] = json!(m);
    }
}

/// Mark the document `processing` and start its worker (a no-op when one
/// already runs for it in this process).
#[allow(clippy::too_many_arguments)]
fn start_batches(
    k: &Kernel,
    koid: KOID,
    subject: crate::Subject,
    artifact_dir: &str,
    hash: &str,
    mime_type: &str,
    policy: &aikoql_ingestion::ProcessingPolicy,
    pages: usize,
    plan: Vec<(u32, u32)>,
) -> Result<J, String> {
    let key = format!("{artifact_dir}/{hash}");
    {
        // justified: Mutex poison is unrecoverable
        let mut running = RUNNING.lock().unwrap();
        if running.contains(&key) {
            return Err(format!("{STILL_PROCESSING}: its batches are running"));
        }
        running.push(key.clone());
    }
    let guard = Running(key);
    std::fs::remove_file(batches_path(artifact_dir, hash)).ok();
    let props = std::collections::BTreeMap::from([
        ("status".to_string(), Value::Text("processing".into())),
        ("page_count".to_string(), Value::Int(pages as i64)),
        ("char_count".to_string(), Value::Int(0)),
        ("batches_total".to_string(), Value::Int(plan.len() as i64)),
        ("batches_done".to_string(), Value::Int(0)),
        ("pages_done".to_string(), Value::Int(0)),
        (
            "batch_pages".to_string(),
            Value::Int(policy.max_pages as i64),
        ),
    ]);
    k.set_document_properties(&koid, props.clone(), &subject)
        .map_err(|e| e.to_string())?;
    let mut out = json!({ "page_count": pages, "char_count": 0, "status": "processing" });
    add_batch_fields(&mut out, &props, &[]);
    let (k, dir, hash, mime, policy) = (
        k.clone(),
        artifact_dir.to_string(),
        hash.to_string(),
        mime_type.to_string(),
        policy.clone(),
    );
    std::thread::spawn(move || {
        run_batches(
            &k, &koid, &subject, &dir, &hash, &mime, &policy, &plan, guard,
        )
    });
    Ok(out)
}

/// Pipe tables on a page: runs of `|` lines.
fn table_count(doc: &aikoql_ingestion::DocumentModel) -> usize {
    doc.pages
        .iter()
        .map(|p| {
            let rows: Vec<bool> = p
                .text
                .lines()
                .map(|l| l.trim_start().starts_with('|'))
                .collect();
            (0..rows.len())
                .filter(|&i| rows[i] && (i == 0 || !rows[i - 1]))
                .count()
        })
        .sum()
}

/// The worker: every batch in page order; a failed batch is recorded and
/// the rest still run, so the document always ends in a final status.
#[allow(clippy::too_many_arguments)]
fn run_batches(
    k: &Kernel,
    koid: &KOID,
    subject: &crate::Subject,
    dir: &str,
    hash: &str,
    mime: &str,
    policy: &aikoql_ingestion::ProcessingPolicy,
    plan: &[(u32, u32)],
    guard: Running,
) {
    let mut guard = Some(guard);
    let path = format!("{dir}/{hash}");
    let asset_dir = format!("{path}.assets");
    let mut results: Vec<J> = Vec::with_capacity(plan.len());
    let mut docs = Vec::new();
    let mut queue: std::collections::VecDeque<(u32, u32)> = plan.iter().copied().collect();
    let mut total = plan.len();
    while let Some((first, last)) = queue.pop_front() {
        let file = batch_path(dir, hash, first.into(), last.into());
        let r = std::panic::catch_unwind(|| {
            aikoql_ingestion::extract_document_pages(
                &path,
                mime,
                Some(&asset_dir),
                policy,
                first,
                last,
            )
        })
        .unwrap_or_else(|_| Err("extraction panicked".into()))
        .and_then(|doc| {
            let bytes = serde_json::to_vec(&doc).map_err(|e| format!("serialize batch: {e}"))?;
            std::fs::write(&file, bytes).map_err(|e| format!("write batch: {e}"))?;
            Ok(doc)
        });
        // over a limit with more than one page: split it in two, in order,
        // so only the pages that cannot fit fail
        if matches!(&r, Err(e) if aikoql_ingestion::is_limit_error(e)) && last > first {
            let mid = first + (last - first) / 2;
            queue.push_front((mid + 1, last));
            queue.push_front((first, mid));
            total += 1;
            let props = [("batches_total".to_string(), Value::Int(total as i64))].into();
            if let Err(e) = k.set_document_properties(koid, props, subject) {
                tracing::warn!("batch progress of {hash}: {e}");
            }
            continue;
        }
        results.push(match r {
            Ok(doc) => {
                let entry = json!({
                    "first_page": first, "last_page": last,
                    "status": doc.ocr_stats.as_ref().map_or("extracted", |s| s.status()),
                    "pages": doc.pages.len(),
                    "char_count": doc.total_chars,
                    "images": doc.pages.iter().map(|p| p.images.len()).sum::<usize>(),
                    "tables": table_count(&doc),
                });
                docs.push(doc);
                entry
            }
            Err(e) => {
                tracing::warn!("batch {first}-{last} of {hash} failed: {e}");
                json!({ "first_page": first, "last_page": last, "status": "failed", "error": e })
            }
        });
        if let Err(e) = std::fs::write(
            batches_path(dir, hash),
            serde_json::to_vec(&results).unwrap_or_default(),
        ) {
            tracing::warn!("batch results of {hash}: {e}");
        }
        let merged_chars: usize = docs.iter().map(|d| d.total_chars).sum();
        let mut props = std::collections::BTreeMap::from([
            ("batches_done".to_string(), Value::Int(results.len() as i64)),
            ("pages_done".to_string(), Value::Int(last as i64)),
            ("char_count".to_string(), Value::Int(merged_chars as i64)),
        ]);
        if queue.is_empty() {
            let failed = results.iter().any(|b| b["status"] == "failed");
            let merged = aikoql_ingestion::merge_batches(std::mem::take(&mut docs));
            let text: Vec<String> = merged
                .pages
                .iter()
                .map(|p| format!("--- Page {} [{}] ---\n{}", p.page_number, p.source, p.text))
                .collect();
            std::fs::write(format!("{dir}/{hash}.extracted.txt"), text.join("\n\n")).ok();
            let status = if failed {
                "partial"
            } else {
                merged
                    .ocr_stats
                    .as_ref()
                    .map_or("extracted", |s| s.status())
            };
            // the status flips last: every file above is in place first,
            // and a re-upload seeing the final status may start a worker
            props.insert("status".into(), Value::Text(status.into()));
            drop(guard.take());
        }
        if let Err(e) = k.set_document_properties(koid, props, subject) {
            tracing::warn!("batch progress of {hash}: {e}");
        }
    }
}

/// Documents a restart left `processing`, marked `interrupted`; the count.
/// Runs at startup on a node that writes, before any worker can start.
pub(crate) fn interrupt_unfinished_documents(k: &Kernel) -> usize {
    let system = crate::Subject::with_roles("aikoql-system", &["admin"]);
    let Ok(docs) = k.list_documents(&system) else {
        return 0;
    };
    let mut n = 0;
    for doc in docs
        .iter()
        .filter(|d| ptext(&d.properties, "status") == "processing")
    {
        let props = [("status".to_string(), Value::Text("interrupted".into()))].into();
        match k.set_document_properties(&doc.koid, props, &system) {
            Ok(_) => n += 1,
            Err(e) => tracing::warn!("document {}: {e}", doc.koid.to_hex()),
        }
    }
    n
}

/// `document_ingest` under an explicit policy (tests; the tool reads `[ingestion]`).
pub(crate) fn document_ingest_with(
    k: &Kernel,
    args: &J,
    db_path: &str,
    policy: &aikoql_ingestion::ProcessingPolicy,
) -> Result<J, String> {
    let filename = args
        .get("filename")
        .and_then(|v| v.as_str())
        .ok_or("missing: filename")?;
    let content_b64 = args
        .get("content_base64")
        .and_then(|v| v.as_str())
        .ok_or("missing: content_base64")?;
    // A known extension decides: browsers send "", octet-stream or legacy
    // types (Windows: application/vnd.ms-excel for .csv) for many formats.
    // An unknown extension falls back to the client's mime type.
    let by_extension = aikoql_ingestion::mime_from_extension(filename);
    let mime_type = match args.get("mime_type").and_then(|v| v.as_str()) {
        _ if by_extension != "application/octet-stream" => by_extension,
        Some(m) if !m.is_empty() => m.to_string(),
        _ => by_extension,
    };
    let mime_type = mime_type.as_str();
    // An image no extractor reads would only fail later, at compile: say so now.
    if mime_type.starts_with("image/") && !aikoql_ingestion::is_raster_image(mime_type) {
        return Err(format!(
            "unsupported mime type: {mime_type} — convert the image to PNG or JPEG"
        ));
    }

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

    // Store artifact on disk.
    let artifact_dir = format!("{}.artifacts", db_path);
    let artifact_path = format!("{}/{}", artifact_dir, hash);
    let store_artifact = || -> Result<(), String> {
        std::fs::create_dir_all(&artifact_dir).map_err(|e| format!("artifact dir: {}", e))?;
        if !std::path::Path::new(&artifact_path).exists() {
            std::fs::write(&artifact_path, &bytes).map_err(|e| format!("write artifact: {}", e))?;
        }
        Ok(())
    };

    // Dedup: if document with this hash already exists, return it.
    if let Ok(docs) = k.list_documents(&subject_of(args)) {
        for doc in &docs {
            if ptext(&doc.properties, "sha256") != hash {
                continue;
            }
            let was = ptext(&doc.properties, "status");
            let mut resp = json!({
                "koid": doc.koid.to_hex(),
                "sha256": hash,
                "size_bytes": size_bytes,
                "duplicate": true,
            });
            if was == "processing" {
                resp["status"] = json!(was);
                resp["page_count"] = json!(pint(&doc.properties, "page_count"));
                add_batch_fields(&mut resp, &doc.properties, &[]);
                return Ok(resp);
            }
            // Ingested while OCR was missing or failing, over a limit, or
            // cut short: try again, so a re-upload after installing
            // Tesseract (or raising a limit, or a restart) picks it up.
            let stale = matches!(
                was,
                "ocr_unavailable" | "ocr_partial" | "limit_exceeded" | "interrupted" | "partial"
            );
            if stale {
                store_artifact()?;
                let ex = extract_for_ingest_with(
                    &artifact_dir,
                    &artifact_path,
                    &hash,
                    filename,
                    mime_type,
                    policy,
                );
                if let Some((pages, plan)) = batch_plan(&ex, &artifact_path, mime_type, policy) {
                    let started = start_batches(
                        k,
                        doc.koid,
                        subject_of(args),
                        &artifact_dir,
                        &hash,
                        mime_type,
                        policy,
                        pages,
                        plan,
                    )?;
                    merge_json(&mut resp, started);
                    return Ok(resp);
                }
                if ex.status != was {
                    k.set_document_properties(
                        &doc.koid,
                        std::collections::BTreeMap::from([
                            ("page_count".to_string(), Value::Int(ex.pages)),
                            ("char_count".to_string(), Value::Int(ex.chars)),
                            ("status".to_string(), Value::Text(ex.status.into())),
                            // read in one pass now: no batches any more
                            ("batches_total".to_string(), Value::Int(0)),
                        ]),
                        &subject_of(args),
                    )
                    .map_err(|e| e.to_string())?;
                    merge_json(
                        &mut resp,
                        json!({
                            "page_count": ex.pages,
                            "char_count": ex.chars,
                            "status": ex.status,
                            "message": "Document already exists; its extraction was refreshed"
                        }),
                    );
                    if let Some(stats) = ex.ocr_stats {
                        resp["ocr_stats"] = stats;
                    }
                    return Ok(resp);
                }
            }
            return Ok(json!({
                "koid": doc.koid.to_hex(),
                "sha256": hash,
                "size_bytes": size_bytes,
                "status": "duplicate",
                "message": "Document with this content already exists"
            }));
        }
    }

    store_artifact()?;
    let ex = extract_for_ingest_with(
        &artifact_dir,
        &artifact_path,
        &hash,
        filename,
        mime_type,
        policy,
    );
    let plan = batch_plan(&ex, &artifact_path, mime_type, policy);

    let r = k
        .deploy_document(
            filename,
            mime_type,
            &hash,
            size_bytes,
            ex.pages,
            ex.chars,
            if plan.is_some() {
                "processing"
            } else {
                ex.status
            },
            &subject_of(args),
        )
        .map_err(|e| e.to_string())?;

    let mut resp = json!({
        "koid": r.koid.to_hex(),
        "sha256": hash,
        "size_bytes": size_bytes,
        "page_count": ex.pages,
        "char_count": ex.chars,
        "status": ex.status
    });
    if let Some((pages, plan)) = plan {
        let started = start_batches(
            k,
            r.koid,
            subject_of(args),
            &artifact_dir,
            &hash,
            mime_type,
            policy,
            pages,
            plan,
        )?;
        merge_json(&mut resp, started);
        return Ok(resp);
    }
    if let Some(ref stats) = ex.ocr_stats {
        resp["ocr_stats"] = stats.clone();
    }
    if let Some(e) = ex.error {
        resp["error"] = json!(e);
    }
    Ok(resp)
}

fn merge_json(into: &mut J, from: J) {
    if let (Some(a), J::Object(b)) = (into.as_object_mut(), from) {
        a.extend(b);
    }
}

/// Outcome of extraction at ingest. `error` is set for a policy limit
/// (status `limit_exceeded`): the document is stored, the reason reported.
pub(crate) struct Extracted {
    pub pages: i64,
    pub chars: i64,
    pub status: &'static str,
    pub ocr_stats: Option<J>,
    pub error: Option<String>,
}

/// D1/D2: extract text from the stored artifact under `policy`.
/// Extraction failure still ingests the document.
pub(crate) fn extract_for_ingest_with(
    artifact_dir: &str,
    artifact_path: &str,
    hash: &str,
    filename: &str,
    mime_type: &str,
    policy: &aikoql_ingestion::ProcessingPolicy,
) -> Extracted {
    let asset_dir = format!("{}.assets", artifact_path);
    match aikoql_ingestion::extract_document_with_policy(
        artifact_path,
        mime_type,
        Some(&asset_dir),
        policy,
    ) {
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
            let ocr_stats = doc.ocr_stats.as_ref().map(|s| {
                json!({
                    "pages_ocr_attempted": s.pages_ocr_attempted,
                    "pages_ocr_succeeded": s.pages_ocr_succeeded,
                    "pages_ocr_failed": s.pages_ocr_failed,
                    "average_confidence": s.average_confidence,
                })
            });
            Extracted {
                pages: doc.page_count as i64,
                chars: doc.total_chars as i64,
                status,
                ocr_stats,
                error: None,
            }
        }
        Err(e) if aikoql_ingestion::is_limit_error(&e) => {
            tracing::warn!("extraction refused for {}: {}", filename, e);
            Extracted {
                pages: 0,
                chars: 0,
                status: "limit_exceeded",
                ocr_stats: None,
                error: Some(e),
            }
        }
        Err(e) => {
            // Unsupported format or extraction failed — still ingest the doc.
            tracing::warn!("extraction skipped for {}: {}", filename, e);
            Extracted {
                pages: 0,
                chars: 0,
                status: "ingested",
                ocr_stats: None,
                error: None,
            }
        }
    }
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
            "char_count": d.properties.get("char_count").and_then(|v| match v { Value::Int(i) => Some(*i), _ => None }).unwrap_or(0),
            "status": d.properties.get("status").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }).unwrap_or("?"),
            "lifecycle": d.lifecycle.state.to_string(),
        })).zip(&docs).map(|(mut row, d)| {
            if pint(&d.properties, "batches_total") > 0 {
                row["page_count"] = json!(pint(&d.properties, "page_count"));
            }
            add_batch_fields(&mut row, &d.properties, &[]);
            row
        }).collect::<Vec<_>>()
    }))
}

pub(crate) fn tool_document_status(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(hex).map_err(|e| e.to_string())?;
    let ctx = KnowledgeContext::from(subject_of(args));
    let ko = k.get(ctx, &koid).map_err(|e| e.to_string())?;
    let batches = if pint(&ko.properties, "batches_total") > 0 {
        read_batches(
            &format!("{db_path}.artifacts"),
            ptext(&ko.properties, "sha256"),
        )
    } else {
        Vec::new()
    };
    let mut out = json!({
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
        "commit_ts": commit_ts_millis(ko.commit_ts),
    });
    add_batch_fields(&mut out, &ko.properties, &batches);
    if pint(&ko.properties, "batches_total") > 0 {
        out["batches"] = json!(batches);
    }
    Ok(out)
}

pub(crate) fn tool_document_compile(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let hex = args
        .get("koid")
        .and_then(|v| v.as_str())
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(hex).map_err(|e| e.to_string())?;
    let ctx = KnowledgeContext::from(subject_of(args));
    let ko = k.get(ctx, &koid).map_err(|e| e.to_string())?;
    let (total, done) = (
        pint(&ko.properties, "batches_total"),
        pint(&ko.properties, "batches_done"),
    );
    match ptext(&ko.properties, "status") {
        "processing" => {
            return Err(format!(
                "{STILL_PROCESSING}: batch {} of {total} is running ({done} done) — wait until \
                 document_status says it has finished, then compile",
                (done + 1).min(total.max(1))
            ))
        }
        "interrupted" => {
            return Err(format!(
                "{INTERRUPTED} by a server restart after {done} of {total} batches — upload the \
                 file again to finish it, then compile"
            ))
        }
        _ => {}
    }

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
        let policy = ingest_policy();
        // A batched document compiles from its stored batch results.
        let batches = (total > 0).then(|| read_batches(&format!("{db_path}.artifacts"), &sha256));
        let doc = match &batches {
            Some(batches) => {
                let mut docs = Vec::new();
                for b in batches.iter().filter(|b| b["status"] != "failed") {
                    let (first, last) = (b["first_page"].as_u64(), b["last_page"].as_u64());
                    let file = batch_path(
                        &format!("{db_path}.artifacts"),
                        &sha256,
                        first.unwrap_or(0),
                        last.unwrap_or(0),
                    );
                    let bytes = std::fs::read(&file)
                        .map_err(|e| format!("batch result {file} unreadable: {e}"))?;
                    docs.push(
                        serde_json::from_slice(&bytes)
                            .map_err(|e| format!("batch result {file}: {e}"))?,
                    );
                }
                aikoql_ingestion::merge_batches(docs)
            }
            None => aikoql_ingestion::extract_document_with_policy(
                &artifact_path,
                &mime_type,
                Some(&asset_dir),
                &policy,
            )
            .map_err(|e| format!("extract for compile: {}", e))?,
        };
        let cr = aikoql_ingestion::compile_document_mock_with_policy(
            &doc,
            &[],
            Some(&asset_dir),
            &policy,
        );
        let mut v = serde_json::to_value(&cr).map_err(|e| format!("serialize: {}", e))?;
        // lets a client say why a compile came back empty (e.g. no OCR tool)
        v["extraction_status"] = json!(doc.ocr_stats.as_ref().map_or("extracted", |s| s.status()));
        v["total_chars"] = json!(doc.total_chars);
        if let Some(batches) = batches {
            let failed: Vec<&J> = batches.iter().filter(|b| b["status"] == "failed").collect();
            v["failed_batches"] = json!(failed);
            v["batches"] = json!(batches);
        }
        v
    };

    // Attach document metadata
    if let Some(obj) = result.as_object_mut() {
        obj.insert("koid".into(), serde_json::Value::String(hex.to_string()));
        obj.insert("mime_type".into(), serde_json::Value::String(mime_type));
    }

    Ok(result)
}

// ---- Context Compiler (MRFC-0070 Phase A6) ---------------------------
