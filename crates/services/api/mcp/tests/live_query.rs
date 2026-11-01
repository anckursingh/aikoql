//! AG-40 live queries over the wire: `koql/subscribe` answers with the
//! current result set, then `notifications/koql/updated` frames carry each
//! change. Re-subscribing with the same id resumes without missing events.
//!
//! The concurrency gate (no missed / no duplicate under concurrent writers)
//! lives in-process in `tools/live_query.rs`; here the protocol surface and
//! the durable-resume surface are pinned.

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("aikoql-ag40-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A pump thread turns the child's stdout into a bounded-wait channel, so
/// notification frames (written by the subscription worker thread) and
/// response frames (written by the dispatch thread) drain in arrival order
/// without the test ever blocking unboundedly on `read_line`.
struct Client {
    child: Child,
    stdin: ChildStdin,
    frames: mpsc::Receiver<J>,
    next: u64,
    notes: Vec<J>,
}

impl Client {
    fn serve(root: &Path) -> Self {
        let mut exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        exe.push("../../../../target/debug/aikoql-mcp");
        #[cfg(windows)]
        exe.set_extension("exe");
        assert!(exe.exists(), "aikoql-mcp not built; run cargo build first");
        let mut child = Command::new(&exe)
            // The kernel opens <db> itself — it must not exist yet (an
            // empty dir is "not an aikoql-v2 database").
            .arg(root.join("db"))
            // No embedding model dir: remember stays text-only.
            .env("HOME", root.join("home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aikoql-mcp serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                if stdout.read_line(&mut line).unwrap() == 0 {
                    break; // server closed stdout
                }
                let frame: J = serde_json::from_str(line.trim())
                    .unwrap_or_else(|_| panic!("bad frame: {line:?}"));
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        let mut c = Client {
            child,
            stdin,
            frames: rx,
            next: 0,
            notes: vec![],
        };
        c.call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
        );
        c
    }

    fn next_frame(&mut self, timeout: Duration) -> J {
        self.frames
            .recv_timeout(timeout)
            .expect("no frame before deadline (server hang or closed stdout)")
    }

    fn call(&mut self, method: &str, params: J) -> J {
        self.next += 1;
        let id = self.next;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            let frame = self.next_frame(Duration::from_secs(30));
            if frame.get("id").is_none() {
                self.notes.push(frame);
                continue;
            }
            if frame["id"].as_u64() == Some(id) {
                return frame;
            }
        }
    }

    fn tool(&mut self, name: &str, args: J) -> J {
        let res = self.call("tools/call", json!({"name": name, "arguments": args}));
        let text = res["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        serde_json::from_str(&text).expect("tool payload is json")
    }

    fn updates(&self) -> usize {
        self.notes
            .iter()
            .filter(|n| n["method"] == "notifications/koql/updated")
            .count()
    }

    /// Pull frames until `n` koql/updated notifications have arrived (the
    /// worker thread writes them asynchronously — a response frame is not a
    /// barrier). No request may be in flight while draining.
    fn drain_updates(&mut self, n: usize, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while self.updates() < n && Instant::now() < deadline {
            let frame = self.next_frame(Duration::from_millis(500));
            assert!(
                frame.get("id").is_none(),
                "unexpected response frame while draining: {frame}"
            );
            self.notes.push(frame);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn ag40_subscribe_emits_changes_and_resumes_durably() {
    let root = tmp("live");
    let mut c = Client::serve(&root);
    let _ = c.tool(
        "remember",
        json!({"subject": "mcp-agent", "type_name": "LiveThing", "properties": {"tag": "seed"}}),
    );

    let sub = c.call(
        "koql/subscribe",
        json!({
            "sub_id": "ag40",
            "subject": "mcp-agent",
            "query": "MATCH LiveThing RETURN *"
        }),
    );
    assert!(
        sub["result"].is_object(),
        "koql/subscribe answers with a result object: {sub}"
    );
    assert_eq!(sub["result"]["sub_id"], "ag40");
    assert_eq!(
        sub["result"]["results"].as_array().map(|a| a.len()),
        Some(1),
        "the initial result set is the current one: {sub}"
    );

    // Sequential writes through the same connection; each changes the set.
    for i in 0..3 {
        let _ = c.tool(
            "remember",
            json!({"subject": "mcp-agent", "type_name": "LiveThing", "properties": {"tag": format!("t{i}")}}),
        );
    }

    // Unsubscribe stops the stream, then drain the frames the worker had
    // already evaluated: exactly one update per change, in commit order.
    let un = c.call("koql/unsubscribe", json!({"sub_id": "ag40"}));
    assert!(un["result"].is_object(), "unsubscribe answers: {un}");
    c.drain_updates(3, Duration::from_secs(10));

    let updates: Vec<&J> = c
        .notes
        .iter()
        .filter(|n| n["method"] == "notifications/koql/updated")
        .collect();
    assert_eq!(updates.len(), 3, "one update per change: {:?}", c.notes);
    let mut last_rows = 0;
    let mut last_seq = 0;
    for u in &updates {
        let seq = u["params"]["seq"]
            .as_u64()
            .expect("each update carries seq");
        assert!(seq > last_seq, "update seqs must strictly increase");
        last_seq = seq;
        let rows = u["params"]["results"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        assert!(rows > last_rows, "each update adds rows (no duplicates)");
        last_rows = rows;
    }
    assert_eq!(last_rows, 4, "the last update holds the full set of 4");

    // Durable resume: re-subscribing with the same id starts from the
    // acked seq and answers with the current set — nothing was missed.
    let sub2 = c.call(
        "koql/subscribe",
        json!({
            "sub_id": "ag40",
            "subject": "mcp-agent",
            "query": "MATCH LiveThing RETURN *"
        }),
    );
    assert_eq!(
        sub2["result"]["results"].as_array().map(|a| a.len()),
        Some(4),
        "a resumed subscription sees the full set: {sub2}"
    );
}
