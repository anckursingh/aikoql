//! AG-01 — tool profiles. 103 tools / ~45 KB of schema land in every
//! agent's context under the full surface; `--tool-profile core` lists a
//! small set plus `tools_more`, which hands out a group's schemas and
//! enables it for the session (`notifications/tools/list_changed`).
//! A profile governs what is LISTED, never what may be CALLED (roles do).

use serde_json::{json, Value as J};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// The core profile's budget: what every agent pays before its first call.
const CORE_BYTES: usize = 8 * 1024;

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("tools-profile-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    db: PathBuf,
    next: u64,
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.db);
    }
}

impl Mcp {
    fn serve(tag: &str, extra: &[&str]) -> Self {
        let db = tmp(tag);
        let mut child = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
            .arg("serve")
            .args(extra)
            .arg(&db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aikoql-mcp serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            stdin,
            stdout,
            db,
            next: 0,
        };
        let init = m
            .call(
                "initialize",
                json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
            )
            .0;
        assert!(init["result"].is_object(), "initialize: {init}");
        m
    }

    /// One request: (its response, the notifications sent before it).
    fn call(&mut self, method: &str, params: J) -> (J, Vec<J>) {
        self.next += 1;
        let id = self.next;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        let mut notes = Vec::new();
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            let Ok(frame) = serde_json::from_str::<J>(line.trim()) else {
                continue;
            };
            if frame["id"].as_u64() == Some(id) {
                return (frame, notes);
            }
            notes.push(frame);
        }
    }

    fn tools(&mut self) -> (Vec<String>, usize) {
        let r = self.call("tools/list", json!({})).0;
        let tools = r["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools/list: {r}"));
        let names = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        (
            names,
            serde_json::to_string(&r["result"]["tools"]).unwrap().len(),
        )
    }

    fn tool(&mut self, name: &str, args: J) -> (J, Vec<J>) {
        let (r, notes) = self.call("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: {r}"));
        (
            serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
            notes,
        )
    }
}

#[test]
fn ag01a_the_core_profile_is_small() {
    let mut m = Mcp::serve("core-size", &["--tool-profile", "core"]);
    let (names, bytes) = m.tools();
    assert!(
        bytes <= CORE_BYTES,
        "core tools/list is {bytes} bytes > {CORE_BYTES} ({} tools)",
        names.len()
    );
    assert!(names.len() <= 13, "{} core tools: {names:?}", names.len());
    for must in [
        "session_init",
        "remember",
        "get",
        "recall",
        "forget",
        "aikoql",
        "tools_more",
    ] {
        assert!(
            names.iter().any(|n| n == must),
            "core lacks {must}: {names:?}"
        );
    }
}

#[test]
fn ag01b_every_full_tool_is_reachable_through_exactly_one_group() {
    let full: BTreeSet<String> = Mcp::serve("full-names", &[])
        .tools()
        .0
        .into_iter()
        .collect();
    let mut m = Mcp::serve("core-groups", &["--tool-profile", "core"]);
    let core: BTreeSet<String> = m
        .tools()
        .0
        .into_iter()
        .filter(|n| n != "tools_more")
        .collect();
    let (groups, _) = m.tool("tools_more", json!({}));
    let groups = groups["groups"]
        .as_object()
        .unwrap_or_else(|| panic!("tools_more lists no groups: {groups}"));
    let mut seen = core.clone();
    for (group, tools) in groups {
        for t in tools.as_array().unwrap() {
            let t = t.as_str().unwrap().to_string();
            assert!(
                seen.insert(t.clone()),
                "{t} is in two places (group {group})"
            );
        }
    }
    assert_eq!(seen, full, "core ∪ groups must be exactly the full surface");
}

#[test]
fn ag01c_enabling_a_group_lists_it_and_says_so() {
    let mut m = Mcp::serve("core-enable", &["--tool-profile", "core"]);
    let (init, _) = m.call(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
    );
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"],
        json!(true),
        "{init}"
    );
    let (before, _) = m.tools();
    assert!(!before.iter().any(|n| n == "txn_begin"));
    let (got, notes) = m.tool("tools_more", json!({"group": "txn"}));
    let defs: Vec<&str> = got["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{got}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        defs.contains(&"txn_begin") && got["tools"][0]["inputSchema"].is_object(),
        "{got}"
    );
    let (after, _) = m.tools();
    assert!(
        after.iter().any(|n| n == "txn_begin"),
        "an enabled group is listed: {after:?}"
    );
    let (_, later) = m.call("ping", json!({}));
    assert!(
        notes
            .iter()
            .chain(&later)
            .any(|f| f["method"] == "notifications/tools/list_changed"),
        "no list_changed notification: {notes:?} {later:?}"
    );
}

#[test]
fn ag01d_full_stays_the_default_and_core_never_blocks_a_call() {
    let (names, _) = Mcp::serve("default", &[]).tools();
    assert!(
        names.len() >= 100 && !names.iter().any(|n| n == "tools_more"),
        "{} tools",
        names.len()
    );
    let mut m = Mcp::serve("core-call", &["--tool-profile", "core"]);
    let (stats, _) = m.tool("storage_stats", json!({}));
    assert!(
        stats.is_object(),
        "an unlisted tool is still callable: {stats}"
    );
}
