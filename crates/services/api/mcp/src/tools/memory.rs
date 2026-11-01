//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::helpers::*;
use crate::session::*;
use crate::{
    json, ExtensionMap, Kernel, Metadata, Origin, PropertyMap, ReferentialPolicy, RememberRequest,
    SecurityDescriptor, Value, J, MEMORY_DIR,
};
use aikoql_kernel::kernel::{mmr_rerank, MMR_LAMBDA};
pub(crate) fn tool_agent_memory(kernel: &Kernel, args: &J) -> Result<J, String> {
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
    // v0.3 K5: TTL is now enforced on the read path — a memory whose
    // commit_ts + ttl has passed is dropped from the result, never returned
    // as if it were still live. commit_ts packs HLC millis in the high bits.
    let now = kernel.clock_now();
    let mut expired_dropped = 0usize;
    let memories: Vec<J> = all.iter()
        .filter(|ko| ko.properties.get("agent_id") == Some(&Value::Text(agent_id.to_string())))
        .filter(|ko| {
            let ttl_ms = ko
                .properties
                .get("ttl")
                .and_then(|v| match v {
                    Value::Int(i) => Some((*i).max(0) as u64),
                    _ => None,
                })
                .unwrap_or(3600)
                * 1000;
            let alive = (ko.commit_ts >> 16) + ttl_ms > now;
            if !alive {
                expired_dropped += 1;
            }
            alive
        })
        .map(|ko| json!({
            "koid": ko.koid.to_hex(),
            "key": ko.properties.get("key").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }),
            "value": ko.properties.get("value").map(value_to_json),
            "ttl": ko.properties.get("ttl").and_then(|v| match v { Value::Int(i) => Some(i), _ => None }),
        }))
        .collect();
    Ok(json!({
        "memories": memories,
        "count": memories.len(),
        "expired_dropped": expired_dropped
    }))
}

// ---- Memory Tools (MRFC-0070) ------------------------------------------

pub(crate) fn resolve_memory_dir(args: &J) -> String {
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

pub(crate) fn parse_memory_frontmatter(raw: &str) -> Option<(String, String, String, String)> {
    // Parse YAML frontmatter between --- delimiters.
    // Returns (name, description, type, trust).
    let body = raw.strip_prefix("---\n")?;
    let (front, _rest) = body.split_once("\n---")?;
    let mut name = String::new();
    let mut desc = String::new();
    let mut mtype = String::new();
    let mut trust = String::new();
    for line in front.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("name:") {
            name = v.trim().trim_matches('"').to_string();
        } else if let Some(v) = trimmed.strip_prefix("description:") {
            desc = v.trim().trim_matches('"').to_string();
        } else if let Some(v) = trimmed.strip_prefix("type:") {
            // may appear under metadata: block
            mtype = v.trim().trim_matches('"').to_string();
        } else if let Some(v) = trimmed.strip_prefix("trust:") {
            // AG-33: may appear under metadata: block
            trust = v.trim().trim_matches('"').to_string();
        }
    }
    if name.is_empty() {
        None
    } else {
        Some((name, desc, mtype, trust))
    }
}

/// The file search, for an explicit `memory_dir` (a directory the server
/// does not own, so not in its knowledge base).
fn search_memory_files(args: &J) -> Result<J, String> {
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
    let fence = Fence::new();
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
        let (name, desc, _mtype, trust) = parse_memory_frontmatter(&raw).unwrap_or_else(|| {
            (
                fname.to_string(),
                String::new(),
                String::new(),
                String::new(),
            )
        });
        let trust = if trust.is_empty() {
            "undeclared"
        } else {
            &trust
        };
        // AG-33: a file without a declared trust class is untrusted data
        let wrap = |s: String| {
            if trust_is_untrusted(trust) {
                fence.wrap(&s)
            } else {
                s
            }
        };

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
                "name": wrap(name.clone()),
                "description": wrap(desc.clone()),
                "file": wrap(path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string()),
                "snippet": wrap(snippet.clone()),
                "score": score,
                "trust": trust,
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

    let mut out = json!({"results": results, "count": results.len(), "query": query});
    out["fence"] = fence.json();
    Ok(out)
}

/// AG-10: the KO type a memory is stored as.
const MEMORY_TYPE: &str = "Memory";

/// The caller's own current Memory KO named `name` (type scans skip
/// deleted objects; another principal's shared memory is readable, never
/// the caller's to write).
// shortcut: a scan of the Memory KOs per call, add a name index when a store
// holds tens of thousands of memories
fn memory_ko(k: &Kernel, args: &J, name: &str) -> Result<Option<crate::KnowledgeObject>, String> {
    let subject = subject_of(args);
    Ok(k.scan_by_type(&subject, MEMORY_TYPE)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|ko| ko.security.owner == subject.name && text_prop(ko, "name") == name))
}

/// Whether `name` is a memory the caller can read but does not own (a shared
/// one) — the caller may not edit it, not even through its file.
fn someone_elses(k: &Kernel, args: &J, name: &str) -> Result<bool, String> {
    let subject = subject_of(args);
    Ok(k.scan_by_type(&subject, MEMORY_TYPE)
        .map_err(|e| e.to_string())?
        .iter()
        .any(|ko| ko.security.owner != subject.name && text_prop(ko, "name") == name))
}

/// AG-11: a session-scoped memory is seen only from its own session
/// (run_id); every other object passes.
pub(crate) fn in_this_session(ko: &crate::KnowledgeObject, args: &J) -> bool {
    if ko.metadata.type_name != MEMORY_TYPE || text_prop(ko, "scope") != "session" {
        return true;
    }
    args.get("run_id").and_then(|r| r.as_str()) == Some(text_prop(ko, "session").as_str())
}

/// AG-11: the memory scope, validated.
fn scope_arg(args: &J) -> Result<Option<&str>, String> {
    match args.get("scope").and_then(|v| v.as_str()) {
        None => Ok(None),
        Some(s @ ("private" | "shared" | "session")) => Ok(Some(s)),
        Some(other) => Err(format!(
            "scope must be private, shared or session, got {other:?}"
        )),
    }
}

fn text_prop(ko: &crate::KnowledgeObject, key: &str) -> String {
    match ko.properties.get(key) {
        Some(Value::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

/// A memory file's body: everything after the closing frontmatter `---`.
fn memory_body(raw: &str) -> String {
    raw.strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---"))
        .map(|(_, after)| after.trim().to_string())
        .unwrap_or_else(|| raw.trim().to_string())
}

/// Upsert the memory into the knowledge base: a new KO, a new version when
/// something changed, nothing when it did not. A new memory is private
/// unless a scope is given; an existing one keeps its scope. AG-33: the
/// trust class is the declared source, else the existing class, else
/// undeclared (untrusted). Returns {koid, version, scope, trust}.
fn mirror(
    k: &Kernel,
    args: &J,
    name: &str,
    description: &str,
    content: &str,
    mtype: &str,
    scope: Option<&str>,
    trust: Option<&str>,
) -> Result<J, String> {
    let existing = memory_ko(k, args, name)?;
    let trust = trust
        .map(str::to_string)
        .filter(|t| !t.is_empty())
        .or_else(|| existing.as_ref().map(|ko| ko.trust_class().to_string()))
        .unwrap_or_else(|| "undeclared".into());
    let scope = scope
        .map(String::from)
        .or_else(|| {
            existing
                .as_ref()
                .map(|ko| text_prop(ko, "scope"))
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "private".into());
    let session = if scope == "session" {
        args.get("run_id")
            .and_then(|r| r.as_str())
            .ok_or("scope session needs a session: call session_init with a run_id first")?
            .to_string()
    } else {
        String::new()
    };
    let props = json!({"name": name, "description": description, "content": content,
        "memory_type": mtype, "scope": scope, "session": session});
    let mut req = args.clone();
    if let Some(ko) = &existing {
        let same = ["description", "content", "memory_type", "scope", "session"]
            .iter()
            .all(|key| text_prop(ko, key) == props[*key].as_str().unwrap_or(""))
            && ko.trust_class() == trust;
        if same {
            return Ok(json!({
                "koid": ko.koid.to_hex(), "version": ko.version, "scope": scope,
                "trust": trust,
            }));
        }
        req["koid"] = json!(ko.koid.to_hex());
        req["expected_version"] = json!(ko.version);
    }
    req["type_name"] = json!(MEMORY_TYPE);
    req["properties"] = props;
    req["trust"] = json!(trust);
    // AG-61: embed at write time — an agent searches what it just stored,
    // and the semantic engine attaches embeddings later (ms001). Without a
    // model, or while it stays busy, the asynchronous path still runs.
    // shortcut: the embedding carries no model name (indexed under the ""
    // model, which memory_search matches); thread the provider's model id
    // through the kernel if a caller ever scopes memory search by model
    let text = if content.starts_with(description) {
        content.to_string()
    } else {
        format!("{description}\n{content}")
    };
    if let (Some(v), _) = crate::tools::query::embed_query(k, &text) {
        req["semantic"] = json!({ "embedding": v });
    }
    req["embed"] = json!(true);
    req["visibility"] = json!(if scope == "shared" {
        "shared"
    } else {
        "private"
    });
    let r = crate::tools::tool_remember(k, &req)?;
    Ok(json!({"koid": r["koid"], "version": r["version"], "scope": scope, "trust": trust}))
}

/// Write the memory's file view and its MEMORY.md index line.
fn write_memory_file(
    dir: &str,
    name: &str,
    description: &str,
    mtype: &str,
    content: &str,
    trust: &str,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("cannot create memory dir '{}': {}", dir, e))?;
    let filename = format!("{}.md", name);
    let filepath = std::path::PathBuf::from(dir).join(&filename);
    let frontmatter =
        format!(
        "---\nname: {}\ndescription: \"{}\"\nmetadata:\n  type: {}\n  trust: {}\n  modified: {}\n---\n\n{}\n",
        name, description, mtype, trust, system_time_iso8601(), content
    );
    std::fs::write(&filepath, &frontmatter)
        .map_err(|e| format!("cannot write memory '{}': {}", filepath.display(), e))?;
    let index_path = std::path::PathBuf::from(dir).join("MEMORY.md");
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
    Ok(filepath)
}

fn slug(args: &J) -> Result<&str, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    if name.is_empty()
        || name.contains(char::is_whitespace)
        || name.contains('\\')
        || name.contains('/')
    {
        return Err(
            "name must be a kebab-case slug (no whitespace, slashes, or backslashes)".into(),
        );
    }
    Ok(name)
}

/// memory_search: the knowledge base's Memory KOs (semantic when the model
/// is loaded), in the file search's shape. An explicit memory_dir names a
/// directory outside the knowledge base: that one is searched as files.
pub(crate) fn tool_memory_search(k: &Kernel, args: &J) -> Result<J, String> {
    if args.get("memory_dir").is_some() {
        return search_memory_files(args);
    }
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing: query")?;
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;
    // a memory is searched right after it is stored: wait briefly for the index
    crate::tools::query::wait_fresh(
        k,
        args.get("wait_for_freshness_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(1000),
    );
    let (vector, _) = crate::tools::query::embed_query(k, query);
    let semantic = vector.is_some();
    let hits = k
        .find_similar(crate::SimilarityQuery {
            context: subject_of(args).into(),
            filter: None,
            text: Some(query.to_string()),
            vector,
            embedding_model: None,
            k: max_results * 3,
            fusion: if semantic {
                crate::Fusion::VectorOnly
            } else {
                crate::Fusion::TextOnly
            },
        })
        .map_err(|e| e.to_string())?;
    // AG-33: agents may filter by trust class; untrusted content is fenced
    let trust_want = args.get("trust").and_then(|v| v.as_str());
    let fence = Fence::new();
    // AG-22: diversify the oversampled candidates before the cut — filter
    // first (a dropped hit must not eat a diversity slot), then MMR.
    let hits = mmr_rerank(
        hits.into_iter()
            .filter(|h| {
                h.score > 0.0
                    && h.ko.metadata.type_name == MEMORY_TYPE
                    && in_this_session(&h.ko, args)
                    && trust_want.is_none_or(|w| h.ko.trust_class() == w)
            })
            .collect(),
        max_results,
        MMR_LAMBDA,
    );
    let results: Vec<J> = hits
        .iter()
        .map(|h| {
            let name = text_prop(&h.ko, "name");
            let content = text_prop(&h.ko, "content");
            let trust = h.ko.trust_class();
            let wrap = |s: String| {
                if trust_is_untrusted(trust) {
                    fence.wrap(&s)
                } else {
                    s
                }
            };
            json!({
                "name": wrap(name.clone()),
                "description": wrap(text_prop(&h.ko, "description")),
                "file": wrap(format!("{name}.md")),
                "snippet": wrap(content.chars().take(200).collect()),
                "score": h.score,
                "koid": h.ko.koid.to_hex(),
                "scope": text_prop(&h.ko, "scope"),
                "owner": h.ko.security.owner,
                "trust": trust,
            })
        })
        .collect();
    let mut out = json!({"results": results, "count": results.len(), "query": query});
    out["fence"] = fence.json();
    Ok(out)
}

pub(crate) fn tool_memory_store(k: &Kernel, args: &J) -> Result<J, String> {
    let name = slug(args)?;
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
    // the knowledge base first: a file never claims a memory it does not hold
    let ko = mirror(
        k,
        args,
        name,
        description,
        content,
        mtype,
        scope_arg(args)?,
        args.get("trust").and_then(|v| v.as_str()),
    )?;
    let trust = ko["trust"].as_str().unwrap_or("undeclared");
    let filepath = write_memory_file(
        &resolve_memory_dir(args),
        name,
        description,
        mtype,
        content,
        trust,
    )?;
    Ok(json!({
        "stored": true,
        "name": name,
        "file": format!("{name}.md"),
        "path": filepath.to_string_lossy(),
        "koid": ko["koid"],
        "version": ko["version"],
        "scope": ko["scope"],
    }))
}

/// memory_update: only the given fields change; the current values come
/// from the knowledge base (else the file, for a not-yet-imported one).
pub(crate) fn tool_memory_update(k: &Kernel, args: &J) -> Result<J, String> {
    let name = slug(args)?;
    let dir = resolve_memory_dir(args);
    let filepath = std::path::PathBuf::from(&dir).join(format!("{name}.md"));
    let (cur_desc, cur_content, cur_type, cur_trust) = match memory_ko(k, args, name)? {
        Some(ko) => (
            text_prop(&ko, "description"),
            text_prop(&ko, "content"),
            text_prop(&ko, "memory_type"),
            args.get("trust").and_then(|v| v.as_str()).map(String::from),
        ),
        None if filepath.exists() && !someone_elses(k, args, name)? => {
            let raw = std::fs::read_to_string(&filepath)
                .map_err(|e| format!("cannot read '{}': {}", filepath.display(), e))?;
            let (_, desc, mtype, trust) = parse_memory_frontmatter(&raw).unwrap_or_default();
            (
                desc,
                memory_body(&raw),
                mtype,
                if trust.is_empty() { None } else { Some(trust) },
            )
        }
        None => return Err(format!("memory '{}' not found among your memories", name)),
    };
    let pick = |key: &str, cur: String| {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(cur)
    };
    let (desc, content) = (pick("description", cur_desc), pick("content", cur_content));
    let mtype = pick(
        "type",
        if cur_type.is_empty() {
            "project".into()
        } else {
            cur_type
        },
    );
    let ko = mirror(
        k,
        args,
        name,
        &desc,
        &content,
        &mtype,
        scope_arg(args)?,
        cur_trust.as_deref(),
    )?;
    let trust = ko["trust"].as_str().unwrap_or("undeclared");
    write_memory_file(&dir, name, &desc, &mtype, &content, trust)?;
    Ok(json!({
        "updated": true,
        "name": name,
        "file": format!("{name}.md"),
        "path": filepath.to_string_lossy(),
        "koid": ko["koid"],
        "version": ko["version"],
        "scope": ko["scope"],
    }))
}

pub(crate) fn tool_memory_delete(k: &Kernel, args: &J) -> Result<J, String> {
    let name = slug(args)?;
    let dir = resolve_memory_dir(args);
    let filename = format!("{}.md", name);
    let filepath = std::path::PathBuf::from(&dir).join(&filename);
    let ko = memory_ko(k, args, name)?;
    if ko.is_none() && !filepath.exists() {
        return Err(format!(
            "memory '{}' not found at {}",
            name,
            filepath.display()
        ));
    }
    let mut description = String::new();
    if let Some(ko) = &ko {
        description = text_prop(ko, "description");
        let mut req = args.clone();
        req["koid"] = json!(ko.koid.to_hex());
        req["mode"] = json!("tombstone");
        crate::tools::tool_forget(k, &req)?;
    }
    if filepath.exists() {
        let raw = std::fs::read_to_string(&filepath)
            .map_err(|e| format!("cannot read '{}': {}", filepath.display(), e))?;
        if description.is_empty() {
            description = parse_memory_frontmatter(&raw)
                .map(|(_, d, _, _)| d)
                .unwrap_or_default();
        }
        std::fs::remove_file(&filepath)
            .map_err(|e| format!("cannot delete '{}': {}", filepath.display(), e))?;
    }
    let index_path = std::path::PathBuf::from(&dir).join("MEMORY.md");
    if index_path.exists() {
        let existing = std::fs::read_to_string(&index_path)
            .map_err(|e| format!("cannot read MEMORY.md: {}", e))?;
        let cleaned: String = existing
            .lines()
            .filter(|l| !l.contains(&filename))
            .collect::<Vec<_>>()
            .join("\n");
        let cleaned = if cleaned.is_empty() {
            cleaned
        } else {
            format!("{}\n", cleaned.trim_end())
        };
        std::fs::write(&index_path, &cleaned)
            .map_err(|e| format!("cannot update MEMORY.md: {}", e))?;
    }
    Ok(json!({"deleted": true, "name": name, "file": filename, "description": description}))
}

/// AG-10: import a memory directory into the knowledge base once — a file
/// whose memory is unchanged adds nothing, a changed one a new version.
/// Returns how many memories were created or updated.
pub(crate) fn import_memory_dir(k: &Kernel, dir: &str) -> Result<usize, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(0); // no directory yet: nothing to import
    };
    let args = json!({});
    let mut changed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_memory = path.extension().is_some_and(|e| e == "md")
            && path.file_name().is_some_and(|n| n != "MEMORY.md");
        if !is_memory {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read '{}': {}", path.display(), e))?;
        let Some((name, desc, mtype, trust)) = parse_memory_frontmatter(&raw) else {
            continue; // not a memory file
        };
        let mtype = if mtype.is_empty() {
            "project".to_string()
        } else {
            mtype
        };
        let before = memory_ko(k, &args, &name)?.map(|ko| ko.version);
        let after = mirror(
            k,
            &args,
            &name,
            &desc,
            &memory_body(&raw),
            &mtype,
            None,
            if trust.is_empty() {
                None
            } else {
                Some(trust.as_str())
            },
        )?;
        if before != after["version"].as_u64() {
            changed += 1;
        }
    }
    Ok(changed)
}

pub(crate) fn name_to_title(name: &str) -> String {
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

pub(crate) fn system_time_iso8601() -> String {
    // ponytail: avoid chrono dep — format manually from UNIX epoch. Good enough for memory timestamps.
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        // justified: clock before epoch is impossible in practice → epoch
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
