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
    /// the server's log (stderr) lines, when started with `start_logged`
    log: Option<mpsc::Receiver<String>>,
}

impl Mcp {
    fn start(db: &Path, extra: &[&str]) -> Self {
        Self::spawn(db, extra, false)
    }

    fn start_logged(db: &Path, extra: &[&str]) -> Self {
        Self::spawn(db, extra, true)
    }

    fn spawn(db: &Path, extra: &[&str], logged: bool) -> Self {
        let mut child = Command::new(bin())
            .args(extra)
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if logged {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .spawn()
            .expect("spawn aikoql-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let log = child.stderr.take().map(lines);
        let mut m = Mcp {
            child,
            stdin,
            stdout,
            next_id: 0,
            log,
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

/// A child's stderr, line by line, on a channel.
fn lines(err: std::process::ChildStderr) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            let _ = tx.send(strip_ansi(&line));
        }
    });
    rx
}

/// The log colors fields on unix (`use_ansi`); tests read plain text.
fn strip_ansi(line: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in line.chars() {
        match (esc, c) {
            (false, '\x1b') => esc = true,
            (false, c) => out.push(c),
            (true, 'm') => esc = false,
            (true, _) => {}
        }
    }
    out
}

/// The first line containing `needle`, or a panic after 30 s.
fn wait_line(rx: &mpsc::Receiver<String>, needle: &str) -> String {
    loop {
        let line = rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("no log line containing {needle:?}"));
        assert!(!line.contains("follow stopped"), "follow failed: {line}");
        if line.contains(needle) {
            return line;
        }
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
    let rx = lines(child.stderr.take().unwrap());
    (child, rx)
}

fn wait_caught_up(rx: &mpsc::Receiver<String>) {
    wait_line(rx, "standby caught up");
}

/// `replayed=N` from the promoted node's maintainer start line.
fn replayed(line: &str) -> u64 {
    let at = line.find("replayed=").expect("replayed= field") + "replayed=".len();
    line[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .unwrap()
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

/// R3b — fast failover: the leader checkpoints its indexes on a timer and
/// ships each checkpoint down the stream, so the promoted node resumes
/// them and replays only the journal tail — not the whole history.
#[test]
fn promotion_resumes_indexes_from_the_shipped_checkpoint() {
    let (ldir, sdir) = (tmp("ckpt-leader"), tmp("ckpt-standby"));
    let addr = format!("127.0.0.1:{}", free_port());
    let mut leader = Mcp::start(
        &ldir,
        &[
            "--replicate-listen",
            &addr,
            "--tcp-token",
            "repl-secret::admin",
            "--index-checkpoint-secs",
            "1",
        ],
    );
    let history = 150;
    let mut koids: Vec<String> = (0..history).map(|i| leader.remember(i)).collect();
    let (mut f, rx) = follow(&sdir, &addr);
    // a shipped checkpoint that covers the history (an early periodic one
    // may predate the writes; the next one, with a moved water, ships too)
    loop {
        let line = wait_line(&rx, "standby index checkpoint at water");
        let water: u64 = line.rsplit(' ').next().unwrap().parse().unwrap();
        if water >= history as u64 {
            break;
        }
    }
    koids.extend((history..history + 5).map(|i| leader.remember(i))); // the tail
    wait_caught_up(&rx);
    drop(leader);
    let _ = f.kill();
    let _ = f.wait();

    let mut promoted = Mcp::start_logged(&sdir, &[]);
    let log = promoted.log.take().unwrap();
    let line = wait_line(&log, "index maintainer");
    assert!(
        line.contains("resumed from checkpoint"),
        "the promoted node rebuilt its indexes from scratch: {line}"
    );
    let n = replayed(&line);
    eprintln!("R3b metric: promotion replayed {n} journal events (history {history} + tail 5)");
    assert!(
        n < history as u64 / 2,
        "replayed {n} events at promotion — the whole history, not the tail"
    );
    for (i, koid) in koids.iter().enumerate() {
        let got = promoted.tool("get", json!({"subject": "admin", "koid": koid}));
        assert_eq!(got["properties"]["n"], json!(i), "object {koid}");
    }
}
