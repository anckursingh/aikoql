//! Knowledge Runtime — MRFC-0030 Phase 7c.
//!
//! Orchestrator, Trigger Engine, and Program Cache. Lives in the MCP
//! service layer because it requires compiler + runtime dependencies
//! which the kernel crate does not (and should not) import.

use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Execution Statistics (MRFC-0030 Phase 7d)
// ---------------------------------------------------------------------------

use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub struct ExecutionStats {
    pub programs_executed: u64,
    pub total_rows_returned: u64,
    pub total_time_ms: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

static EXEC_STATS: std::sync::Mutex<ExecutionStats> = std::sync::Mutex::new(ExecutionStats {
    programs_executed: 0, total_rows_returned: 0, total_time_ms: 0,
    cache_hits: 0, cache_misses: 0,
});

pub fn record_execution(rows: u64, elapsed_ms: u64, cache_hit: bool) {
    let mut s = EXEC_STATS.lock().unwrap();
    s.programs_executed += 1;
    s.total_rows_returned += rows;
    s.total_time_ms += elapsed_ms;
    if cache_hit { s.cache_hits += 1; } else { s.cache_misses += 1; }
}

#[allow(dead_code)]
pub(crate) fn execution_stats() -> ExecutionStats {
    EXEC_STATS.lock().unwrap().clone()
}

// ---------------------------------------------------------------------------
// Program Cache — LRU of compiled IrPlans keyed by KOID
// ---------------------------------------------------------------------------

pub struct ProgramCache {
    cache: Mutex<HashMap<KOID, (mnemosyne_kernel::ir::IrPlan, u64)>>,
    hits: Mutex<u64>,
}

impl ProgramCache {
    pub fn new() -> Self {
        ProgramCache { cache: Mutex::new(HashMap::new()), hits: Mutex::new(0) }
    }

    pub fn get(&self, koid: &KOID, expected_version: u64) -> Option<mnemosyne_kernel::ir::IrPlan> {
        let guard = self.cache.lock().unwrap();
        if let Some((plan, ver)) = guard.get(koid) {
            if *ver == expected_version {
                *self.hits.lock().unwrap() += 1;
                return Some(plan.clone());
            }
        }
        None
    }

    pub fn put(&self, koid: KOID, version: u64, plan: mnemosyne_kernel::ir::IrPlan) {
        let mut guard = self.cache.lock().unwrap();
        // ponytail: simple LRU — if >100 entries, clear half.
        if guard.len() > 100 {
            let keys: Vec<KOID> = guard.keys().take(50).cloned().collect();
            for k in keys { guard.remove(&k); }
        }
        guard.insert(koid, (plan, version));
    }

    pub fn stats(&self) -> u64 { *self.hits.lock().unwrap() }
}

// ---------------------------------------------------------------------------
// Workflow Orchestrator
// ---------------------------------------------------------------------------

/// Execute a Workflow KO: parse its steps, find each Program KO by name,
/// compile and execute in order. Returns (step_logs, final_results).
pub fn execute_workflow(
    kernel: &Kernel, workflow_koid: &KOID, subject: &Subject, cache: Option<&ProgramCache>,
) -> Result<Vec<String>, String> {
    let ctx = KnowledgeContext::from(subject.clone());
    let wf_ko = kernel.get(ctx.clone(), workflow_koid).map_err(|e| e.to_string())?;

    if wf_ko.metadata.type_name != "mnemosyne:workflow" {
        return Err("not a workflow".into());
    }

    // Parse steps JSON.
    let steps_json = match wf_ko.properties.get("steps") {
        Some(Value::Text(s)) => s.clone(),
        _ => return Err("workflow has no steps property".into()),
    };

    let mut steps: Vec<(i64, String)> = Vec::new();
    // Parse steps as JSON array of {order, program} objects.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&steps_json) {
        if let Some(arr) = v.as_array() {
            for item in arr {
                let order = item.get("order").and_then(|o| o.as_i64()).unwrap_or(0);
                let name = item.get("program").and_then(|p| p.as_str()).unwrap_or("?").to_string();
                steps.push((order, name));
            }
        }
    }
    steps.sort_by_key(|s| s.0);

    let programs = kernel.list_programs(subject).map_err(|e| e.to_string())?;
    let mut logs = vec![format!("Workflow: {}", wf_ko.koid.to_hex())];

    for (order, prog_name) in &steps {
        logs.push(format!("  Step {}: {}", order, prog_name));
        let prog = match programs.iter().find(|p| {
            p.properties.get("name").and_then(|v| match v {
                Value::Text(s) => Some(s.as_str()), _ => None,
            }) == Some(prog_name.as_str())
        }) {
            Some(p) => p,
            None => { logs.push(format!("    SKIP: not found")); continue; }
        };

        let cur_ver = match prog.properties.get("version").and_then(|v| match v {
            Value::Int(i) => Some(*i as u64), _ => None,
        }) { Some(v) => v, None => 0 };

        let body = match prog.properties.get("body") {
            Some(Value::Text(s)) => s.clone(),
            _ => { logs.push(format!("    SKIP: no body")); continue; }
        };

        // Check program cache.
        let (plan, cache_hit) = if let Some(c) = cache {
            if let Some(cached) = c.get(&prog.koid, cur_ver) {
                (cached, true)
            } else {
                let plan = mnemosyne_compiler::parser::compile_with_subject(&body, &subject.name)
                    .map_err(|e| format!("compile: {}", e))?;
                c.put(prog.koid, cur_ver, plan.clone());
                (plan, false)
            }
        } else {
            (mnemosyne_compiler::parser::compile_with_subject(&body, &subject.name)
                .map_err(|e| format!("compile: {}", e))?, false)
        };

        if cache_hit { logs.push(format!("    (cache hit)")); }

        let start = Instant::now();
        let optimized = mnemosyne_compiler::planner::Planner::optimize(&plan);
        match mnemosyne_runtime::Interpreter::execute(kernel, &optimized) {
            Ok(rows) => {
                let elapsed = start.elapsed().as_millis() as u64;
                let count = match &rows {
                    mnemosyne_runtime::RowSet::Objects(objs) => objs.len() as u64,
                    _ => 0,
                };
                record_execution(count, elapsed, cache_hit);
                logs.push(format!("    OK: {} results in {}ms", count, elapsed));
            }
            Err(e) => logs.push(format!("    ERROR: {}", e)),
        }
    }
    Ok(logs)
}

// ---------------------------------------------------------------------------
// Trigger Engine
// ---------------------------------------------------------------------------

/// Check journal events since `last_seq` and fire matching Trigger KOs.
/// Returns the new high-water sequence number.
pub fn check_and_fire_triggers(kernel: &Kernel, last_seq: u64) -> Result<u64, String> {
    let (head, _) = kernel.journal_head().map_err(|e| e.to_string())?;
    if head <= last_seq { return Ok(last_seq); }

    let subject = Subject { name: "trigger-engine".into(), roles: vec!["admin".into()] };
    let events = kernel.journal().map_err(|e| e.to_string())?;
    let triggers = kernel.scan_by_type(&subject, "mnemosyne:trigger").unwrap_or_default();
    if triggers.is_empty() { return Ok(head); }

    let mut new_water = last_seq;
    for ke in events.iter().skip(last_seq as usize) {
        new_water = ke.seq;
        for t in &triggers {
            let ek = match t.properties.get("event_kind").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }) {
                Some(s) => s, None => continue,
            };
            let pk = match t.properties.get("program_koid").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }) {
                Some(s) => s, None => continue,
            };

            let ke_kind = format!("{:?}", ke.kind);
            if ke_kind == ek {
                if let Ok(prog_koid) = KOID::from_hex(pk) {
                    let ctx = KnowledgeContext::from(subject.clone());
                    if let Ok(prog_ko) = kernel.get(ctx, &prog_koid) {
                        if let Some(Value::Text(body)) = prog_ko.properties.get("body") {
                            if let Ok(plan) = mnemosyne_compiler::parser::compile_with_subject(body, "trigger-engine") {
                                let optimized = mnemosyne_compiler::planner::Planner::optimize(&plan);
                                let _ = mnemosyne_runtime::Interpreter::execute(kernel, &optimized);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(new_water)
}
