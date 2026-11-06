//! Studio session cookie: password login also sets an HttpOnly,
//! SameSite=Strict `aikoql_session` cookie; a cookie-authenticated request
//! that writes must carry the custom `X-Aikoql-Csrf` header (a cross-site
//! page cannot add it without a CORS preflight); Bearer clients are
//! unchanged; logout clears the cookie and ends the session.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

fn serve(tag: &str) -> common::Server {
    let db = std::env::temp_dir().join(format!("aikoql-cookie-{tag}-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-cookie-{tag}-{}.log", std::process::id()));
    common::register_temp_path(&log);
    let srv = common::spawn_server(&db, &log, &[("AIKOQL_ADMIN_PASSWORD", "pw")], &[]);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(health.contains(" 200 "), "health: {health}");
    srv
}

struct Resp {
    status: u16,
    head: String,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }
}

fn http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let raw = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Resp {
        status,
        head: head.to_string(),
        body: body.to_string(),
    }
}

/// Log in; returns (bearer token, the `aikoql_session=…` pair to send back).
fn login(port: u16) -> (String, String, Resp) {
    let r = http(
        port,
        "POST",
        "/api/login",
        &[],
        r#"{"username":"admin","password":"pw"}"#,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let token = serde_json::from_str::<serde_json::Value>(&r.body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let cookie = r.header("set-cookie").unwrap_or_default();
    let pair = cookie.split(';').next().unwrap_or("").to_string();
    (token, pair, r)
}

#[test]
fn ck001_login_sets_an_httponly_samesite_strict_session_cookie() {
    let srv = serve("001");
    let (token, pair, r) = login(srv.port);
    let cookie = r.header("set-cookie").expect("Set-Cookie on login");
    assert_eq!(pair, format!("aikoql_session={token}"), "{cookie}");
    for attr in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age="] {
        assert!(cookie.contains(attr), "{attr} missing: {cookie}");
    }
    assert!(!cookie.contains("Secure"), "plain HTTP must not be Secure");
}

#[test]
fn ck002_the_cookie_authenticates_reads_and_a_missing_login_does_not() {
    let srv = serve("002");
    let (_, pair, _) = login(srv.port);
    let ok = http(
        srv.port,
        "GET",
        "/api/v1/ontology",
        &[("Cookie", &pair)],
        "",
    );
    assert_eq!(ok.status, 200, "{}", ok.body);
    let no = http(srv.port, "GET", "/api/v1/ontology", &[], "");
    assert_eq!(no.status, 401, "{}", no.body);
}

#[test]
fn ck003_a_cookie_authenticated_write_needs_the_csrf_header() {
    let srv = serve("003");
    let (token, pair, _) = login(srv.port);
    let w = |h: &[(&str, &str)]| http(srv.port, "POST", "/api/v1/ontology", h, "{}");
    let blocked = w(&[("Cookie", &pair)]);
    assert_eq!(blocked.status, 403, "{}", blocked.body);
    let allowed = w(&[("Cookie", &pair), ("X-Aikoql-Csrf", "1")]);
    assert!(
        ![401, 403].contains(&allowed.status),
        "{} {}",
        allowed.status,
        allowed.body
    );
    // a Bearer client never carried ambient credentials: unchanged
    let bearer = w(&[("Authorization", &format!("Bearer {token}"))]);
    assert!(![401, 403].contains(&bearer.status), "{}", bearer.status);
}

#[test]
fn ck004_logout_clears_the_cookie_and_ends_the_session() {
    let srv = serve("004");
    let (_, pair, _) = login(srv.port);
    let out = http(
        srv.port,
        "POST",
        "/api/logout",
        &[("Cookie", &pair), ("X-Aikoql-Csrf", "1")],
        "",
    );
    assert_eq!(out.status, 200, "{}", out.body);
    let cleared = out.header("set-cookie").expect("cookie cleared");
    assert!(
        cleared.contains("Max-Age=0") && cleared.starts_with("aikoql_session="),
        "{cleared}"
    );
    let after = http(
        srv.port,
        "GET",
        "/api/v1/ontology",
        &[("Cookie", &pair)],
        "",
    );
    assert_eq!(after.status, 401, "session survived logout: {}", after.body);
}
