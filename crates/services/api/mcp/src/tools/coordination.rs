//! AG-41 — coordination tools: claim_task / release_task / handoff_task
//! over the kernel's claim/release/handoff ops (CAS under the pipe lock —
//! a task is claimed exactly once; wall-clock leases expire on agent
//! death). The kernel op does the atomicity; this module is JSON → op →
//! response plumbing.

use crate::helpers::*;
use crate::session::subject_of;
use crate::{json, ClaimRequest, HandoffRequest, Kernel, ReleaseRequest, J};

fn task_of(args: &J) -> Result<crate::KOID, String> {
    let hex = args
        .get("task")
        .and_then(|t| t.as_str())
        .ok_or("missing argument: task (32-char hex KOID)")?;
    crate::KOID::from_hex(hex).map_err(|e| e.to_string())
}

pub(crate) fn tool_claim_task(k: &Kernel, args: &J) -> Result<J, String> {
    let task = task_of(args)?;
    let lease_seconds = args
        .get("lease_seconds")
        .and_then(|l| l.as_u64())
        .unwrap_or(300);
    let subject = subject_of(args);
    let r = k
        .claim_task(ClaimRequest::new(subject.clone(), task, lease_seconds))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "task": task.to_hex(),
        "version": r.version,
        "holder": subject.name,
        "claim_until": r.claim_until,
        "renewed": r.renewed,
    }))
}

pub(crate) fn tool_release_task(k: &Kernel, args: &J) -> Result<J, String> {
    let task = task_of(args)?;
    let subject = subject_of(args);
    let r = k
        .release_task(ReleaseRequest::new(subject, task))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "released": true,
        "task": task.to_hex(),
        "version": r.version,
    }))
}

pub(crate) fn tool_handoff_task(k: &Kernel, args: &J) -> Result<J, String> {
    let task = task_of(args)?;
    let to = args
        .get("to")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("missing argument: to (the receiving principal)")?;
    let mut req = HandoffRequest::new(subject_of(args), task, to.to_string());
    req.evidence = parse_evidence(args)?;
    req.note = args.get("note").and_then(|n| n.as_str()).map(String::from);
    let h = k.handoff_task(req).map_err(|e| e.to_string())?;
    Ok(json!({
        "handoff_koid": h.koid.to_hex(),
        "task": task.to_hex(),
        "to": to,
        "version": h.version,
    }))
}
