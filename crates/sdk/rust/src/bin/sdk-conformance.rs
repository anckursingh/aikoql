//! Command sdk-conformance is the Rust adapter for the shared conformance
//! runner (D-12, §7/§23). It executes the language-neutral vectors from
//! tests/sdk-conformance/ (the §23 canonical workload + the §7 category
//! dirs) and protocol/test-vectors/ against a real aikoql-mcp server
//! through this SDK, then checks every assert/assert_any/expect_error. The
//! expected results are the vectors themselves — every SDK produces the
//! same transcript. Mirrors the Go adapter (crates/sdk/go/cmd/
//! sdk-conformance) arm for arm.
//!
//! Run through scripts/sdk-conformance.sh, or directly:
//!
//!   cargo run --bin sdk-conformance -- --bin <aikoql-mcp> \
//!       --vectors <dir> --protocol <dir> --token <token>

use aikoql_sdk::tools::{FindSimilarParams, RememberParams};
use aikoql_sdk::{Client, Error, StagedOp, Tx};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_stream::StreamExt;

#[derive(Deserialize)]
struct VectorFile {
    name: String,
    #[serde(default)]
    operations: Vec<Value>,
}

enum Var {
    Koid(String),
    Tx(Tx),
}

struct Runner {
    addr: String,
    token: String,
    client: Option<Client>,
    vars: HashMap<String, Var>,
    last: String,
    /// D-15: run the whole workload over the native framed protocol
    /// instead of MCP JSON-RPC — the SDK API does not change.
    native: bool,
}

/// Kills the server and sweeps its temp dir on EVERY exit path — the Go
/// adapter's teardown defer, as a Drop guard.
struct ServerGuard {
    child: Option<Child>,
    dir: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn dot_get<'a>(mut obj: &'a Value, path: &str) -> Result<&'a Value, String> {
    for part in path.split('.') {
        obj = match obj {
            Value::Object(m) => m.get(part).ok_or_else(|| format!("no key {part:?}"))?,
            Value::Array(a) => {
                let idx: usize = part.parse().map_err(|_| format!("no index {part:?}"))?;
                a.get(idx).ok_or_else(|| format!("no index {part:?}"))?
            }
            other => return Err(format!("cannot descend into {other:?} at {part:?}")),
        };
    }
    Ok(obj)
}

/// Numeric-aware equality: 3 and 3.0 compare equal, like Python's == and
/// unlike serde_json's Number PartialEq (Go never meets mixed int/float —
/// everything decodes to float64 — but Python does, and the vectors were
/// frozen against both).
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(i), Some(j)) => i == j,
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(i, j)| json_eq(i, j))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).map(|w| json_eq(v, w)).unwrap_or(false))
        }
        _ => a == b,
    }
}

fn map_code(code: &str) -> String {
    // Wire surfaces → SDK-012 codes: the server's -32001 token rejection
    // is AUTHENTICATION_FAILED to the caller.
    if code == "-32001" {
        "AUTHENTICATION_FAILED".into()
    } else {
        code.into()
    }
}

fn props(v: Option<&Value>) -> Option<Map<String, Value>> {
    v.and_then(|v| v.as_object()).cloned()
}

fn or(a: String, b: String) -> String {
    if a.is_empty() {
        b
    } else {
        a
    }
}

fn to_value<T: serde::Serialize>(t: T) -> Result<Value, Error> {
    serde_json::to_value(t).map_err(Error::Json)
}

impl Runner {
    fn client(&self) -> &Client {
        self.client.as_ref().expect("vector must connect first")
    }

    /// ref resolves "$name" against the var map; anything else passes
    /// through.
    fn ref_koid(&self, v: Option<&Value>) -> String {
        let s = v.and_then(|v| v.as_str()).unwrap_or("");
        if let Some(name) = s.strip_prefix('$') {
            if let Some(Var::Koid(k)) = self.vars.get(name) {
                return k.clone();
            }
        }
        s.to_string()
    }

    fn tx_var(&mut self, op: &Map<String, Value>) -> Result<&mut Tx, Error> {
        let name = op
            .get("txn")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .trim_start_matches('$')
            .to_string();
        match self.vars.get_mut(&name) {
            Some(Var::Tx(tx)) => Ok(tx),
            _ => Err(Error::Kernel(format!("var {name:?} is not an open txn"))),
        }
    }

    async fn connect(&mut self, token: &str) -> Result<(), Error> {
        let c = if self.native {
            Client::connect_native(&self.addr).await?
        } else {
            Client::dial(&self.addr).await?
        };
        let c = c.with_token(token.to_string());
        if let Err(e) = c.initialize().await {
            let _ = c.close().await;
            return Err(e);
        }
        self.client = Some(c);
        Ok(())
    }

    async fn run_op(&mut self, op: &Map<String, Value>) -> Result<Value, Error> {
        match op["op"].as_str().unwrap_or("") {
            "connect" => {
                let tok = op
                    .get("token")
                    .and_then(|t| t.as_str())
                    .unwrap_or(&self.token)
                    .to_string();
                self.connect(&tok).await?;
                Ok(serde_json::json!({}))
            }
            "close" => {
                match &self.client {
                    Some(c) => c.close().await?,
                    None => return Err(Error::Kernel("no client to close".into())),
                }
                Ok(serde_json::json!({}))
            }
            "health" => self.client().health().await,
            "metrics" => to_value(self.client().metrics().await?),
            "remember" => {
                let rem = self
                    .client()
                    .remember(RememberParams {
                        type_name: op["type"].as_str().unwrap_or("").to_string(),
                        properties: props(op.get("properties")),
                        ..Default::default()
                    })
                    .await?;
                self.last = rem.koid.clone();
                to_value(rem)
            }
            "update" => {
                let koid = or(self.ref_koid(op.get("koid")), self.last.clone());
                let rem = self
                    .client()
                    .remember(RememberParams {
                        type_name: op["type"].as_str().unwrap_or("").to_string(),
                        koid: Some(koid),
                        properties: props(op.get("properties")),
                        ..Default::default()
                    })
                    .await?;
                self.last = rem.koid.clone();
                to_value(rem)
            }
            "get" => {
                let koid = or(self.ref_koid(op.get("koid")), self.last.clone());
                to_value(self.client().get(&koid, "").await?)
            }
            "delete" => {
                let koid = or(self.ref_koid(op.get("koid")), self.last.clone());
                let m = self.client().forget(&koid, "tombstone", "").await?;
                if let Some(k) = m.get("koid").and_then(|v| v.as_str()) {
                    self.last = k.to_string();
                }
                Ok(m)
            }
            "query" => {
                if op.get("stream").and_then(|s| s.as_bool()).unwrap_or(false) {
                    let stream = self
                        .client()
                        .query_stream(op["query"].as_str().unwrap_or(""), "")
                        .await?;
                    let mut chunks = Vec::new();
                    let mut stream = stream;
                    while let Some(chunk) = stream.next().await {
                        chunks.push(chunk?);
                    }
                    Ok(serde_json::json!({"chunks": chunks}))
                } else {
                    self.client()
                        .aikoql(op["query"].as_str().unwrap_or(""), "")
                        .await
                }
            }
            "relate" => {
                let m = self
                    .client()
                    .relate(
                        &self.ref_koid(op.get("from")),
                        &self.ref_koid(op.get("to")),
                        op["rel_type"].as_str().unwrap_or(""),
                        "",
                    )
                    .await?;
                if let Some(k) = m.get("koid").and_then(|v| v.as_str()) {
                    self.last = k.to_string();
                }
                Ok(m)
            }
            "traverse" => {
                let depth = op.get("depth").and_then(|d| d.as_i64()).unwrap_or(1);
                self.client()
                    .traverse(
                        &self.ref_koid(op.get("koid")),
                        op.get("rel_type").and_then(|r| r.as_str()).unwrap_or(""),
                        "",
                        depth,
                    )
                    .await
            }
            "find_similar" => {
                let p = FindSimilarParams {
                    text: op.get("text").and_then(|t| t.as_str()).map(String::from),
                    wait_for_freshness_ms: op.get("wait_for_freshness_ms").and_then(|w| w.as_i64()),
                    ..Default::default()
                };
                let hits = self.client().find_similar(p).await?;
                Ok(serde_json::json!({"results": hits}))
            }
            "begin" => {
                let tx = self.client().begin(None).await?;
                let id = tx.id().to_string();
                self.vars
                    .insert(op["as"].as_str().unwrap_or("").to_string(), Var::Tx(tx));
                Ok(serde_json::json!({"txn_id": id}))
            }
            "execute" => {
                let staged = StagedOp {
                    action: op["action"].as_str().unwrap_or("").to_string(),
                    type_name: op
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    properties: props(op.get("properties")),
                    ..Default::default()
                };
                self.tx_var(op)?.execute(staged).await?;
                Ok(serde_json::json!({}))
            }
            "commit" => {
                let res = self.tx_var(op)?.commit().await?;
                to_value(res)
            }
            "rollback" => {
                self.tx_var(op)?.rollback().await?;
                Ok(serde_json::json!({"rolled_back": true}))
            }
            "explain" => {
                let koid = self.ref_koid(op.get("koid"));
                self.client().explain(&koid, "", None).await
            }
            "trace" => {
                let koid = self.ref_koid(op.get("koid"));
                self.client().trace(&koid, "").await
            }
            "discover_schema" => self.client().discover_schema().await,
            other => Err(Error::Kernel(format!("op {other:?} has no adapter arm"))),
        }
    }

    fn capture(&mut self, op: &Map<String, Value>, result: &Value) {
        let Some(as_name) = op.get("as").and_then(|a| a.as_str()) else {
            return;
        };
        if let Some(k) = result.get("koid").and_then(|k| k.as_str()) {
            self.vars
                .insert(as_name.to_string(), Var::Koid(k.to_string()));
            return;
        }
        if let Some(first) = result
            .get("results")
            .and_then(|r| r.as_array())
            .and_then(|a| a.first())
        {
            if let Some(k) = first.get("koid").and_then(|k| k.as_str()) {
                self.vars
                    .insert(as_name.to_string(), Var::Koid(k.to_string()));
            }
        }
    }

    /// "$name" in an expected value resolves against the var map.
    fn deref(&self, want: &Value) -> Value {
        if let Some(s) = want.as_str() {
            if let Some(name) = s.strip_prefix('$') {
                if let Some(Var::Koid(k)) = self.vars.get(name) {
                    return Value::String(k.clone());
                }
            }
        }
        want.clone()
    }

    fn check(&self, op: &Map<String, Value>, result: &Value) -> Result<(), String> {
        if let Some(asserts) = op.get("assert").and_then(|a| a.as_object()) {
            for (path, want) in asserts {
                let got = dot_get(result, path)?;
                let want = self.deref(want);
                if !json_eq(got, &want) {
                    return Err(format!("assert {path}: expected {want}, got {got}"));
                }
            }
        }
        if let Some(aa) = op.get("assert_any").and_then(|a| a.as_object()) {
            let path = aa.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let items = dot_get(result, path)?
                .as_array()
                .ok_or_else(|| format!("assert_any {path}: not a list"))?;
            let m = aa
                .get("match")
                .ok_or_else(|| "assert_any: no match".to_string())?;
            let mut found = false;
            if let Some(want_map) = m.as_object() {
                for e in items {
                    if e.as_object().is_none() {
                        continue;
                    }
                    let mut all = true;
                    for (p, v) in want_map {
                        match dot_get(e, p) {
                            Ok(got) if json_eq(got, v) => {}
                            _ => {
                                all = false;
                                break;
                            }
                        }
                    }
                    if all {
                        found = true;
                        break;
                    }
                }
            } else {
                found = items.iter().any(|e| json_eq(e, m));
            }
            if !found {
                return Err(format!("assert_any {path}: no element matches {m}"));
            }
        }
        Ok(())
    }

    async fn run_vector(&mut self, ops: &[Value]) -> Result<(), String> {
        let tok = self.token.clone();
        self.connect(&tok).await.map_err(|e| e.to_string())?;
        self.vars.clear();
        self.last.clear();
        for (i, opv) in ops.iter().enumerate() {
            let Some(op) = opv.as_object() else {
                return Err(format!("op {i}: not an object"));
            };
            let expect = op
                .get("expect_error")
                .and_then(|e| e.as_str())
                .unwrap_or("");
            match self.run_op(op).await {
                Err(e) => {
                    let code = match &e {
                        Error::Mcp(m) => map_code(&m.code),
                        other => format!("{other:?}"),
                    };
                    if expect == code {
                        continue;
                    }
                    return Err(format!(
                        "op {i} {:?}: expected error {expect}, got {code}: {e}",
                        op["op"]
                    ));
                }
                Ok(result) => {
                    if !expect.is_empty() {
                        return Err(format!(
                            "op {i} {:?}: expected error {expect}, none raised",
                            op["op"]
                        ));
                    }
                    self.capture(op, &result);
                    self.check(op, &result)
                        .map_err(|e| format!("op {i} {:?}: {e}", op["op"]))?;
                }
            }
        }
        Ok(())
    }
}

fn load_vectors(dir: &str) -> Result<Vec<VectorFile>, String> {
    let mut paths = Vec::new();
    let mut stack = vec![PathBuf::from(dir)];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).map_err(|e| format!("{dir}: {e}"))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("json") {
                paths.push(p);
            }
        }
    }
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        let raw = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let vf: VectorFile =
            serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", p.display()))?;
        out.push(vf);
    }
    Ok(out)
}

/// The integration_test pattern: a probed free port and a db path that
/// does not exist (the server auto-creates it as aikoql-v2).
fn spawn_server(
    bin: &str,
    token: &str,
    native: bool,
) -> Result<(Child, String, PathBuf), String> {
    let probe = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = probe.local_addr().map_err(|e| e.to_string())?.port();
    drop(probe);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("conformance-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let db = dir.join("db.aikoql"); // does not exist → auto-create (aikoql-v2)
    let addr = format!("127.0.0.1:{port}");
    // D-15: --native serves the framed binary protocol on the same port
    // contract as --listen (probed free port + the same token table).
    let listen_flag = if native { "--native-port" } else { "--listen" };
    let mut child = Command::new(bin)
        .arg("serve")
        .arg(&db)
        .arg(listen_flag)
        .arg(&addr)
        .arg("--tcp-token")
        .arg(format!("{token}::admin"))
        // Stdout/Stderr stay null: an inherited pipe would let an orphaned
        // server hold a pipe-capturing parent open on failure.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if TcpStream::connect(&addr).is_ok() {
            return Ok((child, addr, dir));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(format!("server did not come up on {addr}"))
}

async fn run(
    bin: &str,
    vectors: &str,
    protocol: &str,
    token: &str,
    native: bool,
) -> Result<(), String> {
    let (child, addr, dir) = spawn_server(bin, token, native)?;
    let _guard = ServerGuard {
        child: Some(child),
        dir,
    };

    let mut r = Runner {
        addr,
        token: token.to_string(),
        client: None,
        vars: HashMap::new(),
        last: String::new(),
        native,
    };
    let mut vectors_run = 0usize;
    let mut ops_run = 0usize;
    for dir in [protocol, vectors] {
        for vf in load_vectors(dir)? {
            r.run_vector(&vf.operations)
                .await
                .map_err(|e| format!("{}: {e}", vf.name))?;
            vectors_run += 1;
            ops_run += vf.operations.len();
            println!("  ok {} ({} ops)", vf.name, vf.operations.len());
        }
    }
    let tag = if native { " native" } else { "" };
    println!("sdk-conformance (rust{tag}): {vectors_run} vectors, {ops_run} ops — all passed");
    Ok(())
}

fn main() {
    let mut bin = String::new();
    let mut vectors = String::new();
    let mut protocol = String::new();
    let mut token = "conformance".to_string();
    let mut native = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bin" => bin = args.next().unwrap_or_default(),
            "--vectors" => vectors = args.next().unwrap_or_default(),
            "--protocol" => protocol = args.next().unwrap_or_default(),
            "--token" => token = args.next().unwrap_or_default(),
            "--native" => native = true,
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(1);
            }
        }
    }
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(run(&bin, &vectors, &protocol, &token, native)) {
        eprintln!("sdk-conformance (rust): {e}");
        std::process::exit(1);
    }
}
