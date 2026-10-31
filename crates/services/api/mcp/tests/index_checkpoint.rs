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
    Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
        .args(["serve", root.join("db").to_str().unwrap()])
        .args(["--index-checkpoint-secs", "1"])
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
