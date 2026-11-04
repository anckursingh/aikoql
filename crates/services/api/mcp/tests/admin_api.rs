//! Task 8: the admin API — GET /api/license (any authenticated session),
//! GET/POST /api/users and PUT /api/users/{sub}/roles (admin only).
//! Enterprise servers spawn with a TEST-keypair multi-user license (the
//! debug-only AIKOQL_LICENSE_PUBLIC_KEY override); the community server
//! spawns with no license at all and must report tier "community" while
//! every v0.4.0 route stays byte-for-byte unchanged.

mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value as J};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// A licensed multi-user server (bootstrap admin password "pw").
fn serve(tag: &str, expires: Option<&str>) -> common::Server {
    let (pk, blob) = common::sign("acme-corp", expires.map(String::from), &["multi-user"]);
    let pk_b64 = URL_SAFE_NO_PAD.encode(&pk);
    let lic = std::env::temp_dir().join(format!("aikoql-admin-{tag}-{}.lic", std::process::id()));
    std::fs::write(&lic, &blob).unwrap();
    common::register_temp_path(&lic);
    let db = std::env::temp_dir().join(format!("aikoql-admin-{tag}-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-admin-{tag}-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let lic_str = lic.to_str().unwrap();
    let envs = [
        ("AIKOQL_LICENSE", lic_str),
        ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ("AIKOQL_ADMIN_PASSWORD", "pw"),
    ];
    let srv = common::spawn_server(&db, &log, &envs, &[]);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(
        health.starts_with("HTTP/1.") && health.contains(" 200 "),
        "health: {health}"
    );
    srv
}

struct Resp {
    status: u16,
    body: String,
}

/// One HTTP request; the server closes after the response.
fn http(port: u16, method: &str, path: &str, body: &str) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    s.flush().unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let raw = String::from_utf8_lossy(&out);
    let status: u16 = raw
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    Resp { status, body }
}

/// POST /api/login as the bootstrap admin ("pw") → session token.
fn admin_token(port: u16) -> String {
    let r = http(
        port,
        "POST",
        "/api/login",
        r#"{"username":"admin","password":"pw"}"#,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let j: J = serde_json::from_str(&r.body).expect("json");
    j["token"].as_str().expect("token").to_string()
}

/// Overwrite the stored roles (the store is the authority). The server may
/// hold the file briefly — retry past SQLITE_BUSY.
fn set_roles(db: &std::path::Path, sub: &str, roles: &[&str]) {
    let conn = rusqlite::Connection::open(db.join("users.db")).expect("users.db");
    let roles_json = serde_json::to_string(roles).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match conn.execute(
            "UPDATE users SET roles = ?1 WHERE sub = ?2",
            rusqlite::params![roles_json, sub],
        ) {
            Ok(_) => return,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                assert!(Instant::now() < deadline, "users.db stayed busy");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("set roles: {e}"),
        }
    }
}

#[test]
fn license_endpoint_reports_enterprise() {
    // an expiry far in the future exercises the expires mapping
    let srv = serve("lic", Some("2099-12-31"));
    let token = admin_token(srv.port);
    let r = http(srv.port, "GET", &format!("/api/license?token={token}"), "");
    assert_eq!(r.status, 200, "{}", r.body);
    let j: J = serde_json::from_str(&r.body).expect("json");
    assert_eq!(j["tier"], "enterprise", "{}", r.body);
    assert_eq!(j["org"], "acme-corp", "{}", r.body);
    let features: Vec<&str> = j["features"]
        .as_array()
        .expect("features array")
        .iter()
        .filter_map(|f| f.as_str())
        .collect();
    assert!(features.contains(&"multi-user"), "{}", r.body);
    let expires = j["expires"].as_u64().expect("expires is unix seconds");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(expires > now, "expiry must be in the future: {}", r.body);
}

#[test]
fn license_endpoint_says_community_when_unlicensed() {
    let db = std::env::temp_dir().join(format!("aikoql-admin-parity-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-admin-parity-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let srv = common::spawn_server(&db, &log, &[("AIKOQL_ADMIN_PASSWORD", "pw")], &[]);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(
        health.starts_with("HTTP/1.") && health.contains(" 200 "),
        "health: {health}"
    );

    // without a session the route is closed like every other one
    let anon = http(srv.port, "GET", "/api/license", "");
    assert_eq!(anon.status, 401, "{}", anon.body);

    // v0.4.0 password login works without a license
    let token = admin_token(srv.port);
    let r = http(srv.port, "GET", &format!("/api/license?token={token}"), "");
    assert_eq!(r.status, 200, "{}", r.body);
    let j: J = serde_json::from_str(&r.body).expect("json");
    assert_eq!(j["tier"], "community", "{}", r.body);
    assert_eq!(j["org"], "", "{}", r.body);
    assert_eq!(j["features"], json!([]), "{}", r.body);
    assert!(j["expires"].is_null(), "{}", r.body);
}

#[test]
fn users_list_requires_admin() {
    let srv = serve("list", None);
    let token = admin_token(srv.port);
    let ok = http(srv.port, "GET", &format!("/api/users?token={token}"), "");
    assert_eq!(ok.status, 200, "{}", ok.body);
    let j: J = serde_json::from_str(&ok.body).expect("json");
    let users = j["users"].as_array().expect("users array");
    assert!(
        users
            .iter()
            .any(|u| u["sub"] == "admin" && u["roles"] == json!(["admin"])),
        "the bootstrap admin must be listed: {}",
        ok.body
    );
    // the store is authoritative: flip the admin to viewer and the SAME
    // session is refused on its very next request
    set_roles(&srv.db, "admin", &["viewer"]);
    let denied = http(srv.port, "GET", &format!("/api/users?token={token}"), "");
    assert_eq!(
        denied.status, 403,
        "viewer must not list users: {} {}",
        denied.status, denied.body
    );
    assert!(denied.body.contains("admin role required"), "{}", denied.body);
    // restored, the route serves again
    set_roles(&srv.db, "admin", &["admin"]);
    let token = admin_token(srv.port);
    let ok = http(srv.port, "GET", &format!("/api/users?token={token}"), "");
    assert_eq!(ok.status, 200, "{}", ok.body);
}

#[test]
fn viewer_cannot_create_user() {
    let srv = serve("create", None);
    set_roles(&srv.db, "admin", &["viewer"]);
    let token = admin_token(srv.port);
    let denied = http(
        srv.port,
        "POST",
        &format!("/api/users?token={token}"),
        r#"{"sub":"mallory","roles":["admin"]}"#,
    );
    assert_eq!(
        denied.status, 403,
        "viewer must not create users: {} {}",
        denied.status, denied.body
    );
    assert!(denied.body.contains("admin role required"), "{}", denied.body);
    // fail-closed: the 403 came before the write — restore and confirm
    set_roles(&srv.db, "admin", &["admin"]);
    let token = admin_token(srv.port);
    let list = http(srv.port, "GET", &format!("/api/users?token={token}"), "");
    assert!(
        !list.body.contains("mallory"),
        "the viewer's user must not exist: {}",
        list.body
    );
}

#[test]
fn create_user_preprovisions_oidc_sub() {
    let srv = serve("provision", None);
    let token = admin_token(srv.port);
    let r = http(
        srv.port,
        "POST",
        &format!("/api/users?token={token}"),
        r#"{"sub":"alice-oidc","name":"Alice","roles":["viewer"]}"#,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    // a sub with no name/roles provisions with defaults
    let bare = http(
        srv.port,
        "POST",
        &format!("/api/users?token={token}"),
        r#"{"sub":"bob-oidc"}"#,
    );
    assert_eq!(bare.status, 200, "{}", bare.body);
    // both rows are in the store — a later SSO login's upsert finds them
    let list = http(srv.port, "GET", &format!("/api/users?token={token}"), "");
    assert_eq!(list.status, 200, "{}", list.body);
    let j: J = serde_json::from_str(&list.body).expect("json");
    let users = j["users"].as_array().expect("users array");
    let alice = users
        .iter()
        .find(|u| u["sub"] == "alice-oidc")
        .expect("alice row");
    assert_eq!(alice["name"], "Alice", "{}", list.body);
    assert_eq!(alice["roles"], json!(["viewer"]), "{}", list.body);
    let bob = users
        .iter()
        .find(|u| u["sub"] == "bob-oidc")
        .expect("bob row");
    assert_eq!(bob["roles"], json!([]), "{}", list.body);
}

#[test]
fn unlicensed_user_routes_are_v040_404() {
    let db = std::env::temp_dir().join(format!("aikoql-admin-404-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-admin-404-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let srv = common::spawn_server(&db, &log, &[("AIKOQL_ADMIN_PASSWORD", "pw")], &[]);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(
        health.starts_with("HTTP/1.") && health.contains(" 200 "),
        "health: {health}"
    );
    let token = admin_token(srv.port);
    // community mode has no user store: the admin user routes never existed
    // in v0.4.0 and must keep answering the plain-text 404 — no 401, no
    // JSON, no "user store unavailable" — for authenticated sessions too
    let list = http(srv.port, "GET", &format!("/api/users?token={token}"), "");
    assert_eq!(list.status, 404, "{}", list.body);
    assert_eq!(list.body, "Not Found\n", "v0.4.0 plain-text 404: {}", list.body);
    let sub = http(srv.port, "GET", "/api/users/any-subpath", "");
    assert_eq!(sub.status, 404, "{}", sub.body);
    assert_eq!(sub.body, "Not Found\n", "v0.4.0 plain-text 404: {}", sub.body);
    let sub_auth = http(
        srv.port,
        "GET",
        &format!("/api/users/any-subpath?token={token}"),
        "",
    );
    assert_eq!(sub_auth.status, 404, "{}", sub_auth.body);
    assert_eq!(sub_auth.body, "Not Found\n", "v0.4.0 plain-text 404: {}", sub_auth.body);
}

#[test]
fn set_roles_updates_store() {
    let srv = serve("put", None);
    let token = admin_token(srv.port);
    let create = http(
        srv.port,
        "POST",
        &format!("/api/users?token={token}"),
        r#"{"sub":"carol","roles":["viewer"]}"#,
    );
    assert_eq!(create.status, 200, "{}", create.body);
    let put = http(
        srv.port,
        "PUT",
        &format!("/api/users/carol/roles?token={token}"),
        r#"{"roles":["viewer","admin"]}"#,
    );
    assert_eq!(put.status, 200, "{}", put.body);
    // the store reflects the change (roles_of through the list route)
    let list = http(srv.port, "GET", &format!("/api/users?token={token}"), "");
    let j: J = serde_json::from_str(&list.body).expect("json");
    let carol = j["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["sub"] == "carol")
        .expect("carol row");
    assert_eq!(carol["roles"], json!(["viewer", "admin"]), "{}", list.body);
    // an unknown sub is a 404, never a silent no-op
    let missing = http(
        srv.port,
        "PUT",
        &format!("/api/users/nobody/roles?token={token}"),
        r#"{"roles":["admin"]}"#,
    );
    assert_eq!(missing.status, 404, "{}", missing.body);
}
