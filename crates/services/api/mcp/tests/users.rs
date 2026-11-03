//! Enterprise multi-user store (Task 3). aikoql-mcp is a binary crate (no
//! lib target to link), so users.rs is compiled directly into this test
//! crate for the unit tests; the integration tests spawn the debug binary
//! with a TEST keypair license via the debug-only AIKOQL_LICENSE_PUBLIC_KEY
//! override (release builds ignore the env).

#[path = "../src/users.rs"]
#[allow(dead_code)] // production-only items (UserRow, ADMIN_SUB, …)
mod users;

use users::UserStore;

mod common;

static STORE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A fresh store file unique to this test process, swept at thread exit.
fn tmp_store(tag: &str) -> (UserStore, std::path::PathBuf) {
    let n = STORE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "aikoql-users-{tag}-{}-{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    common::register_temp_path(&p);
    let store = UserStore::open(&p).expect("open a fresh user store");
    (store, p)
}

#[test]
fn bootstrap_admin_seeded_once() {
    let (store, _) = tmp_store("seed");
    assert!(!store.has_admin());
    assert!(store.seed_bootstrap_admin("change-me").unwrap());
    assert!(store.has_admin());
    let rows = store.list_users().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        store.roles_of(users::ADMIN_SUB).unwrap(),
        Some(vec!["admin".to_string()])
    );
    // created_at is RFC 3339 UTC ("YYYY-MM-DDTHH:MM:SSZ")
    assert!(rows[0].created_at.ends_with('Z') && rows[0].created_at.len() == 20);
    // the second seed finds the admin and changes nothing
    assert!(!store.seed_bootstrap_admin("different").unwrap());
    assert_eq!(store.list_users().unwrap().len(), 1);
    // the stored hash is the break-glass credential (Task 5 verifies here)
    assert!(store.verify_admin_password("change-me").unwrap());
    assert!(!store.verify_admin_password("wrong").unwrap());
}

#[test]
fn oidc_upsert_and_roles() {
    let (store, _) = tmp_store("oidc");
    store
        .upsert_oidc_user("alice", Some("Alice"), &["viewer".to_string()])
        .unwrap();
    assert_eq!(
        store.roles_of("alice").unwrap(),
        Some(vec!["viewer".to_string()])
    );
    // an OIDC login may claim admin — the spec's "seeded or claimed"
    store
        .upsert_oidc_user("bob", None, &["admin".to_string()])
        .unwrap();
    assert!(store.has_admin());
    // an OIDC-claimed admin carries no stored hash → verify is a clean false
    assert!(!store.verify_admin_password("anything").unwrap());
    // a re-upsert refreshes the roles, never duplicates the row
    store
        .upsert_oidc_user(
            "alice",
            Some("Alice"),
            &["viewer".to_string(), "admin".to_string()],
        )
        .unwrap();
    assert_eq!(
        store.roles_of("alice").unwrap(),
        Some(vec!["viewer".to_string(), "admin".to_string()])
    );
    assert_eq!(store.list_users().unwrap().len(), 2);
}

#[test]
fn set_roles_overwrites() {
    let (store, _) = tmp_store("roles");
    store
        .upsert_oidc_user("carol", None, &["viewer".to_string()])
        .unwrap();
    store.set_roles("carol", &["admin".to_string()]).unwrap();
    assert_eq!(
        store.roles_of("carol").unwrap(),
        Some(vec!["admin".to_string()])
    );
    store
        .set_roles("carol", &["viewer".to_string(), "admin".to_string()])
        .unwrap();
    assert_eq!(
        store.roles_of("carol").unwrap(),
        Some(vec!["viewer".to_string(), "admin".to_string()])
    );
    assert_eq!(store.list_users().unwrap().len(), 1); // replaced, not duplicated
}

#[test]
fn roles_of_unknown_sub_none() {
    let (store, _) = tmp_store("unknown");
    assert_eq!(store.roles_of("nobody").unwrap(), None);
}

// --- startup wiring (spawns the aikoql-mcp binary) ---

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use std::io::{BufRead, Write};
use std::net::TcpStream;

/// A licensed server with multi-user on and no admin anywhere (no
/// AIKOQL_ADMIN_PASSWORD) must refuse to start: non-zero exit and the
/// operator's error message.
#[test]
fn multi_user_without_admin_refuses_start() {
    let (pk, blob) = common::sign("test", None, &["multi-user"]);
    let lic = std::env::temp_dir().join(format!("aikoql-users-refuse-{}.lic", std::process::id()));
    std::fs::write(&lic, &blob).unwrap();
    common::register_temp_path(&lic);
    let db = std::env::temp_dir().join(format!("aikoql-users-refuse-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-users-refuse-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let lic_str = lic.to_str().unwrap();
    let pk_b64 = URL_SAFE_NO_PAD.encode(&pk);
    let mut srv = common::spawn_server(
        &db,
        &log,
        &[
            ("AIKOQL_LICENSE", lic_str),
            ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
            // no AIKOQL_ADMIN_PASSWORD — the refusal under test
        ],
        &[],
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = common::wait_exit(&mut srv.child, deadline);
    assert!(
        !status.success(),
        "a licensed multi-user server with no admin must refuse to start"
    );
    let log_text = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        log_text.contains("multi-user requires a bootstrap admin (set AIKOQL_ADMIN_PASSWORD)"),
        "stderr must name the missing bootstrap admin: {log_text}"
    );
}

/// No license anywhere ⇒ exactly v0.4.0 behavior: TCP-token MCP auth and
/// password login work as before, and no enterprise users.db is created.
#[test]
fn unlicensed_server_unchanged() {
    let db = std::env::temp_dir().join(format!("aikoql-users-parity-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-users-parity-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let mcp_port = common::free_port();
    let mcp_addr = format!("127.0.0.1:{mcp_port}");
    let extra = [
        "--listen",
        mcp_addr.as_str(),
        "--tcp-token",
        "parity-tok:acme:admin",
    ];
    let _srv = common::spawn_server(&db, &log, &[("AIKOQL_ADMIN_PASSWORD", "parity-pw")], &extra);

    // v0.4.0 TCP-token MCP auth: initialize with the token part succeeds
    let mut stream = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match TcpStream::connect(("127.0.0.1", mcp_port)) {
                Ok(s) => {
                    s.set_read_timeout(Some(std::time::Duration::from_secs(30)))
                        .expect("read timeout");
                    break s;
                }
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => panic!("server never accepted MCP TCP: {e}"),
            }
        }
    };
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"token": "parity-tok"}
    });
    writeln!(stream, "{frame}").expect("write initialize");
    stream.flush().expect("flush initialize");
    let mut line = String::new();
    reader.read_line(&mut line).expect("initialize response");
    let resp: serde_json::Value = serde_json::from_str(line.trim()).expect("JSON response");
    assert!(
        resp.get("result").is_some(),
        "TCP-token initialize failed: {resp}"
    );

    // v0.4.0 password login: the bootstrap admin logs in over HTTP
    let body = r#"{"username":"admin","password":"parity-pw"}"#;
    let request = format!(
        "POST /api/login HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let login = loop {
        if let Ok(resp) = common::http_req(_srv.port, &request) {
            break resp;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "HTTP surface never answered the login"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(
        login.starts_with("HTTP/1.") && login.contains(" 200 "),
        "password login must keep working: {login}"
    );
    assert!(
        login.contains("\"token\""),
        "login must mint a session: {login}"
    );

    // and community mode never creates the enterprise store
    assert!(
        !db.join("users.db").exists(),
        "community mode must not create users.db"
    );
}
