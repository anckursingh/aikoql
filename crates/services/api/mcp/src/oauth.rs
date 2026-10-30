//! OAuth 2.1 for MCP over HTTP — the built-in authorization server Claude's
//! connectors (claude.ai, Desktop, mobile, Claude Code) sign in through.
//! Per the MCP authorization spec (2025-06-18) and Claude's connector docs:
//! RFC 9728 protected resource metadata (a 401 on /mcp points at it), RFC
//! 8414 authorization server metadata, RFC 7591 dynamic registration of
//! public clients, the authorization-code grant with S256 PKCE behind a
//! login + consent page (the server's argon2 `[auth]` logins), and rotating
//! refresh tokens. Tokens are opaque and bound to the MCP resource they were
//! issued for (the audience check). Registered clients and grants live in
//! the database under `sys/oauth/` — keyed by the token's SHA-256, never the
//! token — so a sign-in survives restarts and failovers, and is encrypted,
//! replicated and backed up with the data. Authorization codes (10 minutes,
//! single-use) stay in memory.

use crate::http::AuthResolver;
use crate::mcp_http::{header, Reply};
use crate::{json, Arc, HashMap, Mutex, J};
use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};
use base64::Engine as _;
use sha2::Digest as _;
use std::time::{Duration, Instant};

const CODE_TTL: Duration = Duration::from_secs(600);
const ACCESS_TTL: Duration = Duration::from_secs(3600);
const REFRESH_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// Registered clients held at once (DCR registers one per fresh connection).
const MAX_CLIENTS: usize = 10_000;
/// Expired grants are swept from the store every this many issuances.
const SWEEP_EVERY: u64 = 256;
const P_CLIENT: &str = "sys/oauth/client/";
const P_ACCESS: &str = "sys/oauth/access/";
const P_REFRESH: &str = "sys/oauth/refresh/";

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
    expires: Instant,
}

/// What an access or refresh token stands for — stored as JSON under the
/// token's SHA-256. The roles are not stored: each check reads the login's
/// current ones, so a removed login (or role) stops working at once.
#[derive(Clone)]
struct Grant {
    client_id: String,
    user: String,
    resource: String,
    /// unix seconds (an Instant does not outlive the process)
    expires: u64,
}

impl Grant {
    fn to_json(&self) -> Vec<u8> {
        json!({"client_id": self.client_id, "user": self.user, "resource": self.resource, "expires": self.expires})
            .to_string()
            .into_bytes()
    }

    fn from_json(raw: &[u8]) -> Option<Grant> {
        let v: J = serde_json::from_slice(raw).ok()?;
        Some(Grant {
            client_id: v.get("client_id")?.as_str()?.to_string(),
            user: v.get("user")?.as_str()?.to_string(),
            resource: v.get("resource")?.as_str()?.to_string(),
            expires: v.get("expires")?.as_u64()?,
        })
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A token's store key: its SHA-256, so a copied data directory or backup
/// holds no usable token.
fn token_key(prefix: &str, token: &str) -> Vec<u8> {
    let d = sha2::Sha256::digest(token.as_bytes());
    let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}{hex}").into_bytes()
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
    /// the kernel's store (the encrypted one when encryption is on)
    store: Arc<dyn StorageEngine>,
    codes: Mutex<HashMap<String, Code>>,
    /// serializes the token endpoint's read-check-write (rotation)
    issuing: Mutex<u64>,
}

impl OAuth {
    pub(crate) fn new(
        logins: Arc<AuthResolver>,
        https: bool,
        store: Arc<dyn StorageEngine>,
    ) -> Self {
        let public_url = std::env::var("AIKOQL_PUBLIC_URL")
            .ok()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        OAuth {
            logins,
            https,
            public_url,
            store,
            codes: Mutex::new(HashMap::new()),
            issuing: Mutex::new(0),
        }
    }

    fn client(&self, id: &str) -> Option<Client> {
        let raw = self
            .store
            .get(format!("{P_CLIENT}{id}").as_bytes())
            .ok()??;
        let v: J = serde_json::from_slice(&raw).ok()?;
        Some(Client {
            name: v.get("name")?.as_str()?.to_string(),
            redirect_uris: v
                .get("redirect_uris")?
                .as_array()?
                .iter()
                .filter_map(|u| u.as_str().map(String::from))
                .collect(),
        })
    }

    fn grant(&self, prefix: &str, token: &str) -> Option<Grant> {
        let raw = self.store.get(&token_key(prefix, token)).ok()??;
        Grant::from_json(&raw)
    }

    /// Deletes the store's expired grants.
    fn sweep(&self) {
        let now = unix_now();
        let mut batch = WriteBatch::new();
        for prefix in [P_ACCESS, P_REFRESH] {
            for (k, v) in self.store.scan(prefix.as_bytes()).unwrap_or_default() {
                if Grant::from_json(&v).is_none_or(|g| g.expires <= now) {
                    batch.del(k);
                }
            }
        }
        if !batch.is_empty() {
            let _ = self.store.write_batch(&batch);
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
        let g = self.grant(P_ACCESS, token)?;
        // the audience: a token issued for another resource is refused
        if g.expires <= unix_now() || g.resource != format!("{}/mcp", self.base(headers)) {
            return None;
        }
        // the login's current roles; a removed login is refused
        let roles = self.logins.roles_of(&g.user)?;
        Some(OAuthIdentity {
            principal: format!("oauth:{}:{}", g.client_id, g.user),
            user: g.user,
            roles,
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
        let registered = self.store.scan(P_CLIENT.as_bytes()).map_or(0, |c| c.len());
        if registered >= MAX_CLIENTS {
            return oauth_error(
                "503 Service Unavailable",
                "temporarily_unavailable",
                "too many registered clients",
            );
        }
        let mut batch = WriteBatch::new();
        batch.put(
            format!("{P_CLIENT}{id}").into_bytes(),
            json!({"name": name, "redirect_uris": uris})
                .to_string()
                .into_bytes(),
        );
        if self.store.write_batch(&batch).is_err() {
            // a read replica's store refuses writes
            return oauth_error(
                "503 Service Unavailable",
                "temporarily_unavailable",
                "this server cannot register clients now (a read replica?)",
            );
        }
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
        let Some(client) = self.client(get("client_id")) else {
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
        // the redirect is registered (check_request); its origin, no path
        let origin = redirect
            .split_once("://")
            .map(|(scheme, _)| format!(" {scheme}://{host}"))
            .unwrap_or_default();
        (
            status,
            page_headers(&origin),
            "text/html; charset=utf-8",
            body,
        )
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
        if self.logins.verify(user, get("password")).is_none() {
            return self.authorize_page(f, headers, Some("The username or password is wrong."));
        }
        let code = random_token();
        let mut codes = self.codes.lock().unwrap(); // justified: Mutex poison is unrecoverable
        let now = Instant::now();
        codes.retain(|_, c| c.expires > now);
        codes.insert(
            code.clone(),
            Code {
                client_id: get("client_id").to_string(),
                redirect_uri: redirect.clone(),
                challenge: get("code_challenge").to_string(),
                resource,
                user: user.to_string(),
                expires: now + CODE_TTL,
            },
        );
        drop(codes);
        redirect_to(&redirect, &[("code", &code)], state)
    }

    /// POST /oauth/token: the code exchange and the refresh (rotating).
    fn token(&self, f: &HashMap<String, String>, headers: &str) -> Reply {
        let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
        // one issuance at a time: a refresh token cannot rotate twice
        let mut issued = self.issuing.lock().unwrap(); // justified: Mutex poison is unrecoverable
        let now = unix_now();
        let mut batch = WriteBatch::new();
        let grant = match get("grant_type") {
            "authorization_code" => {
                // single-use: taken before it is checked
                let taken = self.codes.lock().unwrap().remove(get("code")); // justified: Mutex poison is unrecoverable
                let Some(code) = taken else {
                    return oauth_error("400 Bad Request", "invalid_grant", "unknown or used code");
                };
                let verifier_ok = get("code_verifier").len() >= 43
                    && base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(sha2::Sha256::digest(get("code_verifier").as_bytes()))
                        == code.challenge;
                let resource_ok = f
                    .get("resource")
                    .is_none_or(|r| r.trim_end_matches('/') == code.resource);
                if code.expires <= Instant::now()
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
                    resource: code.resource,
                    expires: now,
                }
            }
            "refresh_token" => {
                let Some(old) = self.grant(P_REFRESH, get("refresh_token")) else {
                    return oauth_error(
                        "400 Bad Request",
                        "invalid_grant",
                        "unknown or used refresh token",
                    );
                };
                // rotated: the old refresh token dies in the same batch
                batch.del(token_key(P_REFRESH, get("refresh_token")));
                if old.expires <= now
                    || old.client_id != get("client_id")
                    || self.logins.roles_of(&old.user).is_none()
                {
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
        let (access, refresh) = (random_token(), random_token());
        let access_grant = Grant {
            expires: now + ACCESS_TTL.as_secs(),
            ..grant.clone()
        };
        let refresh_grant = Grant {
            expires: now + REFRESH_TTL.as_secs(),
            ..grant
        };
        batch.put(token_key(P_ACCESS, &access), access_grant.to_json());
        batch.put(token_key(P_REFRESH, &refresh), refresh_grant.to_json());
        // one atomic batch: the old refresh token dies exactly when the new
        // pair is born, and a crash leaves one of the two, never both
        if self.store.write_batch(&batch).is_err() {
            return oauth_error(
                "503 Service Unavailable",
                "temporarily_unavailable",
                "this server cannot issue tokens now (a read replica?)",
            );
        }
        *issued += 1;
        if issued.is_multiple_of(SWEEP_EVERY) {
            self.sweep();
        }
        drop(issued);
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

/// `allow` is an extra form-action origin: Chrome applies form-action to
/// the redirect after the POST, so the sign-in form must allow the client's.
fn page_headers(allow: &str) -> Vec<(&'static str, String)> {
    let mut h = no_store();
    h.push(("X-Frame-Options", "DENY".into()));
    h.push((
        "Content-Security-Policy",
        format!("frame-ancestors 'none'; form-action 'self'{allow}"),
    ));
    h
}

fn page(status: &'static str, title: &str, msg: &str) -> Reply {
    (
        status,
        page_headers(""),
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

    /// One full sign-in against a store: (client_id, access, refresh).
    fn sign_in(o: &OAuth, h: &str) -> (String, String, String) {
        let reg = o.register(r#"{"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"]}"#);
        let client = serde_json::from_str::<J>(&reg.3).unwrap()["client_id"]
            .as_str()
            .unwrap()
            .to_string();
        let verifier = "v".repeat(50);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()));
        let form: HashMap<String, String> = [
            ("response_type", "code"),
            ("client_id", client.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("username", "admin"),
            ("password", "pw"),
            ("approve", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let r = o.authorize(&form, h);
        let loc =
            r.1.iter()
                .find(|(k, _)| *k == "Location")
                .unwrap()
                .1
                .clone();
        let code = loc
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_string();
        let tok: HashMap<String, String> = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", client.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("code_verifier", verifier.as_str()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let t: J = serde_json::from_str(&o.token(&tok, h).3).unwrap();
        (
            client,
            t["access_token"].as_str().unwrap().into(),
            t["refresh_token"].as_str().unwrap().into(),
        )
    }

    fn refresh(o: &OAuth, client: &str, token: &str, h: &str) -> J {
        let f: HashMap<String, String> = [
            ("grant_type", "refresh_token"),
            ("refresh_token", token),
            ("client_id", client),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        serde_json::from_str(&o.token(&f, h).3).unwrap()
    }

    const H: &str = "POST /mcp HTTP/1.1\r\nHost: kb.test\r\n";

    fn store() -> Arc<dyn StorageEngine> {
        Arc::new(aikoql_kernel::storage::store::MemoryEngine::default())
    }

    #[test]
    fn the_store_holds_no_usable_token() {
        let st = store();
        let o = OAuth::new(
            Arc::new(AuthResolver::new(vec![], Some("pw"), 3600)),
            true,
            st.clone(),
        );
        let (_, access, refresh) = sign_in(&o, H);
        for (k, v) in st.scan(b"sys/oauth/").unwrap() {
            let (k, v) = (String::from_utf8_lossy(&k), String::from_utf8_lossy(&v));
            for t in [&access, &refresh] {
                assert!(
                    !k.contains(t.as_str()) && !v.contains(t.as_str()),
                    "a raw token is stored: {k}"
                );
            }
        }
        assert!(o.validate(&access, H).is_some());
    }

    #[test]
    fn a_removed_login_loses_its_tokens() {
        let st = store();
        let o = OAuth::new(
            Arc::new(AuthResolver::new(vec![], Some("pw"), 3600)),
            true,
            st.clone(),
        );
        let (client, access, refresh_token) = sign_in(&o, H);
        // the same store, the login gone (a restart with a changed [auth])
        let gone = OAuth::new(Arc::new(AuthResolver::new(vec![], None, 3600)), true, st);
        assert!(
            gone.validate(&access, H).is_none(),
            "the access token of a removed login"
        );
        assert_eq!(
            refresh(&gone, &client, &refresh_token, H)["error"],
            json!("invalid_grant")
        );
    }

    /// Chrome applies form-action to the redirect after the POST too: the
    /// sign-in page must allow the client's (registered) redirect origin,
    /// or the browser stays on the page after a correct login.
    #[test]
    fn the_sign_in_page_lets_the_form_redirect_to_the_client() {
        let o = OAuth::new(
            Arc::new(AuthResolver::new(vec![], Some("pw"), 3600)),
            true,
            store(),
        );
        let reg = o.register(r#"{"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"]}"#);
        let client = serde_json::from_str::<J>(&reg.3).unwrap()["client_id"]
            .as_str()
            .unwrap()
            .to_string();
        let q: HashMap<String, String> = [
            ("response_type", "code"),
            ("client_id", client.as_str()),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("code_challenge", "c".repeat(43).as_str()),
            ("code_challenge_method", "S256"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (what, page) in [
            ("the page", o.authorize_page(&q, H, None)),
            ("a wrong password", o.authorize_page(&q, H, Some("wrong"))),
        ] {
            let csp = &page
                .1
                .iter()
                .find(|(k, _)| *k == "Content-Security-Policy")
                .unwrap()
                .1;
            assert!(
                csp.contains("form-action 'self' https://claude.ai;")
                    || csp.ends_with("form-action 'self' https://claude.ai"),
                "{what}: {csp}"
            );
        }
    }

    #[test]
    fn a_refresh_token_rotates_once() {
        let o = OAuth::new(
            Arc::new(AuthResolver::new(vec![], Some("pw"), 3600)),
            true,
            store(),
        );
        let (client, _, refresh_token) = sign_in(&o, H);
        assert!(refresh(&o, &client, &refresh_token, H)["access_token"].is_string());
        assert_eq!(
            refresh(&o, &client, &refresh_token, H)["error"],
            json!("invalid_grant")
        );
    }

    #[test]
    fn redirects() {
        assert!(redirect_allowed("https://claude.ai/api/mcp/auth_callback"));
        assert!(redirect_allowed("http://localhost/callback"));
        assert!(redirect_allowed("http://127.0.0.1:5000/cb"));
        assert!(!redirect_allowed("http://evil.example/cb"));
        assert!(!redirect_allowed("https://x.example/cb#frag"));
        assert!(!redirect_allowed("javascript:alert(1)"));
        // the URI lands in Location and the CSP header: no header or
        // directive breaks, no stray authority characters
        assert!(!redirect_allowed("https://a.example/cb\r\nSet-Cookie: x=1"));
        assert!(!redirect_allowed("https://a.example;script-src */cb"));
        assert!(!redirect_allowed("https://a.example x/cb"));
        assert!(!redirect_allowed("https://a.example:12x/cb"));
        assert!(redirect_allowed("https://a-b.example:8443/cb?x=1"));
        assert!(redirect_allowed("http://[::1]:5000/cb"));
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
