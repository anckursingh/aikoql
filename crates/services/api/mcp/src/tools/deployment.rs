//! MCP tool implementations — extracted from main.rs (R7 modularization).
//! No behavior changes.

use crate::*;

use crate::helpers::*;
use crate::session::*;
pub(crate) static PROGRAM_CACHE: std::sync::LazyLock<knowledge_runtime::ProgramCache> =
    std::sync::LazyLock::new(knowledge_runtime::ProgramCache::new);

pub(crate) fn tool_execute_workflow(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_check_triggers(k: &Kernel) -> Result<J, String> {
    let fired = knowledge_runtime::check_and_fire_triggers(k, 0).map_err(|e| e.to_string())?;
    Ok(json!({"water_mark": fired}))
}

#[allow(dead_code)]
pub(crate) fn tool_execution_stats() -> Result<J, String> {
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

pub(crate) fn tool_program_cache_stats() -> Result<J, String> {
    Ok(json!({"cache_hits": PROGRAM_CACHE.stats()}))
}

// ---- Agent KO (MRFC-0030 Phase 7c) ---------------------------------------

pub(crate) fn tool_deploy_agent(k: &Kernel, args: &J) -> Result<J, String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing: name")?;
    let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let skills = args
        .get("skills")
        .and_then(|v| v.as_array())
        .map(|a| serde_json::to_string(a).map_err(|e| format!("serialize skills: {}", e)))
        .transpose()?
        .unwrap_or_else(|| "[]".into());
    let tools = args
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|a| serde_json::to_string(a).map_err(|e| format!("serialize tools: {}", e)))
        .transpose()?
        .unwrap_or_else(|| "[]".into());
    let policies = args
        .get("policies")
        .and_then(|v| v.as_array())
        .map(|a| serde_json::to_string(a).map_err(|e| format!("serialize policies: {}", e)))
        .transpose()?
        .unwrap_or_else(|| "[]".into());
    let r = k
        .deploy_agent(name, prompt, &skills, &tools, &policies, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": r.koid.to_hex(), "version": r.version, "name": name}))
}

pub(crate) fn tool_list_agents(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_execute_agent(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_deploy_connector(k: &Kernel, args: &J) -> Result<J, String> {
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
        .map(|o| serde_json::to_string(o).map_err(|e| format!("serialize config: {}", e)))
        .transpose()?
        .unwrap_or_else(|| "{}".into());
    let mapping = args
        .get("mapping")
        .and_then(|v| v.as_array())
        .map(|a| serde_json::to_string(a).map_err(|e| format!("serialize mapping: {}", e)))
        .transpose()?
        .unwrap_or_else(|| "[]".into());
    let r = k
        .deploy_connector(name, plugin, &config, &mapping, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"koid": r.koid.to_hex(), "version": r.version, "name": name}))
}

pub(crate) fn tool_list_connectors(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_deploy_view(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_list_views(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_deploy_report(k: &Kernel, args: &J) -> Result<J, String> {
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
        .map(|a| serde_json::to_string(a).map_err(|e| format!("serialize parameters: {}", e)))
        .transpose()?
        .unwrap_or_else(|| "[]".into());
    let r = k
        .deploy_report(name, template, format, &parameters, &subject_of(args))
        .map_err(|e| e.to_string())?;
    Ok(json!({"status": "deployed", "koid": r.koid.to_hex(), "type": "aikoql:report"}))
}

pub(crate) fn tool_list_reports(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_deploy_benchmark(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_list_benchmarks(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_deploy_program(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_execute_program(k: &Kernel, args: &J) -> Result<J, String> {
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
    let exec_subject = subject.clone();

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
    // R9: program bodies run with the caller's roles + tenant scope.
    let plan = aikoql_compiler::parser::compile_scoped(
        &query,
        &exec_subject.name,
        &exec_subject.roles,
        exec_subject.tenant.as_deref(),
    )
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

pub(crate) fn tool_list_programs(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_deploy_policy(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_evaluate_policies(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_deploy_workflow(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_deploy_trigger(k: &Kernel, args: &J) -> Result<J, String> {
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

pub(crate) fn tool_add_dependency(k: &Kernel, args: &J) -> Result<J, String> {
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
