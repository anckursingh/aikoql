//! MCP over Streamable HTTP on the HTTP surface — what remote MCP clients
//! (claude.ai connectors, `claude mcp add --transport http`) speak.
//! `POST /mcp`: one JSON-RPC message through the same dispatcher as the TCP
//! and stdio transports, its response answered as application/json.
//! `GET /mcp`: the session's text/event-stream — every frame the server
//! sends on its own (subscription events, a streamed query's later chunks).
//! `initialize` opens a session (`Mcp-Session-Id`); every request carries
//! `Authorization: Bearer <--tcp-token>`, and a session is usable only with
//! the token that opened it. DELETE ends a session (and its stream).

use crate::dispatcher::handle_message;
use crate::session::{McpSession, TcpAuthTable, TcpIdentity, TrustMode};
use crate::tools::TxnRegistry;
use crate::{json, Arc, HashMap, HashSet, Kernel, Mutex, J};
use aikoql_storage_v2::engine::StorageAdminApi;
use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Condvar;
use std::time::{Duration, Instant};

/// The request-body cap (the TCP listener's frame cap).
pub(crate) const MAX_BODY: usize = 1024 * 1024;
/// Sessions held at once, and how long an idle one lives.
const MAX_SESSIONS: usize = 1024;
const SESSION_IDLE: Duration = Duration::from_secs(3600);
/// Open event streams at once (each holds a connection and a thread).
const MAX_STREAMS: usize = 256;
/// Server-initiated frames held for a session with no stream reading them
/// (the oldest are dropped past it), and sent ones kept for Last-Event-ID.
const MAX_QUEUED: usize = 1024;
const REPLAY: usize = 256;
/// A keep-alive comment this often on an idle stream — also how a dead
/// client is noticed (the write fails).
const KEEPALIVE: Duration = Duration::from_secs(15);

/// A session's outbox. Every frame the dispatcher writes for the session
/// lands here — from the request itself or from a background writer (a
/// subscription, a streamed query's later chunks) long after — and is
/// routed: a response goes back to the POST awaiting its id, anything else
/// to the session's event stream.
#[derive(Default)]
struct Outbox {
    partial: Vec<u8>,
    responses: HashMap<String, String>,
    queue: VecDeque<(u64, String)>,
    sent: VecDeque<(u64, String)>,
    next_event: u64,
    /// bumped by each GET: a newer stream supersedes an older one
    stream_gen: u64,
    closed: bool,
}

struct Out {
    inbox: Mutex<Outbox>,
    ready: Condvar,
}

/// The dispatcher's writer for one session (frames are newline-ended).
struct OutWriter(Arc<Out>);

impl Write for OutWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut b = self.0.inbox.lock().unwrap(); // justified: Mutex poison is unrecoverable
        b.partial.extend_from_slice(buf);
        while let Some(pos) = b.partial.iter().position(|&c| c == b'\n') {
            let line: Vec<u8> = b.partial.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line).trim().to_string();
            if line.is_empty() {
                continue;
            }
            let frame: Option<J> = serde_json::from_str(&line).ok();
            let response_id = frame.as_ref().and_then(|f| {
                (f.get("method").is_none()
                    && (f.get("result").is_some() || f.get("error").is_some()))
                .then(|| f.get("id").map(J::to_string))
                .flatten()
            });
            match response_id {
                Some(id) => {
                    b.responses.insert(id, line);
                }
                None => {
                    b.next_event += 1;
                    let n = b.next_event;
                    b.queue.push_back((n, line));
                    if b.queue.len() > MAX_QUEUED {
                        b.queue.pop_front();
                    }
                }
            }
        }
        self.0.ready.notify_all();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct HttpSession {
    /// the token that opened it — the only one that may use it
    token: String,
    session: McpSession,
    sub_ids: HashSet<String>,
    txns: TxnRegistry,
    last_used: Instant,
    writer: Arc<Mutex<OutWriter>>,
    out: Arc<Out>,
}

impl HttpSession {
    /// Ends the session: its stream closes, its subscriptions stop.
    fn close(&mut self, k: &Kernel) {
        for id in self.sub_ids.drain() {
            let _ = k.unsubscribe(&id);
        }
        self.out.inbox.lock().unwrap().closed = true; // justified: Mutex poison is unrecoverable
        self.out.ready.notify_all();
    }
}

/// The endpoint's state: the token table and the live sessions.
pub(crate) struct McpHttp {
    auth: Arc<TcpAuthTable>,
    sessions: Mutex<HashMap<String, Arc<Mutex<HttpSession>>>>,
    streams: AtomicUsize,
    request_timeout_secs: u64,
    /// AIKOQL_MCP_ALLOWED_ORIGINS — browser origins accepted besides loopback
    allowed_origins: Vec<String>,
}

/// One response: status line, extra headers, content type, body.
pub(crate) type Reply = (
    &'static str,
    Vec<(&'static str, String)>,
    &'static str,
    String,
);

impl McpHttp {
    pub(crate) fn new(auth: Arc<TcpAuthTable>, request_timeout_secs: u64) -> Self {
        let allowed_origins = std::env::var("AIKOQL_MCP_ALLOWED_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect();
        McpHttp {
            auth,
            sessions: Mutex::new(HashMap::new()),
            streams: AtomicUsize::new(0),
            request_timeout_secs,
            allowed_origins,
        }
    }

    /// The checks every /mcp request passes: Origin, bearer token, and the
    /// MCP-Protocol-Version header when present.
    fn admit<'h>(&self, headers: &'h str) -> Result<(&'h str, &TcpIdentity), Reply> {
        if let Some(origin) = header(headers, "origin") {
            if !self.origin_allowed(origin) {
                return Err(plain("403 Forbidden", "origin not allowed"));
            }
        }
        let token = header(headers, "authorization")
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::trim)
            .ok_or_else(|| {
                unauthorized("an Authorization: Bearer <--tcp-token> header is required")
            })?;
        let ident = self
            .auth
            .lookup(token)
            .ok_or_else(|| unauthorized("invalid token"))?;
        // 2025-06-18: requests after initialize name the negotiated version
        if let Some(v) = header(headers, "mcp-protocol-version") {
            if !crate::SUPPORTED_PROTOCOL_VERSIONS.contains(&v) {
                return Err(plain("400 Bad Request", "unsupported MCP-Protocol-Version"));
            }
        }
        Ok((token, ident))
    }

    /// The session named in the headers, if it is `token`'s.
    fn owned(
        &self,
        headers: &str,
        token: &str,
    ) -> Result<(String, Arc<Mutex<HttpSession>>), Reply> {
        let Some(sid) = header(headers, "mcp-session-id") else {
            return Err(plain(
                "400 Bad Request",
                "Mcp-Session-Id required (initialize first)",
            ));
        };
        let entry = self.sessions.lock().unwrap().get(sid).cloned(); // justified: Mutex poison is unrecoverable
        match entry {
            Some(e) if e.lock().unwrap().token == token => Ok((sid.to_string(), e)), // justified: Mutex poison is unrecoverable
            _ => Err(plain("404 Not Found", "unknown session")),
        }
    }

    /// One POST or DELETE on /mcp (GET is `stream`). `headers` is the raw
    /// header block.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle(
        &self,
        method: &str,
        headers: &str,
        body: &str,
        k: &Kernel,
        db_path: &Arc<String>,
        rate_limit: &Mutex<crate::rate_limiter::RateLimiter>,
        admin: Option<&dyn StorageAdminApi>,
    ) -> Reply {
        if method != "POST" && method != "DELETE" {
            let mut r = plain("405 Method Not Allowed", "GET, POST or DELETE");
            r.1.push(("Allow", "GET, POST, DELETE".into()));
            return r;
        }
        let (token, ident) = match self.admit(headers) {
            Ok(a) => a,
            Err(r) => return r,
        };

        if method == "DELETE" {
            let (sid, entry) = match self.owned(headers, token) {
                Ok(o) => o,
                Err(r) => return r,
            };
            self.sessions.lock().unwrap().remove(&sid); // justified: Mutex poison is unrecoverable
            entry.lock().unwrap().close(k); // justified: Mutex poison is unrecoverable
            return ("204 No Content", vec![], "text/plain", String::new());
        }

        let msg: J = match serde_json::from_str(body) {
            Ok(m) => m,
            Err(e) => return rpc_error(&J::Null, -32700, &format!("parse error: {e}")),
        };
        if !msg.is_object() {
            return rpc_error(
                &J::Null,
                -32600,
                "one JSON-RPC message per request (no batches)",
            );
        }
        let request_id = msg
            .get("id")
            .filter(|_| msg.get("method").is_some())
            .map(J::to_string);
        let is_initialize = msg.get("method").and_then(|m| m.as_str()) == Some("initialize");

        let (sid, entry) = if is_initialize {
            if request_id.is_none() {
                return plain(
                    "400 Bad Request",
                    "initialize must be a request (with an id)",
                );
            }
            let session = McpSession {
                agent_id: "http-agent".into(),
                tenant: ident.tenant.clone(),
                roles: ident.roles.clone(),
                trust_mode: TrustMode::Tcp,
                ..Default::default()
            };
            match self.open(token, session, k) {
                Some(opened) => opened,
                None => return plain("503 Service Unavailable", "too many MCP sessions"),
            }
        } else {
            match self.owned(headers, token) {
                Ok(o) => o,
                Err(r) => return r,
            }
        };

        let out = {
            let mut s = entry.lock().unwrap(); // justified: Mutex poison is unrecoverable
            s.last_used = Instant::now();
            let HttpSession {
                session,
                sub_ids,
                txns,
                writer,
                out,
                ..
            } = &mut *s;
            handle_message(
                k,
                sub_ids,
                writer,
                rate_limit,
                db_path,
                session,
                msg,
                admin,
                self.request_timeout_secs,
                txns,
            );
            out.clone()
        };
        // notifications and client responses: dispatched, never answered
        let Some(id) = request_id else {
            return ("202 Accepted", vec![], "text/plain", String::new());
        };
        let response = out.inbox.lock().unwrap().responses.remove(&id); // justified: Mutex poison is unrecoverable
        match response {
            Some(frame) => (
                "200 OK",
                vec![("Mcp-Session-Id", sid)],
                "application/json",
                frame,
            ),
            None => plain(
                "500 Internal Server Error",
                "the request produced no response",
            ),
        }
    }

    /// GET /mcp: the session's event stream, until the client goes away, a
    /// newer GET supersedes it, or the session ends. With Last-Event-ID the
    /// events sent after that one are sent again first.
    pub(crate) fn stream(&self, headers: &str, w: &mut impl Write) {
        let (token, _) = match self.admit(headers) {
            Ok(a) => a,
            Err(r) => return write_reply(w, r),
        };
        let (sid, entry) = match self.owned(headers, token) {
            Ok(o) => o,
            Err(r) => return write_reply(w, r),
        };
        if self.streams.fetch_add(1, Ordering::SeqCst) >= MAX_STREAMS {
            self.streams.fetch_sub(1, Ordering::SeqCst);
            return write_reply(
                w,
                plain("503 Service Unavailable", "too many MCP event streams"),
            );
        }
        let out = entry.lock().unwrap().out.clone(); // justified: Mutex poison is unrecoverable
        let last: Option<u64> = header(headers, "last-event-id").and_then(|v| v.parse().ok());
        let gen = {
            let mut b = out.inbox.lock().unwrap(); // justified: Mutex poison is unrecoverable
            b.stream_gen += 1;
            if let Some(last) = last {
                // sent after the client's last event: they may never have arrived
                let again: Vec<(u64, String)> =
                    b.sent.iter().filter(|(n, _)| *n > last).cloned().collect();
                for e in again.into_iter().rev() {
                    b.queue.push_front(e);
                }
            }
            b.stream_gen
        };
        out.ready.notify_all(); // a superseded stream wakes and ends
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
             Connection: close\r\nMcp-Session-Id: {sid}\r\n\r\n"
        );
        if w.write_all(head.as_bytes()).and_then(|_| w.flush()).is_ok() {
            self.pump(&out, gen, w);
        }
        self.streams.fetch_sub(1, Ordering::SeqCst);
    }

    /// Sends the outbox's events as they come, a keep-alive while idle.
    fn pump(&self, out: &Out, gen: u64, w: &mut impl Write) {
        loop {
            let batch: Vec<(u64, String)> = {
                let mut b = out.inbox.lock().unwrap(); // justified: Mutex poison is unrecoverable
                if b.queue.is_empty() && !b.closed && b.stream_gen == gen {
                    b = out.ready.wait_timeout(b, KEEPALIVE).unwrap().0; // justified: Mutex poison is unrecoverable
                }
                if b.closed || b.stream_gen != gen {
                    return;
                }
                let batch: Vec<(u64, String)> = b.queue.drain(..).collect();
                for e in &batch {
                    b.sent.push_back(e.clone());
                    if b.sent.len() > REPLAY {
                        b.sent.pop_front();
                    }
                }
                batch
            };
            let mut text = String::new();
            if batch.is_empty() {
                text.push_str(": keep-alive\n\n");
            }
            for (n, frame) in &batch {
                text.push_str(&format!("id: {n}\nevent: message\ndata: {frame}\n\n"));
            }
            if w.write_all(text.as_bytes())
                .and_then(|_| w.flush())
                .is_err()
            {
                return; // the client went away
            }
        }
    }

    /// A new session for `token`, after closing idle ones; None at the cap.
    fn open(
        &self,
        token: &str,
        session: McpSession,
        k: &Kernel,
    ) -> Option<(String, Arc<Mutex<HttpSession>>)> {
        let mut raw = [0u8; 16];
        getrandom::getrandom(&mut raw).ok()?;
        let sid: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let mut map = self.sessions.lock().unwrap(); // justified: Mutex poison is unrecoverable
        if map.len() >= MAX_SESSIONS {
            map.retain(|_, s| match s.try_lock() {
                Ok(mut s) if s.last_used.elapsed() >= SESSION_IDLE => {
                    s.close(k);
                    false
                }
                _ => true,
            });
            if map.len() >= MAX_SESSIONS {
                return None;
            }
        }
        let out = Arc::new(Out {
            inbox: Mutex::new(Outbox::default()),
            ready: Condvar::new(),
        });
        let entry = Arc::new(Mutex::new(HttpSession {
            token: token.to_string(),
            session,
            sub_ids: HashSet::new(),
            txns: Mutex::new(HashMap::new()),
            last_used: Instant::now(),
            writer: Arc::new(Mutex::new(OutWriter(out.clone()))),
            out,
        }));
        map.insert(sid.clone(), entry.clone());
        Some((sid, entry))
    }

    /// Loopback origins (any port) and AIKOQL_MCP_ALLOWED_ORIGINS — the
    /// DNS-rebinding guard: a page rebound to 127.0.0.1 still sends its own
    /// foreign Origin, so the Host header proves nothing.
    fn origin_allowed(&self, origin: &str) -> bool {
        let origin = origin.trim().trim_end_matches('/');
        if self.allowed_origins.iter().any(|o| o == origin) {
            return true;
        }
        let host = origin
            .split_once("://")
            .map_or(origin, |(_, rest)| rest)
            .split('/')
            .next()
            .unwrap_or("");
        let host = match host.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(""),
            None => host.rsplit_once(':').map_or(host, |(h, _)| h),
        };
        matches!(host, "localhost" | "127.0.0.1" | "::1")
    }
}

/// A header's value from the raw block (names are case-insensitive).
pub(crate) fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// Writes one reply (HTTP/1.0, closed after: the surface's shape).
pub(crate) fn write_reply(stream: &mut impl Write, (status, extra, ct, body): Reply) {
    let mut resp = format!(
        "HTTP/1.0 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in extra {
        resp.push_str(&format!("{k}: {v}\r\n"));
    }
    resp.push_str("\r\n");
    resp.push_str(&body);
    let _ = stream.write_all(resp.as_bytes());
}

fn plain(status: &'static str, msg: &str) -> Reply {
    (status, vec![], "text/plain; charset=utf-8", msg.to_string())
}

fn unauthorized(msg: &str) -> Reply {
    let mut r = plain("401 Unauthorized", msg);
    r.1.push(("WWW-Authenticate", "Bearer".into()));
    r
}

fn rpc_error(id: &J, code: i64, msg: &str) -> Reply {
    let body = json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}});
    (
        "400 Bad Request",
        vec![],
        "application/json",
        body.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        let m = McpHttp::new(
            Arc::new(TcpAuthTable::parse(&["t::admin".into()]).unwrap()),
            30,
        );
        for ok in [
            "http://localhost:9091",
            "https://127.0.0.1",
            "http://[::1]:9091",
            "http://localhost",
        ] {
            assert!(m.origin_allowed(ok), "{ok}");
        }
        for bad in [
            "https://evil.example",
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example:80",
            "null",
        ] {
            assert!(!m.origin_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn headers_are_case_insensitive_and_skip_the_request_line() {
        let h = "POST /mcp HTTP/1.1\r\nAuthorization: Bearer x\r\nmcp-session-id: abc\r\n";
        assert_eq!(header(h, "authorization"), Some("Bearer x"));
        assert_eq!(header(h, "Mcp-Session-Id"), Some("abc"));
        assert_eq!(header(h, "origin"), None);
    }
}
