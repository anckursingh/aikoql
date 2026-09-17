//! P5-M12 (ND-12) — CLI + SDK contracts: cl01 error-code table, cl03 verbs.
//! cl02 (SDK version fail-fast) lives in the Python suite
//! (crates/sdk/python/tests/test_version_contract.py).
//!
//! cl01: docs/error-codes.md is the versioned code table (a); every code
//! literal the source produces appears in it (b); the codes with no existing
//! producer test get one here (c–f).
//! cl03: the five new verbs (status/query/explain/index/schema) round-trip
//! through the repo-built binary (dogfood rule) — in-process subcommands
//! that open the db directly like shell/backup (no token, no server).
//!
//! The binary must be built first: `cargo build --bin aikoql-mcp`
//! (cargo test does NOT build bins).

use serde_json::{Value as J, json};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

// --- helpers ------------------------------------------------------------------

fn docs(name: &str) -> String {
    format!("{}/../../../../docs/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn server_bin() -> PathBuf {
    let mut exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    exe.push("../../../../target/debug/aikoql-mcp");
    #[cfg(windows)]
    exe.set_extension("exe");
    assert!(
        exe.exists(),
        "aikoql-mcp binary not built at {:?}; run `cargo build --bin aikoql-mcp` first",
        exe
    );
    exe
}

fn tmp_db(name: &str) -> PathBuf {
    let mut db = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    db.push(format!(
        "aikoql_m12_{name}_{}_{}",
        std::process::id(),
        stamp
    ));
    let _ = std::fs::remove_dir_all(&db); // v2 database = directory
    db
}

fn free_port() -> u16 {
    // A mostly-free port: bind :0, read the port, drop the listener.
    let l = TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Run the CLI binary with the given arguments; returns exit status + output.
fn run_bin(args: &[&str]) -> (std::process::ExitStatus, String, String) {
    let out = Command::new(server_bin())
        .args(args)
        .output()
        .expect("run aikoql-mcp");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_verb(db: &Path, args: &[&str]) -> (std::process::ExitStatus, String, String) {
    let mut full: Vec<&str> = args.to_vec();
    full.push(db.to_str().unwrap());
    run_bin(&full)
}

/// The CREATE result shape is {"koid": "<32 hex>", ...} — pull the koid.
fn extract_koid(out: &str) -> String {
    let needle = "\"koid\": \"";
    let start = out.find(needle).expect("koid in create output") + needle.len();
    out[start..start + 32].to_string()
}

/// A minimal stdio MCP client — one request at a time, raw frames returned
/// (the existing mcp_stdio client panics on error frames; these pins need
/// the error itself).
struct StdioClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl StdioClient {
    fn start(db: &Path) -> Self {
        let cfg = db.with_file_name("aikoql-rate.toml");
        std::fs::write(&cfg, "[rate_limit]\nmax_calls_per_minute = 100000\n").unwrap();
        let mut child = Command::new(server_bin())
            .arg("--config")
            .arg(&cfg)
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aikoql-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        StdioClient {
            child,
            stdin,
            stdout,
            next_id: 0,
        }
    }

    /// Send a request and return the raw response frame (result OR error).
    fn request_raw(&mut self, method: &str, params: J) -> J {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{}", frame).unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let frame: J = serde_json::from_str(line.trim()).expect("valid json-rpc frame");
            if frame.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return frame;
            }
            // Notifications and other frames are dropped — single-client tests.
        }
    }

    /// Call a tool; the parsed content payload keeps ok:false / error.code.
    /// (Transport-level errors stay on the frame — the caller checks those.)
    fn call_raw(&mut self, name: &str, args: J) -> J {
        let res = self.request_raw("tools/call", json!({"name": name, "arguments": args}));
        assert!(res.get("error").is_none(), "transport-level error: {res}");
        let result = &res["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tool payload missing content text: {res}"))
            .to_string();
        serde_json::from_str(&text).expect("tool payload is json")
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// --- cl01a/b: the error-code table --------------------------------------------

/// Every documented surface appears in the versioned table.
#[test]
fn cl01a_error_code_doc_needles() {
    let doc = std::fs::read_to_string(docs("error-codes.md")).expect("docs/error-codes.md");
    for needle in [
        "# Error Code Contract",
        "Contract version: 1",
        "## Wire codes",
        "## Envelope codes",
        "## Kernel error tags",
        "## SDK-side codes",
        // JSON-RPC wire codes.
        "-32700",
        "-32601",
        "-32602",
        "-32603",
        "-32000",
        "-32001",
        "-32002",
        "-32004",
        // MRFC-0040 envelope codes.
        "ACCESS_DENIED",
        "VERSION_CONFLICT",
        "NOT_FOUND",
        "VALIDATION_ERROR",
        "RATE_LIMITED",
        "TIMEOUT",
        "INTERNAL",
        "NOT_A_PROGRAM",
        "COMPILE_ERROR",
        // Kernel KError display tags.
        "INVALID_OBJECT",
        "INVALID_SCHEMA",
        "INVALID_QUERY",
        "INVALID_STATE",
        "INVALID_EPISTEMIC",
        "UNSUPPORTED_OPERATION",
        "CANCELLED",
        "INDEX_LAG_EXCEEDED",
        "JOB_REJECTED",
        "STORE",
        "CODEC",
        // SDK-side codes.
        "VERSION_MISMATCH",
    ] {
        assert!(
            doc.contains(needle),
            "error-codes.md must document {needle:?}"
        );
    }
}

/// Direction source → doc: every code literal the source produces is in the
/// table (a new code that isn't documented turns this test RED). The counts
/// are pinned so extraction drift can't silently vacate the pin.
#[test]
fn cl01b_error_code_source_completeness() {
    let doc = std::fs::read_to_string(docs("error-codes.md")).expect("docs/error-codes.md");
    let mcp = format!("{}/src", env!("CARGO_MANIFEST_DIR"));

    // Wire codes: every -3xxxx literal in the protocol source.
    let mut wire: Vec<String> = Vec::new();
    for f in [
        "dispatcher.rs",
        "transport.rs",
        "tool_registry.rs",
        "tools/txn.rs",
    ] {
        let src = std::fs::read_to_string(format!("{mcp}/{f}")).unwrap();
        let bytes = src.as_bytes();
        for i in 0..bytes.len().saturating_sub(6) {
            if bytes[i] == b'-'
                && bytes[i + 1] == b'3'
                && bytes[i + 2..i + 6].iter().all(|b| b.is_ascii_digit())
            {
                let code = src[i..i + 6].to_string();
                if !wire.contains(&code) {
                    wire.push(code);
                }
            }
        }
    }
    wire.sort();
    assert_eq!(wire.len(), 8, "wire-code extraction drifted: {wire:?}");
    for code in &wire {
        assert!(
            doc.contains(code),
            "error-codes.md must document wire code {code:?}"
        );
    }

    // Envelope codes: the as_str() literals in the classifier (scoped to the
    // as_str() impl — suggestion() also matches "=> \"" but isn't a code table).
    let src = std::fs::read_to_string(format!("{mcp}/error_codes.rs")).unwrap();
    let start = src.find("pub fn as_str").expect("as_str impl");
    let end = src[start..]
        .find("pub fn retryable")
        .expect("retryable impl");
    let region = &src[start..start + end];
    let mut envelope: Vec<String> = Vec::new();
    for (i, _) in region.match_indices("=> \"") {
        let rest = &region[i + 4..];
        let end = rest.find('"').unwrap();
        envelope.push(rest[..end].to_string());
    }
    assert_eq!(
        envelope.len(),
        9,
        "envelope extraction drifted: {envelope:?}"
    );
    for code in &envelope {
        assert!(
            doc.contains(code),
            "error-codes.md must document envelope code {code:?}"
        );
    }

    // Kernel tags: the ALL-CAPS prefixes inside the KError Display impl.
    // (CARGO_MANIFEST_DIR is crates/services/api/mcp — three ups to crates/.)
    let kom = std::fs::read_to_string(format!(
        "{}/../../../kernel/src/knowledge/kom.rs",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let start = kom
        .find("impl fmt::Display for KError")
        .expect("KError display impl");
    let end = kom[start..]
        .find("impl std::error::Error for KError")
        .expect("KError error impl");
    let region = &kom[start..start + end];
    let mut tags: Vec<String> = Vec::new();
    for (i, _) in region.match_indices('"') {
        let rest = &region[i + 1..];
        // The region's final quote has no partner (it closes the last string).
        let Some(end) = rest.find('"') else { break };
        let tag = rest[..end].split(':').next().unwrap_or("").to_string();
        if !tag.is_empty()
            && tag.len() >= 3
            && tag.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            && !tags.contains(&tag)
        {
            tags.push(tag);
        }
    }
    assert_eq!(tags.len(), 14, "kernel-tag extraction drifted: {tags:?}");
    for tag in &tags {
        assert!(
            doc.contains(tag),
            "error-codes.md must document kernel tag {tag:?}"
        );
    }
}

// --- cl01c–f: producers for the codes with no existing pin -------------------

/// -32601: an unknown method is answered on the wire, not ignored.
#[test]
fn cl01c_wire_32601_unknown_method() {
    let db = tmp_db("cl01c");
    let mut c = StdioClient::start(&db);
    let r = c.request_raw("no/such-method", json!({}));
    assert_eq!(
        r["error"]["code"],
        json!(-32601),
        "unknown method must answer -32601: {r}"
    );
}

/// -32603: an execution error inside a protocol request is answered on the
/// wire — acking a subscription that was never opened. The message carries
/// the kernel tag (NOT_FOUND) as text: kernel tags ride the wire as message
/// substrings, never as codes.
#[test]
fn cl01d_wire_32603_execution_error() {
    let db = tmp_db("cl01d");
    let mut c = StdioClient::start(&db);
    let r = c.request_raw(
        "notifications/ack",
        json!({"id": "never-subscribed", "seq": 0}),
    );
    assert_eq!(
        r["error"]["code"],
        json!(-32603),
        "an unknown-subscription ack must answer -32603: {r}"
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("NOT_FOUND"),
        "the wire message must carry the kernel tag: {r}"
    );
}

/// -32004 is defense in depth (tool_registry PRR-2): serve REFUSES a
/// role-less token at startup, so no session can reach the wire code — the
/// real pin is the startup refusal, exit 2.
#[test]
fn cl01e_wire_32004_roless_session() {
    let db = tmp_db("cl01e");
    let port = free_port();
    let out = Command::new(server_bin())
        .arg("serve")
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--tcp-token")
        .arg("roletok")
        .arg(&db)
        .output()
        .expect("run aikoql-mcp serve");
    assert_eq!(
        out.status.code(),
        Some(2),
        "serve with a role-less token must refuse at startup (exit 2)"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("assigns no roles"),
        "the refusal must name the role-less token: {err}"
    );
}

/// Envelope codes: tool errors classify into the documented codes.
#[test]
fn cl01f_envelope_producers() {
    let db = tmp_db("cl01f");
    let mut c = StdioClient::start(&db);

    // VALIDATION_ERROR: missing required argument.
    let r = c.call_raw("get", json!({"subject": "cl01f"}));
    assert_eq!(
        r["error"]["code"],
        json!("VALIDATION_ERROR"),
        "missing koid must classify VALIDATION_ERROR: {r}"
    );

    // NOT_FOUND: a well-formed koid that doesn't exist.
    let bogus = "0".repeat(32);
    let r = c.call_raw("get", json!({"koid": bogus, "subject": "cl01f"}));
    assert_eq!(
        r["error"]["code"],
        json!("NOT_FOUND"),
        "missing koid must classify NOT_FOUND: {r}"
    );

    // COMPILE_ERROR: a query that is not KOQL.
    let r = c.call_raw(
        "aikoql",
        json!({"query": "not koql at all", "subject": "cl01f"}),
    );
    assert_eq!(
        r["error"]["code"],
        json!("COMPILE_ERROR"),
        "a non-KOQL query must classify COMPILE_ERROR: {r}"
    );

    // INTERNAL: a txn handle that was never opened.
    let begin = c.call_raw("txn_begin", json!({"txn_id": "cl01f-t1"}));
    assert!(
        begin["txn_id"].is_string(),
        "txn_begin must succeed: {begin}"
    );
    let r = c.call_raw("txn_commit", json!({"txn_id": "never-opened"}));
    assert_eq!(
        r["error"]["code"],
        json!("INTERNAL"),
        "an unknown txn handle must classify INTERNAL: {r}"
    );
}

// --- cl03: the five verbs through the repo-built binary -----------------------

/// status: health + metrics + abi version, exit 0.
#[test]
fn cl03a_status() {
    let db = tmp_db("cl03a");
    let (st, out, err) = run_verb(&db, &["status"]);
    assert!(st.success(), "status must exit 0: {st}\n{err}");
    assert!(
        out.contains("\"healthy\""),
        "status must report health: {out}"
    );
    assert!(
        out.contains("\"abi_version\""),
        "status must report abi_version: {out}"
    );
}

/// query: CREATE then MATCH round-trip through the same binary.
#[test]
fn cl03b_query() {
    let db = tmp_db("cl03b");
    let (st, out, err) = run_verb(&db, &["query", "CREATE Node {i: 1}"]);
    assert!(st.success(), "query CREATE must exit 0: {st}\n{err}");
    assert!(
        out.contains("\"koid\""),
        "CREATE must report the koid: {out}"
    );
    assert!(
        out.contains("\"version\""),
        "CREATE must report the version: {out}"
    );

    let (st, out, err) = run_verb(&db, &["query", "MATCH Node RETURN *"]);
    assert!(st.success(), "query MATCH must exit 0: {st}\n{err}");
    assert!(
        out.contains("\"results\""),
        "MATCH must report results: {out}"
    );
    assert!(
        out.contains("\"Node\""),
        "MATCH must return the created node: {out}"
    );
}

/// explain: the koid a CREATE reported is explainable.
#[test]
fn cl03c_explain() {
    let db = tmp_db("cl03c");
    let (st, out, err) = run_verb(&db, &["query", "CREATE Node {i: 2}"]);
    assert!(st.success(), "seed CREATE must exit 0: {st}\n{err}");
    let koid = extract_koid(&out);

    let (st, out, err) = run_verb(&db, &["explain", &koid]);
    assert!(st.success(), "explain must exit 0: {st}\n{err}");
    assert!(
        out.contains(&koid),
        "explain must name the seeded koid: {out}"
    );
}

/// schema: lists the seeded type.
#[test]
fn cl03d_schema() {
    let db = tmp_db("cl03d");
    let (st, _, err) = run_verb(&db, &["query", "CREATE Node {i: 3}"]);
    assert!(st.success(), "seed CREATE must exit 0: {st}\n{err}");

    let (st, out, err) = run_verb(&db, &["schema"]);
    assert!(st.success(), "schema must exit 0: {st}\n{err}");
    assert!(
        out.contains("\"Node\""),
        "schema must list the seeded type: {out}"
    );
}

/// index: storage/index statistics from the v2 backend.
#[test]
fn cl03e_index() {
    let db = tmp_db("cl03e");
    let (st, _, err) = run_verb(&db, &["query", "CREATE Node {i: 4}"]);
    assert!(st.success(), "seed CREATE must exit 0: {st}\n{err}");

    let (st, out, err) = run_verb(&db, &["index"]);
    assert!(st.success(), "index must exit 0: {st}\n{err}");
    assert!(
        out.contains("\"wal_bytes\""),
        "index must report storage statistics: {out}"
    );
}

/// Failure pin: explain of a missing koid exits 1 and surfaces the kernel
/// tag on stderr — CLI errors carry the documented vocabulary.
#[test]
fn cl03f_explain_missing_koid_surfaces_kernel_tag() {
    let db = tmp_db("cl03f");
    let zeros = "0".repeat(32);
    let (st, _out, err) = run_verb(&db, &["explain", &zeros]);
    assert_eq!(
        st.code(),
        Some(1),
        "explain of a missing koid must exit 1: {st}"
    );
    assert!(
        err.contains("NOT_FOUND"),
        "stderr must carry the kernel tag NOT_FOUND: {err}"
    );
}

/// Usage errors exit 2 (the existing CLI convention for bad invocations).
#[test]
fn cl03g_usage_errors_exit_2() {
    let (st, _out, _err) = run_bin(&["query"]);
    assert_eq!(
        st.code(),
        Some(2),
        "query without a statement is a usage error"
    );
    let (st, _out, _err) = run_bin(&["explain"]);
    assert_eq!(
        st.code(),
        Some(2),
        "explain without a koid is a usage error"
    );
}

// --- cl04 — the shell's fresh default is an honest v2 directory (PR6 P1-21) ------

/// Today the shell's default is "./aikoql.redb" — a name that lies. A fresh
/// open creates an aikoql-v2 DATABASE there (the missing-path default flip
/// turns it into a DIRECTORY named like a redb file), and every default-path
/// verb repeats the same misleading name. Pin: a fresh shell in an empty CWD
/// creates "./aikoql-v2" — a v2 directory under an honest name — and a
/// second run reopens it.
#[test]
fn cl04_fresh_shell_default_is_an_honest_v2_directory() {
    let cwd = tmp_db("cl04");
    std::fs::create_dir_all(&cwd).unwrap();
    let run_shell = |line: &str| -> (std::process::ExitStatus, String, String) {
        let mut child = Command::new(server_bin())
            .arg("shell")
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn aikoql-mcp shell");
        let mut si = child.stdin.take().unwrap();
        writeln!(si, "{line}").unwrap();
        drop(si); // EOF — the shell exits
        let out = child.wait_with_output().expect("shell exit");
        (
            out.status,
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (st1, out1, err1) = run_shell("CREATE (n:Node {name: \"first\"})");
    assert!(st1.success(), "fresh shell must exit 0: {st1} {err1}");
    assert!(
        out1.contains("Connected to: ./aikoql-v2"),
        "the shell announces the honest v2 default: {out1}"
    );
    assert!(
        cwd.join("aikoql-v2/CURRENT").is_file(),
        "the default creates an aikoql-v2 database DIRECTORY"
    );
    assert!(
        !cwd.join("aikoql.redb").exists(),
        "the misleading ./aikoql.redb default is gone"
    );

    // A second run reopens the same v2 database (a write lands, no error).
    let (st2, out2, err2) = run_shell("CREATE (n:Node {name: \"second\"})");
    assert!(st2.success(), "reopen run must exit 0: {st2} {err2}");
    assert!(out2.contains("Connected to: ./aikoql-v2"));

    let _ = std::fs::remove_dir_all(&cwd);
}
