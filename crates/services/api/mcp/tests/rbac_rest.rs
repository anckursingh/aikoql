//! Task 6: the `viewer` role gates both MCP tools and REST writes —
//! closing the hole where REST write routes bypassed capability checks.
//! Enterprise servers spawn with a TEST-keypair multi-user license (the
//! debug-only AIKOQL_LICENSE_PUBLIC_KEY override); community servers spawn
//! with no license at all and must behave byte-for-byte as v0.4.0. MCP
//! sessions carry server-assigned --tcp-token roles; REST sessions come
//! from /api/login and resolve their LIVE roles from the user store.

mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value as J};
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// A licensed multi-user server (bootstrap admin password "pw") with the
/// given --tcp-token specs. Returns the server plus its MCP TCP port.
fn serve(tag: &str, tokens: &[&str]) -> (common::Server, u16) {
    let (pk, blob) = common::sign("test", None, &["multi-user"]);
    let pk_b64 = URL_SAFE_NO_PAD.encode(&pk);
    let lic = std::env::temp_dir().join(format!("aikoql-rbac-{tag}-{}.lic", std::process::id()));
    std::fs::write(&lic, &blob).unwrap();
    common::register_temp_path(&lic);
    let db = std::env::temp_dir().join(format!("aikoql-rbac-{tag}-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-rbac-{tag}-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let mcp_port = common::free_port();
    let mcp_addr = format!("127.0.0.1:{mcp_port}");
    let mut extra: Vec<String> = vec!["--listen".into(), mcp_addr];
    for t in tokens {
        extra.push("--tcp-token".into());
        extra.push(t.to_string());
    }
    let extra_refs: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();
    let lic_str = lic.to_str().unwrap();
    let envs = [
        ("AIKOQL_LICENSE", lic_str),
        ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ("AIKOQL_ADMIN_PASSWORD", "pw"),
    ];
    let srv = common::spawn_server(&db, &log, &envs, &extra_refs);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(
        health.starts_with("HTTP/1.") && health.contains(" 200 "),
        "health: {health}"
    );
    (srv, mcp_port)
}

/// A licensed multi-user server with the given --tcp-token specs and NO
/// --metrics-addr: the pure MCP-TCP serve shape (final review I-1). There
/// is no HTTP listener, so readiness is the MCP port accepting and the
/// server staying alive.
fn serve_no_metrics(tag: &str, tokens: &[&str]) -> (common::Server, u16) {
    let (pk, blob) = common::sign("test", None, &["multi-user"]);
    let pk_b64 = URL_SAFE_NO_PAD.encode(&pk);
    let lic = std::env::temp_dir().join(format!("aikoql-rbac-nm-{tag}-{}.lic", std::process::id()));
    std::fs::write(&lic, &blob).unwrap();
    common::register_temp_path(&lic);
    let db = std::env::temp_dir().join(format!("aikoql-rbac-nm-{tag}-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-rbac-nm-{tag}-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let mcp_port = common::free_port();
    let mcp_addr = format!("127.0.0.1:{mcp_port}");
    let mut extra: Vec<String> = vec!["--listen".into(), mcp_addr];
    for t in tokens {
        extra.push("--tcp-token".into());
        extra.push(t.to_string());
    }
    let extra_refs: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();
    let lic_str = lic.to_str().unwrap();
    let envs = [
        ("AIKOQL_LICENSE", lic_str),
        ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ("AIKOQL_ADMIN_PASSWORD", "pw"),
    ];
    let mut srv = common::spawn_server_no_metrics(&db, &log, &envs, &extra_refs);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match TcpStream::connect(("127.0.0.1", mcp_port)) {
            Ok(_) => break,
            Err(_) => {
                assert!(Instant::now() < deadline, "MCP TCP never listened");
                assert!(
                    srv.child.try_wait().expect("try_wait").is_none(),
                    "server exited early"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    (srv, mcp_port)
}

/// One newline-delimited MCP connection: initialize with the `--tcp-token`
/// part, then one tools/call, returning its response.
fn mcp_call(port: u16, token: &str, tool: &str, args: &J) -> J {
    let mut stream = {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => {
                    s.set_read_timeout(Some(Duration::from_secs(30)))
                        .expect("read timeout");
                    break s;
                }
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => panic!("server never accepted MCP TCP: {e}"),
            }
        }
    };
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"token": token}
    });
    writeln!(stream, "{init}").expect("write initialize");
    stream.flush().expect("flush initialize");
    let mut line = String::new();
    reader.read_line(&mut line).expect("initialize response");
    let resp: J = serde_json::from_str(line.trim()).expect("JSON response");
    assert!(resp.get("result").is_some(), "initialize failed: {resp}");
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": tool, "arguments": args}
    });
    writeln!(stream, "{call}").expect("write tools/call");
    stream.flush().expect("flush tools/call");
    line.clear();
    reader.read_line(&mut line).expect("tools/call response");
    serde_json::from_str(line.trim()).expect("JSON response")
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

/// Flip the bootstrap admin to viewer through the admin API — its own
/// last act as an admin (the store is the authority, read per request).
/// PROD-2: the users live in the server's store; there is no file to edit.
fn demote_admin(port: u16) {
    let token = admin_token(port);
    let r = http(
        port,
        "PUT",
        &format!("/api/users/admin/roles?token={token}"),
        r#"{"roles":["viewer"]}"#,
    );
    assert_eq!(r.status, 200, "demote the bootstrap admin: {}", r.body);
}

#[test]
fn viewer_can_call_read_tools_over_mcp() {
    let (_srv, mcp_port) = serve("read", &["vtok:acme:viewer"]);
    for (tool, args) in [
        ("recall", json!({"query": "anything"})),
        ("memory_search", json!({"query": "anything"})),
    ] {
        let resp = mcp_call(mcp_port, "vtok", tool, &args);
        assert!(resp.get("result").is_some(), "{tool} must read: {resp}");
    }
}

#[test]
fn viewer_blocked_on_write_tools() {
    let (_srv, mcp_port) = serve("write", &["vtok:acme:viewer"]);
    // remember is an epistemic-state-changing tool: a viewer gets the
    // fail-closed -32001, never a write.
    let resp = mcp_call(mcp_port, "vtok", "remember", &json!({"type_name": "Fact"}));
    assert_eq!(
        resp["error"]["code"], -32001,
        "viewer must not write: {resp}"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("viewer"),
        "{resp}"
    );
}

#[test]
fn viewer_write_gate_holds_without_metrics_listener() {
    // Final review I-1: the viewer gate must not depend on --metrics-addr.
    // The user store publishes process-wide at startup, so a licensed
    // multi-user server serving MCP over TCP alone (no HTTP listener)
    // still refuses viewer writes — fail-closed, never a silent write.
    let (_srv, mcp_port) = serve_no_metrics("nm", &["vtok:acme:viewer"]);
    let resp = mcp_call(mcp_port, "vtok", "remember", &json!({"type_name": "Fact"}));
    assert_eq!(
        resp["error"]["code"], -32001,
        "viewer must not write without a metrics listener: {resp}"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("viewer"),
        "{resp}"
    );
    // a read tool stays reachable on the same connection shape
    let read = mcp_call(mcp_port, "vtok", "recall", &json!({"query": "anything"}));
    assert!(read.get("result").is_some(), "viewer must read: {read}");
}

#[test]
fn viewer_aikoql_mcp_match_ok_create_denied() {
    let (_srv, mcp_port) = serve("aikoql", &["vtok:acme:viewer"]);
    // a CREATE writes — the check_scope viewer arm denies it fail-closed
    // (the aikoql/kernel path itself carries no role check, so this arm is
    // load-bearing for MCP)
    let create = mcp_call(
        mcp_port,
        "vtok",
        "aikoql",
        &json!({"query": "CREATE Person name == \"Alice\", age == 30"}),
    );
    assert_eq!(
        create["error"]["code"], -32001,
        "viewer must not CREATE: {create}"
    );
    assert!(
        create["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("viewer"),
        "{create}"
    );
    // a MATCH reads — the same arm must pass it
    let m = mcp_call(
        mcp_port,
        "vtok",
        "aikoql",
        &json!({"query": "MATCH Person RETURN *"}),
    );
    assert!(m.get("result").is_some(), "viewer must MATCH: {m}");
}

#[test]
fn viewer_admin_route_rest_gate() {
    let (srv, _mcp) = serve("adminroute", &["ttok:acme:admin"]);
    // admin: the route succeeds with its success marker
    let atok = admin_token(srv.port);
    let ok = http(
        srv.port,
        "POST",
        &format!("/api/v1/backup?token={atok}"),
        "{}",
    );
    assert_eq!(
        ok.status, 200,
        "admin must backup: {} {}",
        ok.status, ok.body
    );
    // viewer: the same route, the same login, refuses with 403
    demote_admin(srv.port);
    let token = admin_token(srv.port);
    let denied = http(
        srv.port,
        "POST",
        &format!("/api/v1/backup?token={token}"),
        "{}",
    );
    assert_eq!(
        denied.status, 403,
        "viewer must not backup: {} {}",
        denied.status, denied.body
    );
    assert!(
        denied.body.contains("admin role required"),
        "{}",
        denied.body
    );
    let j: J = serde_json::from_str(&ok.body).expect("json");
    assert_eq!(j["data"]["verified"], true, "backup marker: {}", ok.body);
}

#[test]
fn viewer_read_post_routes_reachable() {
    let (srv, _mcp) = serve("readpost", &["ttok:acme:admin"]);
    demote_admin(srv.port);
    let token = admin_token(srv.port);
    // aikoql MATCH over REST reads (Studio drives queries over POST)
    let m = http(
        srv.port,
        "POST",
        &format!("/api/v1/aikoql?token={token}"),
        r#"{"query": "MATCH Person RETURN *"}"#,
    );
    assert_eq!(
        m.status, 200,
        "viewer must MATCH over REST: {} {}",
        m.status, m.body
    );
    // find-similar reads
    let fs = http(
        srv.port,
        "POST",
        &format!("/api/v1/find-similar?token={token}"),
        r#"{"text": "anything"}"#,
    );
    assert_eq!(
        fs.status, 200,
        "viewer must find-similar: {} {}",
        fs.status, fs.body
    );
    // the list-* POST twins read (they have GET twins that stay open)
    let lp = http(
        srv.port,
        "POST",
        &format!("/api/v1/list-programs?token={token}"),
        "{}",
    );
    assert_eq!(
        lp.status, 200,
        "viewer must list programs: {} {}",
        lp.status, lp.body
    );
    // but a non-MATCH aikoql statement still writes — fail-closed
    let c = http(
        srv.port,
        "POST",
        &format!("/api/v1/aikoql?token={token}"),
        r#"{"query": "CREATE Person name == \"Alice\", age == 30"}"#,
    );
    assert_eq!(
        c.status, 403,
        "viewer must not CREATE over REST: {} {}",
        c.status, c.body
    );
}

#[test]
fn viewer_cannot_post_rest_write() {
    let (srv, _mcp) = serve("rest", &["ttok:acme:admin"]);
    // flip the bootstrap admin to viewer — the store is authoritative, so
    // the session's LIVE roles are viewer from here on
    demote_admin(srv.port);
    let token = admin_token(srv.port);
    // reads keep working...
    let get = http(
        srv.port,
        "GET",
        &format!("/api/v1/abi-version?token={token}"),
        "",
    );
    assert_eq!(get.status, 200, "viewer must read: {}", get.body);
    // ...but a REST write is refused with 403
    let post = http(
        srv.port,
        "POST",
        &format!("/api/v1/remember?token={token}"),
        r#"{"type_name":"Fact"}"#,
    );
    assert_eq!(
        post.status, 403,
        "viewer must not write over REST: {} {}",
        post.status, post.body
    );
}

#[test]
fn a_viewer_cannot_save_or_remove_a_documents_knowledge_but_can_read_it() {
    let (srv, mcp_port) = serve("docsave", &["ttok:acme:admin", "vtok:acme:viewer"]);
    let token = admin_token(srv.port);
    demote_admin(srv.port);
    let koid = "00000000000000000000000000000001";
    // REST: saving and removing are writes (a viewer would otherwise delete a book's 12k objects)
    for route in ["commit", "uncommit"] {
        let r = http(
            srv.port,
            "POST",
            &format!("/api/v1/documents/{route}?token={token}"),
            &format!(r#"{{"koid":"{koid}"}}"#),
        );
        assert_eq!(
            r.status, 403,
            "viewer must not POST documents/{route}: {} {}",
            r.status, r.body
        );
    }
    // reading a saved document's objects is not blocked by the role gate (the koid is unknown: 4xx/5xx, not 403)
    let r = http(
        srv.port,
        "GET",
        &format!("/api/v1/documents/objects?koid={koid}&token={token}"),
        "",
    );
    assert_ne!(r.status, 403, "viewer may read saved objects: {}", r.body);
    // MCP: a viewer is refused the write tools, and the read tool is classified as a read
    for tool in ["document_commit", "document_uncommit"] {
        let resp = mcp_call(mcp_port, "vtok", tool, &json!({"koid": koid}));
        assert_eq!(
            resp["error"]["code"], -32001,
            "viewer must not call {tool}: {resp}"
        );
    }
    let resp = mcp_call(mcp_port, "vtok", "document_objects", &json!({"koid": koid}));
    assert_ne!(
        resp["error"]["code"], -32001,
        "document_objects is a read: {resp}"
    );
}

#[test]
fn admin_passes_all_gates() {
    let (srv, mcp_port) = serve("admin", &["atok:acme:admin"]);
    // REST write
    let token = admin_token(srv.port);
    let post = http(
        srv.port,
        "POST",
        &format!("/api/v1/remember?token={token}"),
        r#"{"type_name":"Fact"}"#,
    );
    assert_eq!(post.status, 200, "admin must write: {}", post.body);
    // MCP write
    let resp = mcp_call(mcp_port, "atok", "remember", &json!({"type_name": "Fact"}));
    assert!(
        resp.get("result").is_some(),
        "admin must write over MCP: {resp}"
    );
}

#[test]
fn unlicensed_tcp_token_roles_unchanged() {
    // no license anywhere: v0.4.0 role behavior, byte-for-byte
    let db = std::env::temp_dir().join(format!("aikoql-rbac-parity-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-rbac-parity-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let mcp_port = common::free_port();
    let mcp_addr = format!("127.0.0.1:{mcp_port}");
    let extra = [
        "--listen",
        mcp_addr.as_str(),
        "--tcp-token",
        "atok:acme:admin",
        "--tcp-token",
        "rtok:acme:scope:read",
        "--tcp-token",
        "vtok:acme:viewer",
    ];
    let srv = common::spawn_server(&db, &log, &[], &extra);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(
        health.starts_with("HTTP/1.") && health.contains(" 200 "),
        "health: {health}"
    );

    // the admin token writes, as it always has
    let admin = mcp_call(mcp_port, "atok", "remember", &json!({"type_name": "Fact"}));
    assert!(
        admin.get("result").is_some(),
        "admin token must write: {admin}"
    );
    // the mcp:read-scope token is still write-denied with its v0.4.0 message
    let read = mcp_call(mcp_port, "rtok", "remember", &json!({"type_name": "Fact"}));
    assert_eq!(read["error"]["code"], -32001, "{read}");
    assert!(
        read["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("mcp:read"),
        "{read}"
    );
    // a "viewer" role name is uninterpreted in community mode — no gate
    let viewer = mcp_call(mcp_port, "vtok", "remember", &json!({"type_name": "Fact"}));
    assert!(
        viewer.get("result").is_some(),
        "community viewer token stays v0.4.0-unrestricted: {viewer}"
    );
    // the read-scope token still reaches read tools
    let health_tool = mcp_call(mcp_port, "rtok", "health", &json!({}));
    assert!(
        health_tool.get("result").is_some(),
        "mcp:read must read: {health_tool}"
    );
}
