//! The production workflow through a real server: register (config) →
//! discover → snapshot → preview → merge → publish → import → query the
//! canonical class across sources → drift. Also: a canary connection string
//! must appear nowhere, source files must stay byte-identical, and a source
//! that vanishes mid-flow is a clean 502 that changes nothing.

mod common;
mod connectors;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Resp {
    status: u16,
    body: String,
}

fn http(port: u16, method: &str, path: &str, token: Option<&str>, body: &str) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
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
        body: body.to_string(),
    }
}

fn json(r: &Resp) -> serde_json::Value {
    serde_json::from_str(&r.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", r.body))
}

fn make(path: &Path, sql: &str) {
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

struct Rig {
    srv: common::Server,
    token: String,
    crm: PathBuf,
    shop: PathBuf,
    canary: String,
    /// Every response body, to sweep for the canary.
    seen: std::cell::RefCell<Vec<String>>,
}

impl Rig {
    fn call(&self, method: &str, path: &str, body: serde_json::Value) -> Resp {
        let r = http(
            self.srv.port,
            method,
            path,
            Some(&self.token),
            &body.to_string(),
        );
        self.seen.borrow_mut().push(r.body.clone());
        r
    }
}

fn rig(tag: &str) -> Rig {
    let root = std::env::temp_dir().join(format!("aikoql-srcdisc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    common::register_temp_path(&root);
    // the canary rides in the directory name, so it is inside every connection string
    let canary = format!("CANARY-{}-secret", std::process::id());
    let data = root.join(&canary);
    std::fs::create_dir_all(&data).unwrap();
    let (crm, shop) = (data.join("crm.sqlite"), data.join("shop.sqlite"));
    make(
        &crm,
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL);
         CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), total REAL);
         INSERT INTO customers VALUES (1, 'a@x.test');",
    );
    make(
        &shop,
        "CREATE TABLE clients (id INTEGER PRIMARY KEY, email_address TEXT NOT NULL);
         INSERT INTO clients VALUES (7, 'b@y.test');",
    );
    let cfg = root.join("aikoql.toml");
    std::fs::write(
        &cfg,
        "[[sources]]\nname = \"crm\"\nkind = \"sqlite\"\nurl_env = \"SD_CRM\"\n\n[[sources]]\nname = \"shop\"\nkind = \"sqlite\"\nurl_env = \"SD_SHOP\"\n",
    )
    .unwrap();
    let (db, log) = (root.join("db"), root.join("server.log"));
    let srv = common::spawn_server(
        &db,
        &log,
        &[
            ("AIKOQL_ADMIN_PASSWORD", "pw"),
            ("SD_CRM", crm.to_str().unwrap()),
            ("SD_SHOP", shop.to_str().unwrap()),
        ],
        &["--config", cfg.to_str().unwrap()],
    );
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(health.contains(" 200 "), "health: {health}");
    let login = http(
        srv.port,
        "POST",
        "/api/login",
        None,
        r#"{"username":"admin","password":"pw"}"#,
    );
    let token = json(&login)["token"].as_str().expect("token").to_string();
    Rig {
        srv,
        token,
        crm,
        shop,
        canary,
        seen: Default::default(),
    }
}

fn data(r: &Resp) -> serde_json::Value {
    json(r)["data"].clone()
}

fn all_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                all_files(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

#[test]
fn two_sqlite_sources_end_to_end_with_merge_import_query_and_drift() {
    let r = rig("e2e");
    let (crm_before, shop_before) = (
        std::fs::read(&r.crm).unwrap(),
        std::fs::read(&r.shop).unwrap(),
    );

    let list = r.call("GET", "/api/v1/sources", serde_json::json!({}));
    assert_eq!(list.status, 200, "{}", list.body);
    assert_eq!(data(&list).as_array().unwrap().len(), 2);

    for name in ["crm", "shop"] {
        let d = r.call(
            "POST",
            "/api/v1/sources/discover",
            serde_json::json!({"source": name}),
        );
        assert_eq!(d.status, 200, "{}", d.body);
        assert_eq!(data(&d)["changed"], true);
    }
    let again = r.call(
        "POST",
        "/api/v1/sources/discover",
        serde_json::json!({"source": "crm"}),
    );
    assert_eq!(
        data(&again)["changed"],
        false,
        "an unchanged source is a no-op"
    );

    let preview = r.call(
        "POST",
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["crm", "shop"], "preview": true}),
    );
    assert_eq!(preview.status, 200, "{}", preview.body);
    let p = data(&preview);
    let names: Vec<&str> = p["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["Customer", "Order", "Client"],
        "no automatic cross-source merge"
    );

    let merges = serde_json::json!([["crm.customers", "shop.clients"]]);
    let merged_preview = data(&r.call(
        "POST",
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["crm", "shop"], "preview": true, "merges": merges}),
    ));
    let publish = r.call(
        "POST",
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["crm", "shop"], "preview": false, "merges": merges, "fingerprints": merged_preview["fingerprints"]}),
    );
    assert_eq!(publish.status, 200, "{}", publish.body);
    let published_version = data(&publish)["version"].clone();

    for (name, table) in [("crm", "customers"), ("shop", "clients")] {
        let i = r.call(
            "POST",
            "/api/v1/sources/import",
            serde_json::json!({"source": name, "containers": [table]}),
        );
        assert_eq!(i.status, 200, "{}", i.body);
        assert_eq!(data(&i)["imported"][table], 1);
    }

    let q = r.call(
        "GET",
        &format!("/api/aikoql?query={}", "MATCH%20Customer%20RETURN%20*"),
        serde_json::json!({}),
    );
    assert_eq!(q.status, 200, "{}", q.body);
    assert_eq!(
        q.body.matches("\"koid\"").count(),
        2,
        "one canonical class, rows from both sources: {}",
        q.body
    );

    // drift: a new column is reported, and the approved ontology is untouched
    make(&r.crm, "ALTER TABLE customers ADD COLUMN phone TEXT;");
    let drift = r.call(
        "POST",
        "/api/v1/sources/discover",
        serde_json::json!({"source": "crm"}),
    );
    assert_eq!(
        data(&drift)["diff"]["changed"][0]["added_fields"],
        serde_json::json!(["phone"])
    );
    let onto = r.call("GET", "/api/v1/ontology", serde_json::json!({}));
    assert_eq!(
        data(&onto)["version"],
        published_version,
        "drift alone never rewrites the ontology"
    );

    // a stale preview cannot publish
    let stale = r.call(
        "POST",
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["crm", "shop"], "preview": false, "merges": merges, "fingerprints": merged_preview["fingerprints"], "expected_version": published_version}),
    );
    assert_eq!(stale.status, 409, "{}", stale.body);

    // the source files are untouched (apart from this test's own ALTER)
    assert_eq!(std::fs::read(&r.shop).unwrap(), shop_before);
    assert_ne!(std::fs::read(&r.crm).unwrap(), crm_before);

    // the canary connection string is nowhere: not in a response, the log, or any stored file
    for body in r.seen.borrow().iter() {
        assert!(
            !body.contains(&r.canary),
            "a response leaked the connection string: {body}"
        );
    }
    let log = std::fs::read(&r.srv.log).unwrap_or_default();
    assert!(
        !String::from_utf8_lossy(&log).contains(&r.canary),
        "the server log leaked the connection string"
    );
    // every file the server wrote (data dir, audit log, journal) — not the sources themselves
    let root = r.srv.db.parent().unwrap().to_path_buf();
    let mut files = Vec::new();
    all_files(&root, &mut files);
    files.retain(|f| !f.to_string_lossy().contains(&r.canary));
    assert!(
        files
            .iter()
            .any(|f| f.to_string_lossy().ends_with(".audit.log")),
        "the audit log is part of the sweep: {files:?}"
    );
    for f in files {
        let bytes = std::fs::read(&f).unwrap_or_default();
        assert!(
            !bytes
                .windows(r.canary.len())
                .any(|w| w == r.canary.as_bytes()),
            "{} stores the connection string",
            f.display()
        );
    }
}

#[test]
fn a_source_that_vanishes_is_a_clean_502_and_changes_nothing() {
    let r = rig("vanish");
    assert_eq!(
        r.call(
            "POST",
            "/api/v1/sources/discover",
            serde_json::json!({"source": "crm"})
        )
        .status,
        200
    );
    let before = data(&r.call("GET", "/api/v1/sources", serde_json::json!({})));
    std::fs::remove_file(&r.crm).unwrap();
    let gone = r.call(
        "POST",
        "/api/v1/sources/discover",
        serde_json::json!({"source": "crm"}),
    );
    assert_eq!(gone.status, 502, "{}", gone.body);
    assert!(
        !gone.body.contains(&r.canary) && !gone.body.contains("crm.sqlite"),
        "{}",
        gone.body
    );
    assert_eq!(
        data(&r.call("GET", "/api/v1/sources", serde_json::json!({}))),
        before,
        "state is unchanged"
    );
    let preview = r.call(
        "POST",
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["crm"], "preview": true}),
    );
    assert_eq!(
        preview.status, 200,
        "the recorded schema is still there to review: {}",
        preview.body
    );
}

#[test]
fn concurrent_discover_and_preview_calls_agree_with_the_serial_result() {
    let r = rig("conc");
    let port = r.srv.port;
    let token = r.token.clone();
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let token = token.clone();
            std::thread::spawn(move || {
                let mut statuses = Vec::new();
                for _ in 0..3 {
                    let name = if i % 2 == 0 { "crm" } else { "shop" };
                    let d = http(
                        port,
                        "POST",
                        "/api/v1/sources/discover",
                        Some(&token),
                        &format!(r#"{{"source":"{name}"}}"#),
                    );
                    let p = http(
                        port,
                        "POST",
                        "/api/v1/sources/ontology",
                        Some(&token),
                        r#"{"sources":["crm","shop"],"preview":true}"#,
                    );
                    statuses.push((d.status, p.status));
                }
                statuses
            })
        })
        .collect();
    for h in handles {
        for (d, p) in h.join().unwrap() {
            assert_eq!(d, 200);
            assert!(
                p == 200 || p == 400,
                "preview before both sources are recorded may be a 400, never a 5xx: {p}"
            );
        }
    }
    let list = data(&r.call("GET", "/api/v1/sources", serde_json::json!({})));
    for s in list.as_array().unwrap() {
        assert!(s["last"]["containers"].as_u64().unwrap() >= 1, "{s}");
    }
    let preview = data(&r.call(
        "POST",
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["crm", "shop"], "preview": true}),
    ));
    assert_eq!(preview["classes"].as_array().unwrap().len(), 3);
}

/// A real Postgres source: its password must be stored, logged and returned nowhere.
#[test]
fn a_live_postgres_source_works_end_to_end_and_its_password_never_leaks() {
    let Some(live) = connectors::Live::pg() else {
        return;
    };
    let dsn = connectors::pg_private_db(&live.dsn, "cert_srcdisc_pg");
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE TABLE customers (id INT PRIMARY KEY, email TEXT NOT NULL UNIQUE)",
            "CREATE TABLE orders (id INT PRIMARY KEY, customer_id INT REFERENCES customers(id), total NUMERIC)",
            "INSERT INTO customers VALUES (1, 'pg@x.test')",
            "INSERT INTO orders VALUES (10, 1, 9.5)",
        ],
    );
    let password = dsn
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("password="))
        .expect("the live DSN has a password")
        .to_string();
    assert!(
        password.len() >= 6,
        "use a distinctive password for this sweep"
    );

    let root = std::env::temp_dir().join(format!("aikoql-srcdisc-pg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    common::register_temp_path(&root);
    let cfg = root.join("aikoql.toml");
    std::fs::write(
        &cfg,
        "[[sources]]\nname = \"pg\"\nkind = \"postgres\"\nurl_env = \"SD_PG\"\n",
    )
    .unwrap();
    let (db, log) = (root.join("db"), root.join("server.log"));
    let srv = common::spawn_server(
        &db,
        &log,
        &[("AIKOQL_ADMIN_PASSWORD", "pw"), ("SD_PG", &dsn)],
        &["--config", cfg.to_str().unwrap()],
    );
    assert!(
        common::wait_health(srv.port, Instant::now() + Duration::from_secs(60)).contains(" 200 ")
    );
    let login = http(
        srv.port,
        "POST",
        "/api/login",
        None,
        r#"{"username":"admin","password":"pw"}"#,
    );
    let token = json(&login)["token"].as_str().unwrap().to_string();
    let mut bodies = Vec::new();
    let mut call = |m: &str, p: &str, b: serde_json::Value| {
        let r = http(srv.port, m, p, Some(&token), &b.to_string());
        bodies.push(r.body.clone());
        r
    };

    let d = call(
        "POST",
        "/api/v1/sources/discover",
        serde_json::json!({"source": "pg"}),
    );
    assert_eq!(d.status, 200, "{}", d.body);
    assert_eq!(data(&d)["containers"], 2);
    assert_eq!(data(&d)["references"], 1);
    let p = data(&call(
        "POST",
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["pg"], "preview": true}),
    ));
    let fp = p["fingerprints"].clone();
    assert_eq!(
        call(
            "POST",
            "/api/v1/sources/ontology",
            serde_json::json!({"sources": ["pg"], "preview": false, "fingerprints": fp})
        )
        .status,
        200
    );
    let i = call(
        "POST",
        "/api/v1/sources/import",
        serde_json::json!({"source": "pg", "containers": ["customers"]}),
    );
    assert_eq!(data(&i)["imported"]["customers"], 1, "{}", i.body);
    let q = call(
        "GET",
        "/api/aikoql?query=MATCH%20Customer%20RETURN%20*",
        serde_json::json!({}),
    );
    assert_eq!(q.body.matches("\"koid\"").count(), 1, "{}", q.body);

    for b in &bodies {
        assert!(
            !b.contains(&password),
            "a response leaked the password: {b}"
        );
    }
    let logtext = std::fs::read(&log).unwrap_or_default();
    assert!(
        !String::from_utf8_lossy(&logtext).contains(&password),
        "the log leaked the password"
    );
    let mut files = Vec::new();
    all_files(&root, &mut files);
    for f in files {
        let bytes = std::fs::read(&f).unwrap_or_default();
        assert!(
            !bytes
                .windows(password.len())
                .any(|w| w == password.as_bytes()),
            "{} stores the password",
            f.display()
        );
    }
}
