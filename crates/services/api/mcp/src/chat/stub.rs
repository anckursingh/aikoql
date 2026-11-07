//! Test-only scripted OpenAI-style server.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) struct StubResponse {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}

impl StubResponse {
    /// A well-formed chat-completions reply carrying `content`.
    pub(crate) fn ok(content: &str) -> Self {
        let body = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": content}}]});
        Self::raw(200, &body.to_string())
    }

    pub(crate) fn raw(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
            delay: Duration::ZERO,
        }
    }
}

/// Serves `responses` in order (the last one repeats) on a free loopback
/// port. Returns the base URL and every raw request received (headers+body).
pub(crate) fn spawn_stub_server(responses: Vec<StubResponse>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for (i, conn) in listener.incoming().enumerate() {
            let Ok(mut s) = conn else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let Ok(n) = s.read(&mut chunk) else {
                    break None;
                };
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(p + 4);
                }
            };
            let Some(end) = header_end else { continue };
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let want = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            while buf.len() < end + want {
                let Ok(n) = s.read(&mut chunk) else { break };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            // justified: Mutex poison is unrecoverable
            log.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf).to_string());
            let r = &responses[i.min(responses.len() - 1)];
            std::thread::sleep(r.delay);
            let _ = write!(
                s,
                "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                r.status,
                r.body.len(),
                r.body
            );
        }
    });
    (url, seen)
}

use super::client::Generate;
use crate::{
    json, ExtensionMap, Kernel, Metadata, Origin, PropertyMap, ReferentialPolicy, RememberRequest,
    Value,
};
use std::collections::VecDeque;

/// Scripted model: pops one reply per call and records every prompt.
pub(crate) struct StubModel {
    replies: Mutex<VecDeque<Result<String, String>>>,
    pub prompts: Mutex<Vec<String>>,
}

impl StubModel {
    pub(crate) fn new(replies: Vec<Result<String, String>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            prompts: Mutex::new(Vec::new()),
        }
    }
}

impl Generate for StubModel {
    fn generate(&self, prompt: &str) -> Result<String, String> {
        // justified: Mutex poison is unrecoverable
        self.prompts.lock().unwrap().push(prompt.to_string());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err("stub exhausted".into()))
    }
}

/// An in-memory kernel with a live text index.
pub(crate) fn kernel() -> Arc<Kernel> {
    let k = Arc::new(
        Kernel::open(
            Arc::new(aikoql_kernel::MemoryEngine::new()),
            Arc::new(aikoql_kernel::ManualClock::new(10_000)),
            0xBEEF,
        )
        .unwrap(),
    );
    let m = aikoql_scheduler::IndexMaintainer::start(
        &k,
        Arc::new(aikoql_kernel::BruteForceVectorIndex::new()),
        Arc::new(aikoql_kernel::TokenTextIndex::new()),
    )
    .unwrap();
    k.attach_indexes(m);
    k
}

/// Create an object and wait for the index; returns its koid.
pub(crate) fn seed(k: &Kernel, tenant: Option<&str>, ty: &str, props: &[(&str, &str)]) -> String {
    let mut p = PropertyMap::new();
    for (key, v) in props {
        p.insert((*key).into(), Value::Text((*v).into()));
    }
    let r = k
        .remember(RememberRequest {
            context: crate::session::subject_of(&json!({"subject": "alice"})).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: ty.into(),
                tenant: tenant.map(String::from),
                schema_version: 1,
                tags: vec![],
            },
            properties: p,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap();
    crate::tools::query::wait_fresh(k, 5000);
    r.koid.to_hex()
}
