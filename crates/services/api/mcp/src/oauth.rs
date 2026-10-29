//! OAuth 2.1 for MCP over HTTP — the built-in authorization server Claude's
//! connectors (claude.ai, Desktop, mobile, Claude Code) sign in through.
//! Per the MCP authorization spec (2025-06-18) and Claude's connector docs:
//! RFC 9728 protected resource metadata (a 401 on /mcp points at it), RFC
//! 8414 authorization server metadata, RFC 7591 dynamic registration of
//! public clients, the authorization-code grant with S256 PKCE behind a
//! login + consent page (the server's argon2 `[auth]` logins), and rotating
//! refresh tokens. Tokens are opaque, bound to the MCP resource they were
//! issued for (the audience check), and held in memory: a restart signs
//! everyone out (the client re-runs the flow).

use crate::http::AuthResolver;
use crate::mcp_http::{header, Reply};
use crate::{json, Arc, HashMap, Mutex, J};
use base64::Engine as _;
use sha2::Digest as _;
use std::time::{Duration, Instant};

const CODE_TTL: Duration = Duration::from_secs(600);
const ACCESS_TTL: Duration = Duration::from_secs(3600);
const REFRESH_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// Registered clients held at once (DCR registers one per fresh connection).
const MAX_CLIENTS: usize = 10_000;
const MAX_GRANTS: usize = 100_000;

struct Client {
    name: String,
    redirect_uris: Vec<String>,
}

struct Code {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    resource: String,
    user: String,
    roles: Vec<String>,
    expires: Instant,
}

/// What an access or refresh token stands for.
#[derive(Clone)]
struct Grant {
    client_id: String,
    user: String,
    roles: Vec<String>,
    resource: String,
    expires: Instant,
}

#[derive(Default)]
struct State {
    clients: HashMap<String, Client>,
    codes: HashMap<String, Code>,
    access: HashMap<String, Grant>,
    refresh: HashMap<String, Grant>,
}

/// The identity an access token carries to /mcp.
pub(crate) struct OAuthIdentity {
    /// what an MCP session is bound to — stable across token refreshes
    pub(crate) principal: String,
    pub(crate) user: String,
    pub(crate) roles: Vec<String>,
}

pub(crate) struct OAuth {
    logins: Arc<AuthResolver>,
    https: bool,
    /// AIKOQL_PUBLIC_URL — the externally visible origin behind a proxy
    public_url: Option<String>,
    state: Mutex<State>,
}

impl OAuth {
    pub(crate) fn new(logins: Arc<AuthResolver>, https: bool) -> Self {
        let public_url = std::env::var("AIKOQL_PUBLIC_URL")
            .ok()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        OAuth {
            logins,
            https,
            public_url,
            state: Mutex::new(State::default()),
        }
    }

    /// The issuer: the configured public URL, else this request's origin.
    pub(crate) fn base(&self, headers: &str) -> String {
        if let Some(u) = &self.public_url {
            return u.clone();
        }
        let host = header(headers, "host").unwrap_or("localhost");
        format!("{}://{host}", if self.https { "https" } else { "http" })
    }

    /// The 401 challenge pointing at the protected resource metadata.
    pub(crate) fn challenge(&self, headers: &str) -> String {
        format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\", scope=\"mcp\"",
            self.base(headers)
        )
    }

    /// One of the OAuth routes, or None if `route` is not one.
    pub(crate) fn route(
        &self,
        method: &str,
        route: &str,
        path: &str,
        headers: &str,
        body: &str,
    ) -> Option<Reply> {
        Some(match (method, route) {
            (
                "GET",
                "/.well-known/oauth-protected-resource"
                | "/.well-known/oauth-protected-resource/mcp",
            ) => {
                let base = self.base(headers);
                ok_json(json!({
                    "resource": format!("{base}/mcp"),
                    "authorization_servers": [base],
                    "scopes_supported": ["mcp"],
                    "bearer_methods_supported": ["header"],
                    "resource_name": "aikoql",
                }))
            }
            ("GET", "/.well-known/oauth-authorization-server") => {
                let base = self.base(headers);
                ok_json(json!({
                    "issuer": base,
                    "authorization_endpoint": format!("{base}/oauth/authorize"),
                    "token_endpoint": format!("{base}/oauth/token"),
                    "registration_endpoint": format!("{base}/oauth/register"),
                    "response_types_supported": ["code"],
                    "grant_types_supported": ["authorization_code", "refresh_token"],
                    "code_challenge_methods_supported": ["S256"],
                    "token_endpoint_auth_methods_supported": ["none"],
                    "scopes_supported": ["mcp", "offline_access"],
                }))
            }
            ("POST", "/oauth/register") => self.register(body),
            ("GET", "/oauth/authorize") => {
                let q = parse_form(path.split_once('?').map_or("", |(_, q)| q));
                self.authorize_page(&q, headers, None)
            }
            ("POST", "/oauth/authorize") => self.authorize(&parse_form(body), headers),
            ("POST", "/oauth/token") => self.token(&parse_form(body), headers),
            _ => return None,
        })
    }

    /// The identity behind an access token issued for this server's /mcp.
    pub(crate) fn validate(&self, token: &str, headers: &str) -> Option<OAuthIdentity> {
        let st = self.state.lock().unwrap(); // justified: Mutex poison is unrecoverable
        let g = st.access.get(token)?;
        // the audience: a token issued for another resource is refused
        if g.expires <= Instant::now() || g.resource != format!("{}/mcp", self.base(headers)) {
            return None;
        }
        Some(OAuthIdentity {
            principal: format!("oauth:{}:{}", g.client_id, g.user),
            user: g.user.clone(),
            roles: g.roles.clone(),
        })
    }

    /// RFC 7591: a public client, its redirects HTTPS or loopback.
    fn register(&self, body: &str) -> Reply {
        let Ok(req) = serde_json::from_str::<J>(body) else {
            return oauth_error(
                "400 Bad Request",
                "invalid_client_metadata",
                "the body is not JSON",
            );
        };
        let uris: Vec<String> = req
            .get("redirect_uris")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|u| u.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if uris.is_empty() || !uris.iter().all(|u| redirect_allowed(u)) {
            return oauth_error(
                "400 Bad Request",
                "invalid_redirect_uri",
                "redirect_uris must be HTTPS or loopback http, without a fragment",
            );
        }
        let method = req
            .get("token_endpoint_auth_method")
            .and_then(|v| v.as_str())
            .unwrap_or("none");
        if method != "none" {
            return oauth_error(
                "400 Bad Request",
                "invalid_client_metadata",
                "only public clients (token_endpoint_auth_method \"none\")",
            );
        }
        let name = req
            .get("client_name")
            .and_then(|v| v.as_str())
            .unwrap_or("an MCP client")
            .chars()
            .take(100)
            .collect::<String>();
        let id = random_token();
        let mut st = self.state.lock().unwrap(); // justified: Mutex poison is unrecoverable
        if st.clients.len() >= MAX_CLIENTS {
            return oauth_error(
                "503 Service Unavailable",
                "temporarily_unavailable",
                "too many registered clients",
            );
        }
        st.clients.insert(
            id.clone(),
            Client {
                name: name.clone(),
                redirect_uris: uris.clone(),
            },
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        (
            "201 Created",
            no_store(),
            "application/json",
            json!({
                "client_id": id,
                "client_id_issued_at": now,
                "client_name": name,
                "redirect_uris": uris,
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
            })
            .to_string(),
        )
    }

    /// The authorization request's checks; Err is a page, never a redirect
    /// (an unverified redirect URI is never followed).
    fn check_request(
        &self,
        q: &HashMap<String, String>,
        headers: &str,
    ) -> Result<(String, String), Reply> {
        let get = |k: &str| q.get(k).map(String::as_str).unwrap_or("");
        let st = self.state.lock().unwrap(); // justified: Mutex poison is unrecoverable
        let Some(client) = st.clients.get(get("client_id")) else {
            return Err(page(
                "400 Bad Request",
                "Unknown client",
                "This client is not registered.",
            ));
        };
        if !client
            .redirect_uris
            .iter()
            .any(|r| redirect_matches(r, get("redirect_uri")))
        {
            return Err(page(
                "400 Bad Request",
                "Unregistered redirect",
                "The redirect URI is not registered for this client.",
            ));
        }
        if get("response_type") != "code" {
            return Err(page(
                "400 Bad Request",
                "Unsupported request",
                "Only response_type=code is supported.",
            ));
        }
        if get("code_challenge_method") != "S256" || get("code_challenge").len() < 43 {
            return Err(page(
                "400 Bad Request",
                "PKCE required",
                "An S256 code_challenge is required.",
            ));
        }
        let expected = format!("{}/mcp", self.base(headers));
        let resource = q
            .get("resource")
            .cloned()
            .unwrap_or_else(|| expected.clone());
        if resource.trim_end_matches('/') != expected {
            return Err(page(
                "400 Bad Request",
                "Unknown resource",
                "The resource is not this server's MCP endpoint.",
            ));
        }
        Ok((client.name.clone(), expected))
    }

    /// GET /oauth/authorize: the login + consent page.
    fn authorize_page(
        &self,
        q: &HashMap<String, String>,
        headers: &str,
        error: Option<&str>,
    ) -> Reply {
        let (name, _) = match self.check_request(q, headers) {
            Ok(c) => c,
            Err(r) => return r,
        };
        let redirect = q.get("redirect_uri").cloned().unwrap_or_default();
        let host = redirect
            .split_once("://")
            .map_or(redirect.as_str(), |(_, r)| r)
            .split(['/', '?'])
            .next()
            .unwrap_or("");
        let loopback_note = if is_loopback_uri(&redirect) {
            "<p class=\"warn\">This sends you back to a program on this computer. Approve only if you started this sign-in yourself.</p>"
        } else {
            ""
        };
        let hidden: String = [
            "response_type",
            "client_id",
            "redirect_uri",
            "code_challenge",
            "code_challenge_method",
            "state",
            "scope",
            "resource",
        ]
        .iter()
        .filter_map(|k| q.get(*k).map(|v| (k, v)))
        .map(|(k, v)| format!("<input type=\"hidden\" name=\"{k}\" value=\"{}\">", esc(v)))
        .collect();
        let error = error
            .map(|e| format!("<p class=\"err\">{}</p>", esc(e)))
            .unwrap_or_default();
        let body = format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Sign in to aikoql</title>\
             <meta name=\"viewport\" content=\"width=device-width\"><style>body{{font-family:system-ui;max-width:26rem;\
             margin:3rem auto;padding:0 1rem}}input{{display:block;width:100%;margin:.4rem 0 .9rem;padding:.5rem}}\
             button{{padding:.6rem 1.2rem}}.err{{color:#b00}}.warn{{color:#8a5a00}}</style></head><body>\
             <h1>Sign in to aikoql</h1><p><strong>{}</strong> wants access to this knowledge base. \
             After you sign in you are sent back to <strong>{}</strong>.</p>{loopback_note}{error}\
             <form method=\"post\" action=\"/oauth/authorize\">{hidden}\
             <label>Username<input name=\"username\" autocomplete=\"username\" required></label>\
             <label>Password<input name=\"password\" type=\"password\" autocomplete=\"current-password\" required></label>\
             <button name=\"approve\" value=\"1\">Allow</button> <button name=\"approve\" value=\"0\">Deny</button>\
             </form></body></html>",
            esc(&name),
            esc(host)
        );
        let status = if error.is_empty() {
            "200 OK"
        } else {
            "401 Unauthorized"
        };
        (status, page_headers(), "text/html; charset=utf-8", body)
    }

    /// POST /oauth/authorize: the login; a code back to the client.
    fn authorize(&self, f: &HashMap<String, String>, headers: &str) -> Reply {
        let (_, resource) = match self.check_request(f, headers) {
            Ok(c) => c,
            Err(r) => return r,
        };
        let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
        let redirect = get("redirect_uri").to_string();
        let state = f.get("state");
        if get("approve") != "1" {
            return redirect_to(&redirect, &[("error", "access_denied")], state);
        }
        let user = get("username");
        let Some(roles) = self.logins.verify(user, get("password")) else {
            return self.authorize_page(f, headers, Some("The username or password is wrong."));
        };
        let code = random_token();
        let mut st = self.state.lock().unwrap(); // justified: Mutex poison is unrecoverable
        let now = Instant::now();
        st.codes.retain(|_, c| c.expires > now);
        st.codes.insert(
            code.clone(),
            Code {
                client_id: get("client_id").to_string(),
                redirect_uri: redirect.clone(),
                challenge: get("code_challenge").to_string(),
                resource,
                user: user.to_string(),
                roles,
                expires: now + CODE_TTL,
            },
        );
        drop(st);
        redirect_to(&redirect, &[("code", &code)], state)
    }

    /// POST /oauth/token: the code exchange and the refresh (rotating).
    fn token(&self, f: &HashMap<String, String>, headers: &str) -> Reply {
        let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
        let mut st = self.state.lock().unwrap(); // justified: Mutex poison is unrecoverable
        let now = Instant::now();
        let grant = match get("grant_type") {
            "authorization_code" => {
                // single-use: taken before it is checked
                let Some(code) = st.codes.remove(get("code")) else {
                    return oauth_error("400 Bad Request", "invalid_grant", "unknown or used code");
                };
                let verifier_ok = get("code_verifier").len() >= 43
                    && base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(sha2::Sha256::digest(get("code_verifier").as_bytes()))
                        == code.challenge;
                let resource_ok = f
                    .get("resource")
                    .is_none_or(|r| r.trim_end_matches('/') == code.resource);
                if code.expires <= now
                    || code.client_id != get("client_id")
                    || code.redirect_uri != get("redirect_uri")
                    || !verifier_ok
                    || !resource_ok
                {
                    return oauth_error(
                        "400 Bad Request",
                        "invalid_grant",
                        "the code does not match this request",
                    );
                }
                Grant {
                    client_id: code.client_id,
                    user: code.user,
                    roles: code.roles,
                    resource: code.resource,
                    expires: now,
                }
            }
            "refresh_token" => {
                let Some(old) = st.refresh.remove(get("refresh_token")) else {
                    return oauth_error(
                        "400 Bad Request",
                        "invalid_grant",
                        "unknown or used refresh token",
                    );
                };
                if old.expires <= now || old.client_id != get("client_id") {
                    return oauth_error(
                        "400 Bad Request",
                        "invalid_grant",
                        "the refresh token is not valid for this client",
                    );
                }
                old
            }
            "" => {
                return oauth_error(
                    "400 Bad Request",
                    "invalid_request",
                    "grant_type is required",
                )
            }
            _ => {
                return oauth_error(
                    "400 Bad Request",
                    "unsupported_grant_type",
                    "authorization_code or refresh_token",
                )
            }
        };
        let _ = headers;
        st.access.retain(|_, g| g.expires > now);
        st.refresh.retain(|_, g| g.expires > now);
        if st.access.len() >= MAX_GRANTS || st.refresh.len() >= MAX_GRANTS {
            return oauth_error(
                "503 Service Unavailable",
                "temporarily_unavailable",
                "too many live tokens",
            );
        }
        let (access, refresh) = (random_token(), random_token());
        st.access.insert(
            access.clone(),
            Grant {
                expires: now + ACCESS_TTL,
                ..grant.clone()
            },
        );
        st.refresh.insert(
            refresh.clone(),
            Grant {
                expires: now + REFRESH_TTL,
                ..grant
            },
        );
        (
            "200 OK",
            no_store(),
            "application/json",
            json!({
                "access_token": access,
                "token_type": "Bearer",
                "expires_in": ACCESS_TTL.as_secs(),
                "refresh_token": refresh,
                "scope": "mcp",
            })
            .to_string(),
        )
    }
}

/// 256 random bits, URL-safe.
fn random_token() -> String {
    let mut raw = [0u8; 32];
    // getrandom fails only without an OS entropy source — unrecoverable
    getrandom::getrandom(&mut raw).expect("OS randomness");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// scheme, host, port, path+query of an absolute http(s) URI.
fn split_uri(u: &str) -> Option<(&str, &str, Option<&str>, &str)> {
    let (scheme, rest) = u.split_once("://")?;
    let cut = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(cut);
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (h, after) = v6.split_once(']')?;
            (h, after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        },
    };
    Some((scheme, host, port, if tail.is_empty() { "/" } else { tail }))
}

fn is_loopback_uri(u: &str) -> bool {
    split_uri(u)
        .is_some_and(|(s, h, _, _)| s == "http" && matches!(h, "localhost" | "127.0.0.1" | "::1"))
}

/// A registrable redirect: HTTPS, or loopback http (RFC 8252); no fragment.
fn redirect_allowed(u: &str) -> bool {
    !u.contains('#')
        && split_uri(u)
            .is_some_and(|(s, h, _, _)| !h.is_empty() && (s == "https" || is_loopback_uri(u)))
}

/// Exact match, except a loopback redirect matches on any port (RFC 8252
/// §7.3 — Claude Code redirects to an ephemeral port).
fn redirect_matches(registered: &str, requested: &str) -> bool {
    if registered == requested {
        return true;
    }
    match (split_uri(registered), split_uri(requested)) {
        (Some((s1, h1, _, p1)), Some((s2, h2, _, p2))) => {
            is_loopback_uri(registered) && s1 == s2 && h1 == h2 && p1 == p2
        }
        _ => false,
    }
}

/// application/x-www-form-urlencoded (and query strings) → map.
pub(crate) fn parse_form(s: &str) -> HashMap<String, String> {
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (crate::http::url_decode(k), crate::http::url_decode(v))
        })
        .collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn redirect_to(uri: &str, params: &[(&str, &str)], state: Option<&String>) -> Reply {
    let mut q: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    if let Some(s) = state {
        q.push(format!("state={}", enc(s)));
    }
    let sep = if uri.contains('?') { '&' } else { '?' };
    let mut h = no_store();
    h.push(("Location", format!("{uri}{sep}{}", q.join("&"))));
    ("302 Found", h, "text/plain", String::new())
}

fn no_store() -> Vec<(&'static str, String)> {
    vec![
        ("Cache-Control", "no-store".into()),
        ("Pragma", "no-cache".into()),
    ]
}

fn page_headers() -> Vec<(&'static str, String)> {
    let mut h = no_store();
    h.push(("X-Frame-Options", "DENY".into()));
    h.push((
        "Content-Security-Policy",
        "frame-ancestors 'none'; form-action 'self'".into(),
    ));
    h
}

fn page(status: &'static str, title: &str, msg: &str) -> Reply {
    (
        status,
        page_headers(),
        "text/html; charset=utf-8",
        format!(
            "<!doctype html><meta charset=\"utf-8\"><title>{t}</title><h1>{t}</h1><p>{}</p>",
            esc(msg),
            t = esc(title)
        ),
    )
}

fn ok_json(v: J) -> Reply {
    (
        "200 OK",
        vec![("Cache-Control", "no-store".into())],
        "application/json",
        v.to_string(),
    )
}

fn oauth_error(status: &'static str, error: &str, description: &str) -> Reply {
    (
        status,
        no_store(),
        "application/json",
        json!({"error": error, "error_description": description}).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects() {
        assert!(redirect_allowed("https://claude.ai/api/mcp/auth_callback"));
        assert!(redirect_allowed("http://localhost/callback"));
        assert!(redirect_allowed("http://127.0.0.1:5000/cb"));
        assert!(!redirect_allowed("http://evil.example/cb"));
        assert!(!redirect_allowed("https://x.example/cb#frag"));
        assert!(!redirect_allowed("javascript:alert(1)"));
        assert!(redirect_matches(
            "http://localhost/callback",
            "http://localhost:5555/callback"
        ));
        assert!(!redirect_matches(
            "http://localhost/callback",
            "http://localhost:5555/other"
        ));
        assert!(!redirect_matches(
            "http://localhost/callback",
            "http://127.0.0.1:5555/callback"
        ));
        assert!(
            !redirect_matches("https://claude.ai/cb", "https://claude.ai:8443/cb"),
            "only loopback ignores the port"
        );
    }

    #[test]
    fn form_decoding_handles_utf8_plus_and_a_trailing_escape() {
        let f = parse_form("a=caf%C3%A9&b=x+y&c=pw%21");
        assert_eq!(f["a"], "café");
        assert_eq!(f["b"], "x y");
        assert_eq!(f["c"], "pw!");
        // a '%' before a multi-byte character is kept, never a panic
        assert_eq!(parse_form("d=%é1")["d"], "%é1");
        assert_eq!(parse_form("e=100%")["e"], "100%");
    }
}
