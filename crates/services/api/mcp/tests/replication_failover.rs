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

/// A port for a server to bind, below every OS ephemeral range (Linux
/// 32768+, macOS/Windows 49152+). An ephemeral one (bind :0, release)
/// can be taken as the local end of any client connection before the
/// server binds it — "Address already in use" under the parallel suite.
fn free_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(0);
    // a per-run slot of 100 ports: back-to-back runs (whose last sockets
    // may still be closing) start apart
    let base = 20_000 + (std::process::id() % 120) as u16 * 100;
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let port = 20_000 + (base - 20_000 + n) % 12_000;
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    /// the server's log (stderr) lines, when started with `start_logged`
    log: Option<mpsc::Receiver<String>>,
    /// where an unlogged server's stderr goes
    stderr: PathBuf,
}

fn stderr_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}.stderr", db.display()))
}

impl Mcp {
    fn start(db: &Path, extra: &[&str]) -> Self {
        Self::spawn(db, extra, false)
    }

    fn start_logged(db: &Path, extra: &[&str]) -> Self {
        Self::spawn(db, extra, true)
    }

    fn spawn(db: &Path, extra: &[&str], logged: bool) -> Self {
        let mut m = Self::spawn_raw(db, extra, logged);
        m.init();
        m
    }

    /// Started, not yet initialized — an HA standby answers nothing until
    /// it is promoted.
    fn spawn_raw(db: &Path, extra: &[&str], logged: bool) -> Self {
        Self::spawn_env(db, extra, logged, &[])
    }

    fn spawn_env(db: &Path, extra: &[&str], logged: bool, env: &[(String, String)]) -> Self {
        let mut child = Command::new(bin())
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(extra)
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if logged {
                Stdio::piped()
            } else {
                // kept for the "server exited" panic
                Stdio::from(std::fs::File::create(stderr_path(db)).unwrap())
            })
            .spawn()
            .expect("spawn aikoql-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let log = child.stderr.take().map(lines);
        Mcp {
            child,
            stdin,
            stdout,
            next_id: 0,
            log,
            stderr: stderr_path(db),
        }
    }

    fn init(&mut self) {
        self.request(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "repl", "version": "0"}}),
        );
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
            if self.stdout.read_line(&mut line).unwrap() == 0 {
                let err = std::fs::read_to_string(&self.stderr).unwrap_or_default();
                let tail: Vec<&str> = err.lines().rev().take(8).collect();
                panic!(
                    "server exited; stderr tail:\n{}",
                    tail.into_iter()
                        .rev()
                        .map(strip_ansi)
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
            let frame: J = serde_json::from_str(line.trim()).expect("json-rpc frame");
            if frame["id"].as_u64() == Some(id) {
                assert!(frame.get("error").is_none(), "json-rpc error: {frame}");
                return frame["result"].clone();
            }
        }
    }

    /// One remember that may fail: Some(koid) when acked, None when
    /// refused or when the server is gone (a write it never acked).
    fn try_remember(&mut self, i: usize) -> Option<String> {
        self.next_id += 1;
        let id = self.next_id;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "remember",
            "arguments": {"subject": "admin", "type_name": "note", "properties": {"n": i}}}});
        writeln!(self.stdin, "{req}").ok()?;
        self.stdin.flush().ok()?;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let frame: J = serde_json::from_str(line.trim()).ok()?;
            if frame["id"].as_u64() == Some(id) {
                if frame["result"]["isError"] != json!(false) {
                    return None;
                }
                let text = frame["result"]["content"][0]["text"].as_str()?;
                let body: J = serde_json::from_str(text).ok()?;
                return body["koid"].as_str().map(str::to_string);
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

/// A child killed when dropped — a failing test must not leak it.
struct Proc(Child);

impl std::ops::Deref for Proc {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Proc {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `follow` as a child; its stderr lines arrive on the channel.
fn follow(standby: &Path, addr: &str) -> (Proc, mpsc::Receiver<String>) {
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
    (Proc(child), rx)
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
    // R4: promotion takes the next term
    assert_eq!(
        std::fs::read_to_string(sdir.join("REPL-TERM")).unwrap(),
        "1 leader\n"
    );
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

/// R4 — a node fenced by a newer term stays fenced across a restart: it
/// serves reads but refuses every write until it rejoins as a follower.
#[test]
fn a_fenced_node_refuses_writes_after_a_restart() {
    let dir = tmp("fenced");
    let mut m = Mcp::start(&dir, &[]);
    let koid = m.remember(1);
    drop(m);
    std::fs::write(dir.join("REPL-TERM"), "4 fenced\n").unwrap();
    std::fs::write(dir.join("REPL-LEADER"), "10.0.0.9:7070\n").unwrap();
    let mut m = Mcp::start(&dir, &[]);
    assert_eq!(
        m.tool("get", json!({"subject": "admin", "koid": koid}))["properties"]["n"],
        json!(1)
    );
    let res = m.request(
        "tools/call",
        json!({"name": "remember", "arguments": {"subject": "admin", "type_name": "note", "properties": {"n": 2}}}),
    );
    assert_eq!(
        res["isError"],
        json!(true),
        "a fenced node took a write: {res}"
    );
    // R4d: machine-readable, with the newer leader's client address
    let err: J = serde_json::from_str(res["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(err["error"]["code"], json!("NOT_LEADER"), "{err}");
    assert_eq!(err["error"]["leader"], json!("10.0.0.9:7070"), "{err}");
}

/// R4d — `--advertise` is the client address a leader hands its standbys
/// (and they hand a deposed leader, for its NOT_LEADER hint).
#[test]
fn a_leader_records_its_advertised_address() {
    let dir = tmp("advertise");
    let addr = format!("127.0.0.1:{}", free_port());
    let _m = Mcp::start(
        &dir,
        &[
            "--replicate-listen",
            &addr,
            "--tcp-token",
            "repl-secret::admin",
            "--advertise",
            "db1.internal:7070",
        ],
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("REPL-LEADER")).unwrap(),
        "db1.internal:7070\n"
    );
}

/// R4b — `--replicate-sync-ms`: a client write is acked only once a
/// standby holds it; with none connected it fails after the timeout.
#[test]
fn a_sync_leader_acks_writes_only_once_a_standby_holds_them() {
    let (ldir, sdir) = (tmp("sync-leader"), tmp("sync-standby"));
    let addr = format!("127.0.0.1:{}", free_port());
    let mut leader = Mcp::start(
        &ldir,
        &[
            "--replicate-listen",
            &addr,
            "--tcp-token",
            "repl-secret::admin",
            "--replicate-sync-ms",
            "300",
        ],
    );
    let res = leader.request(
        "tools/call",
        json!({"name": "remember", "arguments": {"subject": "admin", "type_name": "note", "properties": {"n": 0}}}),
    );
    assert_eq!(res["isError"], json!(true), "acked with no standby: {res}");
    assert!(res.to_string().contains("not replicated"), "{res}");

    let (mut f, rx) = follow(&sdir, &addr);
    wait_caught_up(&rx);
    leader.remember(1);
    let _ = f.kill(); // a bound `_f` would leak the child past the test
    let _ = f.wait();
}

/// R4c — the old leader comes back and simply follows the promoted
/// standby: its unreplicated tail is detected and replaced by the new
/// leader's history (snapshot over the stream), no operator step.
#[test]
fn a_diverged_old_leader_rejoins_the_promoted_standby_by_itself() {
    let (adir, bdir) = (tmp("rejoin-a"), tmp("rejoin-b"));
    let (addr_a, addr_b) = (
        format!("127.0.0.1:{}", free_port()),
        format!("127.0.0.1:{}", free_port()),
    );
    let tok = ["--tcp-token", "repl-secret::admin"];
    let mut a = Mcp::start(
        &adir,
        &[&["--replicate-listen", &addr_a][..], &tok].concat(),
    );
    let mut kept: Vec<String> = (0..10).map(|i| a.remember(i)).collect();
    let (mut f, rx) = follow(&bdir, &addr_a);
    wait_caught_up(&rx);
    let _ = f.kill();
    let _ = f.wait();
    let lost = a.remember(100); // B never sees it
    drop(a);

    let mut b = Mcp::start(
        &bdir,
        &[&["--replicate-listen", &addr_b][..], &tok].concat(),
    );
    kept.push(b.remember(10));
    let (mut f, rx) = follow(&adir, &addr_b);
    wait_line(&rx, "re-bootstrapped");
    wait_caught_up(&rx);
    let _ = f.kill();
    let _ = f.wait();
    drop(b);

    let mut a = Mcp::start(&adir, &[]);
    for (i, koid) in kept.iter().enumerate() {
        let got = a.tool("get", json!({"subject": "admin", "koid": koid}));
        assert_eq!(got["properties"]["n"], json!(i), "object {koid}");
    }
    let res = a.request(
        "tools/call",
        json!({"name": "get", "arguments": {"subject": "admin", "koid": lost}}),
    );
    assert_eq!(
        res["isError"],
        json!(true),
        "the diverged write survived: {res}"
    );
}

fn ha_args(lease: &Path, node: &str, repl: &str) -> Vec<String> {
    [
        "--ha-lease",
        lease.to_str().unwrap(),
        "--ha-node",
        node,
        "--ha-ttl-ms",
        // headroom: under the parallel suite a starved renewal must not
        // look like a dead leader (1.5 s flaked once in six runs)
        "3000",
        "--replicate-listen",
        repl,
        "--tcp-token",
        "repl-secret::admin",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// R5a — automatic failover: A wins the lease and leads, B follows it
/// in-process; A crashes, B sees the lease go stale for a TTL, wins it
/// with the next term and promotes itself — no operator. Standby reads:
/// before the failover B already answers reads and refuses writes with
/// A's address; it promotes by re-executing itself (a fresh MCP session).
#[test]
fn ha_standby_takes_over_when_the_leader_dies() {
    let (adir, bdir) = (tmp("ha-a"), tmp("ha-b"));
    let lease = tmp("ha-lease").join("lease.json");
    let (ra, rb) = (
        format!("127.0.0.1:{}", free_port()),
        format!("127.0.0.1:{}", free_port()),
    );
    let a_args = [
        ha_args(&lease, "a", &ra),
        vec!["--advertise".into(), "a.internal:7070".into()],
    ]
    .concat();
    let mut a = Mcp::start(&adir, &strs(&a_args));
    let koids: Vec<String> = (0..10).map(|i| a.remember(i)).collect();
    let mut b = Mcp::spawn_raw(&bdir, &strs(&ha_args(&lease, "b", &rb)), true);
    let log = b.log.take().unwrap();
    wait_line(&log, "standby caught up");

    // standby reads: B serves before any failover...
    b.init();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let res = b.request(
            "tools/call",
            json!({"name": "get", "arguments": {"subject": "admin", "koid": koids[9]}}),
        );
        if res["isError"] == json!(false) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the HA standby never served a read: {res}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // ...and refuses writes, naming the leader
    let res = b.request(
        "tools/call",
        json!({"name": "remember", "arguments": {"subject": "admin", "type_name": "note", "properties": {"n": 0}}}),
    );
    let err: J = serde_json::from_str(res["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(err["error"]["code"], json!("NOT_LEADER"), "{err}");
    assert_eq!(err["error"]["leader"], json!("a.internal:7070"), "{err}");

    let t = std::time::Instant::now();
    drop(a); // a crash
             // B wins and re-executes itself as the leader: a fresh MCP session
    wait_line(&log, "won the lease");
    wait_line(&log, "aikoql-mcp ready");
    b.init();
    eprintln!("R5a metric: failover took {:?} (ttl 3000 ms)", t.elapsed());
    for (i, koid) in koids.iter().enumerate() {
        let got = b.tool("get", json!({"subject": "admin", "koid": koid}));
        assert_eq!(got["properties"]["n"], json!(i), "object {koid}");
    }
    b.remember(99);
    assert_eq!(
        std::fs::read_to_string(bdir.join("REPL-TERM")).unwrap(),
        "2 leader\n"
    );
}

/// R5a — self-fencing: a leader that finds its lease taken stops
/// writing on its own (fenced, with the new holder's address as the
/// hint) and exits for its supervisor to restart it as a standby.
#[test]
fn ha_leader_fences_itself_when_its_lease_is_taken() {
    let dir = tmp("ha-fence");
    let lease = tmp("ha-fence-lease").join("lease.json");
    let repl = format!("127.0.0.1:{}", free_port());
    let mut a = Mcp::start(&dir, &strs(&ha_args(&lease, "a", &repl)));
    a.remember(1);
    std::fs::write(
        &lease,
        r#"{"holder":"intruder","term":7,"repl":"127.0.0.1:1","client":"10.0.0.7:7070","seq":0,"beat":0}"#,
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(st) = a.child.try_wait().unwrap() {
            break st;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the leader kept its lost lease"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(75), "self-fence exit code");
    assert_eq!(
        std::fs::read_to_string(dir.join("REPL-TERM")).unwrap(),
        "1 fenced\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("REPL-LEADER")).unwrap(),
        "10.0.0.7:7070\n"
    );
}

// ---------------------------------------------------------------- R5c
// Split-brain certs. A fake S3 (conditional PUT) holds the lease; a Gate
// in front of one node's S3 endpoint cuts that node off (blackhole: the
// connection hangs, like a partition) or rejects it (403: what AWS sends a
// request signed with a skewed clock). The ordering proof uses the two
// nodes' log timestamps — one machine, one clock: the old leader logs
// "fenced" after its fence took effect, the standby logs "taking over"
// before its CAS, so fenced < taking-over proves the writers never overlap.

#[derive(Default)]
struct S3Object {
    body: Option<(Vec<u8>, u64)>,
    next: u64,
}

/// One HTTP/1.1 request off a connection: (method, headers, body).
fn read_http(
    conn: &std::net::TcpStream,
) -> Option<(String, std::collections::HashMap<String, String>, Vec<u8>)> {
    use std::io::Read;
    let mut r = BufReader::new(conn);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let method = line.split_whitespace().next()?.to_string();
    let mut headers = std::collections::HashMap::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.insert(k.to_ascii_lowercase(), v.trim().to_string());
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).ok()?;
    Some((method, headers, body))
}

fn http_reply(mut conn: &std::net::TcpStream, status: &str, etag: Option<String>, body: &[u8]) {
    let etag = etag.map(|e| format!("ETag: {e}\r\n")).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {status}\r\n{etag}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(body);
}

/// A fake S3 with conditional PUT (If-None-Match: * / If-Match → 412).
fn fake_s3() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let state = std::sync::Arc::new(std::sync::Mutex::new(S3Object::default()));
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let state = state.clone();
            std::thread::spawn(move || {
                let Some((method, headers, body)) = read_http(&conn) else {
                    return;
                };
                let mut s = state.lock().unwrap();
                let etag = |n: u64| format!("\"v{n}\"");
                if method == "GET" {
                    match &s.body {
                        Some((b, n)) => http_reply(&conn, "200 OK", Some(etag(*n)), b),
                        None => http_reply(&conn, "404 Not Found", None, b"<Error/>"),
                    }
                    return;
                }
                let current = s.body.as_ref().map(|(_, n)| etag(*n));
                let ok = match (headers.get("if-none-match"), headers.get("if-match")) {
                    (Some(_), _) => current.is_none(),
                    (_, Some(m)) => current.as_deref() == Some(m.as_str()),
                    _ => true,
                };
                if ok {
                    s.next += 1;
                    let n = s.next;
                    s.body = Some((body, n));
                    http_reply(&conn, "200 OK", Some(etag(n)), b"");
                } else {
                    http_reply(&conn, "412 Precondition Failed", None, b"<Error/>");
                }
            });
        }
    });
    endpoint
}

const GATE_OPEN: u8 = 0;
const GATE_BLACKHOLE: u8 = 1;
const GATE_REJECT: u8 = 2;

/// A switchable forwarder in front of one node's S3 endpoint.
struct Gate {
    endpoint: String,
    mode: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

impl Gate {
    fn start(target: &str) -> Self {
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let mode = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(GATE_OPEN));
        let (m, target) = (
            mode.clone(),
            target.trim_start_matches("http://").to_string(),
        );
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (m, target) = (m.clone(), target.clone());
                std::thread::spawn(move || match m.load(Ordering::SeqCst) {
                    GATE_OPEN => {
                        let Ok(up) = std::net::TcpStream::connect(&target) else {
                            return;
                        };
                        let (mut c1, mut u1) = (conn.try_clone().unwrap(), up.try_clone().unwrap());
                        std::thread::spawn(move || {
                            let _ = std::io::copy(&mut c1, &mut u1);
                        });
                        let (mut u2, mut c2) = (up, conn);
                        let _ = std::io::copy(&mut u2, &mut c2);
                        let _ = c2.shutdown(std::net::Shutdown::Both);
                    }
                    GATE_BLACKHOLE => {
                        // hold the connection, answer nothing, until reopened
                        while m.load(Ordering::SeqCst) == GATE_BLACKHOLE {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                    _ => {
                        let _ = read_http(&conn);
                        http_reply(
                            &conn,
                            "403 Forbidden",
                            None,
                            b"<Error><Code>RequestTimeTooSkewed</Code></Error>",
                        );
                    }
                });
            }
        });
        Gate { endpoint, mode }
    }

    fn set(&self, mode: u8) {
        self.mode.store(mode, std::sync::atomic::Ordering::SeqCst);
    }
}

fn s3_env(endpoint: &str) -> Vec<(String, String)> {
    [
        ("AWS_ACCESS_KEY_ID", "AKIDTEST"),
        ("AWS_SECRET_ACCESS_KEY", "secret"),
        ("AWS_REGION", "us-east-1"),
        ("AIKOQL_S3_ENDPOINT", endpoint),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// A node's HA args on the S3 lease, with synchronous acks (R4b) so every
/// acked write is on the standby.
fn s3_node(node: &str, repl: &str) -> Vec<String> {
    let mut args = ha_args(Path::new("s3://ha-bucket/lease.json"), node, repl);
    args.extend(["--replicate-sync-ms".to_string(), "3000".to_string()]);
    args
}

/// The first log line containing `needle`, and its timestamp (the
/// line's first token, RFC 3339 UTC — comparable as text).
fn stamped(rx: &mpsc::Receiver<String>, needle: &str) -> String {
    let line = wait_line(rx, needle);
    line.split_whitespace().next().unwrap().to_string()
}

/// R5c — partition the leader from the lease store. It must fence
/// itself before the standby takes over (one writer per term), and
/// every write it acked — before or during the partition — must be on
/// the new leader.
#[test]
fn split_brain_a_leader_cut_off_from_its_lease_fences_before_the_standby_takes_over() {
    let s3 = fake_s3();
    let gate = Gate::start(&s3);
    let (adir, bdir) = (tmp("sb-a"), tmp("sb-b"));
    let (ra, rb) = (
        format!("127.0.0.1:{}", free_port()),
        format!("127.0.0.1:{}", free_port()),
    );
    let mut a = Mcp::spawn_env(
        &adir,
        &strs(&s3_node("a", &ra)),
        true,
        &s3_env(&gate.endpoint),
    );
    a.init();
    let alog = a.log.take().unwrap();
    let mut b = Mcp::spawn_env(&bdir, &strs(&s3_node("b", &rb)), true, &s3_env(&s3));
    let blog = b.log.take().unwrap();
    wait_line(&blog, "standby caught up");
    let mut acked: Vec<(usize, String)> = (0..5).map(|i| (i, a.remember(i))).collect();

    gate.set(GATE_BLACKHOLE); // the partition
    let cut = std::time::Instant::now();
    let mut i = 100;
    while a.child.try_wait().unwrap().is_none() {
        assert!(
            cut.elapsed() < Duration::from_secs(20),
            "the cut-off leader never fenced"
        );
        if let Some(k) = a.try_remember(i) {
            acked.push((i, k));
        }
        i += 1;
    }
    assert_eq!(
        a.child.wait().unwrap().code(),
        Some(75),
        "self-fence exit code"
    );
    let fenced = stamped(&alog, "fenced; exiting");
    let taking_over = stamped(&blog, "taking over");
    wait_line(&blog, "aikoql-mcp ready"); // the promoted, re-executed B
    assert!(
        fenced < taking_over,
        "two writers: the standby began taking over at {taking_over}, the old leader fenced at {fenced}"
    );
    eprintln!(
        "R5c metric: fenced at {fenced}, standby took over at {taking_over} ({} writes acked)",
        acked.len()
    );
    b.init();
    for (n, koid) in &acked {
        let got = b.tool("get", json!({"subject": "admin", "koid": koid}));
        assert_eq!(got["properties"]["n"], json!(n), "acked write {koid} lost");
    }
    assert_eq!(
        std::fs::read_to_string(adir.join("REPL-TERM")).unwrap(),
        "1 fenced\n"
    );
    assert_eq!(
        std::fs::read_to_string(bdir.join("REPL-TERM")).unwrap(),
        "2 leader\n"
    );
}

/// R5c — kill the leader mid-write (a request in flight, SIGKILL): every
/// write it acked is on the promoted standby.
#[test]
fn split_brain_a_leader_killed_mid_write_loses_no_acked_write() {
    let lease = tmp("sb-kill-lease").join("lease.json");
    let (adir, bdir) = (tmp("sb-kill-a"), tmp("sb-kill-b"));
    let (ra, rb) = (
        format!("127.0.0.1:{}", free_port()),
        format!("127.0.0.1:{}", free_port()),
    );
    let sync = ["--replicate-sync-ms".to_string(), "3000".to_string()];
    let mut a = Mcp::start(
        &adir,
        &strs(&[ha_args(&lease, "a", &ra), sync.to_vec()].concat()),
    );
    let mut b = Mcp::spawn_raw(
        &bdir,
        &strs(&[ha_args(&lease, "b", &rb), sync.to_vec()].concat()),
        true,
    );
    let blog = b.log.take().unwrap();
    wait_line(&blog, "standby caught up");
    let acked: Vec<String> = (0..30).map(|i| a.remember(i)).collect();
    // one more write in flight, then the crash
    writeln!(
        a.stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 9999, "method": "tools/call", "params": {"name": "remember",
            "arguments": {"subject": "admin", "type_name": "note", "properties": {"n": 9999}}}})
    )
    .unwrap();
    a.stdin.flush().unwrap();
    let _ = a.child.kill();
    let _ = a.child.wait();
    wait_line(&blog, "won the lease");
    wait_line(&blog, "aikoql-mcp ready"); // the promoted, re-executed B
    b.init();
    for (i, koid) in acked.iter().enumerate() {
        let got = b.tool("get", json!({"subject": "admin", "koid": koid}));
        assert_eq!(got["properties"]["n"], json!(i), "acked write {koid} lost");
    }
    assert_eq!(
        std::fs::read_to_string(bdir.join("REPL-TERM")).unwrap(),
        "2 leader\n"
    );
}

/// R5c — a skewed clock: AWS rejects a request whose SigV4 date is too
/// far off (403 RequestTimeTooSkewed). The agent never compares wall
/// clocks, so skew can only surface as a store that refuses the leader —
/// and a refused leader fences itself, fail-safe.
#[test]
fn split_brain_a_leader_whose_store_rejects_it_fences_itself() {
    let s3 = fake_s3();
    let gate = Gate::start(&s3);
    let dir = tmp("sb-skew");
    let repl = format!("127.0.0.1:{}", free_port());
    // no standby here, so no synchronous acks
    let args = ha_args(Path::new("s3://ha-bucket/lease.json"), "a", &repl);
    let mut a = Mcp::spawn_env(&dir, &strs(&args), false, &s3_env(&gate.endpoint));
    a.init();
    a.remember(1);
    gate.set(GATE_REJECT);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(st) = a.child.try_wait().unwrap() {
            break st;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a rejected leader kept writing"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(75));
    assert_eq!(
        std::fs::read_to_string(dir.join("REPL-TERM")).unwrap(),
        "1 fenced\n"
    );
}

/// R3c — with TLS a leader may bind a non-loopback address (the stream
/// is encrypted and the standby verifies the leader's certificate); a
/// standby trusting the CA follows it.
#[test]
fn replicate_listen_over_tls_accepts_a_non_loopback_bind() {
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls");
    let (ldir, sdir) = (tmp("tls-leader"), tmp("tls-standby"));
    let port = free_port();
    let mut leader = Mcp::start(
        &ldir,
        &[
            "--replicate-listen",
            &format!("0.0.0.0:{port}"),
            "--replicate-tls-cert",
            fx.join("server.pem").to_str().unwrap(),
            "--replicate-tls-key",
            fx.join("server.key").to_str().unwrap(),
            "--tcp-token",
            "repl-secret::admin",
        ],
    );
    let koids: Vec<String> = (0..10).map(|i| leader.remember(i)).collect();
    let mut child = Command::new(bin())
        .arg("follow")
        .arg(&sdir)
        .args(["--leader", &format!("127.0.0.1:{port}")])
        .args(["--tls-ca", fx.join("ca.pem").to_str().unwrap()])
        .env("AIKOQL_REPLICATION_TOKEN", "repl-secret")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let rx = lines(child.stderr.take().unwrap());
    wait_caught_up(&rx);
    let _ = child.kill();
    let _ = child.wait();
    drop(leader);
    let mut promoted = Mcp::start(&sdir, &[]);
    for (i, koid) in koids.iter().enumerate() {
        let got = promoted.tool("get", json!({"subject": "admin", "koid": koid}));
        assert_eq!(got["properties"]["n"], json!(i), "object {koid}");
    }
}

/// R3c-2 — the leader's /metrics scrape carries the replication series:
/// a following standby shows up with its lag, which drains to 0.
#[test]
fn the_leader_scrape_carries_replication_lag() {
    let (ldir, sdir) = (tmp("lag-leader"), tmp("lag-standby"));
    let (repl, metrics) = (
        format!("127.0.0.1:{}", free_port()),
        format!("127.0.0.1:{}", free_port()),
    );
    // metrics-only serve reads no stdin (S-12): no client writes — the
    // kernel's open-time records are the history the standby follows
    let _leader = Mcp::spawn_raw(
        &ldir,
        &[
            "--replicate-listen",
            &repl,
            "--metrics-addr",
            &metrics,
            "--tcp-token",
            "repl-secret::admin",
        ],
        false,
    );
    let (mut f, rx) = follow(&sdir, &repl);
    wait_caught_up(&rx);
    let scrape = || {
        let mut s = std::net::TcpStream::connect(&metrics).unwrap();
        write!(
            s,
            "GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        // the server may keep the connection open: read until it pauses
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut out = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(n) = std::io::Read::read(&mut s, &mut chunk) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&out);
            if let Some(at) = text.find("\r\n\r\n") {
                let len = text[..at]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().to_string())
                    })
                    .and_then(|v| v.parse::<usize>().ok());
                if len.is_some_and(|len| out.len() >= at + 4 + len) {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let m = scrape();
        if m.contains("aikoql_replication_standbys 1")
            && m.contains("_lag_frames{")
            && m.contains("\"} 0\n")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no replication lag series:\n{m}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = f.kill();
    let _ = f.wait();
}

/// R6 — the continuous archive and point-in-time restore through the
/// binary: a leader archives (base snapshot + log batches) while it
/// writes; it dies; `pitr` restores the latest state, and a restore until
/// an instant between two write bursts holds the first and not the second.
#[test]
fn pitr_restores_the_archive_to_the_latest_and_to_a_point_in_time() {
    let (ldir, store) = (tmp("pitr-leader"), tmp("pitr-archive"));
    let mut leader = Mcp::start_logged(
        &ldir,
        &[
            "--archive",
            store.to_str().unwrap(),
            "--archive-log-ms",
            "200",
        ],
    );
    let log = leader.log.take().unwrap();
    let first: Vec<String> = (0..10).map(|i| leader.remember(i)).collect();
    // wait for the archive to cover the first burst
    let covered = |log: &mpsc::Receiver<String>, n: usize| loop {
        let line = wait_line(log, "archive: log up to seq");
        let seq: usize = line.rsplit(' ').next().unwrap().parse().unwrap();
        if seq >= n {
            break;
        }
    };
    covered(&log, 10);
    std::thread::sleep(Duration::from_millis(300));
    let boundary = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    std::thread::sleep(Duration::from_millis(300));
    let second: Vec<String> = (10..20).map(|i| leader.remember(i)).collect();
    covered(&log, 20);
    drop(leader);

    let pitr = |to: &Path, until: &[&str]| {
        let out = Command::new(bin())
            .arg("pitr")
            .args([
                "--archive",
                store.to_str().unwrap(),
                "--to",
                to.to_str().unwrap(),
            ])
            .args(until)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "pitr failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let latest = tmp("pitr-latest");
    pitr(&latest, &[]);
    let mut m = Mcp::start(&latest, &[]);
    for (i, koid) in first.iter().chain(&second).enumerate() {
        let got = m.tool("get", json!({"subject": "admin", "koid": koid}));
        assert_eq!(got["properties"]["n"], json!(i), "object {koid}");
    }
    drop(m);

    let earlier = tmp("pitr-earlier");
    pitr(&earlier, &["--until-ms", &boundary]);
    let mut m = Mcp::start(&earlier, &[]);
    for (i, koid) in first.iter().enumerate() {
        let got = m.tool("get", json!({"subject": "admin", "koid": koid}));
        assert_eq!(got["properties"]["n"], json!(i), "object {koid}");
    }
    let res = m.request(
        "tools/call",
        json!({"name": "get", "arguments": {"subject": "admin", "koid": second[0]}}),
    );
    assert_eq!(
        res["isError"],
        json!(true),
        "a write after the instant survived: {res}"
    );
}

/// Standby reads — `serve --follow LEADER` is a read replica: it applies
/// the leader's log into its own directory while serving reads (which
/// converge on the leader's writes), and refuses writes with NOT_LEADER
/// and the leader's client address.
#[test]
fn a_read_replica_serves_converging_reads_and_refuses_writes() {
    let (ldir, sdir) = (tmp("replica-leader"), tmp("replica"));
    let repl = format!("127.0.0.1:{}", free_port());
    let mut leader = Mcp::start(
        &ldir,
        &[
            "--replicate-listen",
            &repl,
            "--tcp-token",
            "repl-secret::admin",
            "--advertise",
            "db1.internal:7070",
        ],
    );
    let first = leader.remember(1);
    let env = [(
        "AIKOQL_REPLICATION_TOKEN".to_string(),
        "repl-secret".to_string(),
    )];
    let mut replica = Mcp::spawn_env(&sdir, &["--follow", &repl], false, &env);
    replica.init();
    let mut read = |m: &mut Mcp, koid: &str| -> Option<J> {
        let res = m.request(
            "tools/call",
            json!({"name": "get", "arguments": {"subject": "admin", "koid": koid}}),
        );
        if res["isError"] == json!(false) {
            serde_json::from_str(res["content"][0]["text"].as_str()?).ok()
        } else {
            None
        }
    };
    let converge = |m: &mut Mcp, koid: &str, read: &mut dyn FnMut(&mut Mcp, &str) -> Option<J>| {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(ko) = read(m, koid) {
                return ko;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the replica never served {koid}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    assert_eq!(
        converge(&mut replica, &first, &mut read)["properties"]["n"],
        json!(1)
    );
    let second = leader.remember(2); // written while the replica serves
    assert_eq!(
        converge(&mut replica, &second, &mut read)["properties"]["n"],
        json!(2)
    );

    // queries and text search follow too: the type index (storage) and the
    // text index, which the replica's maintainer feeds from caught-up events
    let zebra = leader.tool(
        "remember",
        json!({"subject": "admin", "type_name": "note", "properties": {"text": "a zebra crossing"}}),
    )["koid"]
        .as_str()
        .unwrap()
        .to_string();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let q = replica.request("tools/call", json!({"name": "aikoql", "arguments": {"subject": "admin", "query": "MATCH note RETURN *"}}));
        let s = replica.request("tools/call", json!({"name": "find_similar", "arguments": {"subject": "admin", "text": "zebra", "k": 5}}));
        let (q, s) = (q.to_string(), s.to_string());
        if q.contains(&zebra) && s.contains(&zebra) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let l = leader.request(
                "tools/call",
                json!({"name": "find_similar", "arguments": {"subject": "admin", "text": "zebra", "k": 5}}),
            );
            panic!("replica query/search never saw it:\n{q}\n{s}\nleader search: {l}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let res = replica.request(
        "tools/call",
        json!({"name": "remember", "arguments": {"subject": "admin", "type_name": "note", "properties": {"n": 3}}}),
    );
    assert_eq!(res["isError"], json!(true), "a replica took a write: {res}");
    let err: J = serde_json::from_str(res["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(err["error"]["code"], json!("NOT_LEADER"), "{err}");
    assert_eq!(err["error"]["leader"], json!("db1.internal:7070"), "{err}");
}
