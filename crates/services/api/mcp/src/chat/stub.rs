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
