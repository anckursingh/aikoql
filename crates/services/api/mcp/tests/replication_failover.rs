//! MRFC-0080 R3a — the manual failover drill through the real binary: a
//! leader serves with `--replicate-listen`, `follow` streams its log into a
//! standby directory, the leader dies, and `serve` on the standby answers
//! every object the leader committed.

mod common;

use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aikoql-mcp"))
}

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("repl-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    common::register_temp_path(&p);
    p
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(db: &Path, extra: &[&str]) -> Self {
        let mut child = Command::new(bin())
            .args(extra)
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aikoql-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        m.request(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "repl", "version": "0"}}),
        );
        m
    }

    fn request(&mut self, method: &str, params: J) -> J {
        self.next_id += 1;
        let id = self.next_id;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server exited"
            );
            let frame: J = serde_json::from_str(line.trim()).expect("json-rpc frame");
            if frame["id"].as_u64() == Some(id) {
                assert!(frame.get("error").is_none(), "json-rpc error: {frame}");
                return frame["result"].clone();
            }
        }
    }

    fn tool(&mut self, name: &str, args: J) -> J {
        let res = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert_eq!(res["isError"], json!(false), "tool error: {res}");
        serde_json::from_str(res["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn remember(&mut self, i: usize) -> String {
        self.tool(
            "remember",
            json!({"subject": "admin", "type_name": "note", "properties": {"n": i}}),
        )["koid"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill(); // a crash, not a clean shutdown
        let _ = self.child.wait();
    }
}

/// `follow` as a child; its stderr lines arrive on the channel.
fn follow(standby: &Path, addr: &str) -> (Child, mpsc::Receiver<String>) {
    let mut child = Command::new(bin())
        .arg("follow")
        .arg(standby)
        .args(["--leader", addr])
        .env("AIKOQL_REPLICATION_TOKEN", "repl-secret")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn follow");
    let err = BufReader::new(child.stderr.take().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in err.lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    (child, rx)
}

fn wait_caught_up(rx: &mpsc::Receiver<String>) {
    loop {
        let line = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the standby never reported catching up");
        assert!(!line.contains("follow stopped"), "follow failed: {line}");
        if line.contains("standby caught up") {
            return;
        }
    }
}

#[test]
fn leader_dies_standby_is_promoted_with_every_object() {
    let (ldir, sdir) = (tmp("leader"), tmp("standby"));
    let addr = format!("127.0.0.1:{}", free_port());
    let mut leader = Mcp::start(
        &ldir,
        &[
            "--replicate-listen",
            &addr,
            "--tcp-token",
            "repl-secret::admin",
        ],
    );
    let mut koids: Vec<String> = (0..20).map(|i| leader.remember(i)).collect();

    let (mut f, rx) = follow(&sdir, &addr);
    wait_caught_up(&rx);
    koids.extend((20..40).map(|i| leader.remember(i))); // live while streaming
    wait_caught_up(&rx);

    drop(leader); // the leader dies
    let _ = f.kill(); // the operator stops following before promoting
    let _ = f.wait();

    let mut promoted = Mcp::start(&sdir, &[]);
    for (i, koid) in koids.iter().enumerate() {
        let got = promoted.tool("get", json!({"subject": "admin", "koid": koid}));
        assert_eq!(got["properties"]["n"], json!(i), "object {koid}");
    }
    let fresh = promoted.remember(99);
    assert!(
        !koids.contains(&fresh),
        "the promoted node re-issued a KOID"
    );
}

#[test]
fn replicate_listen_without_a_token_refuses_to_serve() {
    let out = Command::new(bin())
        .args(["--replicate-listen", "127.0.0.1:0"])
        .arg(tmp("no-token"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--tcp-token"));
}

#[test]
fn replicate_listen_rejects_a_non_loopback_bind() {
    let out = Command::new(bin())
        .args([
            "--replicate-listen",
            "0.0.0.0:7071",
            "--tcp-token",
            "t::admin",
        ])
        .arg(tmp("public"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
