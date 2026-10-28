//! MRFC-0080 R3a — the replication stream: a leader serves its storage
//! log over TCP to standbys that apply it with `apply_replicated`.
//!
//! Wire (`aikoql-repl/1`): the standby sends one JSON line
//! `{"proto","token","after"}`; the leader answers `{"ok":true}` or
//! `{"error":..}` and closes. Then leader → standby is a stream of tagged
//! messages — `1` + a raw WAL frame (self-delimiting, checksummed), `2` +
//! u64 LE leader seq (heartbeat), `3` + u32 LE len + reason (the log no
//! longer holds what the standby needs: re-bootstrap), `4` + an index
//! checkpoint (R3b: u64 water, u32 file count, per file u32 path len +
//! relative path + u64 size + bytes). Standby → leader is
//! a stream of u64 LE acked seqs; the leader releases its archive to the
//! slowest connected standby's ack.
//!
//! R4 — terms: each node keeps `{dir}/REPL-TERM` ("<term> <role>"). The
//! hello carries the standby's term, the `ok` the leader's. A standby
//! never takes frames from an older term; a leader that meets a newer one
//! was deposed — it persists `fenced` and refuses every client write.
//!
//! R4b — `sync_gate`: a client write returns only once a standby has
//! acked (applied + fsynced) its seq, or fails after the timeout.
//!
//! R4c — term history (`{dir}/REPL-HISTORY`, Postgres-timeline style:
//! one "<term> <first seq>" line per promotion). Hello and `ok` carry it;
//! a standby whose last frame the leader's history does not hold
//! (`diverged`), or whose position the log no longer holds, gets `5` + a
//! storage snapshot (checkpoint format) and re-bootstraps from it.

use crate::session::TcpAuthTable;
use aikoql_storage_v2::db::{CommitGate, Config, Db};
use aikoql_storage_v2::engine::StorageAdminApi;
use aikoql_storage_v2::wal::{decode_frame, encode_frame, FRAME_HEADER_LEN};
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tracing::warn;

const PROTO: &str = "aikoql-repl/2";
const TAG_FRAME: u8 = 1;
const TAG_HEARTBEAT: u8 = 2;
const TAG_CHECKPOINT: u8 = 4;
const TAG_SNAPSHOT: u8 = 5;
/// How often a caught-up sender looks for a newer index checkpoint.
const CHECKPOINT_POLL: Duration = Duration::from_secs(1);
const MAX_CHECKPOINT_FILES: usize = 1 << 20;
const HEARTBEAT: Duration = Duration::from_secs(1);
/// A standby that hears nothing — not even a heartbeat — this long
/// reconnects: the leader is gone or the link is dead.
const LEADER_SILENCE: Duration = Duration::from_secs(5);
/// Trust-boundary bounds on lengths read off the wire.
const MAX_FRAME_PAYLOAD: usize = 256 << 20;
const MAX_LINE: usize = 4096;
/// Frames per log read.
const BATCH: usize = 256;

/// R4 — the term file inside the database directory.
pub(crate) const TERM_FILE: &str = "REPL-TERM";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Leader,
    Standby,
    /// Deposed by a newer term: reads only, until it rejoins as a standby.
    Fenced,
}

/// The node's (term, role); no file is term 0, leader — a node that
/// never replicated.
pub(crate) fn load_term(dir: &Path) -> std::io::Result<(u64, Role)> {
    let s = match std::fs::read_to_string(dir.join(TERM_FILE)) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok((0, Role::Leader)),
        Err(e) => return Err(e),
    };
    let bad = || {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!("unreadable {TERM_FILE}: {s:?}"),
        )
    };
    let (term, role) = s.trim().split_once(' ').ok_or_else(bad)?;
    let role = match role {
        "leader" => Role::Leader,
        "standby" => Role::Standby,
        "fenced" => Role::Fenced,
        _ => return Err(bad()),
    };
    Ok((term.parse().map_err(|_| bad())?, role))
}

/// Durable on return (tmp → fsync → rename → dir fsync): a lost or torn
/// write must never unfence a node or forget a term.
pub(crate) fn store_term(dir: &Path, term: u64, role: Role) -> std::io::Result<()> {
    let role = match role {
        Role::Leader => "leader",
        Role::Standby => "standby",
        Role::Fenced => "fenced",
    };
    write_durable(dir, TERM_FILE, &format!("{term} {role}\n"))
}

fn write_durable(dir: &Path, name: &str, contents: &str) -> std::io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, dir.join(name))?;
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// R4c — the term history file inside the database directory.
pub(crate) const HISTORY_FILE: &str = "REPL-HISTORY";

/// (term, first seq written under it), oldest first. No file is one
/// timeline: term 0 from seq 1.
pub(crate) type History = Vec<(u64, u64)>;

pub(crate) fn load_history(dir: &Path) -> std::io::Result<History> {
    let s = match std::fs::read_to_string(dir.join(HISTORY_FILE)) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(vec![(0, 1)]),
        Err(e) => return Err(e),
    };
    s.lines()
        .map(|l| {
            l.split_once(' ')
                .and_then(|(t, q)| Some((t.parse().ok()?, q.parse().ok()?)))
                .ok_or_else(|| {
                    std::io::Error::new(
                        ErrorKind::InvalidData,
                        format!("unreadable {HISTORY_FILE}: {l:?}"),
                    )
                })
        })
        .collect()
}

fn store_history(dir: &Path, history: &[(u64, u64)]) -> std::io::Result<()> {
    let body: String = history.iter().map(|(t, q)| format!("{t} {q}\n")).collect();
    write_durable(dir, HISTORY_FILE, &body)
}

/// R4c — is a standby whose history is `theirs` and whose last frame is
/// `after` off our timeline? Its last frame's term must be one we had,
/// and we must not have moved past that term before `after`.
pub(crate) fn diverged(ours: &[(u64, u64)], theirs: &[(u64, u64)], after: u64) -> bool {
    let term = theirs
        .iter()
        .rev()
        .find(|(_, first)| *first <= after)
        .map_or(0, |(t, _)| *t);
    match ours.iter().position(|(t, _)| *t == term) {
        None => true,
        Some(i) => ours.get(i + 1).is_some_and(|(_, next)| after >= *next),
    }
}

/// `serve` on a standby is its promotion: it takes the next term, which
/// starts right after the last frame it holds (recorded before the kernel
/// opens and writes under the new term).
pub(crate) fn claim_leadership(dir: &Path) -> std::io::Result<(u64, Role)> {
    match load_term(dir)? {
        (term, Role::Standby) => {
            let next = term + 1;
            let last = Db::open(Config::new(dir.to_path_buf()))
                .map_err(|e| std::io::Error::other(e.to_string()))?
                .last_seq();
            let mut history = load_history(dir)?;
            history.retain(|(t, _)| *t < next); // a crash-retried claim
            history.push((next, last + 1));
            store_history(dir, &history)?;
            store_term(dir, next, Role::Leader)?;
            Ok((next, Role::Leader))
        }
        other => Ok(other),
    }
}

/// R4b — the highest seq any standby has acked.
#[derive(Default)]
pub(crate) struct Acked {
    seq: Mutex<u64>,
    cv: Condvar,
}

impl Acked {
    fn record(&self, seq: u64) {
        // justified: Mutex poison is unrecoverable
        let mut acked = self.seq.lock().unwrap();
        if seq > *acked {
            *acked = seq;
            self.cv.notify_all();
        }
    }
}

/// R4b — the commit gate: wait until a standby acked `seq`, at most
/// `timeout`.
pub(crate) fn sync_gate(acked: Arc<Acked>, timeout: Duration) -> CommitGate {
    Box::new(move |seq| {
        // justified: Mutex poison is unrecoverable
        let guard = acked.seq.lock().unwrap();
        let (guard, _) = acked
            .cv
            .wait_timeout_while(guard, timeout, |a| *a < seq)
            .unwrap();
        if *guard >= seq {
            Ok(())
        } else {
            Err(format!(
                "no standby acked seq {seq} within {} ms",
                timeout.as_millis()
            ))
        }
    })
}

/// MRFC-0080 R3b — the leader's index checkpoint directory, shipped to
/// standbys whenever its water moves. `lock` is held by every checkpoint
/// writer: the sender never reads a directory mid-swap.
#[derive(Clone)]
pub(crate) struct ShippedCheckpoint {
    pub(crate) dir: PathBuf,
    pub(crate) lock: Arc<Mutex<()>>,
}

/// Serve the replication stream until `stop`. Every standby must present
/// a `--tcp-token` carrying the `admin` role: the stream is the whole
/// database, across tenants.
pub(crate) fn serve_replication(
    listener: TcpListener,
    source: Arc<dyn StorageAdminApi>,
    auth: Arc<TcpAuthTable>,
    stop: Arc<AtomicBool>,
    checkpoint: Option<ShippedCheckpoint>,
    dir: PathBuf,
    acked: Arc<Acked>,
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
                let (source, auth, acks, stop, ckpt, dir, acked) = (
                    source.clone(),
                    auth.clone(),
                    acks.clone(),
                    stop.clone(),
                    checkpoint.clone(),
                    dir.clone(),
                    acked.clone(),
                );
                std::thread::spawn(move || {
                    let served = serve_standby(
                        &stream,
                        id,
                        &*source,
                        &auth,
                        &acks,
                        &stop,
                        ckpt.as_ref(),
                        &dir,
                        acked,
                    );
                    if let Err(e) = served {
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
    checkpoint: Option<&ShippedCheckpoint>,
    dir: &Path,
    acked: Arc<Acked>,
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
    let their_term = hello["term"].as_u64().unwrap_or(0);
    let theirs: History = serde_json::from_value(hello["history"].clone()).unwrap_or_default();
    let (term, role) = load_term(dir)?;
    let refusal = if hello["proto"] != PROTO {
        Some(format!(
            "replication refused: protocol {} (this leader: {PROTO})",
            hello["proto"]
        ))
    } else if !admin {
        Some("replication refused: the token is unknown or lacks the admin role".to_string())
    } else if after.is_none() {
        Some("replication refused: no resume seq".to_string())
    } else if role == Role::Fenced {
        Some(format!(
            "replication refused: this node is fenced at term {term}"
        ))
    } else if their_term > term {
        // the standby has followed a newer leader: this one was deposed
        store_term(dir, their_term, Role::Fenced)?;
        source.fence(format!("deposed by replication term {their_term}"));
        warn!(
            term,
            their_term, "a standby carries a newer term: this node is fenced"
        );
        Some(format!(
            "replication refused: this leader's term {term} is older than the standby's {their_term} — it is now fenced"
        ))
    } else {
        None
    };
    if let Some(reason) = refusal {
        w.write_all(format!("{}\n", serde_json::json!({ "error": reason })).as_bytes())?;
        return Ok(());
    }
    let mut sent = after.unwrap_or(0);
    let ours = load_history(dir)?;
    // a standby ahead of our head holds frames we never wrote
    let rebootstrap = diverged(&ours, &theirs, sent) || sent > source.last_seq();
    let ok = serde_json::json!({ "ok": true, "term": term, "history": ours, "rebootstrap": rebootstrap });
    w.write_all(format!("{ok}\n").as_bytes())?;
    if rebootstrap {
        warn!(
            standby = id,
            after = sent,
            "the standby's tail diverged from this history: shipping a snapshot"
        );
        return send_snapshot(&mut w, source, dir, id);
    }
    // justified: Mutex poison is unrecoverable
    acks.lock().unwrap().insert(id, sent);

    // Acks arrive on their own thread; it ends when the socket closes.
    stream.set_read_timeout(None)?;
    let mut ack_reader = stream.try_clone()?;
    let ack_map = acks.clone();
    std::thread::spawn(move || {
        let mut b = [0u8; 8];
        while ack_reader.read_exact(&mut b).is_ok() {
            let seq = u64::from_le_bytes(b);
            // justified: Mutex poison is unrecoverable
            ack_map.lock().unwrap().insert(id, seq);
            acked.record(seq);
        }
    });

    let mut released = 0;
    let mut last_send = Instant::now();
    let mut shipped_water = None;
    let mut last_ckpt_look = Instant::now() - CHECKPOINT_POLL;
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
                    warn!(
                        standby = id,
                        "replication log gap ({e}): shipping a snapshot"
                    );
                    return send_snapshot(&mut w, source, dir, id);
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
            // R3b: only when caught up — every frame up to the head precedes
            // the checkpoint on the wire, so its water is already applied.
            if let Some(c) = checkpoint {
                if last_ckpt_look.elapsed() >= CHECKPOINT_POLL {
                    last_ckpt_look = Instant::now();
                    if let Some(water) = send_checkpoint(&mut w, c, shipped_water)? {
                        shipped_water = Some(water);
                        last_send = Instant::now();
                    }
                }
            }
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

/// Send the checkpoint when it is complete and its water differs from
/// `shipped`; returns the water sent.
fn send_checkpoint(
    w: &mut &TcpStream,
    c: &ShippedCheckpoint,
    shipped: Option<u64>,
) -> std::io::Result<Option<u64>> {
    // justified: Mutex poison is unrecoverable
    let _g = c.lock.lock().unwrap();
    if !c.dir.join("COMPLETE").exists() {
        return Ok(None);
    }
    let water: u64 = match std::fs::read_to_string(c.dir.join("water.txt"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
    {
        Some(w) if Some(w) != shipped => w,
        _ => return Ok(None),
    };
    send_dir(w, TAG_CHECKPOINT, water, &c.dir)?;
    Ok(Some(water))
}

/// R4c — snapshot the storage into `{dir}.ship-{id}` and send it as a
/// tagged directory; the standby re-bootstraps from it.
fn send_snapshot(
    w: &mut &TcpStream,
    source: &dyn StorageAdminApi,
    dir: &Path,
    id: u64,
) -> std::io::Result<()> {
    let tmp = PathBuf::from(format!("{}.ship-{id}", dir.display()));
    let _ = std::fs::remove_dir_all(&tmp);
    let sent = source
        .snapshot_to(&tmp)
        .map_err(|e| std::io::Error::other(e.to_string()))
        .and_then(|_| send_dir(w, TAG_SNAPSHOT, 0, &tmp));
    let _ = std::fs::remove_dir_all(&tmp);
    sent
}

/// `tag` + u64 `water` + u32 file count + per file (u32 path len, path,
/// u64 size, bytes).
fn send_dir(w: &mut &TcpStream, tag: u8, water: u64, dir: &Path) -> std::io::Result<()> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;
    let mut head = vec![tag];
    head.extend_from_slice(&water.to_le_bytes());
    head.extend_from_slice(&(files.len() as u32).to_le_bytes());
    w.write_all(&head)?;
    for (rel, path) in files {
        let mut f = std::fs::File::open(&path)?;
        let size = f.metadata()?.len();
        let mut entry = (rel.len() as u32).to_le_bytes().to_vec();
        entry.extend_from_slice(rel.as_bytes());
        entry.extend_from_slice(&size.to_le_bytes());
        w.write_all(&entry)?;
        // shortcut: ships the whole checkpoint each time its water moves;
        // send only changed files (by name + size) once indexes get large.
        let copied = std::io::copy(&mut (&mut f).take(size), w)?;
        if copied != size {
            return Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "file shrank while shipping",
            ));
        }
    }
    Ok(())
}

/// Every file under `dir`, as ('/'-joined path relative to `root`, path).
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let path = e?.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let parts: Vec<String> = rel
                .iter()
                .map(|c| c.to_string_lossy().into_owned())
                .collect();
            out.push((parts.join("/"), path));
        }
    }
    Ok(())
}

/// Receive a shipped directory into `{dir}.recv`, then swap it in as
/// `dir` — a crash mid-swap leaves none, never a torn one. `None`: read
/// and discard. `marker`: a file the directory must hold to be complete.
fn receive_dir(r: &mut Reader, dir: Option<&Path>, marker: Option<&str>) -> Result<u64, End> {
    let mut b8 = [0u8; 8];
    let mut b4 = [0u8; 4];
    r.fill(&mut b8)?;
    let water = u64::from_le_bytes(b8);
    r.fill(&mut b4)?;
    let count = u32::from_le_bytes(b4) as usize;
    if count > MAX_CHECKPOINT_FILES {
        return Err(End::Fatal(format!(
            "checkpoint of {count} files exceeds the bound"
        )));
    }
    let fatal = |e: std::io::Error| End::Fatal(format!("store checkpoint: {e}"));
    let tmp = dir.map(|d| PathBuf::from(format!("{}.recv", d.display())));
    if let Some(t) = &tmp {
        let _ = std::fs::remove_dir_all(t);
        std::fs::create_dir_all(t).map_err(fatal)?;
    }
    let mut chunk = vec![0u8; 64 << 10];
    for _ in 0..count {
        r.fill(&mut b4)?;
        let len = u32::from_le_bytes(b4) as usize;
        if len > MAX_LINE {
            return Err(End::Fatal("checkpoint path too long".into()));
        }
        let mut rel = vec![0u8; len];
        r.fill(&mut rel)?;
        let rel = String::from_utf8(rel)
            .map_err(|_| End::Fatal("checkpoint path is not UTF-8".into()))?;
        // trust boundary: plain relative components only — never `..`, a
        // root or a drive prefix that would write outside the directory
        let rel_path = Path::new(&rel);
        if rel.is_empty()
            || !rel_path
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(End::Fatal(format!(
                "checkpoint path {rel:?} is not a plain relative path"
            )));
        }
        r.fill(&mut b8)?;
        let mut left = u64::from_le_bytes(b8);
        let mut out = match &tmp {
            Some(t) => {
                let p = t.join(rel_path);
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(fatal)?;
                }
                Some(std::fs::File::create(p).map_err(fatal)?)
            }
            None => None,
        };
        while left > 0 {
            let n = left.min(chunk.len() as u64) as usize;
            r.fill(&mut chunk[..n])?;
            if let Some(f) = out.as_mut() {
                f.write_all(&chunk[..n]).map_err(fatal)?;
            }
            left -= n as u64;
        }
        if let Some(f) = out {
            f.sync_all().map_err(fatal)?;
        }
    }
    if let (Some(dir), Some(tmp)) = (dir, tmp) {
        if let Some(m) = marker.filter(|m| !tmp.join(m).exists()) {
            return Err(End::Fatal(format!(
                "{} arrived without its {m} marker",
                dir.display()
            )));
        }
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(fatal)?;
        }
        std::fs::rename(&tmp, dir).map_err(fatal)?;
    }
    Ok(water)
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
    /// R4c — a snapshot landed in `{dir}.rejoin`.
    Rebootstrap { term: u64, history: History },
}

/// How `follow` ended without an error.
#[derive(Debug)]
pub(crate) enum Followed {
    /// `stop` was raised.
    Stopped,
    /// R4c — the leader shipped a snapshot (our tail diverged, or the log
    /// no longer holds our position): close the db, `rebootstrap`, follow
    /// again.
    Rebootstrap { term: u64, history: History },
}

fn sibling(dir: &Path, ext: &str) -> PathBuf {
    PathBuf::from(format!("{}.{ext}", dir.display()))
}

/// R4c — replace the standby at `dir` (closed) with the snapshot in
/// `{dir}.rejoin`, carrying the leader's term and history. A crash
/// between the two renames leaves no `dir`: the next `follow` starts
/// empty and is re-bootstrapped again — never a mixed history.
pub(crate) fn rebootstrap(dir: &Path, term: u64, history: &[(u64, u64)]) -> Result<(), String> {
    let (snap, new, old) = (
        sibling(dir, "rejoin"),
        sibling(dir, "new"),
        sibling(dir, "old"),
    );
    let _ = std::fs::remove_dir_all(&new);
    drop(
        aikoql_storage_v2::snapshot::restore_from(&snap, new.clone())
            .map_err(|e| format!("restore the leader's snapshot: {e}"))?,
    );
    let io = |e: std::io::Error| format!("re-bootstrap {}: {e}", dir.display());
    store_history(&new, history).map_err(io)?;
    store_term(&new, term, Role::Standby).map_err(io)?;
    let _ = std::fs::remove_dir_all(&old);
    if dir.exists() {
        std::fs::rename(dir, &old).map_err(io)?;
    }
    std::fs::rename(&new, dir).map_err(io)?;
    let _ = std::fs::remove_dir_all(&old);
    let _ = std::fs::remove_dir_all(&snap);
    // the index checkpoint described the replaced history
    let _ = std::fs::remove_dir_all(sibling(dir, "ckpt"));
    Ok(())
}

/// Follow the leader at `addr` into `standby` until `stop` (Ok) or a fatal
/// end (Err): a refusal, a gap that needs a re-bootstrap, a bad frame.
/// Link failures reconnect with backoff, resuming from `last_seq`.
/// `checkpoint`: where shipped index checkpoints land (R3b) — the
/// standby's `{dir}.ckpt`, which `serve` resumes from at promotion.
pub(crate) fn follow(
    addr: &str,
    token: &str,
    standby: &Db,
    dir: &Path,
    checkpoint: Option<&Path>,
    stop: &AtomicBool,
) -> Result<Followed, String> {
    let mut backoff = Duration::from_millis(100);
    while !stop.load(Ordering::Relaxed) {
        match follow_once(addr, token, standby, dir, checkpoint, stop) {
            End::Stopped => return Ok(Followed::Stopped),
            End::Rebootstrap { term, history } => {
                return Ok(Followed::Rebootstrap { term, history })
            }
            End::Fatal(e) => return Err(e),
            End::Retry(e) => {
                warn!("replication link to {addr}: {e} — reconnecting");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(LEADER_SILENCE);
            }
        }
    }
    Ok(Followed::Stopped)
}

fn follow_once(
    addr: &str,
    token: &str,
    standby: &Db,
    dir: &Path,
    checkpoint: Option<&Path>,
    stop: &AtomicBool,
) -> End {
    let (own_term, role) = match load_term(dir) {
        Ok(t) => t,
        Err(e) => return End::Fatal(format!("read {TERM_FILE}: {e}")),
    };
    let mut s = match TcpStream::connect(addr) {
        Ok(s) => s,
        Err(e) => return End::Retry(format!("connect: {e}")),
    };
    // short reads so `stop` is seen promptly; silence is judged separately
    if let Err(e) = s.set_read_timeout(Some(Duration::from_millis(100))) {
        return End::Retry(e.to_string());
    }
    let hello = serde_json::json!({
        "proto": PROTO,
        "token": token,
        "after": standby.last_seq(),
        "term": own_term,
        "history": match load_history(dir) {
            Ok(h) => h,
            Err(e) => return End::Fatal(format!("read {HISTORY_FILE}: {e}")),
        },
    });
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
    let term = reply["term"].as_u64().unwrap_or(0);
    if term < own_term {
        return End::Fatal(format!(
            "refused a stale leader: its term {term} is older than this node's {own_term}"
        ));
    }
    let history: History = serde_json::from_value(reply["history"].clone()).unwrap_or_default();
    // a re-bootstrap writes term + history only with the new data: until
    // then this node keeps the history its (diverged) frames belong to
    if reply["rebootstrap"] != true {
        if let Err(e) = store_history(dir, &history) {
            return End::Fatal(format!("store {HISTORY_FILE}: {e}"));
        }
        if (term, role) != (own_term, Role::Standby) {
            if let Err(e) = store_term(dir, term, Role::Standby) {
                return End::Fatal(format!("store {TERM_FILE}: {e}"));
            }
        }
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
            TAG_SNAPSHOT => {
                return match receive_dir(&mut r, Some(&sibling(dir, "rejoin")), None) {
                    Ok(_) => End::Rebootstrap { term, history },
                    Err(end) => end,
                }
            }
            TAG_CHECKPOINT => match receive_dir(&mut r, checkpoint, Some("COMPLETE")) {
                Ok(water) => {
                    if checkpoint.is_some() {
                        eprintln!("standby index checkpoint at water {water}");
                    }
                }
                Err(end) => return end,
            },
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
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// A db at `dir`, once whoever held it (a stopped leader's threads)
    /// has let go.
    fn open_when_free(dir: &Path) -> Db {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match Db::open(Config::new(dir.to_path_buf())) {
                Ok(db) => return db,
                Err(e) if Instant::now() < deadline => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("open {}: {e}", dir.display()),
            }
        }
    }

    /// The `follow` subcommand's loop: re-bootstrap when the leader says
    /// so, then follow until the standby holds `head`. True when a
    /// re-bootstrap happened.
    fn follow_to(addr: &str, token: &str, sdir: &Path, head: u64) -> bool {
        let mut rebootstrapped = false;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let db = Arc::new(open_when_free(sdir));
            let stop = Arc::new(AtomicBool::new(false));
            let h = {
                let (addr, token, db, d, st) = (
                    addr.to_string(),
                    token.to_string(),
                    db.clone(),
                    sdir.to_path_buf(),
                    stop.clone(),
                );
                std::thread::spawn(move || follow(&addr, &token, &db, &d, None, &st))
            };
            while !h.is_finished() && db.last_seq() != head {
                assert!(
                    Instant::now() < deadline,
                    "the standby never reached {head}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            stop.store(true, Ordering::SeqCst);
            match h.join().unwrap().unwrap() {
                Followed::Stopped => return rebootstrapped,
                Followed::Rebootstrap { term, history } => {
                    drop(db);
                    rebootstrap(sdir, term, &history).unwrap();
                    rebootstrapped = true;
                }
            }
        }
    }

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
        acked: Arc<Acked>,
    }

    fn leader(tag: &str) -> Leader {
        leader_at(tmp(tag))
    }

    fn leader_at(dir: PathBuf) -> Leader {
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
        let d = dir.clone();
        let acked = Arc::new(Acked::default());
        let a = acked.clone();
        std::thread::spawn(move || serve_replication(listener, source, auth, s, None, d, a));
        Leader {
            engine,
            addr,
            dir,
            stop,
            acked,
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
        let sdir = tmp("stream-standby");
        let standby = Arc::new(Db::open(Config::new(sdir.clone())).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let run = |stop: Arc<AtomicBool>| {
            let (addr, sb, d) = (l.addr.clone(), standby.clone(), sdir.clone());
            std::thread::spawn(move || follow(&addr, "leader-tok", &sb, &d, None, &stop))
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
        let sdir = tmp("auth-standby");
        let standby = Db::open(Config::new(sdir.clone())).unwrap();
        let stop = AtomicBool::new(false);
        for tok in ["user-tok", "nope"] {
            let err = follow(&l.addr, tok, &standby, &sdir, None, &stop).unwrap_err();
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
    fn a_standby_behind_the_retained_log_is_rebootstrapped_automatically() {
        let gap_dir = tmp("gap");
        let mut cfg = Config::new(gap_dir.clone());
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
        std::thread::spawn(move || {
            serve_replication(
                listener,
                source,
                auth,
                s,
                None,
                gap_dir,
                Arc::new(Acked::default()),
            )
        });

        // R4c: the leader ships a snapshot; the standby swaps it in and
        // follows on from it — no operator step
        let sdir = tmp("gap-standby");
        assert!(follow_to(&addr, "t", &sdir, engine.db().last_seq()));
        let standby = Db::open(Config::new(sdir)).unwrap();
        assert_eq!(standby.scan(b"").unwrap(), engine.db().scan(b"").unwrap());
        stop.store(true, Ordering::SeqCst);
    }

    /// R4 — a standby that has followed a term-2 leader refuses a term-0
    /// one; the stale leader learns the newer term and fences itself.
    #[test]
    fn a_standby_refuses_an_older_term_leader_which_then_fences_itself() {
        let l = leader("fence");
        write(&l, 0, 10);
        let sdir = tmp("fence-standby");
        let standby = Db::open(Config::new(sdir.clone())).unwrap();
        store_term(&sdir, 2, Role::Standby).unwrap();
        store_leader(&sdir, "10.0.0.9:7070").unwrap(); // its term-2 leader
        let err = follow(
            &l.addr,
            "leader-tok",
            &standby,
            &sdir,
            None,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(err.contains("term"), "{err}");
        assert_eq!(
            standby.last_seq(),
            0,
            "a frame from a stale leader was applied"
        );
        assert_eq!(load_term(&l.dir).unwrap(), (2, Role::Fenced));
        assert_eq!(load_leader(&l.dir).as_deref(), Some("10.0.0.9:7070"));
        let e = l.engine.db().put(b"late", b"x").unwrap_err();
        assert!(e.to_string().contains("not leader"), "{e}");
        assert!(e.to_string().contains("leader=10.0.0.9:7070"), "{e}");
        // a fenced node serves no stream either
        let fresh = tmp("fence-fresh");
        let db = Db::open(Config::new(fresh.clone())).unwrap();
        let err = follow(
            &l.addr,
            "leader-tok",
            &db,
            &fresh,
            None,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(err.contains("fenced"), "{err}");
        l.stop.store(true, Ordering::SeqCst);
    }

    /// R4 — a standby adopts (and persists) a newer leader's term.
    #[test]
    fn a_standby_adopts_a_newer_leader_term() {
        let l = leader("adopt");
        store_term(&l.dir, 3, Role::Leader).unwrap();
        store_leader(&l.dir, "10.0.0.1:7070").unwrap(); // its --advertise
        write(&l, 0, 10);
        let sdir = tmp("adopt-standby");
        let standby = Arc::new(Db::open(Config::new(sdir.clone())).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let h = {
            let (addr, sb, d, st) = (l.addr.clone(), standby.clone(), sdir.clone(), stop.clone());
            std::thread::spawn(move || follow(&addr, "leader-tok", &sb, &d, None, &st))
        };
        wait_for(|| standby.last_seq() == l.engine.db().last_seq());
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap().unwrap();
        assert_eq!(load_term(&sdir).unwrap(), (3, Role::Standby));
        assert_eq!(load_leader(&sdir).as_deref(), Some("10.0.0.1:7070"));
        assert_eq!(load_term(&tmp("no-term")).unwrap(), (0, Role::Leader));
        l.stop.store(true, Ordering::SeqCst);
    }

    /// R4b — under the sync gate a client write returns only once a
    /// standby holds it: a failover right after the ack loses nothing.
    #[test]
    fn a_sync_write_returns_only_after_a_standby_holds_it() {
        let l = leader("sync");
        l.engine
            .db()
            .set_commit_gate(sync_gate(l.acked.clone(), Duration::from_secs(10)));
        let sdir = tmp("sync-standby");
        let standby = Arc::new(Db::open(Config::new(sdir.clone())).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let h = {
            let (addr, sb, d, st) = (l.addr.clone(), standby.clone(), sdir.clone(), stop.clone());
            std::thread::spawn(move || follow(&addr, "leader-tok", &sb, &d, None, &st))
        };
        for i in 0..50u32 {
            let seq = l.engine.db().put(format!("s{i}").as_bytes(), b"v").unwrap();
            assert!(
                standby.last_seq() >= seq,
                "write {seq} was acked before any standby held it"
            );
        }
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap().unwrap();
        l.stop.store(true, Ordering::SeqCst);
    }

    /// R4b — no standby acks within the timeout: the write fails rather
    /// than ack a write a failover would lose.
    #[test]
    fn a_sync_write_with_no_standby_fails_after_the_timeout() {
        let l = leader("sync-alone");
        l.engine
            .db()
            .set_commit_gate(sync_gate(l.acked.clone(), Duration::from_millis(200)));
        let e = l.engine.db().put(b"k", b"v").unwrap_err();
        assert!(e.to_string().contains("not replicated"), "{e}");
        l.stop.store(true, Ordering::SeqCst);
    }

    /// R4c — a frame is consistent with our history iff the term it was
    /// written under is one we had and we had not yet moved past it.
    #[test]
    fn divergence_is_judged_by_term_history() {
        let ours = [(0, 1), (1, 51)];
        assert!(!diverged(&ours, &[(0, 1)], 50), "a prefix of term 0");
        assert!(diverged(&ours, &[(0, 1)], 60), "term-0 frames past 50");
        assert!(!diverged(&ours, &[(0, 1), (1, 51)], 60), "our own timeline");
        assert!(
            diverged(&ours, &[(0, 1), (2, 40)], 45),
            "a term we never had"
        );
        assert!(!diverged(&[(0, 1)], &[], 0), "an empty standby");
    }

    /// The R4c drill: A leads, B follows, A takes writes B never sees and
    /// dies; B is promoted and writes; A follows B — its diverged tail is
    /// detected and replaced by B's history, no operator step.
    fn old_leader_rejoins(tag: &str, tail: u32) -> (bool, Db, Leader) {
        let a = leader(&format!("{tag}-a"));
        write(&a, 0, 50);
        store_leader(&a.dir, "10.0.0.1:7070").unwrap();
        let bdir = tmp(&format!("{tag}-b"));
        assert!(!follow_to(
            &a.addr,
            "leader-tok",
            &bdir,
            a.engine.db().last_seq()
        ));
        for i in 0..tail {
            a.engine
                .db()
                .put(format!("lost{i}").as_bytes(), b"x")
                .unwrap();
        }
        a.stop.store(true, Ordering::SeqCst);
        let adir = a.dir.clone();
        drop(a);
        assert_eq!(load_leader(&bdir).as_deref(), Some("10.0.0.1:7070"));
        assert_eq!(claim_leadership(&bdir).unwrap(), (1, Role::Leader));
        assert_eq!(
            load_leader(&bdir),
            None,
            "a promoted node kept its old leader's address"
        );
        let b = leader_at(bdir);
        for i in 0..5u32 {
            b.engine
                .db()
                .put(format!("new{i}").as_bytes(), b"y")
                .unwrap();
        }
        let rebootstrapped = follow_to(&b.addr, "leader-tok", &adir, b.engine.db().last_seq());
        assert_eq!(load_term(&adir).unwrap(), (1, Role::Standby));
        (rebootstrapped, open_when_free(&adir), b)
    }

    #[test]
    fn a_diverged_old_leader_is_rebootstrapped_and_rejoins() {
        let (rebootstrapped, a, b) = old_leader_rejoins("div", 10);
        assert!(rebootstrapped, "the diverged tail was not detected");
        assert_eq!(a.scan(b"").unwrap(), b.engine.db().scan(b"").unwrap());
        assert_eq!(a.get(b"lost0").unwrap(), None, "the diverged tail survived");
        b.stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn a_caught_up_old_leader_rejoins_without_a_snapshot() {
        let (rebootstrapped, a, b) = old_leader_rejoins("nodiv", 0);
        assert!(!rebootstrapped, "a consistent standby was re-bootstrapped");
        assert_eq!(a.scan(b"").unwrap(), b.engine.db().scan(b"").unwrap());
        b.stop.store(true, Ordering::SeqCst);
    }
}
