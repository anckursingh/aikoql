//! MRFC-0080 R3a — the replication stream: a leader serves its storage
//! log over TCP to standbys that apply it with `apply_replicated`.
//!
//! Wire (`aikoql-repl/1`): the standby sends one JSON line
//! `{"proto","token","after"}`; the leader answers `{"ok":true}` or
//! `{"error":..}` and closes. Then leader → standby is a stream of tagged
//! messages — `1` + a raw WAL frame (self-delimiting, checksummed), `2` +
//! u64 LE leader seq (heartbeat), `3` + u32 LE len + reason (the log no
//! longer holds what the standby needs: re-bootstrap). Standby → leader is
//! a stream of u64 LE acked seqs; the leader releases its archive to the
//! slowest connected standby's ack.

use crate::session::TcpAuthTable;
use aikoql_storage_v2::db::Db;
use aikoql_storage_v2::engine::StorageAdminApi;
use aikoql_storage_v2::wal::{decode_frame, encode_frame, FRAME_HEADER_LEN};
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::warn;

const PROTO: &str = "aikoql-repl/1";
const TAG_FRAME: u8 = 1;
const TAG_HEARTBEAT: u8 = 2;
const TAG_GAP: u8 = 3;
const HEARTBEAT: Duration = Duration::from_secs(1);
/// A standby that hears nothing — not even a heartbeat — this long
/// reconnects: the leader is gone or the link is dead.
const LEADER_SILENCE: Duration = Duration::from_secs(5);
/// Trust-boundary bounds on lengths read off the wire.
const MAX_FRAME_PAYLOAD: usize = 256 << 20;
const MAX_LINE: usize = 4096;
const MAX_REASON: usize = 64 << 10;
/// Frames per log read.
const BATCH: usize = 256;

/// Serve the replication stream until `stop`. Every standby must present
/// a `--tcp-token` carrying the `admin` role: the stream is the whole
/// database, across tenants.
pub(crate) fn serve_replication(
    listener: TcpListener,
    source: Arc<dyn StorageAdminApi>,
    auth: Arc<TcpAuthTable>,
    stop: Arc<AtomicBool>,
) {
    if let Err(e) = listener.set_nonblocking(true) {
        warn!("replication listener: {e}");
        return;
    }
    let acks: Arc<Mutex<HashMap<u64, u64>>> = Arc::new(Mutex::new(HashMap::new()));
    let next_id = AtomicU64::new(0);
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                let (source, auth, acks, stop) =
                    (source.clone(), auth.clone(), acks.clone(), stop.clone());
                std::thread::spawn(move || {
                    if let Err(e) = serve_standby(&stream, id, &*source, &auth, &acks, &stop) {
                        warn!("replication standby {id}: {e}");
                    }
                    // justified: Mutex poison is unrecoverable
                    acks.lock().unwrap().remove(&id);
                    let _ = stream.shutdown(Shutdown::Both);
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(e) => warn!("replication accept: {e}"),
        }
    }
}

fn serve_standby(
    stream: &TcpStream,
    id: u64,
    source: &dyn StorageAdminApi,
    auth: &TcpAuthTable,
    acks: &Arc<Mutex<HashMap<u64, u64>>>,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(LEADER_SILENCE))?;
    let mut w = stream;
    let hello: serde_json::Value = serde_json::from_slice(&read_line(stream)?)
        .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e))?;
    let token = hello["token"].as_str().unwrap_or_default();
    let admin = auth
        .lookup(token)
        .is_some_and(|i| i.roles.iter().any(|r| r == "admin"));
    let after = hello["after"].as_u64();
    let refusal = if hello["proto"] != PROTO {
        Some(format!(
            "replication refused: protocol {} (this leader: {PROTO})",
            hello["proto"]
        ))
    } else if !admin {
        Some("replication refused: the token is unknown or lacks the admin role".to_string())
    } else if after.is_none() {
        Some("replication refused: no resume seq".to_string())
    } else {
        None
    };
    if let Some(reason) = refusal {
        w.write_all(format!("{}\n", serde_json::json!({ "error": reason })).as_bytes())?;
        return Ok(());
    }
    let mut sent = after.unwrap_or(0);
    w.write_all(b"{\"ok\":true}\n")?;
    // justified: Mutex poison is unrecoverable
    acks.lock().unwrap().insert(id, sent);

    // Acks arrive on their own thread; it ends when the socket closes.
    stream.set_read_timeout(None)?;
    let mut ack_reader = stream.try_clone()?;
    let ack_map = acks.clone();
    std::thread::spawn(move || {
        let mut b = [0u8; 8];
        while ack_reader.read_exact(&mut b).is_ok() {
            // justified: Mutex poison is unrecoverable
            ack_map.lock().unwrap().insert(id, u64::from_le_bytes(b));
        }
    });

    let mut released = 0;
    let mut last_send = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        // justified: Mutex poison is unrecoverable
        let slowest = acks.lock().unwrap().values().copied().min().unwrap_or(0);
        if slowest > released {
            if let Err(e) = source.release_log(slowest) {
                warn!("release replication log to {slowest}: {e}");
            }
            released = slowest;
        }
        let head = source.last_seq();
        if head > sent {
            let frames = match source.log_since(sent, BATCH) {
                Ok(f) => f,
                Err(e) => {
                    let reason = e.to_string();
                    let mut msg = vec![TAG_GAP];
                    msg.extend_from_slice(&(reason.len() as u32).to_le_bytes());
                    msg.extend_from_slice(reason.as_bytes());
                    return w.write_all(&msg);
                }
            };
            let mut buf = Vec::new();
            for f in frames {
                buf.push(TAG_FRAME);
                buf.extend_from_slice(
                    &encode_frame(f.seq, &f.ops)
                        .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e.to_string()))?,
                );
                sent = f.seq;
            }
            w.write_all(&buf)?;
            last_send = Instant::now();
        } else {
            if last_send.elapsed() >= HEARTBEAT {
                let mut msg = vec![TAG_HEARTBEAT];
                msg.extend_from_slice(&head.to_le_bytes());
                w.write_all(&msg)?;
                last_send = Instant::now();
            }
            // shortcut: polls the log head every 10 ms; a commit notify
            // from the Db replaces it when replication lag must drop below.
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

/// How one connection ended, from the standby's side.
enum End {
    /// `stop` was raised.
    Stopped,
    /// The link failed — reconnect.
    Retry(String),
    /// The leader refused us, cannot serve our position, or sent a frame
    /// we cannot apply — reconnecting would only repeat it.
    Fatal(String),
}

/// Follow the leader at `addr` into `standby` until `stop` (Ok) or a fatal
/// end (Err): a refusal, a gap that needs a re-bootstrap, a bad frame.
/// Link failures reconnect with backoff, resuming from `last_seq`.
pub(crate) fn follow(
    addr: &str,
    token: &str,
    standby: &Db,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut backoff = Duration::from_millis(100);
    while !stop.load(Ordering::Relaxed) {
        match follow_once(addr, token, standby, stop) {
            End::Stopped => return Ok(()),
            End::Fatal(e) => return Err(e),
            End::Retry(e) => {
                warn!("replication link to {addr}: {e} — reconnecting");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(LEADER_SILENCE);
            }
        }
    }
    Ok(())
}

fn follow_once(addr: &str, token: &str, standby: &Db, stop: &AtomicBool) -> End {
    let mut s = match TcpStream::connect(addr) {
        Ok(s) => s,
        Err(e) => return End::Retry(format!("connect: {e}")),
    };
    // short reads so `stop` is seen promptly; silence is judged separately
    if let Err(e) = s.set_read_timeout(Some(Duration::from_millis(100))) {
        return End::Retry(e.to_string());
    }
    let hello = serde_json::json!({ "proto": PROTO, "token": token, "after": standby.last_seq() });
    if let Err(e) = s.write_all(format!("{hello}\n").as_bytes()) {
        return End::Retry(format!("handshake: {e}"));
    }
    let mut reported = 0;
    let mut r = Reader {
        s: &s,
        stop,
        heard: Instant::now(),
    };
    let mut line = Vec::new();
    loop {
        let mut b = [0u8; 1];
        if let Err(end) = r.fill(&mut b) {
            return end;
        }
        if b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
        if line.len() > MAX_LINE {
            return End::Fatal("leader handshake line too long".into());
        }
    }
    let reply: serde_json::Value = match serde_json::from_slice(&line) {
        Ok(v) => v,
        Err(e) => return End::Fatal(format!("leader handshake unreadable: {e}")),
    };
    if let Some(e) = reply["error"].as_str() {
        return End::Fatal(e.to_string());
    }
    loop {
        let mut tag = [0u8; 1];
        if let Err(end) = r.fill(&mut tag) {
            return end;
        }
        match tag[0] {
            TAG_FRAME => {
                let mut frame = vec![0u8; FRAME_HEADER_LEN];
                if let Err(end) = r.fill(&mut frame) {
                    return end;
                }
                // payload_len sits after magic 4 + version 2 + type 1 + seq 8
                let len = u32::from_le_bytes([frame[15], frame[16], frame[17], frame[18]]) as usize;
                if len > MAX_FRAME_PAYLOAD {
                    return End::Fatal(format!("frame payload {len} bytes exceeds the bound"));
                }
                frame.resize(FRAME_HEADER_LEN + len + 8, 0);
                if let Err(end) = r.fill(&mut frame[FRAME_HEADER_LEN..]) {
                    return end;
                }
                let f = match decode_frame(&frame) {
                    Ok((f, _)) => f,
                    Err(e) => return End::Fatal(format!("bad frame from the leader: {e}")),
                };
                if let Err(e) = standby.apply_replicated(f.seq, &f.ops) {
                    return End::Fatal(format!("apply frame {}: {e}", f.seq));
                }
                if let Err(e) = (&s).write_all(&f.seq.to_le_bytes()) {
                    return End::Retry(format!("ack: {e}"));
                }
            }
            TAG_HEARTBEAT => {
                let mut b = [0u8; 8];
                if let Err(end) = r.fill(&mut b) {
                    return end;
                }
                // the operator's convergence signal: once per catch-up
                let head = u64::from_le_bytes(b);
                if head == standby.last_seq() && head != reported {
                    eprintln!("standby caught up at seq {head}");
                    reported = head;
                }
            }
            TAG_GAP => {
                let mut b = [0u8; 4];
                if let Err(end) = r.fill(&mut b) {
                    return end;
                }
                let len = (u32::from_le_bytes(b) as usize).min(MAX_REASON);
                let mut reason = vec![0u8; len];
                if let Err(end) = r.fill(&mut reason) {
                    return end;
                }
                return End::Fatal(format!(
                    "the leader no longer holds this standby's position ({}) — re-bootstrap it from a snapshot",
                    String::from_utf8_lossy(&reason)
                ));
            }
            t => return End::Fatal(format!("unknown replication message tag {t}")),
        }
    }
}

/// Exact-length reads over a short-timeout socket: keeps partial progress
/// across timeouts, honors `stop`, and judges leader silence by wall time.
struct Reader<'a> {
    s: &'a TcpStream,
    stop: &'a AtomicBool,
    heard: Instant,
}

impl Reader<'_> {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), End> {
        let mut n = 0;
        while n < buf.len() {
            if self.stop.load(Ordering::Relaxed) {
                return Err(End::Stopped);
            }
            match (&*self.s).read(&mut buf[n..]) {
                Ok(0) => return Err(End::Retry("leader closed the link".into())),
                Ok(k) => {
                    n += k;
                    self.heard = Instant::now();
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if self.heard.elapsed() > LEADER_SILENCE {
                        return Err(End::Retry("leader silent".into()));
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(End::Retry(e.to_string())),
            }
        }
        Ok(())
    }
}

/// One `\n`-terminated line, bounded (the leader's view of the handshake).
fn read_line(mut s: &TcpStream) -> std::io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        s.read_exact(&mut b)?;
        if b[0] == b'\n' {
            return Ok(line);
        }
        line.push(b[0]);
        if line.len() > MAX_LINE {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "handshake line too long",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TcpAuthTable;
    use aikoql_storage_v2::db::{Config, Db};
    use aikoql_storage_v2::engine::StorageAdminApi;
    use aikoql_storage_v2::AikoqlStorageEngineV2;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "repl-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    struct Leader {
        engine: Arc<AikoqlStorageEngineV2>,
        addr: String,
        dir: PathBuf,
        stop: Arc<AtomicBool>,
    }

    fn leader(tag: &str) -> Leader {
        let dir = tmp(tag);
        let mut cfg = Config::new(dir.clone());
        cfg.retain_log = true;
        cfg.memtable_bytes = 1024; // the stream crosses flush truncations
        let engine = Arc::new(AikoqlStorageEngineV2::open_with_config(cfg).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let auth = Arc::new(
            TcpAuthTable::parse(&["leader-tok::admin".into(), "user-tok::viewer".into()]).unwrap(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let source: Arc<dyn StorageAdminApi> = engine.clone();
        let s = stop.clone();
        std::thread::spawn(move || serve_replication(listener, source, auth, s));
        Leader {
            engine,
            addr,
            dir,
            stop,
        }
    }

    fn write(l: &Leader, from: u32, to: u32) {
        for i in from..to {
            l.engine
                .db()
                .put(format!("k{i:04}").as_bytes(), &[i as u8; 64])
                .unwrap();
        }
    }

    fn wait_for(cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cond() {
            assert!(Instant::now() < deadline, "replication did not converge");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn standby_streams_catches_up_and_resumes_after_disconnect() {
        let l = leader("stream");
        write(&l, 0, 100);
        let standby = Arc::new(Db::open(Config::new(tmp("stream-standby"))).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let run = |stop: Arc<AtomicBool>| {
            let (addr, sb) = (l.addr.clone(), standby.clone());
            std::thread::spawn(move || follow(&addr, "leader-tok", &sb, &stop))
        };
        let h = run(stop.clone());
        write(&l, 100, 200); // live writes while streaming
        wait_for(|| standby.last_seq() == l.engine.db().last_seq());
        assert_eq!(standby.scan(b"").unwrap(), l.engine.db().scan(b"").unwrap());

        // the leader released what the standby acked
        let rlog = std::fs::read_dir(l.dir.join("rlog")).unwrap().count();
        assert!(
            rlog <= 1,
            "acked archive files were not released ({rlog} left)"
        );

        stop.store(true, Ordering::SeqCst);
        h.join().unwrap().unwrap();
        write(&l, 200, 300); // written while the standby is away
        let stop2 = Arc::new(AtomicBool::new(false));
        let h2 = run(stop2.clone());
        wait_for(|| standby.last_seq() == l.engine.db().last_seq());
        assert_eq!(standby.scan(b"").unwrap(), l.engine.db().scan(b"").unwrap());
        stop2.store(true, Ordering::SeqCst);
        h2.join().unwrap().unwrap();
        l.stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn non_admin_and_unknown_tokens_are_refused() {
        let l = leader("auth");
        write(&l, 0, 10);
        let standby = Db::open(Config::new(tmp("auth-standby"))).unwrap();
        let stop = AtomicBool::new(false);
        for tok in ["user-tok", "nope"] {
            let err = follow(&l.addr, tok, &standby, &stop).unwrap_err();
            assert!(err.contains("refused"), "{tok}: {err}");
        }
        assert_eq!(
            standby.last_seq(),
            0,
            "nothing streamed to a refused standby"
        );
        l.stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn a_standby_behind_the_retained_log_is_told_to_rebootstrap() {
        let mut cfg = Config::new(tmp("gap"));
        cfg.memtable_bytes = 1024; // no retain_log: flushed history is gone
        let engine = Arc::new(AikoqlStorageEngineV2::open_with_config(cfg).unwrap());
        for i in 0..100u32 {
            engine
                .db()
                .put(format!("k{i}").as_bytes(), &[0u8; 64])
                .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let auth = Arc::new(TcpAuthTable::parse(&["t::admin".into()]).unwrap());
        let source: Arc<dyn StorageAdminApi> = engine.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        std::thread::spawn(move || serve_replication(listener, source, auth, s));

        let standby = Db::open(Config::new(tmp("gap-standby"))).unwrap();
        let err = follow(&addr, "t", &standby, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("re-bootstrap"), "{err}");
        stop.store(true, Ordering::SeqCst);
    }
}
