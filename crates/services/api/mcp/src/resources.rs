//! AG-03: the three resources and `resources/subscribe`. Reads compile fresh
//! state at read time; a subscription emits `notifications/resources/updated`
//! — a hint to re-read — whenever a kernel event the uri could reflect
//! arrives. shortcut: the kernel's EventFilter has no type dimension, so a
//! `conflicts/open` subscription hints on any write under the subject
//! (contradict/resolve/supersede can all change the open set); the re-read
//! is one cheap scan.

use crate::protocol::write_frame;
use crate::session::{subject_of, McpSession};
use crate::{json, Arc, EventFilter, EventKind, HashSet, Kernel, Mutex, Write, J};

pub(crate) fn resources_list() -> J {
    json!({"resources": [
        {"uri": "aikoql://ko/{koid}", "name": "Knowledge object",
         "description": "A knowledge object by KOID: properties, provenance, evidence, status.",
         "mimeType": "application/json"},
        {"uri": "aikoql://conflicts/open", "name": "Open conflicts",
         "description": "Every unresolved conflict: the two claims and their snapshots.",
         "mimeType": "application/json"},
        {"uri": "aikoql://session/{id}/context", "name": "Session context",
         "description": "The session's identity and enabled surface, by run id (session_init).",
         "mimeType": "application/json"}
    ]})
}

/// The open conflicts, as one JSON object per conflict (the recall tool's
/// scan — no claim → conflict index exists yet).
pub(crate) fn open_conflicts(k: &Kernel, subject: &str) -> Result<J, String> {
    let subject = subject_of(&json!({"subject": subject}));
    let open: Vec<J> = k
        .scan_by_type(&subject, "aikoql:conflict")
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|c| {
            matches!(c.extensions.get("resolution"),
                Some(aikoql_kernel::Value::Text(r)) if r == "unresolved")
        })
        .map(|c| {
            let side = |n: &str| match c.properties.get(n) {
                Some(aikoql_kernel::Value::Text(t)) => t.clone(),
                _ => String::new(),
            };
            json!({
                "koid": c.koid.to_hex(),
                "status": "open",
                "claim_a": side("claim_a"),
                "claim_b": side("claim_b"),
                "description": side("description"),
            })
        })
        .collect();
    Ok(json!(open))
}

fn session_context(session: &McpSession) -> J {
    json!({
        "agent_id": session.agent_id,
        "run_id": session.run_id,
        "roles": session.roles,
        "tool_groups": session.tool_groups.iter().copied().collect::<Vec<_>>(),
        "trust_mode": format!("{:?}", session.trust_mode),
    })
}

pub(crate) fn resources_read(
    k: &Kernel,
    session: &McpSession,
    params: &J,
) -> Result<J, (i64, String)> {
    let uri = params
        .get("uri")
        .and_then(|u| u.as_str())
        .ok_or((-32602, "missing uri".to_string()))?
        .to_string();
    let subject = params
        .get("subject")
        .and_then(|s| s.as_str())
        .unwrap_or(&session.agent_id)
        .to_string();
    let text = if let Some(koid) = uri.strip_prefix("aikoql://ko/") {
        crate::tools::tool_get(k, &json!({"subject": subject, "koid": koid}))
            .map_err(|e| (-32603, e))?
            .to_string()
    } else if uri == "aikoql://conflicts/open" {
        open_conflicts(k, &subject)
            .map_err(|e| (-32603, e))?
            .to_string()
    } else if let Some(id) = uri
        .strip_prefix("aikoql://session/")
        .and_then(|r| r.strip_suffix("/context"))
    {
        match &session.run_id {
            Some(run) if run == id => session_context(session).to_string(),
            _ => {
                return Err((
                    -32602,
                    format!("unknown session id {id:?} — run session_init first"),
                ))
            }
        }
    } else {
        return Err((-32602, format!("unknown resource uri {uri:?}")));
    };
    Ok(json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": text}]}))
}

fn resource_filter(uri: &str) -> Result<EventFilter, (i64, String)> {
    if uri == "aikoql://conflicts/open" {
        return Ok(EventFilter {
            koid: None,
            kinds: Some(vec![
                EventKind::Created,
                EventKind::Updated,
                EventKind::LifecycleChanged,
            ]),
        });
    }
    if let Some(koid) = uri.strip_prefix("aikoql://ko/") {
        let koid =
            crate::KOID::from_hex(koid).map_err(|e| (-32602, format!("invalid koid: {e}")))?;
        return Ok(EventFilter {
            koid: Some(koid),
            kinds: None,
        });
    }
    Err((-32602, format!("not subscribable: {uri:?} — session context has no change events, read it before each task")))
}

pub(crate) fn resources_subscribe(
    k: &Kernel,
    sub_ids: &mut HashSet<String>,
    out: &Arc<Mutex<impl Write + Send + 'static>>,
    params: &J,
) -> Result<J, (i64, String)> {
    let uri = params
        .get("uri")
        .and_then(|u| u.as_str())
        .ok_or((-32602, "missing uri".to_string()))?
        .to_string();
    let filter = resource_filter(&uri)?;
    // res: prefix keeps the id namespace separate from notifications'
    let id = format!("res:{uri}");
    let rx = k
        .subscribe(id.clone(), filter)
        .map_err(|e| (-32603, e.to_string()))?;
    let out = out.clone();
    let uri_owned = uri.clone();
    std::thread::spawn(move || {
        for _ke in rx {
            let mut out = out.lock().unwrap(); // justified: Mutex poison is unrecoverable
            write_frame(
                &mut *out,
                json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/resources/updated",
                    "params": {"uri": uri_owned.clone()}
                }),
            );
        }
    });
    sub_ids.insert(id);
    Ok(json!({"subscribed": true, "uri": uri}))
}

pub(crate) fn resources_unsubscribe(
    k: &Kernel,
    sub_ids: &mut HashSet<String>,
    params: &J,
) -> Result<J, (i64, String)> {
    let uri = params
        .get("uri")
        .and_then(|u| u.as_str())
        .ok_or((-32602, "missing uri".to_string()))?;
    let id = format!("res:{uri}");
    k.unsubscribe(&id).map_err(|e| (-32603, e.to_string()))?;
    sub_ids.remove(&id);
    Ok(json!({}))
}
