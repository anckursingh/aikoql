//! Adversarial tests against the assembled source-discovery feature through a
//! real server: secrets, read-only guarantees, resource bounds, tenant and role
//! isolation, crash consistency, a source that goes silent, and route fuzz.
//! Seeded; raise the volume with AIKOQL_FUZZ_ITERS.

mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value as J};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Resp {
    status: u16,
    head: String,
    body: String,
}

fn http(port: u16, method: &str, path: &str, token: Option<&str>, body: &str) -> Resp {
    http_to(port, method, path, token, body, Duration::from_secs(120))
}

fn http_to(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
    read: Duration,
) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(read)).unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
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

fn make(path: &Path, sql: &str) {
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

fn login(port: u16) -> String {
    let r = http(
        port,
        "POST",
        "/api/login",
        None,
        r#"{"username":"admin","password":"pw"}"#,
    );
    serde_json::from_str::<J>(&r.body).unwrap()["token"]
        .as_str()
        .expect("token")
        .to_string()
}

fn data(r: &Resp) -> J {
    serde_json::from_str::<J>(&r.body).unwrap_or(J::Null)["data"].clone()
}

fn error(r: &Resp) -> String {
    serde_json::from_str::<J>(&r.body).unwrap_or(J::Null)["error"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn iters() -> u64 {
    std::env::var("AIKOQL_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000)
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                files_under(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

struct Src {
    name: &'static str,
    kind: &'static str,
    tenant: Option<&'static str>,
    timeout: u64,
}

struct Rig {
    root: PathBuf,
    srv: common::Server,
    token: String,
    canary: String,
    paths: Vec<PathBuf>,
    /// head+body of every response, for the canary sweep.
    seen: std::cell::RefCell<Vec<String>>,
}

impl Rig {
    fn call(&self, method: &str, path: &str, body: J) -> Resp {
        self.call_raw(method, path, &body.to_string())
    }
    fn call_raw(&self, method: &str, path: &str, body: &str) -> Resp {
        let r = http(self.srv.port, method, path, Some(&self.token), body);
        self.seen
            .borrow_mut()
            .push(format!("{}\n{}", r.head, r.body));
        r
    }
}

/// A server with SQLite sources (one file each, in a canary-named directory).
/// `licensed` makes it multi-user so a viewer role exists.
fn rig(
    tag: &str,
    sources: &[Src],
    licensed: bool,
    extra_toml: &str,
    prep: impl Fn(&str, &Path),
) -> Rig {
    let root = std::env::temp_dir().join(format!("aikoql-hard-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    common::register_temp_path(&root);
    let canary = format!("CANARY-{}-{tag}", std::process::id());
    let dir = root.join(&canary);
    std::fs::create_dir_all(&dir).unwrap();
    let mut toml = String::new();
    let mut envs: Vec<(String, String)> = vec![("AIKOQL_ADMIN_PASSWORD".into(), "pw".into())];
    let mut paths = Vec::new();
    for s in sources {
        let p = dir.join(format!("{}.sqlite", s.name));
        prep(s.name, &p);
        toml.push_str(&format!(
            "[[sources]]\nname = \"{}\"\nkind = \"{}\"\nurl_env = \"SD_{}\"\ntimeout_secs = {}\n",
            s.name,
            s.kind,
            s.name.to_uppercase(),
            s.timeout
        ));
        if let Some(t) = s.tenant {
            toml.push_str(&format!("tenant = \"{t}\"\n"));
        }
        toml.push('\n');
        envs.push((
            format!("SD_{}", s.name.to_uppercase()),
            p.to_str().unwrap().to_string(),
        ));
        paths.push(p);
    }
    let cfg = root.join("aikoql.toml");
    std::fs::write(&cfg, format!("{toml}{extra_toml}")).unwrap();
    let (db, log) = (root.join("db"), root.join("server.log"));
    if licensed {
        let (pk, blob) = common::sign("test", None, &["multi-user"]);
        let lic = root.join("test.lic");
        std::fs::write(&lic, &blob).unwrap();
        envs.push(("AIKOQL_LICENSE".into(), lic.to_str().unwrap().to_string()));
        envs.push((
            "AIKOQL_LICENSE_PUBLIC_KEY".into(),
            URL_SAFE_NO_PAD.encode(&pk),
        ));
    }
    let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let srv = common::spawn_server(&db, &log, &env_refs, &["--config", cfg.to_str().unwrap()]);
    assert!(
        common::wait_health(srv.port, Instant::now() + Duration::from_secs(60)).contains(" 200 ")
    );
    let token = login(srv.port);
    Rig {
        root,
        srv,
        token,
        canary,
        paths,
        seen: Default::default(),
    }
}

fn crm_like(name: &str, p: &Path) {
    make(
        p,
        &format!(
            "CREATE TABLE {name}_t (id INTEGER PRIMARY KEY, email TEXT NOT NULL);
             CREATE TABLE {name}_u (id INTEGER PRIMARY KEY, t_id INTEGER REFERENCES {name}_t(id));
             INSERT INTO {name}_t VALUES (1, 'a@x.test');"
        ),
    );
}

fn one(name: &'static str) -> Src {
    Src {
        name,
        kind: "sqlite",
        tenant: None,
        timeout: 5,
    }
}

#[test]
fn nothing_secret_appears_anywhere_after_every_route_including_failures_and_hostile_input() {
    let r = rig("secret", &[one("crm"), one("shop")], false, "", crm_like);
    let before: Vec<Vec<u8>> = r.paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let hostile = [
        "",
        "\u{0}",
        "../../etc/passwd",
        "'; DROP TABLE x; --",
        "💥",
        &"x".repeat(5_000),
    ];
    for name in ["crm", "shop", "nope"] {
        r.call("POST", "/api/v1/sources/discover", json!({"source": name}));
        r.call(
            "POST",
            "/api/v1/sources/discover",
            json!({"source": name, "mode": "sample"}),
        );
        r.call(
            "POST",
            "/api/v1/sources/ontology",
            json!({"sources": [name], "preview": true}),
        );
        r.call(
            "POST",
            "/api/v1/sources/import",
            json!({"source": name, "containers": ["crm_t", "shop_t", "nope"]}),
        );
    }
    for h in hostile {
        r.call("POST", "/api/v1/sources/discover", json!({"source": h}));
        r.call(
            "POST",
            "/api/v1/sources/import",
            json!({"source": "crm", "containers": [h]}),
        );
        r.call(
            "POST",
            "/api/v1/sources/ontology",
            json!({"sources": [h], "merges": [[h, h]]}),
        );
        r.call(
            "GET",
            &format!(
                "/api/v1/sources?tenant={}",
                h.replace('\u{0}', "%00").replace(' ', "%20")
            ),
            json!({}),
        );
    }
    let p = data(&r.call(
        "POST",
        "/api/v1/sources/ontology",
        json!({"sources": ["crm", "shop"], "preview": true}),
    ));
    r.call(
        "POST",
        "/api/v1/sources/ontology",
        json!({"sources": ["crm", "shop"], "preview": false, "fingerprints": p["fingerprints"]}),
    );
    r.call(
        "POST",
        "/api/v1/sources/import",
        json!({"source": "crm", "containers": ["crm_t"]}),
    );
    r.call("GET", "/api/v1/sources", json!({}));

    for body in r.seen.borrow().iter() {
        assert!(
            !body.contains(&r.canary),
            "a response (headers or body) leaked the connection string: {body}"
        );
    }
    let log = std::fs::read(&r.srv.log).unwrap_or_default();
    assert!(
        !String::from_utf8_lossy(&log).contains(&r.canary),
        "the server log leaked the connection string"
    );
    let mut files = Vec::new();
    files_under(&r.root, &mut files);
    files.retain(|f| !f.to_string_lossy().contains(&r.canary));
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
    // read-only proof: byte-identical after every route, failures included
    for (p, b) in r.paths.iter().zip(before) {
        assert_eq!(
            std::fs::read(p).unwrap(),
            b,
            "{} was modified by discovery or import",
            p.display()
        );
    }
}

#[test]
fn a_5000_table_source_is_bounded_in_time_size_and_memory() {
    let r = rig("big", &[one("big")], false, "", |_, p| {
        let mut sql = String::from("BEGIN;");
        for i in 0..5_000 {
            sql.push_str(&format!(
                "CREATE TABLE t{i:05} (id INTEGER PRIMARY KEY, v TEXT);"
            ));
        }
        sql.push_str("COMMIT;");
        make(p, &sql);
    });
    let rss = |pid: u32| -> u64 {
        std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0)
    };
    let before = rss(r.srv.child.id());
    let t = Instant::now();
    let d = r.call("POST", "/api/v1/sources/discover", json!({"source": "big"}));
    assert_eq!(d.status, 200, "{}", d.body);
    assert!(
        t.elapsed() < Duration::from_secs(20),
        "discover took {:?}",
        t.elapsed()
    );
    assert_eq!(data(&d)["containers"], 500, "capped at max_containers");
    assert_eq!(data(&d)["truncated"], true);
    assert!(
        d.body.len() < 8 * 1024 * 1024,
        "response {} bytes",
        d.body.len()
    );
    let list = r.call("GET", "/api/v1/sources", json!({}));
    assert!(list.body.len() < 8 * 1024 * 1024);
    let grown_mb = rss(r.srv.child.id()).saturating_sub(before) / 1024;
    assert!(
        before == 0 || grown_mb < 200,
        "server RSS grew {grown_mb} MiB"
    );
}

#[test]
fn tenants_see_only_their_own_sources_on_every_route() {
    let srcs = [
        Src {
            name: "acme_src",
            kind: "sqlite",
            tenant: Some("acme"),
            timeout: 5,
        },
        Src {
            name: "globex_src",
            kind: "sqlite",
            tenant: Some("globex"),
            timeout: 5,
        },
        Src {
            name: "global_src",
            kind: "sqlite",
            tenant: None,
            timeout: 5,
        },
    ];
    let r = rig("tenants", &srcs, false, "", crm_like);
    let tenants: [Option<&str>; 3] = [Some("acme"), Some("globex"), None];
    let with = |b: J, t: Option<&str>| -> J {
        let mut b = b;
        if let Some(t) = t {
            b["tenant"] = json!(t);
        }
        b
    };
    for (owner_idx, s) in srcs.iter().enumerate() {
        for (ti, t) in tenants.iter().enumerate() {
            let owned = owner_idx == ti;
            let d = r.call(
                "POST",
                "/api/v1/sources/discover",
                with(json!({"source": s.name}), *t),
            );
            let o = r.call(
                "POST",
                "/api/v1/sources/ontology",
                with(json!({"sources": [s.name], "preview": true}), *t),
            );
            let i = r.call(
                "POST",
                "/api/v1/sources/import",
                with(
                    json!({"source": s.name, "containers": [format!("{}_t", s.name)]}),
                    *t,
                ),
            );
            if owned {
                assert_eq!(d.status, 200, "{} as {t:?}: {}", s.name, d.body);
                assert_eq!(o.status, 200, "{}", o.body);
                assert_eq!(i.status, 200, "{}", i.body);
            } else {
                for (what, x) in [("discover", &d), ("ontology", &o), ("import", &i)] {
                    assert_eq!(
                        x.status, 404,
                        "{what} of {} as {t:?} must look like a missing source: {}",
                        s.name, x.body
                    );
                    assert_eq!(
                        error(x),
                        "source not found",
                        "{what}: the wording must not reveal that it exists"
                    );
                }
            }
        }
    }
    for (ti, t) in tenants.iter().enumerate() {
        let path = match t {
            Some(t) => format!("/api/v1/sources?tenant={t}"),
            None => "/api/v1/sources".to_string(),
        };
        let names: Vec<String> = data(&r.call("GET", &path, json!({})))
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [srcs[ti].name],
            "tenant {t:?} lists only its own source"
        );
    }
    // a tenant's objects are invisible to the others
    for (name, t) in [("acme_src", "acme"), ("globex_src", "globex")] {
        let _ = (name, t);
    }
    let q = |t: &str| {
        r.call(
            "GET",
            &format!("/api/aikoql?query=MATCH%20acme_src_t%20RETURN%20*&tenant={t}"),
            json!({}),
        )
        .body
        .matches("\"koid\"")
        .count()
    };
    assert_eq!(q("acme"), 1);
    assert_eq!(
        q("globex"),
        0,
        "another tenant cannot see acme's imported objects"
    );
}

#[test]
fn a_viewer_cannot_discover_publish_or_import_but_can_list() {
    let r = rig("viewer", &[one("crm")], true, "", crm_like);
    assert_eq!(
        r.call("POST", "/api/v1/sources/discover", json!({"source": "crm"}))
            .status,
        200,
        "admin discovers"
    );
    // the store is authoritative for roles: the bootstrap admin demotes
    // itself through the admin API (PROD-2: no users.db file to edit)
    assert_eq!(
        r.call(
            "PUT",
            "/api/users/admin/roles",
            json!({"roles": ["viewer"]})
        )
        .status,
        200,
        "demote the bootstrap admin"
    );
    for (path, body) in [
        ("/api/v1/sources/discover", json!({"source": "crm"})),
        (
            "/api/v1/sources/ontology",
            json!({"sources": ["crm"], "preview": true}),
        ),
        (
            "/api/v1/sources/import",
            json!({"source": "crm", "containers": ["crm_t"]}),
        ),
    ] {
        let x = r.call("POST", path, body);
        assert_eq!(
            x.status, 403,
            "{path} must be admin-only: {} {}",
            x.status, x.body
        );
    }
    assert_eq!(
        r.call("GET", "/api/v1/sources", json!({})).status,
        200,
        "a viewer may list the sources"
    );
}

#[test]
fn killing_the_server_mid_import_leaves_a_consistent_store_and_a_resumable_import() {
    let root = std::env::temp_dir().join(format!("aikoql-hard-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    common::register_temp_path(&root);
    let src = root.join("crm.sqlite");
    let mut sql = String::from("CREATE TABLE items (id INTEGER PRIMARY KEY, v TEXT); BEGIN;");
    for i in 0..4_000 {
        sql.push_str(&format!("INSERT INTO items VALUES ({i}, 'row {i}');"));
    }
    sql.push_str("COMMIT;");
    make(&src, &sql);
    let cfg = root.join("aikoql.toml");
    std::fs::write(
        &cfg,
        "[[sources]]\nname = \"crm\"\nkind = \"sqlite\"\nurl_env = \"SD_CRM\"\n",
    )
    .unwrap();
    let db = root.join("db");
    let envs = [
        ("AIKOQL_ADMIN_PASSWORD", "pw"),
        ("SD_CRM", src.to_str().unwrap()),
    ];
    let extra = ["--config", cfg.to_str().unwrap()];

    let srv = common::spawn_server(&db, &root.join("one.log"), &envs, &extra);
    assert!(
        common::wait_health(srv.port, Instant::now() + Duration::from_secs(60)).contains(" 200 ")
    );
    let (port, token) = (srv.port, login(srv.port));
    assert_eq!(
        http(
            port,
            "POST",
            "/api/v1/sources/discover",
            Some(&token),
            r#"{"source":"crm"}"#
        )
        .status,
        200
    );
    let t2 = token.clone();
    let importer = std::thread::spawn(move || {
        http(
            port,
            "POST",
            "/api/v1/sources/import",
            Some(&t2),
            r#"{"source":"crm","containers":["items"]}"#,
        )
        .status
    });
    std::thread::sleep(Duration::from_millis(300));
    srv.shutdown(); // SIGKILL, data dir kept
    let _ = importer.join();

    let srv2 = common::spawn_server(&db, &root.join("two.log"), &envs, &extra);
    assert!(
        common::wait_health(srv2.port, Instant::now() + Duration::from_secs(60)).contains(" 200 "),
        "the store must reopen after a kill"
    );
    let token = login(srv2.port);
    let count = |label: &str| {
        let q = http(
            srv2.port,
            "GET",
            "/api/aikoql?query=MATCH%20items%20RETURN%20*",
            Some(&token),
            "",
        );
        assert_eq!(q.status, 200, "{label}: {}", q.body);
        assert!(
            serde_json::from_str::<J>(&q.body).is_ok(),
            "{label}: a torn result: {}",
            &q.body[..q.body.len().min(200)]
        );
        q.body.matches("\"koid\"").count()
    };
    let partial = count("after the crash");
    assert!(partial <= 4_000);
    let list = http(srv2.port, "GET", "/api/v1/sources", Some(&token), "");
    assert_eq!(
        data(&list)[0]["last"]["containers"],
        1,
        "the recorded schema survived"
    );
    let resume = http(
        srv2.port,
        "POST",
        "/api/v1/sources/import",
        Some(&token),
        r#"{"source":"crm","containers":["items"]}"#,
    );
    assert_eq!(resume.status, 200, "{}", resume.body);
    let d = data(&resume);
    assert_eq!(
        d["imported"]["items"].as_u64().unwrap() + d["unchanged"]["items"].as_u64().unwrap(),
        4_000,
        "the resumed import accounts for every row exactly once: {d}"
    );
    assert_eq!(count("after resuming"), 4_000, "no duplicates, no gaps");
}

#[test]
fn a_silent_source_times_out_and_does_not_wedge_the_server() {
    // accepts connections and never says a word
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let h2 = held.clone();
    std::thread::spawn(move || {
        for c in listener.incoming().flatten() {
            h2.lock().unwrap().push(c);
        }
    });
    let root = std::env::temp_dir().join(format!("aikoql-hard-slow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    common::register_temp_path(&root);
    let cfg = root.join("aikoql.toml");
    std::fs::write(&cfg, "[[sources]]\nname = \"slow\"\nkind = \"postgres\"\nurl_env = \"SD_SLOW\"\ntimeout_secs = 1\n").unwrap();
    let dsn = format!("host=127.0.0.1 port={port} user=u password=slowpw dbname=d");
    let srv = common::spawn_server(
        &root.join("db"),
        &root.join("s.log"),
        &[("AIKOQL_ADMIN_PASSWORD", "pw"), ("SD_SLOW", &dsn)],
        &["--config", cfg.to_str().unwrap()],
    );
    assert!(
        common::wait_health(srv.port, Instant::now() + Duration::from_secs(60)).contains(" 200 ")
    );
    let token = login(srv.port);
    let started = Instant::now();
    let d = http(
        srv.port,
        "POST",
        "/api/v1/sources/discover",
        Some(&token),
        r#"{"source":"slow"}"#,
    );
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "a silent source must fail within its timeout, took {:?}",
        started.elapsed()
    );
    assert_eq!(d.status, 502, "{}", d.body);
    assert!(
        !d.body.contains("slowpw") && !d.body.contains("127.0.0.1"),
        "{}",
        d.body
    );
    let t = Instant::now();
    assert_eq!(
        http(srv.port, "GET", "/api/v1/sources", Some(&token), "").status,
        200
    );
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "the server must stay responsive afterwards: {:?}",
        t.elapsed()
    );
    // repeated silent attempts must not pile up unbounded blocked threads
    for _ in 0..12 {
        let r = http(
            srv.port,
            "POST",
            "/api/v1/sources/discover",
            Some(&token),
            r#"{"source":"slow"}"#,
        );
        assert!(r.status == 502, "{}", r.body);
    }
    assert_eq!(http(srv.port, "GET", "/health", None, "").status, 200);
}

#[test]
fn route_fuzz_never_a_5xx_never_invalid_json_and_the_server_stays_healthy() {
    let r = rig(
        "fuzz",
        &[one("crm"), one("shop")],
        false,
        "[rate_limit]\nmax_calls_per_minute = 10000000\n",
        crm_like,
    );
    r.call("POST", "/api/v1/sources/discover", json!({"source": "crm"}));
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }
    fn val(r: &mut Rng, depth: u32) -> J {
        match r.below(if depth > 4 { 6 } else { 8 }) {
            0 => J::Null,
            1 => json!(r.next().is_multiple_of(2)),
            2 => json!(r.next() as i64),
            3 => json!(["", "crm", "shop", "nope", "a\u{0}b", "💥", "x", "crm.crm_t"][r.below(8)]),
            4 => json!(1.5e300),
            5 => json!("y".repeat(r.below(3_000))),
            6 => J::Array((0..r.below(5)).map(|_| val(r, depth + 1)).collect()),
            _ => J::Object(
                (0..r.below(6))
                    .map(|_| {
                        (
                            [
                                "source",
                                "sources",
                                "mode",
                                "tenant",
                                "merges",
                                "containers",
                                "preview",
                                "fingerprints",
                                "expected_version",
                                "x",
                            ][r.below(10)]
                            .to_string(),
                            val(r, depth + 1),
                        )
                    })
                    .collect(),
            ),
        }
    }
    let routes = [
        "/api/v1/sources/discover",
        "/api/v1/sources/ontology",
        "/api/v1/sources/import",
        "/api/v1/sources/zzz",
        "/api/v1/sources",
    ];
    for seed in 1..=iters() {
        let mut g = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let route = routes[g.below(routes.len())];
        let body = match g.below(6) {
            0 => "not json at all".to_string(),
            1 => String::new(),
            2 => "[".repeat(5_000),
            _ => val(&mut g, 0).to_string(),
        };
        let method = if route == "/api/v1/sources" && g.below(2) == 0 {
            "GET"
        } else {
            "POST"
        };
        let resp = r.call_raw(method, route, &body);
        assert!(
            matches!(resp.status, 200 | 400 | 404 | 409 | 502),
            "seed {seed}: {method} {route} body {:.120} -> {} {:.200}",
            body,
            resp.status,
            resp.body
        );
        assert!(
            serde_json::from_str::<J>(&resp.body).is_ok(),
            "seed {seed}: invalid JSON reply: {:.200}",
            resp.body
        );
        assert!(!resp.body.contains(&r.canary), "seed {seed}: leaked");
    }
    assert_eq!(
        http(r.srv.port, "GET", "/health", None, "").status,
        200,
        "the server survived the fuzz"
    );
}
