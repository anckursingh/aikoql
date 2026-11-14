//! AG-20d — a crashed server resumes its indexes from a checkpoint.
//! RED: the periodic index checkpoint ran only inside the replication
//! leader's block, so a standalone server checkpointed at clean shutdown
//! only — after a crash (kill -9, OOM, a `docker stop` timeout) the next
//! start rebuilt the whole HNSW from the journal (~11 min at 1M objects).

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ckpt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn serve(root: &Path, log: &str) -> Child {
    serve_every(root, log, "1")
}

/// `serve` with the periodic index checkpoint every `secs` seconds.
fn serve_every(root: &Path, log: &str, secs: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
        .args(["serve", root.join("db").to_str().unwrap()])
        .args(["--index-checkpoint-secs", secs])
        .current_dir(root)
        .env("HOME", root.join("home"))
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(root.join(log)).unwrap())
        .spawn()
        .expect("spawn aikoql-mcp serve")
}

#[test]
fn ck001_a_killed_server_resumes_its_indexes_from_a_checkpoint() {
    let root = tmp("ck001");
    let mut child = serve(&root, "first.log");
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut call = |id: u64, method: &str, params: J| {
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                out.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            if serde_json::from_str::<J>(line.trim()).is_ok_and(|f| f["id"] == id) {
                return;
            }
        }
    };
    call(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
    );
    for i in 0..20 {
        call(
            2 + i,
            "tools/call",
            json!({"name": "remember", "arguments": {"type_name": "note", "properties": {"body": format!("note {i}")}}}),
        );
    }
    // past two checkpoint intervals, then a crash: no shutdown checkpoint
    std::thread::sleep(Duration::from_millis(2600));
    child.kill().unwrap();
    child.wait().unwrap();

    let mut again = serve(&root, "second.log");
    let t = Instant::now();
    let log = loop {
        let log = std::fs::read_to_string(root.join("second.log")).unwrap_or_default();
        if log.contains("resumed from checkpoint") || log.contains("full replay") {
            break log;
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "no index start line: {log}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = again.kill();
    let _ = again.wait();
    assert!(
        log.contains("resumed from checkpoint"),
        "a killed server must resume from its periodic checkpoint, got: {log}"
    );
}

/// One newline-delimited JSON-RPC exchange over the child's stdio.
fn rpc(stdin: &mut impl Write, out: &mut impl BufRead, id: u64, method: &str, params: J) -> J {
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    )
    .unwrap();
    stdin.flush().unwrap();
    loop {
        let mut line = String::new();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "server closed stdout"
        );
        if let Ok(frame) = serde_json::from_str::<J>(line.trim()) {
            if frame["id"] == id {
                return frame;
            }
        }
    }
}

/// IDX-CKPT — a resumed server's indexes do not live in the checkpoint
/// they were resumed from. The resume opened the text index IN the
/// checkpoint directory, and the next periodic checkpoint replaces that
/// directory: on the Vedas test server every text search after a restart
/// failed ("tantivy reader: ... meta.json") and every later checkpoint
/// failed to remove the old one. The test does the replacement's removal
/// step itself (the resumed server checkpoints only hourly here, so no
/// timing race decides the outcome): the directory must be removable, and
/// the server must keep indexing and answering text search without it.
#[test]
fn ck002_a_resumed_server_keeps_searching_without_its_checkpoint() {
    let root = tmp("ck002");
    let mut child = serve(&root, "first.log");
    let (mut stdin, mut out) = (
        child.stdin.take().unwrap(),
        BufReader::new(child.stdout.take().unwrap()),
    );
    let init = json!({"protocolVersion": "2025-06-18", "capabilities": {}});
    rpc(&mut stdin, &mut out, 1, "initialize", init.clone());
    for i in 0..20 {
        rpc(
            &mut stdin,
            &mut out,
            2 + i,
            "tools/call",
            json!({"name": "remember", "arguments": {"type_name": "note", "properties": {"body": format!("vedic hymn number {i}")}}}),
        );
    }
    std::thread::sleep(Duration::from_millis(2600)); // past two checkpoints
    child.kill().unwrap();
    child.wait().unwrap();

    let mut again = serve_every(&root, "second.log", "3600");
    let (mut stdin, mut out) = (
        again.stdin.take().unwrap(),
        BufReader::new(again.stdout.take().unwrap()),
    );
    rpc(&mut stdin, &mut out, 1, "initialize", init);
    let t = Instant::now();
    while !std::fs::read_to_string(root.join("second.log"))
        .unwrap_or_default()
        .contains("resumed from checkpoint")
    {
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "the server never resumed"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let removed = std::fs::remove_dir_all(root.join("db.ckpt"));
    let wrote = rpc(
        &mut stdin,
        &mut out,
        2,
        "tools/call",
        json!({"name": "remember", "arguments": {"type_name": "note", "properties": {"body": "zebra hymn after the removal"}}}),
    );
    let found = rpc(
        &mut stdin,
        &mut out,
        3,
        "tools/call",
        json!({"name": "find_similar", "arguments": {"text": "zebra hymn", "k": 5, "wait_for_freshness_ms": 5000}}),
    );
    let _ = again.kill();
    let _ = again.wait();
    removed.expect("the checkpoint a resumed server loaded from must be removable");
    assert!(
        wrote["result"]["isError"] != json!(true),
        "the resumed server keeps indexing: {wrote}"
    );
    let text = found.to_string();
    assert!(
        found["result"]["isError"] != json!(true) && !text.contains("tantivy"),
        "text search after the removal must answer: {found}"
    );
    // remember answers {"koid": ...}, as structured content or as text
    let koid: String = wrote["result"]["structuredContent"]["koid"]
        .as_str()
        .map(String::from)
        .or_else(|| {
            let t = wrote["result"]["content"][0]["text"].as_str()?;
            serde_json::from_str::<J>(t).ok()?["koid"]
                .as_str()
                .map(String::from)
        })
        .expect("remember returns a koid");
    assert!(
        text.contains(&koid),
        "the note written after the removal is found: {found}"
    );
}
