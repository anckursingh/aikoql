//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::helpers::*;
use crate::session::*;
use crate::{
    json, ExtensionMap, Kernel, Metadata, Origin, PropertyMap, ReferentialPolicy, RememberRequest,
    SecurityDescriptor, Value, J, MEMORY_DIR,
};
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

pub(crate) fn parse_memory_frontmatter(raw: &str) -> Option<(String, String, String)> {
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

pub(crate) fn tool_memory_search(args: &J) -> Result<J, String> {
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

pub(crate) fn tool_memory_store(args: &J) -> Result<J, String> {
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

pub(crate) fn tool_memory_update(args: &J) -> Result<J, String> {
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
            // justified: no body after frontmatter → empty body
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

pub(crate) fn tool_memory_delete(args: &J) -> Result<J, String> {
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
