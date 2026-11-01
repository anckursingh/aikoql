//! AG-12b: MCP sampling — server-initiated requests to the client's model.
//! The stdio transport (one client per process) registers its stdout as the
//! sink; initialize records whether the client advertised `sampling`.
//! shortcut: per-process state — an HTTP sampling leg needs sink and
//! capability on the session instead of these statics.

use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

/// The stdout the stdio transport writes server-initiated frames to.
static SINK: Mutex<Option<Arc<Mutex<dyn Write + Send + 'static>>>> = Mutex::new(None);
/// Waiters for sampling replies, keyed by the request id.
static PENDING: OnceLock<Mutex<HashMap<u64, mpsc::Sender<J>>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static CLIENT_SAMPLING: AtomicBool = AtomicBool::new(false);

const SAMPLING_TIMEOUT: Duration = Duration::from_secs(30);

fn pending() -> &'static Mutex<HashMap<u64, mpsc::Sender<J>>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn register_sink(sink: Arc<Mutex<dyn Write + Send + 'static>>) {
    *SINK.lock().unwrap() = Some(sink);
}

pub(crate) fn set_client_sampling(on: bool) {
    CLIENT_SAMPLING.store(on, Ordering::Relaxed);
}

pub(crate) fn client_sampling() -> bool {
    CLIENT_SAMPLING.load(Ordering::Relaxed)
}

/// Hand a reply frame to its waiter (the stdio loop calls this for frames
/// that carry an id but no method). False = no waiter, not ours.
pub(crate) fn deliver(id: u64, frame: J) -> bool {
    if let Some(tx) = pending().lock().unwrap().remove(&id) {
        tx.send(frame).is_ok()
    } else {
        false
    }
}

/// Ask the client's model to extract facts, and parse its JSON array answer.
pub(crate) fn ask(messages: &J, prompt: &str) -> Result<Vec<J>, String> {
    let sink = SINK
        .lock()
        .unwrap()
        .clone()
        .ok_or("no sampling sink registered (stdio only)")?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    pending().lock().unwrap().insert(id, tx);
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "sampling/createMessage",
        "params": {
            "messages": messages,
            "systemPrompt": prompt,
            "maxTokens": 512,
            "includeContext": "none"
        }
    });
    {
        let mut out = sink.lock().unwrap();
        if let Err(e) = writeln!(out, "{frame}").and_then(|_| out.flush()) {
            pending().lock().unwrap().remove(&id);
            return Err(format!("writing the sampling request failed: {e}"));
        }
    }
    let reply = rx
        .recv_timeout(SAMPLING_TIMEOUT)
        .map_err(|_| format!("sampling request {id} got no answer in {SAMPLING_TIMEOUT:?}"))?;
    parse_facts(&reply)
}

fn parse_facts(reply: &J) -> Result<Vec<J>, String> {
    let text = reply
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .ok_or("sampling reply has no content text")?;
    // models wrap arrays in prose or code fences: take the [..] slice
    let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) else {
        return Err("sampling reply holds no JSON array".into());
    };
    if end <= start {
        return Err("sampling reply holds no JSON array".into());
    }
    let facts: Vec<J> = serde_json::from_str(&text[start..=end])
        .map_err(|e| format!("sampling reply is not a facts array: {e}"))?;
    if facts.is_empty() {
        return Err("sampling reply holds no facts".into());
    }
    Ok(facts)
}
