//! MCP over Streamable HTTP — `POST /mcp` on the HTTP surface, what remote
//! MCP clients (claude.ai connectors, `claude mcp add --transport http`)
//! speak. One JSON-RPC message per POST through the same dispatcher as the
//! TCP and stdio transports, answered as application/json (several frames
//! as text/event-stream). `initialize` opens a session (`Mcp-Session-Id`);
//! every request carries `Authorization: Bearer <--tcp-token>`, and a
//! session is usable only with the token that opened it. DELETE ends a
//! session; GET (a server-initiated stream) is 405 — server pushes
//! (subscriptions) are not carried over HTTP yet.

use crate::dispatcher::handle_message;
use crate::session::{McpSession, TcpAuthTable, TrustMode};
use crate::tools::TxnRegistry;
use crate::{json, Arc, HashMap, HashSet, Kernel, Mutex, J};
use aikoql_storage_v2::engine::StorageAdminApi;
use std::time::{Duration, Instant};

/// The request-body cap (the TCP listener's frame cap).
pub(crate) const MAX_BODY: usize = 1024 * 1024;
/// Sessions held at once, and how long an idle one lives.
const MAX_SESSIONS: usize = 1024;
const SESSION_IDLE: Duration = Duration::from_secs(3600);

struct HttpSession {
    /// the token that opened it — the only one that may use it
    token: String,
    session: McpSession,
    sub_ids: HashSet<String>,
    txns: TxnRegistry,
    last_used: Instant,
}

/// The endpoint's state: the token table and the live sessions.
pub(crate) struct McpHttp {
    auth: Arc<TcpAuthTable>,
    sessions: Mutex<HashMap<String, Arc<Mutex<HttpSession>>>>,
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
            request_timeout_secs,
            allowed_origins,
        }
    }

    /// One HTTP request on /mcp. `headers` is the raw header block.
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
        if let Some(origin) = header(headers, "origin") {
            if !self.origin_allowed(origin) {
                return plain("403 Forbidden", "origin not allowed");
            }
        }
        if method == "GET" {
            let mut r = plain(
                "405 Method Not Allowed",
                "no server-initiated stream; POST requests",
            );
            r.1.push(("Allow", "POST, DELETE".into()));
            return r;
        }
        if method != "POST" && method != "DELETE" {
            let mut r = plain("405 Method Not Allowed", "POST or DELETE");
            r.1.push(("Allow", "POST, DELETE".into()));
            return r;
        }
        let Some(token) = header(headers, "authorization")
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::trim)
        else {
            return unauthorized("an Authorization: Bearer <--tcp-token> header is required");
        };
        let Some(ident) = self.auth.lookup(token) else {
            return unauthorized("invalid token");
        };
        let sid = header(headers, "mcp-session-id").map(str::to_string);
        // 2025-06-18: requests after initialize name the negotiated version
        if let Some(v) = header(headers, "mcp-protocol-version") {
            if !crate::SUPPORTED_PROTOCOL_VERSIONS.contains(&v) {
                return plain("400 Bad Request", "unsupported MCP-Protocol-Version");
            }
        }

        if method == "DELETE" {
            let Some(sid) = sid else {
                return plain("400 Bad Request", "Mcp-Session-Id required");
            };
            let mut map = self.sessions.lock().unwrap(); // justified: Mutex poison is unrecoverable
            let owned = map
                .get(&sid)
                .is_some_and(|s| s.lock().unwrap().token == token); // justified: Mutex poison is unrecoverable
            if !owned {
                return plain("404 Not Found", "unknown session");
            }
            map.remove(&sid);
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
        let is_request = msg.get("method").is_some() && msg.get("id").is_some();
        let is_initialize = msg.get("method").and_then(|m| m.as_str()) == Some("initialize");

        let (sid, entry) = if is_initialize {
            if !is_request {
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
            match self.open(token, session) {
                Some(opened) => opened,
                None => return plain("503 Service Unavailable", "too many MCP sessions"),
            }
        } else {
            let Some(sid) = sid else {
                return plain(
                    "400 Bad Request",
                    "Mcp-Session-Id required (initialize first)",
                );
            };
            let entry = self.sessions.lock().unwrap().get(&sid).cloned(); // justified: Mutex poison is unrecoverable
            match entry {
                Some(e) if e.lock().unwrap().token == token => (sid, e), // justified: Mutex poison is unrecoverable
                _ => return plain("404 Not Found", "unknown session"),
            }
        };

        // notifications and client responses: dispatched, never answered
        let writer = Arc::new(Mutex::new(Vec::<u8>::new()));
        {
            let mut s = entry.lock().unwrap(); // justified: Mutex poison is unrecoverable
            s.last_used = Instant::now();
            let HttpSession {
                session,
                sub_ids,
                txns,
                ..
            } = &mut *s;
            handle_message(
                k,
                sub_ids,
                &writer,
                rate_limit,
                db_path,
                session,
                msg,
                admin,
                self.request_timeout_secs,
                txns,
            );
        }
        if !is_request {
            return ("202 Accepted", vec![], "text/plain", String::new());
        }
        let out = std::mem::take(&mut *writer.lock().unwrap()); // justified: Mutex poison is unrecoverable
        let frames: Vec<&str> = std::str::from_utf8(&out)
            .unwrap_or("")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        let session_header = vec![("Mcp-Session-Id", sid)];
        match frames.as_slice() {
            [] => plain(
                "500 Internal Server Error",
                "the request produced no response",
            ),
            [one] => (
                "200 OK",
                session_header,
                "application/json",
                one.to_string(),
            ),
            many => {
                let mut sse = String::new();
                for f in many {
                    sse.push_str("event: message\ndata: ");
                    sse.push_str(f);
                    sse.push_str("\n\n");
                }
                ("200 OK", session_header, "text/event-stream", sse)
            }
        }
    }

    /// A new session for `token`, after sweeping idle ones; None at the cap.
    fn open(&self, token: &str, session: McpSession) -> Option<(String, Arc<Mutex<HttpSession>>)> {
        let mut raw = [0u8; 16];
        getrandom::getrandom(&mut raw).ok()?;
        let sid: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let mut map = self.sessions.lock().unwrap(); // justified: Mutex poison is unrecoverable
        if map.len() >= MAX_SESSIONS {
            map.retain(|_, s| {
                s.try_lock()
                    .map_or(true, |s| s.last_used.elapsed() < SESSION_IDLE)
            });
            if map.len() >= MAX_SESSIONS {
                return None;
            }
        }
        let entry = Arc::new(Mutex::new(HttpSession {
            token: token.to_string(),
            session,
            sub_ids: HashSet::new(),
            txns: Mutex::new(HashMap::new()),
            last_used: Instant::now(),
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
