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
