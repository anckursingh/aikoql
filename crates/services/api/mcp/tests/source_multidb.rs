//! Rigorous multi-database workflow through a real server and REAL databases
//! (docker compose: Postgres, MongoDB, Neo4j, ClickHouse). Env-gated like the
//! connector certifications: unset `AIKOQL_TEST_*` skips, set-but-dead fails.
//!
//! Every test drives the same production routes Studio calls — discover,
//! ontology preview/publish, import — and sweeps responses, logs and stored
//! files for the live passwords.

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

fn data(r: &Resp) -> serde_json::Value {
    json(r)["data"].clone()
}

/// A server configured with `[[sources]]` blocks whose connection strings come
/// from the given env vars. Returns (server, admin token, temp root).
struct Live {
    srv: common::Server,
    token: String,
    root: PathBuf,
    log: PathBuf,
    secrets: Vec<String>,
    seen: std::cell::RefCell<Vec<String>>,
}

impl Live {
    fn start(tag: &str, toml: &str, env: &[(&str, &str)], secrets: &[&str]) -> Live {
        let root =
            std::env::temp_dir().join(format!("aikoql-multidb-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        common::register_temp_path(&root);
        let cfg = root.join("aikoql.toml");
        std::fs::write(&cfg, toml).unwrap();
        let (db, log) = (root.join("db"), root.join("server.log"));
        let mut envs: Vec<(&str, &str)> = vec![("AIKOQL_ADMIN_PASSWORD", "pw")];
        envs.extend_from_slice(env);
        let srv = common::spawn_server(&db, &log, &envs, &["--config", cfg.to_str().unwrap()]);
        assert!(
            common::wait_health(srv.port, Instant::now() + Duration::from_secs(60))
                .contains(" 200 ")
        );
        let login = http(
            srv.port,
            "POST",
            "/api/login",
            None,
            r#"{"username":"admin","password":"pw"}"#,
        );
        let token = json(&login)["token"].as_str().expect("token").to_string();
        Live {
            srv,
            token,
            root,
            log,
            secrets: secrets.iter().map(|s| s.to_string()).collect(),
            seen: Default::default(),
        }
    }

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

    fn post(&self, path: &str, body: serde_json::Value) -> serde_json::Value {
        let r = self.call("POST", path, body);
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        data(&r)
    }

    /// No live password appears in any response, the log, or any file the server wrote.
    fn assert_no_secret_anywhere(&self) {
        for s in &self.secrets {
            for b in self.seen.borrow().iter() {
                assert!(!b.contains(s.as_str()), "a response leaked a secret: {b}");
            }
            let log = std::fs::read(&self.log).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&log).contains(s.as_str()),
                "the server log leaked a secret"
            );
            let mut files = Vec::new();
            all_files(&self.root, &mut files);
            for f in files {
                let bytes = std::fs::read(&f).unwrap_or_default();
                assert!(
                    !bytes.windows(s.len()).any(|w| w == s.as_bytes()),
                    "{} stores a secret",
                    f.display()
                );
            }
        }
    }
}

fn class_names(preview: &serde_json::Value) -> Vec<String> {
    preview["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}

fn query(l: &Live, q: &str) -> serde_json::Value {
    let enc: String = q
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    let r = l.call(
        "GET",
        &format!("/api/aikoql?query={enc}"),
        serde_json::json!({}),
    );
    assert_eq!(r.status, 200, "{q}: {}", r.body);
    json(&r)
}

/// Postgres + MongoDB, one canonical class across both, through the real routes.
#[test]
fn postgres_and_mongo_become_one_canonical_customer_class() {
    let (Some(pg), Some(mongo)) = (connectors::Live::pg(), connectors::Live::mongo()) else {
        return;
    };
    let pid = std::process::id();
    let dsn = connectors::pg_private_db(&pg.dsn, &format!("cert_multidb_pg_{pid}"));
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE TABLE customers (id INT PRIMARY KEY, email TEXT NOT NULL UNIQUE, tier TEXT, created_at TIMESTAMPTZ)",
            "CREATE TABLE orders (id INT PRIMARY KEY, customer_id INT NOT NULL REFERENCES customers(id), total NUMERIC(10,2) NOT NULL)",
            "CREATE INDEX orders_customer_idx ON orders(customer_id)",
            "INSERT INTO customers VALUES (1,'ann@pg.test','gold','2026-01-01T00:00:00Z'),(2,'bob@pg.test',NULL,NULL),(3,'cy@pg.test','silver',NULL)",
            "INSERT INTO orders VALUES (10,1,9.50),(11,3,20.00)",
        ],
    );
    let mdb = format!("cert_multidb_mongo_{pid}");
    connectors::mongo_seed(
        &mongo.mongo_uri,
        &mdb,
        "clients",
        vec![
            mongodb::bson::doc! {"email_address": "d@mongo.test", "age": 30_i32, "profile": {"city": "Oslo"}},
            mongodb::bson::doc! {"email_address": "e@mongo.test", "age": "31"},
            mongodb::bson::doc! {"email_address": "f@mongo.test", "age": mongodb::bson::Bson::Null},
            mongodb::bson::doc! {"email_address": "g@mongo.test"},
        ],
    );
    let pg_pw = "aikoql-dev-only";
    let l = Live::start(
        "pgmongo",
        &format!(
            "[[sources]]\nname = \"pg\"\nkind = \"postgres\"\nurl_env = \"MD_PG\"\n\n[[sources]]\nname = \"mongo\"\nkind = \"mongodb\"\nurl_env = \"MD_MONGO\"\ndatabase = \"{mdb}\"\n"
        ),
        &[("MD_PG", &dsn), ("MD_MONGO", &mongo.mongo_uri)],
        &[pg_pw],
    );

    let d = l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "pg"}),
    );
    assert_eq!(
        (d["containers"].clone(), d["references"].clone()),
        (2.into(), 1.into())
    );
    let d = l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "mongo"}),
    );
    assert_eq!(d["containers"], 1, "{d}");

    let preview = l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["pg", "mongo"], "preview": true}),
    );
    assert_eq!(
        class_names(&preview),
        ["Customer", "Order", "Client"],
        "no automatic merge"
    );
    let warnings = preview["warnings"].to_string();
    assert!(warnings.contains("observed from a sample"), "{warnings}");

    let merges = serde_json::json!([["pg.customers", "mongo.clients"]]);
    let mp = l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["pg", "mongo"], "preview": true, "merges": merges}),
    );
    assert_eq!(class_names(&mp), ["Customer", "Order"]);
    let published = l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["pg", "mongo"], "preview": false, "merges": merges, "fingerprints": mp["fingerprints"]}),
    );
    assert_eq!(published["saved"], true, "{published}");

    let i = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "pg", "containers": ["customers", "orders"]}),
    );
    assert_eq!(
        (
            i["imported"]["customers"].clone(),
            i["imported"]["orders"].clone()
        ),
        (3.into(), 2.into())
    );
    let i = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "mongo", "containers": ["clients"]}),
    );
    assert_eq!(i["imported"]["clients"], 4, "{i}");

    // one canonical class reaches the rows of BOTH sources, each typed by its physical container
    let customers = query(&l, "MATCH Customer RETURN *");
    let rows = customers["results"].as_array().unwrap();
    assert_eq!(rows.len(), 7, "{customers}");
    let by_type = |t: &str| rows.iter().filter(|r| r["type_name"] == t).count();
    assert_eq!((by_type("customers"), by_type("clients")), (3, 4));

    // values the driver cannot decode natively survive (NUMERIC, TIMESTAMPTZ)
    // the class generated from the table `orders` is `Order`, a keyword: it must be addressable
    let class_orders = query(&l, "MATCH Order RETURN *");
    assert_eq!(
        class_orders["results"].as_array().unwrap().len(),
        2,
        "{class_orders}"
    );
    // predicates compare the stored values: Order.id stays numeric although Mongo's
    // ObjectId `_id` widened the shared `id` property definition to Text
    let big = query(&l, "MATCH Order WHERE id > 10 RETURN *");
    assert_eq!(big["results"].as_array().unwrap().len(), 1, "{big}");
    let pricey = query(&l, "MATCH Order WHERE total > 10 RETURN *");
    assert_eq!(pricey["results"][0]["properties"]["id"], 11, "{pricey}");
    let bob = query(&l, "MATCH Customer WHERE id == 2 RETURN *");
    assert_eq!(
        bob["results"][0]["properties"]["email"], "bob@pg.test",
        "{bob}"
    );
    let orders = query(&l, "MATCH orders RETURN *");
    let mut totals: Vec<f64> = orders["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["properties"]["total"]
                .as_f64()
                .expect("total must not be NULL")
        })
        .collect();
    totals.sort_by(f64::total_cmp);
    assert_eq!(totals, [9.5, 20.0]);
    let ann = rows
        .iter()
        .find(|r| r["properties"]["email"] == "ann@pg.test")
        .expect("ann");
    assert_eq!(ann["properties"]["created_at"], "2026-01-01 00:00:00+00");

    // Mongo nesting is preserved, not flattened to text
    let oslo = rows
        .iter()
        .find(|r| r["properties"]["email_address"] == "d@mongo.test")
        .expect("d");
    assert_eq!(oslo["properties"]["profile"]["city"], "Oslo");

    l.assert_no_secret_anywhere();
}

/// One row of every common Postgres type must import exactly — a value the
/// driver cannot decode natively must never degrade to NULL silently.
#[test]
fn postgres_import_keeps_every_common_column_type() {
    let Some(pg) = connectors::Live::pg() else {
        return;
    };
    let pid = std::process::id();
    let dsn = connectors::pg_private_db(&pg.dsn, &format!("cert_multidb_types_{pid}"));
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE TYPE mood AS ENUM ('sad','happy')",
            "CREATE TABLE zoo (id INT PRIMARY KEY, s2 SMALLINT, i4 INT, i8 BIGINT, r4 REAL, r8 DOUBLE PRECISION, \
              n NUMERIC(10,2), nbig NUMERIC(30,10), b BOOL, t TEXT, vc VARCHAR(10), ch CHAR(3), u UUID, \
              ts TIMESTAMP, tstz TIMESTAMPTZ, d DATE, tm TIME, iv INTERVAL, j JSON, jb JSONB, \
              ia INT[], em mood, ip INET, by BYTEA)",
            "INSERT INTO zoo VALUES (1, 7, 42, 9007199254740993, 9.4, 0.1, 12345.67, \
              12345678901234567890.1234567890, true, 'héllo ✓', 'hi', 'ab', \
              '123e4567-e89b-12d3-a456-426614174000', '2026-01-02 03:04:05', '2026-01-02 03:04:05+02', \
              '2026-01-02', '03:04:05', '1 day 2 hours', '{\"a\": 1}', '{\"a\":   1}', '{1,2,3}', 'happy', \
              '192.168.0.1', '\\xdeadbeef')",
            "INSERT INTO zoo (id) VALUES (2)",
        ],
    );
    let l = Live::start(
        "pgtypes",
        "[[sources]]\nname = \"pg\"\nkind = \"postgres\"\nurl_env = \"MD_PG\"\n",
        &[("MD_PG", &dsn)],
        &["aikoql-dev-only"],
    );
    l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "pg"}),
    );
    let i = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "pg", "containers": ["zoo"]}),
    );
    assert_eq!(i["imported"]["zoo"], 2, "{i}");
    let q = query(&l, "MATCH zoo RETURN *");
    let rows = q["results"].as_array().unwrap();
    let prop = |id: i64| {
        rows.iter()
            .find(|r| r["properties"]["id"] == id)
            .unwrap_or_else(|| panic!("row {id} missing: {q}"))["properties"]
            .clone()
    };
    let full = prop(1);
    let want = serde_json::json!({
        "id": 1, "s2": 7, "i4": 42, "i8": 9007199254740993_i64, "r4": 9.4, "r8": 0.1,
        "n": 12345.67, "nbig": "12345678901234567890.1234567890", "b": true,
        "t": "héllo ✓", "vc": "hi", "ch": "ab ",
        "u": "123e4567-e89b-12d3-a456-426614174000",
        "ts": "2026-01-02 03:04:05", "tstz": "2026-01-02 01:04:05+00",
        "d": "2026-01-02", "tm": "03:04:05", "iv": "1 day 02:00:00",
        "j": "{\"a\": 1}", "jb": "{\"a\": 1}", "ia": "{1,2,3}", "em": "happy", "ip": "192.168.0.1/32",
    });
    for (k, v) in want.as_object().unwrap() {
        assert_eq!(&full[k], v, "column {k}: row = {full}");
    }
    assert!(!full["by"].is_null(), "bytea lost: {full}");
    let empty = prop(2);
    for k in want.as_object().unwrap().keys().filter(|k| *k != "id") {
        assert!(
            empty[k].is_null(),
            "NULL column {k} must stay null: {empty}"
        );
    }
}

/// MongoDB through the real routes: BSON types survive, `_id` stays readable,
/// a field seen as Int and String is published as Text.
#[test]
fn mongo_bson_types_survive_import_and_mixed_fields_publish_as_text() {
    use mongodb::bson::{doc, Bson, DateTime, Decimal128, Regex, Timestamp};
    let Some(mongo) = connectors::Live::mongo() else {
        return;
    };
    let mdb = format!("cert_multidb_bson_{}", std::process::id());
    let dec: Decimal128 = "12345.6789".parse().unwrap();
    connectors::mongo_seed(
        &mongo.mongo_uri,
        &mdb,
        "zoo",
        vec![
            doc! {"_id": 1_i32, "s": "héllo ✓", "i32": 7_i32, "i64": 9007199254740993_i64, "dbl": 0.1,
            "dec": dec, "dt": DateTime::parse_rfc3339_str("2026-01-02T03:04:05Z").unwrap(),
            "b": true, "arr": [1_i32, "a", {"x": 1_i32}], "doc": {"a": {"b": 2_i32}},
            "nul": Bson::Null, "ts": Timestamp { time: 1, increment: 2 },
            "re": Regex { pattern: "^a".into(), options: "i".into() }, "mixed": 5_i32},
            doc! {"_id": "str-id", "mixed": "five"},
        ],
    );
    let l = Live::start(
        "mongobson",
        &format!("[[sources]]\nname = \"m\"\nkind = \"mongodb\"\nurl_env = \"MD_MONGO\"\ndatabase = \"{mdb}\"\n"),
        &[("MD_MONGO", &mongo.mongo_uri)],
        &[],
    );
    l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "m"}),
    );
    let p = l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["m"], "preview": true}),
    );
    assert!(
        p["warnings"].to_string().contains("mixed observed types"),
        "{p}"
    );
    l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["m"], "preview": false, "fingerprints": p["fingerprints"]}),
    );
    let onto = data(&l.call("GET", "/api/v1/ontology", serde_json::json!({})));
    let ty = |name: &str| {
        onto["properties"]["property_defs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("no property {name}: {onto}"))["type"]
            .clone()
    };
    assert_eq!(ty("mixed"), "Text");
    assert_eq!(ty("i32"), "Int");
    assert_eq!(ty("dbl"), "Float");
    assert_eq!(ty("dec"), "Text");
    assert_eq!(ty("dt"), "DateTime");

    let i = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "m", "containers": ["zoo"]}),
    );
    assert_eq!(i["imported"]["zoo"], 2, "{i}");
    let q = query(&l, "MATCH Zoo RETURN *");
    let rows = q["results"].as_array().unwrap();
    let full = rows
        .iter()
        .find(|r| r["properties"]["_id"] == 1)
        .expect("_id 1 kept");
    let want = serde_json::json!({
        "_id": 1, "s": "héllo ✓", "i32": 7, "i64": 9007199254740993_i64, "dbl": 0.1,
        "dec": "12345.6789", "dt": "2026-01-02T03:04:05Z", "b": true,
        "arr": [1, "a", {"x": 1}], "doc": {"a": {"b": 2}}, "nul": null,
        "ts": {"t": 1, "i": 2}, "re": "/^a/i", "mixed": 5,
    });
    for (k, v) in want.as_object().unwrap() {
        assert_eq!(&full["properties"][k], v, "field {k}: {full}");
    }
    let other = rows
        .iter()
        .find(|r| r["properties"]["_id"] == "str-id")
        .expect("string _id kept");
    assert_eq!(other["properties"]["mixed"], "five");
}

/// Neo4j and ClickHouse sources through discover → preview → publish: classes
/// per label/table, typed properties, relationship domain/range from the graph.
#[test]
fn neo4j_and_clickhouse_discover_into_classes_relationships_and_types() {
    let (Some(neo), Some(ch)) = (connectors::Live::neo4j(), connectors::Live::clickhouse()) else {
        return;
    };
    // Neo4j community has one database: namespaced labels, cleaned before and after
    let clean = || {
        connectors::neo4j_exec(
            &neo.neo4j_uri,
            &neo.neo4j_user,
            &neo.neo4j_password,
            &[
                "MATCH (n:MdPerson) DETACH DELETE n",
                "MATCH (n:MdCompany) DETACH DELETE n",
            ],
        )
    };
    clean();
    connectors::neo4j_exec(
        &neo.neo4j_uri,
        &neo.neo4j_user,
        &neo.neo4j_password,
        &[
            "CREATE (a:MdPerson {name: 'Ann', md_age: 30}), (b:MdPerson {name: 'Bob', md_nick: 'bobby'}), \
              (c:MdCompany {name: 'Acme'}), (a)-[:MD_WORKS_AT {since: 2020}]->(c), (b)-[:MD_WORKS_AT]->(c)",
        ],
    );
    let chdb = format!("md_{}", std::process::id());
    connectors::clickhouse_exec(
        &ch.clickhouse_url,
        "default",
        &[
            &format!("DROP DATABASE IF EXISTS {chdb}"),
            &format!("CREATE DATABASE {chdb}"),
            &format!(
                "CREATE TABLE {chdb}.events (id UInt64, user_id UInt64, kind LowCardinality(String), \
                 at DateTime, note Nullable(String), score Float64) ENGINE = MergeTree \
                 PARTITION BY toYYYYMM(at) ORDER BY (id)"
            ),
            &format!("CREATE TABLE {chdb}.users (id UInt64, email String) ENGINE = MergeTree ORDER BY id"),
        ],
    );
    // the source URL carries the credentials, as an operator would configure it
    let (host, user, pass) = {
        let rest = neo.neo4j_uri.split_once("://").unwrap().1;
        (
            rest.to_string(),
            neo.neo4j_user.clone(),
            neo.neo4j_password.clone(),
        )
    };
    let neo_url = format!("http://{user}:{pass}@{host}");
    let l = Live::start(
        "neoch",
        &format!(
            "[[sources]]\nname = \"graph\"\nkind = \"neo4j\"\nurl_env = \"MD_NEO\"\n\n\
             [[sources]]\nname = \"olap\"\nkind = \"clickhouse\"\nurl_env = \"MD_CH\"\ndatabase = \"{chdb}\"\n"
        ),
        &[("MD_NEO", &neo_url), ("MD_CH", &ch.clickhouse_url)],
        &[&pass],
    );

    let g = l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "graph"}),
    );
    assert!(g["containers"].as_u64().unwrap() >= 2, "{g}");
    let o = l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "olap"}),
    );
    assert_eq!(o["containers"], 2, "{o}");

    let p = l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["graph", "olap"], "preview": true}),
    );
    let classes = class_names(&p);
    for want in ["MdPerson", "MdCompany", "Event", "User"] {
        assert!(
            classes.contains(&want.to_string()),
            "{want} missing from {classes:?}"
        );
    }
    l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["graph", "olap"], "preview": false, "fingerprints": p["fingerprints"]}),
    );
    let onto = data(&l.call("GET", "/api/v1/ontology", serde_json::json!({})));
    let defs = onto["properties"]["property_defs"].as_array().unwrap();
    let ty = |n: &str| {
        defs.iter()
            .find(|p| p["name"] == n)
            .unwrap_or_else(|| panic!("no {n}: {onto}"))["type"]
            .clone()
    };
    // sampled across nodes, not from one: Bob's `md_nick` and Ann's `md_age` are both found
    assert_eq!(ty("md_nick"), "Text");
    assert_eq!(ty("md_age"), "Int");
    assert_eq!(ty("at"), "DateTime");
    assert_eq!(ty("score"), "Float");
    assert_eq!(ty("user_id"), "Int");
    let note = defs.iter().find(|p| p["name"] == "note").unwrap();
    assert_eq!(note["nullable"], true, "{note}");
    // domain and range come from the actual graph pattern
    let rel = onto["properties"]["relationships"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"].as_str().unwrap().contains("md_works_at"))
        .unwrap_or_else(|| {
            panic!(
                "no works_at relationship: {}",
                onto["properties"]["relationships"]
            )
        });
    assert_eq!(
        (rel["domain"].clone(), rel["range"].clone()),
        ("MdPerson".into(), "MdCompany".into()),
        "{rel}"
    );

    l.assert_no_secret_anywhere();
    clean();
    connectors::clickhouse_exec(
        &ch.clickhouse_url,
        "default",
        &[&format!("DROP DATABASE IF EXISTS {chdb}")],
    );
}

/// The suggestion workflow end to end on real databases: preview proposes the
/// merge and the property alignment, the reviewer applies it, and the canonical
/// property answers across Postgres AND MongoDB.
#[test]
fn a_suggested_merge_with_aligned_properties_queries_across_both_databases() {
    let (Some(pg), Some(mongo)) = (connectors::Live::pg(), connectors::Live::mongo()) else {
        return;
    };
    let pid = std::process::id();
    let dsn = connectors::pg_private_db(&pg.dsn, &format!("cert_multidb_sug_{pid}"));
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE TABLE customers (id INT PRIMARY KEY, email TEXT NOT NULL UNIQUE, tier TEXT)",
            "CREATE TABLE orders (id INT PRIMARY KEY, customer_id INT REFERENCES customers(id), total NUMERIC(10,2))",
            "INSERT INTO customers VALUES (1,'ann@pg.test','gold'),(2,'bob@pg.test','silver')",
            "INSERT INTO orders VALUES (10,1,9.50)",
        ],
    );
    let mdb = format!("cert_multidb_sug_{pid}");
    connectors::mongo_seed(
        &mongo.mongo_uri,
        &mdb,
        "clients",
        vec![
            mongodb::bson::doc! {"email_address": "d@mongo.test", "tier": "bronze"},
            mongodb::bson::doc! {"email_address": "e@mongo.test", "tier": "gold"},
        ],
    );
    let l = Live::start(
        "suggest",
        &format!(
            "[[sources]]\nname = \"pg\"\nkind = \"postgres\"\nurl_env = \"MD_PG\"\n\n[[sources]]\nname = \"mongo\"\nkind = \"mongodb\"\nurl_env = \"MD_MONGO\"\ndatabase = \"{mdb}\"\n"
        ),
        &[("MD_PG", &dsn), ("MD_MONGO", &mongo.mongo_uri)],
        &["aikoql-dev-only"],
    );
    for s in ["pg", "mongo"] {
        l.post("/api/v1/sources/discover", serde_json::json!({"source": s}));
    }
    let preview = l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["pg", "mongo"], "preview": true}),
    );
    let suggestions = preview["suggestions"]
        .as_array()
        .expect("preview carries suggestions");
    assert_eq!(
        suggestions.len(),
        1,
        "only customers~clients: {suggestions:?}"
    );
    let s = &suggestions[0];
    assert_eq!(
        (s["a"].as_str(), s["b"].as_str()),
        (Some("pg.customers"), Some("mongo.clients"))
    );
    assert!(s["confidence"].as_f64().unwrap() >= 0.5, "{s}");
    assert!(!s["evidence"].as_array().unwrap().is_empty(), "{s}");
    let pair = &s["properties"][0];
    assert_eq!(
        (pair["a_field"].as_str(), pair["b_field"].as_str()),
        (Some("email"), Some("email_address")),
        "{s}"
    );
    // a suggestion is advice: nothing was merged
    assert_eq!(class_names(&preview), ["Customer", "Order", "Client"]);

    // the reviewer accepts it: the merge object form carries the alignment
    let merges = serde_json::json!([{
        "a": s["a"], "b": s["b"],
        "properties": s["properties"].as_array().unwrap().iter()
            .map(|p| serde_json::json!([p["a_field"], p["b_field"]])).collect::<Vec<_>>(),
    }]);
    let applied = l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["pg", "mongo"], "preview": true, "merges": merges}),
    );
    assert_eq!(class_names(&applied), ["Customer", "Order"]);
    let clients_map = applied["mappings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["physical_type"] == "clients")
        .unwrap();
    assert_eq!(
        clients_map["property_map"]["email_address"], "email",
        "{clients_map}"
    );
    l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["pg", "mongo"], "preview": false, "merges": merges, "fingerprints": applied["fingerprints"]}),
    );
    l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "pg", "containers": ["customers", "orders"]}),
    );
    l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "mongo", "containers": ["clients"]}),
    );

    // the canonical property answers in BOTH databases
    let pg_hit = query(&l, "MATCH Customer WHERE email == \"ann@pg.test\" RETURN *");
    assert_eq!(pg_hit["results"].as_array().unwrap().len(), 1, "{pg_hit}");
    let mongo_hit = query(
        &l,
        "MATCH Customer WHERE email == \"d@mongo.test\" RETURN *",
    );
    let rows = mongo_hit["results"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the mongo row must answer to the canonical `email`: {mongo_hit}"
    );
    assert_eq!(rows[0]["type_name"], "clients");
    // and a shared canonical predicate spans both
    let gold = query(&l, "MATCH Customer WHERE tier == \"gold\" RETURN *");
    assert_eq!(gold["results"].as_array().unwrap().len(), 2, "{gold}");
    l.assert_no_secret_anywhere();
}

/// The stored schema KO of one container, read back through the query language.
fn stored_container(l: &Live, source: &str, name: &str) -> serde_json::Value {
    let q = query(l, "MATCH aikoql:source-table RETURN *");
    q["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| {
            r["properties"]["source"] == source && r["properties"]["container"]["name"] == name
        })
        .unwrap_or_else(|| panic!("no stored schema for {source}.{name}: {q}"))["properties"]
        ["container"]
        .clone()
}

/// Postgres indexes (incl. unique indexes that are not constraints, expression
/// and partial indexes) and CHECK constraints are discovered, persisted, and
/// their later change shows in the diff.
#[test]
fn postgres_indexes_unique_indexes_and_checks_are_discovered_and_diffed() {
    let Some(pg) = connectors::Live::pg() else {
        return;
    };
    let dsn =
        connectors::pg_private_db(&pg.dsn, &format!("cert_multidb_idx_{}", std::process::id()));
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE TABLE accounts (id INT PRIMARY KEY, email TEXT NOT NULL, handle TEXT, org INT, seq INT, \
              code TEXT, balance NUMERIC CHECK (balance >= 0), CONSTRAINT handle_fmt CHECK (handle ~ '^[a-z]+$'))",
            "CREATE UNIQUE INDEX accounts_email_uidx ON accounts (lower(email))",
            "CREATE UNIQUE INDEX accounts_org_seq_uidx ON accounts (org, seq)",
            "CREATE UNIQUE INDEX accounts_handle_partial ON accounts (handle) WHERE handle IS NOT NULL",
            "CREATE UNIQUE INDEX accounts_code_uidx ON accounts (code)",
            "CREATE INDEX accounts_balance_idx ON accounts USING hash (balance)",
        ],
    );
    let l = Live::start(
        "pgidx",
        "[[sources]]\nname = \"pg\"\nkind = \"postgres\"\nurl_env = \"MD_PG\"\n",
        &[("MD_PG", &dsn)],
        &["aikoql-dev-only"],
    );
    l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "pg"}),
    );
    let c = stored_container(&l, "pg", "accounts");
    let idx = |name: &str| {
        c["indexes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == name)
            .unwrap_or_else(|| panic!("no index {name}: {}", c["indexes"]))
            .clone()
    };
    assert!(
        c["indexes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["name"] != "accounts_pkey"),
        "the primary key is not a secondary index: {}",
        c["indexes"]
    );
    let email = idx("accounts_email_uidx");
    assert_eq!(
        (
            email["unique"].clone(),
            email["columns"][0].as_str().map(str::to_lowercase)
        ),
        (true.into(), Some("lower(email)".to_string())),
        "{email}"
    );
    let org = idx("accounts_org_seq_uidx");
    assert_eq!(
        (org["unique"].clone(), org["columns"].clone()),
        (true.into(), serde_json::json!(["org", "seq"]))
    );
    let partial = idx("accounts_handle_partial");
    assert!(
        partial["predicate"]
            .as_str()
            .unwrap()
            .contains("handle IS NOT NULL"),
        "{partial}"
    );
    let balance = idx("accounts_balance_idx");
    assert_eq!(
        (balance["unique"].clone(), balance["method"].clone()),
        (false.into(), "hash".into())
    );

    // uniqueness the SOURCE guarantees: a full-column unique index counts, an
    // expression or partial one does not
    let groups = c["unique_groups"].to_string();
    assert!(
        groups.contains("[\"org\",\"seq\"]"),
        "composite unique index: {groups}"
    );
    assert!(
        groups.contains("[\"code\"]"),
        "single unique index: {groups}"
    );
    assert!(
        !groups.contains("handle") && !groups.contains("lower"),
        "partial/expression index is no guarantee: {groups}"
    );
    let code = c["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "code")
        .unwrap();
    assert_eq!(code["is_unique"], true, "{code}");
    let handle = c["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "handle")
        .unwrap();
    assert_eq!(handle["is_unique"], false, "{handle}");

    let checks = c["checks"].to_string();
    assert!(
        checks.contains("handle_fmt") && checks.contains("handle ~"),
        "{checks}"
    );
    assert!(
        checks.contains("accounts_balance_check") && checks.contains(">="),
        "{checks}"
    );

    // an unchanged source is a no-op; a new index and a dropped check show in the diff
    let again = l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "pg"}),
    );
    assert_eq!(again["changed"], false);
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE INDEX accounts_tail_idx ON accounts (seq DESC)",
            "ALTER TABLE accounts DROP CONSTRAINT handle_fmt",
        ],
    );
    let drift = l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "pg"}),
    );
    assert_eq!(drift["changed"], true);
    assert_eq!(
        drift["diff"]["changed"][0]["added_indexes"],
        serde_json::json!(["accounts_tail_idx"]),
        "{drift}"
    );
    assert_eq!(
        drift["diff"]["changed"][0]["removed_checks"],
        serde_json::json!(["handle_fmt"]),
        "{drift}"
    );
    l.assert_no_secret_anywhere();
}

/// Neo4j uniqueness constraints and indexes become container metadata: a
/// uniqueness constraint is a guarantee, a plain index is not.
#[test]
fn neo4j_constraints_and_indexes_are_discovered() {
    let Some(neo) = connectors::Live::neo4j() else {
        return;
    };
    let run = |stmts: &[&str]| {
        connectors::neo4j_exec(&neo.neo4j_uri, &neo.neo4j_user, &neo.neo4j_password, stmts)
    };
    // Neo4j refuses schema and data statements in one transaction: one call each
    let cleanup = || {
        run(&["DROP CONSTRAINT md_acct_handle_unique IF EXISTS"]);
        run(&["DROP INDEX md_acct_score IF EXISTS"]);
        run(&["DROP INDEX md_acct_pair IF EXISTS"]);
        run(&["MATCH (n:MdAcct) DETACH DELETE n"]);
    };
    cleanup();
    run(&[
        "CREATE (:MdAcct {handle: 'a', md_score: 1, md_city: 'x'}), (:MdAcct {handle: 'b', md_score: 2, md_city: 'y'})",
    ]);
    run(&["CREATE CONSTRAINT md_acct_handle_unique FOR (n:MdAcct) REQUIRE n.handle IS UNIQUE"]);
    run(&["CREATE INDEX md_acct_score FOR (n:MdAcct) ON (n.md_score)"]);
    run(&["CREATE INDEX md_acct_pair FOR (n:MdAcct) ON (n.md_city, n.md_score)"]);
    let (user, pass) = (neo.neo4j_user.clone(), neo.neo4j_password.clone());
    let host = neo.neo4j_uri.split_once("://").unwrap().1.to_string();
    let l = Live::start(
        "neoidx",
        "[[sources]]\nname = \"graph\"\nkind = \"neo4j\"\nurl_env = \"MD_NEO\"\n",
        &[("MD_NEO", &format!("http://{user}:{pass}@{host}"))],
        &[&pass],
    );
    l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "graph"}),
    );
    let c = stored_container(&l, "graph", "MdAcct");
    let idx = |name: &str| {
        c["indexes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == name)
            .unwrap_or_else(|| panic!("no index {name}: {}", c["indexes"]))
            .clone()
    };
    let score = idx("md_acct_score");
    assert_eq!(
        (
            score["unique"].clone(),
            score["columns"].clone(),
            score["method"].clone()
        ),
        (
            false.into(),
            serde_json::json!(["md_score"]),
            "range".into()
        ),
        "{score}"
    );
    assert_eq!(
        idx("md_acct_pair")["columns"],
        serde_json::json!(["md_city", "md_score"])
    );
    // the constraint's backing index is unique
    assert_eq!(idx("md_acct_handle_unique")["unique"], true);
    assert!(
        c["unique_groups"].to_string().contains("[\"handle\"]"),
        "{}",
        c["unique_groups"]
    );
    let field = |n: &str| {
        c["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == n)
            .unwrap()
            .clone()
    };
    assert_eq!(field("handle")["is_unique"], true);
    assert_eq!(field("md_score")["is_unique"], false);
    // the token lookup index every Neo4j database has is not a property index of this label
    assert!(c["indexes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["name"] != "index_343aff4e" && !i["columns"].as_array().unwrap().is_empty()));
    cleanup();
}

/// ClickHouse storage facts (engine, keys, skipping indexes), views and
/// materialized views with their lineage; a materialized view's hidden inner
/// table is not a container of its own.
#[test]
fn clickhouse_storage_views_and_lineage_are_discovered() {
    let Some(ch) = connectors::Live::clickhouse() else {
        return;
    };
    let db = format!("md_view_{}", std::process::id());
    let exec = |stmts: &[String]| {
        let refs: Vec<&str> = stmts.iter().map(String::as_str).collect();
        connectors::clickhouse_exec(&ch.clickhouse_url, "default", &refs);
    };
    exec(&[
        format!("DROP DATABASE IF EXISTS {db}"),
        format!("CREATE DATABASE {db}"),
        format!(
            "CREATE TABLE {db}.events (id UInt64, kind LowCardinality(String), at DateTime, note Nullable(String), \
             INDEX idx_note note TYPE bloom_filter GRANULARITY 4) ENGINE = MergeTree \
             PARTITION BY toYYYYMM(at) PRIMARY KEY kind ORDER BY (kind, id)"
        ),
        format!("CREATE TABLE {db}.rollup_target (day Date, n UInt64) ENGINE = SummingMergeTree ORDER BY day"),
        format!(
            "CREATE MATERIALIZED VIEW {db}.events_daily ENGINE = SummingMergeTree ORDER BY day \
             AS SELECT toDate(at) AS day, count() AS n FROM {db}.events GROUP BY day"
        ),
        format!(
            "CREATE MATERIALIZED VIEW {db}.to_rollup TO {db}.rollup_target \
             AS SELECT toDate(at) AS day, count() AS n FROM {db}.events GROUP BY day"
        ),
        format!("CREATE VIEW {db}.recent AS SELECT id, kind FROM {db}.events WHERE kind = 'x'"),
    ]);
    let l = Live::start(
        "chview",
        &format!("[[sources]]\nname = \"olap\"\nkind = \"clickhouse\"\nurl_env = \"MD_CH\"\ndatabase = \"{db}\"\n"),
        &[("MD_CH", &ch.clickhouse_url)],
        &["aikoql-dev-only"],
    );
    let d = l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "olap"}),
    );
    // events, rollup_target, events_daily, to_rollup, recent - the inner storage table is hidden
    assert_eq!(d["containers"], 5, "{d}");

    let events = stored_container(&l, "olap", "events");
    assert_eq!(
        events["kind"],
        serde_json::Value::Null,
        "a plain table has no kind: {events}"
    );
    let p = &events["properties"];
    assert_eq!(p["engine"], "MergeTree", "{events}");
    assert_eq!(p["sorting_key"], "kind, id");
    assert_eq!(p["primary_key"], "kind");
    assert_eq!(p["partition_key"], "toYYYYMM(at)");
    let skip = events["indexes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "idx_note")
        .expect("skipping index");
    assert_eq!(
        (
            skip["columns"].clone(),
            skip["method"].clone(),
            skip["unique"].clone()
        ),
        (
            serde_json::json!(["note"]),
            "bloom_filter".into(),
            false.into()
        ),
        "{skip}"
    );

    let mv = stored_container(&l, "olap", "events_daily");
    assert_eq!(mv["kind"], "materialized_view", "{mv}");
    assert!(
        mv["properties"]["as_select"]
            .as_str()
            .unwrap()
            .contains("FROM"),
        "{mv}"
    );
    assert_eq!(stored_container(&l, "olap", "recent")["kind"], "view");
    assert_eq!(
        stored_container(&l, "olap", "to_rollup")["kind"],
        "materialized_view"
    );

    // lineage: each view is derived from the table it reads
    let src = query(&l, "MATCH aikoql:source RETURN *");
    let refs = src["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["properties"]["name"] == "olap")
        .unwrap()["properties"]["references"]
        .to_string();
    for view in ["events_daily", "to_rollup"] {
        assert!(
            refs.contains(view) && refs.contains("derived_from"),
            "{view} not derived from events: {refs}"
        );
    }
    l.assert_no_secret_anywhere();
    exec(&[format!("DROP DATABASE IF EXISTS {db}")]);
}

/// Neo4j nodes AND the relationships between the imported labels import through
/// the production route: bounded, idempotent, relationships kept with their
/// properties, anything pointing outside the selection left out.
#[test]
fn neo4j_nodes_and_relationships_import_through_the_route() {
    let Some(neo) = connectors::Live::neo4j() else {
        return;
    };
    let run = |stmts: &[&str]| {
        connectors::neo4j_exec(&neo.neo4j_uri, &neo.neo4j_user, &neo.neo4j_password, stmts)
    };
    let clean = || {
        for label in ["MdImpPerson", "MdImpCompany", "MdImpOther"] {
            run(&[&format!("MATCH (n:{label}) DETACH DELETE n")]);
        }
    };
    clean();
    run(&[
        "CREATE (a:MdImpPerson {name: 'Ann', md_imp_age: 30}), (b:MdImpPerson {name: 'Bob'}), (c:MdImpPerson {name: 'Cy'}), \
          (x:MdImpCompany {name: 'Acme'}), (y:MdImpCompany {name: 'Beta'}), (o:MdImpOther {name: 'Elsewhere'}), \
          (a)-[:MD_IMP_WORKS_AT {since: 2020}]->(x), (b)-[:MD_IMP_WORKS_AT {since: 2021}]->(x), (c)-[:MD_IMP_WORKS_AT]->(y), \
          (a)-[:MD_IMP_KNOWS]->(o)",
    ]);
    let (user, pass) = (neo.neo4j_user.clone(), neo.neo4j_password.clone());
    let host = neo.neo4j_uri.split_once("://").unwrap().1.to_string();
    let url = format!("http://{user}:{pass}@{host}");
    let toml = |cap: u32| {
        format!("[[sources]]\nname = \"graph\"\nkind = \"neo4j\"\nurl_env = \"MD_NEO\"\nimport_max_rows = {cap}\n")
    };
    let l = Live::start("neoimp", &toml(10_000), &[("MD_NEO", &url)], &[&pass]);
    l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "graph"}),
    );
    let i = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "graph", "containers": ["MdImpPerson", "MdImpCompany"]}),
    );
    assert_eq!(
        (
            i["imported"]["MdImpPerson"].clone(),
            i["imported"]["MdImpCompany"].clone()
        ),
        (3.into(), 2.into()),
        "{i}"
    );
    assert_eq!(i["truncated"], serde_json::json!([]));
    assert_eq!(i["skipped"], serde_json::json!([]), "{i}");

    let people = query(&l, "MATCH MdImpPerson RETURN *");
    let rows = people["results"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{people}");
    let companies = query(&l, "MATCH MdImpCompany RETURN *");
    let acme = companies["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["properties"]["name"] == "Acme")
        .unwrap();
    let ann = rows
        .iter()
        .find(|r| r["properties"]["name"] == "Ann")
        .unwrap();
    assert_eq!(
        ann["properties"]["labels"],
        serde_json::json!(["MdImpPerson"])
    );
    assert_eq!(ann["properties"]["md_imp_age"], 30);
    let works = ann["properties"]["rel:MD_IMP_WORKS_AT"]
        .as_array()
        .expect("relationship kept on its start node");
    assert_eq!(works.len(), 1, "{ann}");
    assert_eq!(works[0]["since"], 2020);
    assert_eq!(
        works[0]["target"], acme["koid"],
        "the relationship points at the imported company"
    );
    assert!(
        ann["properties"].get("rel:MD_IMP_KNOWS").is_none(),
        "a relationship to an unselected label is not imported: {ann}"
    );

    // idempotent: the same graph again writes nothing
    let again = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "graph", "containers": ["MdImpPerson", "MdImpCompany"]}),
    );
    assert_eq!(
        (
            again["imported"]["MdImpPerson"].clone(),
            again["unchanged"]["MdImpPerson"].clone()
        ),
        (0.into(), 3.into()),
        "{again}"
    );

    // the row cap bounds a label and says so
    let capped = Live::start("neoimpcap", &toml(2), &[("MD_NEO", &url)], &[&pass]);
    capped.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "graph"}),
    );
    let c = capped.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "graph", "containers": ["MdImpPerson"]}),
    );
    assert_eq!(c["imported"]["MdImpPerson"], 2, "{c}");
    assert_eq!(c["truncated"], serde_json::json!(["MdImpPerson"]));
    l.assert_no_secret_anywhere();
    capped.assert_no_secret_anywhere();
    clean();
}

/// ClickHouse tables import through the production route: every type exact,
/// replacing engines at their latest versions, a view readable, a big table cut
/// at the row cap quickly and reported, and nothing leaked.
#[test]
fn clickhouse_tables_import_through_the_route() {
    let Some(ch) = connectors::Live::clickhouse() else {
        return;
    };
    let db = format!("md_imp_{}", std::process::id());
    let exec = |stmts: &[String]| {
        let refs: Vec<&str> = stmts.iter().map(String::as_str).collect();
        connectors::clickhouse_exec(&ch.clickhouse_url, "default", &refs);
    };
    exec(&[
        format!("DROP DATABASE IF EXISTS {db}"),
        format!("CREATE DATABASE {db}"),
        format!(
            "CREATE TABLE {db}.zoo (id UInt64, i64 Int64, u64 UInt64, f Float64, fnan Float64, dec Decimal(20, 5), \
             decbig Decimal(38, 10), s String, nul Nullable(String), arr Array(UInt64), mp Map(String, UInt8), \
             tp Tuple(a UInt8, b String), d Date, dt DateTime, dt64 DateTime64(3), en Enum8('a' = 1, 'b' = 2), \
             u UUID, ip IPv4, lc LowCardinality(String), b Bool) ENGINE = MergeTree ORDER BY id"
        ),
        format!(
            "INSERT INTO {db}.zoo VALUES (1, -9223372036854775808, 18446744073709551615, 0.1, nan, 12345.67, \
             123456789012345678901234567.8901234567, 'héllo ✓', NULL, [1, 2], {{'a': 1}}, (1, 'x'), '2026-01-02', \
             '2026-01-02 03:04:05', '2026-01-02 03:04:05.123', 'b', '123e4567-e89b-12d3-a456-426614174000', \
             '192.168.0.1', 'lc', true)"
        ),
        format!("INSERT INTO {db}.zoo (id) VALUES (2)"),
        format!("CREATE TABLE {db}.versions (k UInt32, v String, ver UInt32) ENGINE = ReplacingMergeTree(ver) ORDER BY k"),
        format!("INSERT INTO {db}.versions VALUES (1, 'old', 1)"),
        format!("INSERT INTO {db}.versions VALUES (1, 'new', 2)"),
        format!("INSERT INTO {db}.versions VALUES (2, 'only', 1)"),
        format!("CREATE VIEW {db}.recent AS SELECT id, s FROM {db}.zoo WHERE id = 1"),
        format!("CREATE TABLE {db}.empty (id UInt8) ENGINE = MergeTree ORDER BY id"),
        format!("CREATE TABLE {db}.big (id UInt64, label String) ENGINE = MergeTree ORDER BY id"),
        format!("INSERT INTO {db}.big SELECT number, toString(number) FROM numbers(100000)"),
    ]);
    let toml = |cap: u32| {
        format!("[[sources]]\nname = \"olap\"\nkind = \"clickhouse\"\nurl_env = \"MD_CH\"\ndatabase = \"{db}\"\nimport_max_rows = {cap}\n")
    };
    let l = Live::start(
        "chimp",
        &toml(10_000),
        &[("MD_CH", &ch.clickhouse_url)],
        &["aikoql-dev-only"],
    );
    l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "olap"}),
    );
    let i = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "olap", "containers": ["zoo", "versions", "recent", "empty"]}),
    );
    assert_eq!(i["skipped"], serde_json::json!([]), "{i}");
    assert_eq!(
        (
            i["imported"]["zoo"].clone(),
            i["imported"]["versions"].clone(),
            i["imported"]["recent"].clone(),
            i["imported"]["empty"].clone()
        ),
        (2.into(), 2.into(), 1.into(), 0.into()),
        "FINAL: the replaced version is not a row; {i}"
    );

    let zoo = query(&l, "MATCH zoo RETURN *");
    let rows = zoo["results"].as_array().unwrap();
    let full = rows
        .iter()
        .find(|r| r["properties"]["id"] == 1)
        .expect("row 1");
    let want = serde_json::json!({
        "id": 1, "i64": i64::MIN, "u64": "18446744073709551615", "f": 0.1, "fnan": "nan", "dec": 12345.67,
        "decbig": "123456789012345678901234567.8901234567", "s": "héllo ✓", "nul": null, "arr": [1, 2],
        "mp": {"a": 1}, "tp": {"a": 1, "b": "x"}, "d": "2026-01-02", "dt": "2026-01-02 03:04:05",
        "dt64": "2026-01-02 03:04:05.123", "en": "b", "u": "123e4567-e89b-12d3-a456-426614174000",
        "ip": "192.168.0.1", "lc": "lc", "b": true,
    });
    for (k, v) in want.as_object().unwrap() {
        assert_eq!(&full["properties"][k], v, "column {k}: {full}");
    }
    let versions = query(&l, "MATCH versions RETURN *");
    let mut seen: Vec<(i64, String)> = versions["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["properties"]["k"].as_i64().unwrap(),
                r["properties"]["v"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(seen, [(1, "new".to_string()), (2, "only".to_string())]);
    assert_eq!(
        query(&l, "MATCH recent RETURN *")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // idempotent: the same snapshot again writes nothing
    let again = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "olap", "containers": ["zoo", "versions"]}),
    );
    assert_eq!(
        (
            again["imported"]["zoo"].clone(),
            again["unchanged"]["zoo"].clone()
        ),
        (0.into(), 2.into()),
        "{again}"
    );

    // a big table is cut at the cap, quickly, and says so
    let capped = Live::start(
        "chimpcap",
        &toml(1_000),
        &[("MD_CH", &ch.clickhouse_url)],
        &["aikoql-dev-only"],
    );
    capped.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "olap"}),
    );
    let started = Instant::now();
    let c = capped.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "olap", "containers": ["big"]}),
    );
    assert_eq!(c["imported"]["big"], 1_000, "{c}");
    assert_eq!(c["truncated"], serde_json::json!(["big"]));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "cutting a 100k-row table took {:?}",
        started.elapsed()
    );
    // the cut is the first 1000 rows in sorting-key order, repeatably
    let ids: std::collections::BTreeSet<i64> = query(&capped, "MATCH big RETURN *")["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["properties"]["id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        (ids.first().copied(), ids.last().copied()),
        (Some(0), Some(999))
    );
    l.assert_no_secret_anywhere();
    capped.assert_no_secret_anywhere();
    exec(&[format!("DROP DATABASE IF EXISTS {db}")]);
}

/// Relationships between multi-label nodes and self-loops survive discovery and
/// import (found in review: discovery looked only at each node's first label,
/// and the one-row-per-relationship filter dropped a node's loop to itself).
#[test]
fn neo4j_import_keeps_multilabel_and_self_loop_relationships() {
    let Some(neo) = connectors::Live::neo4j() else {
        return;
    };
    let run = |stmts: &[&str]| {
        connectors::neo4j_exec(&neo.neo4j_uri, &neo.neo4j_user, &neo.neo4j_password, stmts)
    };
    let clean = || {
        for label in ["MdMlPerson", "MdMlEmployee", "MdMlCompany", "MdMlCustomer"] {
            run(&[&format!("MATCH (n:{label}) DETACH DELETE n")]);
        }
    };
    clean();
    run(&[
        "CREATE (m:MdMlPerson:MdMlEmployee {name: 'Meg'}), (z:MdMlCompany:MdMlCustomer {name: 'Gamma'}), \
          (m)-[:MD_ML_WORKS_AT {since: 2022}]->(z), (m)-[:MD_ML_PARENT_OF]->(m)",
    ]);
    let (user, pass) = (neo.neo4j_user.clone(), neo.neo4j_password.clone());
    let host = neo.neo4j_uri.split_once("://").unwrap().1.to_string();
    let l = Live::start(
        "neoml",
        "[[sources]]\nname = \"graph\"\nkind = \"neo4j\"\nurl_env = \"MD_NEO\"\n",
        &[("MD_NEO", &format!("http://{user}:{pass}@{host}"))],
        &[&pass],
    );
    l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "graph"}),
    );
    // the edge is between the SECOND labels of its nodes: selecting only those must find it
    let i = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "graph", "containers": ["MdMlEmployee", "MdMlCustomer"]}),
    );
    assert_eq!(i["skipped"], serde_json::json!([]), "{i}");
    let meg = query(&l, "MATCH MdMlEmployee RETURN *");
    let meg = &meg["results"][0]["properties"];
    let gamma = query(&l, "MATCH MdMlCustomer RETURN *");
    let works = meg["rel:MD_ML_WORKS_AT"]
        .as_array()
        .unwrap_or_else(|| panic!("the edge between second labels was lost: {meg}"));
    assert_eq!(
        (works[0]["since"].clone(), works[0]["target"].clone()),
        (2022.into(), gamma["results"][0]["koid"].clone())
    );
    // a node's relationship to itself is kept, pointing at itself
    let own = query(&l, "MATCH MdMlEmployee RETURN *")["results"][0]["koid"].clone();
    let parent = meg["rel:MD_ML_PARENT_OF"]
        .as_array()
        .unwrap_or_else(|| panic!("the self-loop was dropped: {meg}"));
    assert_eq!(
        (parent.len(), parent[0]["target"].clone()),
        (1, own),
        "a loop is one relationship, not two"
    );
    l.assert_no_secret_anywhere();
    clean();
}

/// Sorting keys are server-rendered expressions; they must order the import
/// whatever they contain (review claimed expression or odd-named keys break it).
#[test]
fn clickhouse_expression_and_oddly_named_sorting_keys_import() {
    let Some(ch) = connectors::Live::clickhouse() else {
        return;
    };
    let db = format!("md_ord_{}", std::process::id());
    let exec = |stmts: &[String]| {
        let refs: Vec<&str> = stmts.iter().map(String::as_str).collect();
        connectors::clickhouse_exec(&ch.clickhouse_url, "default", &refs);
    };
    exec(&[
        format!("DROP DATABASE IF EXISTS {db}"),
        format!("CREATE DATABASE {db}"),
        format!(
            "CREATE TABLE {db}.odd (`we ird` UInt8, `a``b` String, at DateTime, `select` UInt8) ENGINE = MergeTree \
             ORDER BY (toDate(at), `we ird`, lower(`a``b`), `select`)"
        ),
        format!("INSERT INTO {db}.odd VALUES (2, 'B', '2026-01-02 00:00:00', 1), (1, 'a', '2026-01-02 00:00:00', 2), (9, 'z', '2026-01-01 00:00:00', 3)"),
    ]);
    let l = Live::start(
        "chodd",
        &format!("[[sources]]\nname = \"olap\"\nkind = \"clickhouse\"\nurl_env = \"MD_CH\"\ndatabase = \"{db}\"\n"),
        &[("MD_CH", &ch.clickhouse_url)],
        &["aikoql-dev-only"],
    );
    l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "olap"}),
    );
    let i = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "olap", "containers": ["odd"]}),
    );
    assert_eq!(
        (i["imported"]["odd"].clone(), i["skipped"].clone()),
        (3.into(), serde_json::json!([])),
        "{i}"
    );
    let rows = query(&l, "MATCH odd RETURN *");
    let mut weird: Vec<i64> = rows["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["properties"]["we ird"].as_i64().unwrap())
        .collect();
    weird.sort();
    assert_eq!(weird, [1, 2, 9]);
    exec(&[format!("DROP DATABASE IF EXISTS {db}")]);
}

/// Case study (docs/ONTOLOGY-IN-PRACTICE.md, Phase A): a per-device aggregate VIEW in
/// ClickHouse has no key of its own. The preview proposes `visitor_id` with its
/// evidence, the owner declares it, and re-imports then keep ONE object per device
/// with a version history — instead of a new object per change.
#[test]
fn a_declared_key_gives_a_clickhouse_view_one_object_per_device_and_a_history() {
    let Some(ch) = connectors::Live::clickhouse() else {
        return;
    };
    let db = format!("md_idk_{}", std::process::id());
    let exec = |stmts: &[String]| {
        let refs: Vec<&str> = stmts.iter().map(String::as_str).collect();
        connectors::clickhouse_exec(&ch.clickhouse_url, "default", &refs);
    };
    exec(&[
        format!("DROP DATABASE IF EXISTS {db}"),
        format!("CREATE DATABASE {db}"),
        format!("CREATE TABLE {db}.devices (visitor_id String, updated_at DateTime, risk Float32) ENGINE = ReplacingMergeTree(updated_at) ORDER BY visitor_id"),
        format!("INSERT INTO {db}.devices SELECT concat('v', toString(number)), now(), 0.1 FROM numbers(20)"),
        format!("CREATE TABLE {db}.visits (visitor_id String, ts DateTime, ip String) ENGINE = MergeTree ORDER BY (visitor_id, ts)"),
        format!("INSERT INTO {db}.visits SELECT concat('v', toString(number % 20)), now() - number, '203.0.113.1' FROM numbers(200)"),
        format!("CREATE VIEW {db}.device_activity AS SELECT visitor_id, count() AS visit_count, uniqExact(ip) AS ips FROM {db}.visits GROUP BY visitor_id"),
    ]);
    let l = Live::start(
        "idkey",
        &format!("[[sources]]\nname = \"olap\"\nkind = \"clickhouse\"\nurl_env = \"MD_CH\"\ndatabase = \"{db}\"\n"),
        &[("MD_CH", &ch.clickhouse_url)],
        &["aikoql-dev-only"],
    );
    l.post(
        "/api/v1/sources/discover",
        serde_json::json!({"source": "olap"}),
    );
    let p = l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["olap"], "preview": true}),
    );
    let sug = |c: &str| {
        p["identities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["container"] == c)
            .cloned()
            .unwrap_or_else(|| panic!("no suggestion for {c}: {}", p["identities"]))
    };
    let devices = sug("olap.devices");
    assert_eq!(
        (devices["declared"].clone(), devices["columns"].clone()),
        (true.into(), serde_json::json!(["visitor_id"])),
        "{devices}"
    );
    let activity = sug("olap.device_activity");
    assert_eq!(
        (activity["declared"].clone(), activity["columns"].clone()),
        (false.into(), serde_json::json!(["visitor_id"])),
        "{activity}"
    );
    assert!(
        activity["evidence"].to_string().contains("Device"),
        "{activity}"
    );
    assert_eq!(
        sug("olap.visits")["columns"],
        serde_json::json!([]),
        "a table of events gets no guessed key"
    );

    // the owner accepts the proposal; it is recorded in the published mapping
    l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["olap"], "preview": false, "fingerprints": p["fingerprints"],
                           "identities": {"olap.device_activity": ["visitor_id"]}}),
    );
    let onto = data(&l.call("GET", "/api/v1/ontology", serde_json::json!({})));
    let mapping = |t: &str| {
        onto["properties"]["mappings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["physical_type"] == t)
            .unwrap()
            .clone()
    };
    assert_eq!(
        mapping("device_activity")["identity"],
        serde_json::json!(["visitor_id"])
    );
    assert_eq!(
        mapping("devices")["identity"],
        serde_json::json!(["visitor_id"]),
        "a replacing key is recorded as the identity"
    );

    let first = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "olap", "containers": ["device_activity", "devices"]}),
    );
    assert_eq!(
        (
            first["imported"]["device_activity"].clone(),
            first["imported"]["devices"].clone()
        ),
        (20.into(), 20.into()),
        "{first}"
    );
    // one more visit for one device, then the same import again
    exec(&[format!(
        "INSERT INTO {db}.visits VALUES ('v3', now(), '198.51.100.7')"
    )]);
    let again = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "olap", "containers": ["device_activity"]}),
    );
    assert_eq!(
        (
            again["imported"]["device_activity"].clone(),
            again["unchanged"]["device_activity"].clone()
        ),
        (1.into(), 19.into()),
        "{again}"
    );
    assert_eq!(again["pruned"]["device_activity"], 0, "{again}");
    let all = query(&l, "MATCH device_activity RETURN *");
    assert_eq!(
        all["results"].as_array().unwrap().len(),
        20,
        "one object per device, not 21: {all}"
    );
    let hist = query(
        &l,
        "MATCH device_activity WHERE visitor_id == \"v3\" HISTORICAL RETURN *",
    );
    let counts: Vec<i64> = hist["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["properties"]["visit_count"].as_i64().unwrap())
        .collect();
    assert_eq!(
        counts.len(),
        2,
        "device v3 has a history of two versions: {hist}"
    );
    assert!(counts[1] > counts[0], "the aggregate grew: {counts:?}");
    l.assert_no_secret_anywhere();
    exec(&[format!("DROP DATABASE IF EXISTS {db}")]);
}

/// Case study (docs/ONTOLOGY-IN-PRACTICE.md, Phase B): which accounts share a device?
/// ClickHouse has no foreign keys and the accounts live in Postgres. The preview proposes
/// the links from the keys, the owner accepts them, import materializes them as
/// relationships across both databases, and TRAVERSE walks them in both directions.
#[test]
fn accounts_sharing_a_device_are_a_traversal_across_clickhouse_and_postgres() {
    let (Some(ch), Some(pg)) = (connectors::Live::clickhouse(), connectors::Live::pg()) else {
        return;
    };
    let pid = std::process::id();
    let db = format!("md_lnk_{pid}");
    let exec = |stmts: &[String]| {
        let refs: Vec<&str> = stmts.iter().map(String::as_str).collect();
        connectors::clickhouse_exec(&ch.clickhouse_url, "default", &refs);
    };
    exec(&[
        format!("DROP DATABASE IF EXISTS {db}"),
        format!("CREATE DATABASE {db}"),
        format!("CREATE TABLE {db}.devices (visitor_id String, updated_at DateTime, risk Float32) ENGINE = ReplacingMergeTree(updated_at) ORDER BY visitor_id"),
        format!("INSERT INTO {db}.devices VALUES ('v1', now(), 0.1), ('v7', now(), 0.9)"),
        format!("CREATE TABLE {db}.visits (visitor_id String, account_id String, ts DateTime) ENGINE = MergeTree ORDER BY (visitor_id, ts)"),
        // v7 is used by five accounts, v1 by one
        format!("INSERT INTO {db}.visits SELECT 'v7', concat('a', toString(number % 5 + 1)), now() - number FROM numbers(50)"),
        format!("INSERT INTO {db}.visits SELECT 'v1', 'a1', now() - number FROM numbers(10)"),
        format!("CREATE VIEW {db}.device_accounts AS SELECT visitor_id, account_id, count() AS visit_count FROM {db}.visits GROUP BY visitor_id, account_id"),
    ]);
    let dsn = connectors::pg_private_db(&pg.dsn, &format!("cert_multidb_lnk_{pid}"));
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, email TEXT NOT NULL, status TEXT NOT NULL)",
            "INSERT INTO accounts SELECT 'a' || g, 'user' || g || '@example.test', 'active' FROM generate_series(1, 6) g",
        ],
    );
    let l = Live::start(
        "links",
        &format!("[[sources]]\nname = \"olap\"\nkind = \"clickhouse\"\nurl_env = \"MD_CH\"\ndatabase = \"{db}\"\n\n[[sources]]\nname = \"crm\"\nkind = \"postgres\"\nurl_env = \"MD_PG\"\n"),
        &[("MD_CH", &ch.clickhouse_url), ("MD_PG", &dsn)],
        &["aikoql-dev-only"],
    );
    for s in ["olap", "crm"] {
        l.post("/api/v1/sources/discover", serde_json::json!({"source": s}));
    }
    let key = serde_json::json!({"olap.device_accounts": ["visitor_id", "account_id"]});
    let p = l.post(
        "/api/v1/sources/ontology",
        serde_json::json!({"sources": ["olap", "crm"], "preview": true, "identities": key}),
    );
    let sug = |from: &str| {
        p["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["from"] == from)
            .cloned()
            .unwrap_or_else(|| panic!("no link from {from}: {}", p["links"]))
    };
    let dev = sug("olap.device_accounts.visitor_id");
    assert_eq!(
        (dev["name"].as_str(), dev["to"].as_str()),
        (Some("for_device"), Some("olap.devices.visitor_id")),
        "{dev}"
    );
    let acct = sug("olap.device_accounts.account_id");
    assert_eq!(
        (acct["name"].as_str(), acct["to"].as_str()),
        (Some("for_account"), Some("crm.accounts.id")),
        "across sources: {acct}"
    );

    // the owner accepts both proposals
    let relationships: Vec<serde_json::Value> = [&dev, &acct]
        .iter()
        .map(|s| serde_json::json!({"name": s["name"], "from": s["from"], "to": s["to"]}))
        .collect();
    let applied = l.post("/api/v1/sources/ontology", serde_json::json!({"sources": ["olap", "crm"], "preview": true, "identities": key, "relationships": relationships}));
    assert!(
        applied["links"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| !s["from"]
                .as_str()
                .unwrap()
                .starts_with("olap.device_accounts.")),
        "accepted links are not offered again: {}",
        applied["links"]
    );
    let published = l.post("/api/v1/sources/ontology", serde_json::json!({"sources": ["olap", "crm"], "preview": false, "fingerprints": applied["fingerprints"], "identities": key, "relationships": relationships}));
    assert_eq!(published["saved"], true, "{published}");

    // targets first, then the link table
    l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "olap", "containers": ["devices"]}),
    );
    l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "crm", "containers": ["accounts"]}),
    );
    let i = l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "olap", "containers": ["device_accounts"]}),
    );
    assert_eq!(
        i["imported"]["device_accounts"], 6,
        "(v7 x 5 accounts) + (v1 x 1): {i}"
    );
    let stats = &i["links"]["device_accounts"];
    assert_eq!(
        (
            stats["for_device"]["linked"].clone(),
            stats["for_account"]["linked"].clone()
        ),
        (6.into(), 6.into()),
        "{i}"
    );
    assert_eq!(stats["for_account"]["dangling"], 0, "{i}");

    // a traversal returns objects by id (hydrating properties is Phase E): name them via their rows
    let name_of = |ty: &str, col: &str| -> std::collections::BTreeMap<String, String> {
        query(&l, &format!("MATCH {ty} RETURN *"))["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["koid"].as_str().unwrap().to_string(),
                    r["properties"][col].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let (account_ids, device_ids) = (name_of("accounts", "id"), name_of("devices", "visitor_id"));
    let reached = |q: &str, names: &std::collections::BTreeMap<String, String>| -> Vec<String> {
        let r = query(&l, q);
        let mut out: Vec<String> = r["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| names[x["koid"].as_str().unwrap()].clone())
            .collect();
        out.sort();
        out
    };
    // which accounts share device v7? one hop from its link rows
    assert_eq!(
        reached(
            "MATCH device_accounts WHERE visitor_id == \"v7\" TRAVERSE for_account RETURN *",
            &account_ids
        ),
        ["a1", "a2", "a3", "a4", "a5"]
    );
    // and the other way: which devices does a1 use?
    assert_eq!(
        reached(
            "MATCH device_accounts WHERE account_id == \"a1\" TRAVERSE for_device RETURN *",
            &device_ids
        ),
        ["v1", "v7"]
    );
    l.assert_no_secret_anywhere();
    exec(&[format!("DROP DATABASE IF EXISTS {db}")]);
}

/// Case study question 4 (docs/ONTOLOGY-IN-PRACTICE.md, Phase C): the risk engine says
/// *block*, the CRM says *active* — which wins, and why? Two databases, one account.
#[test]
fn a_crm_and_a_risk_engine_that_disagree_leave_a_contradiction_with_the_rule_that_decides() {
    let (Some(ch), Some(pg)) = (connectors::Live::clickhouse(), connectors::Live::pg()) else {
        return;
    };
    let pid = std::process::id();
    let db = format!("md_res_{pid}");
    let exec = |stmts: &[String]| {
        let refs: Vec<&str> = stmts.iter().map(String::as_str).collect();
        connectors::clickhouse_exec(&ch.clickhouse_url, "default", &refs);
    };
    exec(&[
        format!("DROP DATABASE IF EXISTS {db}"),
        format!("CREATE DATABASE {db}"),
        format!("CREATE TABLE {db}.risk_decisions (account_id String, decision String, ts DateTime) ENGINE = MergeTree ORDER BY (account_id, ts)"),
        // acct_0077 was allowed, challenged, allowed, then blocked; acct_0078 is allowed throughout
        format!("INSERT INTO {db}.risk_decisions VALUES ('acct_0077','allow','2026-01-01 00:00:00'),('acct_0077','challenge','2026-01-02 00:00:00'),('acct_0077','allow','2026-01-03 00:00:00'),('acct_0077','block','2026-01-04 00:00:00'),('acct_0078','allow','2026-01-01 00:00:00')"),
        // the latest decision per account: one claim per account
        format!("CREATE VIEW {db}.account_risk AS SELECT account_id, argMax(decision, ts) AS decision FROM {db}.risk_decisions GROUP BY account_id"),
    ]);
    let dsn = connectors::pg_private_db(&pg.dsn, &format!("cert_multidb_res_{pid}"));
    connectors::pg_exec(
        &dsn,
        &[
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, email TEXT NOT NULL, status TEXT NOT NULL)",
            "INSERT INTO accounts VALUES ('acct_0077','a@example.test','active'),('acct_0078','b@example.test','allow'),('acct_0079','c@example.test','active')",
        ],
    );
    let l = Live::start(
        "resolve",
        &format!("[[sources]]\nname = \"olap\"\nkind = \"clickhouse\"\nurl_env = \"MD_CH\"\ndatabase = \"{db}\"\n\n[[sources]]\nname = \"crm\"\nkind = \"postgres\"\nurl_env = \"MD_PG\"\n"),
        &[("MD_CH", &ch.clickhouse_url), ("MD_PG", &dsn)],
        &["aikoql-dev-only"],
    );
    for s in ["olap", "crm"] {
        l.post("/api/v1/sources/discover", serde_json::json!({"source": s}));
    }
    // both sources assert the account's `status`; the risk engine outranks the CRM
    let assertions = serde_json::json!([
        {"from": "crm.accounts.id", "class": "Account", "properties": {"status": "status"}, "rank": 20},
        {"from": "olap.account_risk.account_id", "class": "Account", "properties": {"decision": "status"}, "rank": 5},
    ]);
    let identities = serde_json::json!({"olap.account_risk": ["account_id"]});
    let body = |preview: bool, fp: &serde_json::Value| serde_json::json!({"sources": ["olap", "crm"], "preview": preview, "fingerprints": fp, "identities": identities, "assertions": assertions});
    let p = l.post(
        "/api/v1/sources/ontology",
        body(true, &serde_json::json!({})),
    );
    let published = l.post("/api/v1/sources/ontology", body(false, &p["fingerprints"]));
    assert_eq!(published["saved"], true, "{published}");
    assert!(
        !published["warnings"]
            .to_string()
            .contains("assertion ignored"),
        "{published}"
    );

    l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "crm", "containers": ["accounts"]}),
    );
    l.post(
        "/api/v1/sources/import",
        serde_json::json!({"source": "olap", "containers": ["account_risk"]}),
    );
    let r = l.post(
        "/api/v1/sources/resolve",
        serde_json::json!({"class": "Account"}),
    );
    assert_eq!(
        (r["entities"].clone(), r["claims"].clone()),
        (3.into(), 5.into()), // 3 CRM accounts + 2 risk accounts
        "{r}"
    );
    assert_eq!(
        (
            r["contradictions"]["resolved"].clone(),
            r["contradictions"]["open"].clone()
        ),
        (1.into(), 0.into()),
        "{r}"
    );

    // the disagreement is a queryable object that names both sides, the rule and the winner
    let found = query(&l, "MATCH aikoql:contradiction RETURN *");
    let rows = found["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{found}");
    let c = &rows[0]["properties"];
    assert_eq!(
        (
            c["key"].as_str(),
            c["property"].as_str(),
            c["status"].as_str()
        ),
        (Some("acct_0077"), Some("status"), Some("resolved")),
        "{c}"
    );
    assert_eq!(c["winner"], "clickhouse.account_risk", "{c}");
    assert!(c["rule"].as_str().unwrap().contains("rank 5"), "{c}");
    let mut sides: Vec<(String, String)> = c["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["source"].as_str().unwrap().into(),
                v["value"].as_str().unwrap().into(),
            )
        })
        .collect();
    sides.sort();
    assert_eq!(
        sides,
        [
            ("clickhouse.account_risk".into(), "block".into()),
            ("postgres.accounts".into(), "active".into())
        ],
        "{c}"
    );

    // the canonical account carries the winning value (acct_0078 agrees in both: no contradiction)
    let canon = query(
        &l,
        "MATCH aikoql:canonical WHERE key == \"acct_0077\" RETURN *",
    );
    assert_eq!(
        canon["results"][0]["properties"]["values"]["status"], "block",
        "{canon}"
    );
    l.assert_no_secret_anywhere();
    exec(&[format!("DROP DATABASE IF EXISTS {db}")]);
}
