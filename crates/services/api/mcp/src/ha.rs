//! MRFC-0080 R5a — automatic failover: one lease, one term.
//!
//! The lease record `{holder, term, repl, client, seq, beat}` lives in a
//! `LeaseStore` whose only write is a compare-and-swap on a version. A
//! node writes only while it holds the lease, and the lease term is the
//! replication term (R4a): every node fences on it.
//!
//! Timing never compares clocks across nodes:
//! - a standby takes over only after it has seen the record unchanged
//!   for a full TTL on its own monotonic clock;
//! - a leader fences itself once 2/3 TTL have passed since its last
//!   successful renewal *started*.
//!
//! A renewal lands after it starts and is seen after it lands, so the old
//! leader stops writing at least TTL/3 before anyone may take over — the
//! margin that covers clock-rate drift and in-flight writes.

use crate::replication::{self, Role};
use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::engine::StorageAdminApi;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Exit code of a self-fenced leader: its supervisor restarts it, and it
/// comes back as a standby of whoever holds the lease.
pub(crate) const FENCED_EXIT: i32 = 75;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Lease {
    pub(crate) holder: String,
    pub(crate) term: u64,
    /// The holder's replication address — what standbys follow.
    pub(crate) repl: String,
    /// The holder's client address (`--advertise`) — the NOT_LEADER hint.
    pub(crate) client: Option<String>,
    /// The holder's head seq at its last renewal: a standby behind it may
    /// not take over.
    pub(crate) seq: u64,
    /// Bumped by every renewal, so every renewal changes the version.
    pub(crate) beat: u64,
}

#[derive(Debug)]
pub(crate) enum LeaseError {
    /// The version moved: someone else wrote first.
    Conflict,
    Other(String),
}

pub(crate) trait LeaseStore: Send + Sync {
    /// The record and its version; `None` when there is none.
    fn read(&self) -> Result<Option<(Lease, String)>, String>;
    /// Write iff the current version is `expected` (`None`: iff there is
    /// no record); returns the new version.
    fn write(&self, lease: &Lease, expected: Option<&str>) -> Result<String, LeaseError>;
}

/// A lease in a local file — one host, or tests. The version is the
/// content hash; the CAS runs under an OS lock on `{path}.lock`.
pub(crate) struct FileLease {
    path: PathBuf,
}

impl FileLease {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn lock(&self) -> std::io::Result<std::fs::File> {
        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(format!("{}.lock", self.path.display()))?;
        f.lock()?;
        Ok(f)
    }

    fn bytes(&self) -> std::io::Result<Option<Vec<u8>>> {
        match std::fs::read(&self.path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn version(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl LeaseStore for FileLease {
    fn read(&self) -> Result<Option<(Lease, String)>, String> {
        let _g = self.lock().map_err(|e| format!("lock the lease: {e}"))?;
        let Some(b) = self.bytes().map_err(|e| format!("read the lease: {e}"))? else {
            return Ok(None);
        };
        let lease = serde_json::from_slice(&b).map_err(|e| format!("unreadable lease: {e}"))?;
        Ok(Some((lease, version(&b))))
    }

    fn write(&self, lease: &Lease, expected: Option<&str>) -> Result<String, LeaseError> {
        let other = |e: std::io::Error| LeaseError::Other(format!("lease file: {e}"));
        let _g = self.lock().map_err(other)?;
        let current = self.bytes().map_err(other)?.map(|b| version(&b));
        if current.as_deref() != expected {
            return Err(LeaseError::Conflict);
        }
        let body = serde_json::to_vec(lease).map_err(|e| LeaseError::Other(e.to_string()))?;
        let tmp = PathBuf::from(format!("{}.tmp", self.path.display()));
        let mut f = std::fs::File::create(&tmp).map_err(other)?;
        f.write_all(&body)
            .and_then(|_| f.sync_all())
            .map_err(other)?;
        std::fs::rename(&tmp, &self.path).map_err(other)?;
        Ok(version(&body))
    }
}

pub(crate) struct HaConfig {
    pub(crate) store: Arc<dyn LeaseStore>,
    pub(crate) node: String,
    pub(crate) ttl: Duration,
    /// This node's replication address, recorded in the lease it holds.
    pub(crate) repl: String,
    pub(crate) client: Option<String>,
    /// Presented to the leader when following.
    pub(crate) token: String,
    pub(crate) dir: PathBuf,
}

/// The renewer's late-bound fence target: the storage admin surface,
/// set once the kernel has opened.
pub(crate) type AdminSlot = Arc<OnceLock<Arc<dyn StorageAdminApi>>>;

/// The standby phase: follow whoever holds the lease, until this node
/// holds it. Then the directory is claimed at the lease's term, the
/// renewer starts (before the kernel opens — a slow open must not let
/// the lease lapse), and the caller serves as the leader.
pub(crate) fn lead(cfg: HaConfig) -> Result<AdminSlot, String> {
    let cfg = Arc::new(cfg);
    let mut seen: Option<(String, Instant)> = None;
    let mut following: Option<Follower> = None;
    let mut behind_warned = false;
    loop {
        let (term, role) = replication::load_term(&cfg.dir).map_err(|e| e.to_string())?;
        match cfg.store.read() {
            Err(e) => warn!("ha: read the lease: {e}"),
            Ok(None) => {
                stop(&mut following);
                if let Some(won) = try_win(&cfg, None, term + 1)? {
                    return Ok(won);
                }
            }
            Ok(Some((rec, ver))) => {
                if rec.holder == cfg.node && rec.term == term && role == Role::Leader {
                    // our own live lease from before a restart: keep it
                    stop(&mut following);
                    info!(term, "ha: resuming the lease this node holds");
                    return Ok(start_renewer(cfg.clone(), rec, ver, Instant::now()));
                }
                if seen.as_ref().map(|(v, _)| v) != Some(&ver) {
                    seen = Some((ver.clone(), Instant::now()));
                }
                let lead_addr = (rec.holder != cfg.node).then_some(&rec.repl);
                if following.as_ref().map(|f| &f.leader) != lead_addr {
                    stop(&mut following);
                    following = lead_addr.map(|a| Follower::start(&cfg, a.clone()));
                }
                let stale = seen.as_ref().is_some_and(|(_, at)| at.elapsed() >= cfg.ttl);
                if stale {
                    stop(&mut following);
                    let last = last_seq(&cfg)?;
                    if last >= rec.seq {
                        if let Some(won) = try_win(&cfg, Some(&ver), term.max(rec.term) + 1)? {
                            return Ok(won);
                        }
                    } else if !behind_warned {
                        behind_warned = true;
                        warn!(
                            last,
                            lease_seq = rec.seq,
                            "ha: the lease lapsed but this standby is behind its holder — not taking over"
                        );
                    }
                }
            }
        }
        std::thread::sleep(cfg.ttl / 5);
    }
}

/// One CAS attempt for `term`; on a win the directory is claimed and the
/// renewer runs.
fn try_win(
    cfg: &Arc<HaConfig>,
    expected: Option<&str>,
    term: u64,
) -> Result<Option<AdminSlot>, String> {
    let lease = Lease {
        holder: cfg.node.clone(),
        term,
        repl: cfg.repl.clone(),
        client: cfg.client.clone(),
        seq: last_seq(cfg)?,
        beat: 0,
    };
    let started = Instant::now();
    match cfg.store.write(&lease, expected) {
        Ok(ver) => {
            replication::claim_term(&cfg.dir, term)
                .map_err(|e| format!("claim term {term}: {e}"))?;
            info!(term, "ha: won the lease — promoting this node");
            Ok(Some(start_renewer(cfg.clone(), lease, ver, started)))
        }
        Err(LeaseError::Conflict) => Ok(None),
        Err(LeaseError::Other(e)) => {
            warn!("ha: write the lease: {e}");
            Ok(None)
        }
    }
}

fn last_seq(cfg: &HaConfig) -> Result<u64, String> {
    Db::open(Config::new(cfg.dir.clone()))
        .map(|db| db.last_seq())
        .map_err(|e| format!("open {}: {e}", cfg.dir.display()))
}

fn start_renewer(cfg: Arc<HaConfig>, lease: Lease, ver: String, last_ok: Instant) -> AdminSlot {
    let slot: AdminSlot = Arc::new(OnceLock::new());
    let s = slot.clone();
    std::thread::spawn(move || renew(&cfg, lease, ver, last_ok, &s));
    slot
}

fn renew(
    cfg: &HaConfig,
    mut lease: Lease,
    mut ver: String,
    mut last_ok: Instant,
    slot: &AdminSlot,
) {
    loop {
        std::thread::sleep(cfg.ttl / 3);
        let started = Instant::now();
        let next = Lease {
            beat: lease.beat + 1,
            seq: slot.get().map_or(lease.seq, |a| a.last_seq()),
            ..lease.clone()
        };
        match cfg.store.write(&next, Some(&ver)) {
            Ok(v) => {
                (lease, ver, last_ok) = (next, v, started);
            }
            Err(LeaseError::Conflict) => fence(cfg, slot, lease.term, "the lease was taken"),
            Err(LeaseError::Other(e)) => {
                warn!("ha: renew the lease: {e}");
                if last_ok.elapsed() >= cfg.ttl * 2 / 3 {
                    fence(
                        cfg,
                        slot,
                        lease.term,
                        "the lease could not be renewed in time",
                    );
                }
            }
        }
    }
}

/// Stop writing first, then record why, then exit for the supervisor.
fn fence(cfg: &HaConfig, slot: &AdminSlot, term: u64, why: &str) -> ! {
    if let Some(admin) = slot.get() {
        admin.fence(format!("{why} (self-fenced)"));
    }
    if let Ok(Some((rec, _))) = cfg.store.read() {
        if let Some(c) = rec.client.filter(|c| replication::valid_addr(c)) {
            let _ = replication::store_leader(&cfg.dir, &c);
        }
    }
    let _ = replication::store_term(&cfg.dir, term, Role::Fenced);
    error!(
        term,
        "ha: {why} — fenced; exiting for the supervisor to restart this node as a standby"
    );
    std::process::exit(FENCED_EXIT);
}

/// `follow_loop` on a thread, stoppable.
struct Follower {
    leader: String,
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl Follower {
    fn start(cfg: &Arc<HaConfig>, leader: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (c, l, st) = (cfg.clone(), leader.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            let ckpt = PathBuf::from(format!("{}.ckpt", c.dir.display()));
            if let Err(e) = replication::follow_loop(&c.dir, &l, &c.token, &ckpt, &st) {
                warn!("ha: following {l}: {e}");
            }
        });
        Follower {
            leader,
            stop,
            handle,
        }
    }
}

fn stop(following: &mut Option<Follower>) {
    if let Some(f) = following.take() {
        f.stop.store(true, Ordering::SeqCst);
        let _ = f.handle.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ha-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn lease(holder: &str, term: u64, beat: u64) -> Lease {
        Lease {
            holder: holder.into(),
            term,
            repl: "127.0.0.1:1".into(),
            client: None,
            seq: 0,
            beat,
        }
    }

    /// The whole safety argument rests on this: a write lands only when
    /// the version it read is still current.
    #[test]
    fn a_file_lease_is_a_compare_and_swap() {
        cas_contract(&FileLease::new(tmp("cas").join("lease.json")));
    }

    fn cas_contract(store: &dyn LeaseStore) {
        assert!(store.read().unwrap().is_none());
        let v1 = store.write(&lease("a", 1, 0), None).unwrap();
        assert!(
            matches!(
                store.write(&lease("b", 1, 0), None),
                Err(LeaseError::Conflict)
            ),
            "two creators won"
        );
        let (got, v) = store.read().unwrap().unwrap();
        assert_eq!((got.holder.as_str(), got.term), ("a", 1));
        assert_eq!(v, v1);
        let v2 = store.write(&lease("a", 1, 1), Some(&v1)).unwrap();
        assert_ne!(v1, v2, "a renewal must change the version");
        assert!(
            matches!(
                store.write(&lease("b", 2, 0), Some(&v1)),
                Err(LeaseError::Conflict)
            ),
            "a stale version won"
        );
    }

    #[test]
    fn concurrent_creators_have_exactly_one_winner() {
        let store = Arc::new(FileLease::new(tmp("race").join("lease.json")));
        let wins: usize = (0..8)
            .map(|i| {
                let s = store.clone();
                std::thread::spawn(move || s.write(&lease(&format!("n{i}"), 1, 0), None).is_ok())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap() as usize)
            .sum();
        assert_eq!(wins, 1);
    }

    /// R5a-2 — the S3 store against a conditional-write fake: the same
    /// CAS contract, every request signed.
    #[test]
    fn an_s3_lease_is_a_compare_and_swap() {
        let fake = FakeS3::start();
        let store = S3Lease::new(
            Some(fake.endpoint.clone()),
            "lease-bucket".into(),
            "ha/lease.json".into(),
            "eu-west-1".into(),
            Creds {
                akid: "AKIDTEST".into(),
                secret: "secret".into(),
                token: None,
            },
            Duration::from_secs(5),
        );
        cas_contract(&store);
        let auth = fake.state.lock().unwrap().auth.clone();
        assert!(
            auth.iter()
                .all(|a| a.starts_with("AWS4-HMAC-SHA256 Credential=AKIDTEST/")
                    && a.contains("/eu-west-1/s3/aws4_request")),
            "unsigned request: {auth:?}"
        );
        assert_eq!(
            fake.state.lock().unwrap().paths,
            ["/lease-bucket/ha/lease.json"]
        );
    }

    /// AWS SigV4 documentation, "Example: GET Object".
    #[test]
    fn sigv4_matches_the_aws_get_object_example() {
        let headers = [
            ("host", "examplebucket.s3.amazonaws.com"),
            ("range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", "20130524T000000Z"),
        ];
        let auth = authorization(
            &Creds {
                akid: "AKIAIOSFODNN7EXAMPLE".into(),
                secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
                token: None,
            },
            "us-east-1",
            "GET",
            "/test.txt",
            &headers,
            EMPTY_SHA256,
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn amz_date_is_utc_basic_format() {
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
        assert_eq!(amz_date(951_782_400 + 3_661), "20000229T010101Z"); // leap day
    }

    #[test]
    fn s3_lease_urls_parse() {
        assert_eq!(
            parse_s3_url("s3://bucket/path/to/lease.json"),
            Some(("bucket".to_string(), "path/to/lease.json".to_string()))
        );
        assert_eq!(parse_s3_url("s3://bucket"), None);
        assert_eq!(parse_s3_url("/var/lease.json"), None);
    }

    #[derive(Default)]
    struct FakeState {
        object: Option<(Vec<u8>, String)>,
        next: u64,
        auth: Vec<String>,
        paths: Vec<String>,
    }

    /// Just enough S3: GET / PUT on one object, `If-None-Match: *` and
    /// `If-Match` → 412 when they fail, quoted ETags.
    struct FakeS3 {
        endpoint: String,
        state: Arc<std::sync::Mutex<FakeState>>,
    }

    impl FakeS3 {
        fn start() -> Self {
            use std::io::{BufRead, BufReader, Read};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let state = Arc::new(std::sync::Mutex::new(FakeState::default()));
            let st = state.clone();
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    let mut conn = conn.unwrap();
                    let mut r = BufReader::new(conn.try_clone().unwrap());
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    let mut parts = line.split_whitespace();
                    let (method, path) = (
                        parts.next().unwrap().to_string(),
                        parts.next().unwrap().to_string(),
                    );
                    let mut headers = std::collections::HashMap::new();
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        let h = h.trim_end();
                        if h.is_empty() {
                            break;
                        }
                        let (k, v) = h.split_once(':').unwrap();
                        headers.insert(k.to_ascii_lowercase(), v.trim().to_string());
                    }
                    let len: usize = headers
                        .get("content-length")
                        .map_or(0, |v| v.parse().unwrap());
                    let mut body = vec![0u8; len];
                    r.read_exact(&mut body).unwrap();
                    let mut s = st.lock().unwrap();
                    s.auth
                        .push(headers.get("authorization").cloned().unwrap_or_default());
                    if !s.paths.contains(&path) {
                        s.paths.push(path);
                    }
                    let (status, etag, out) = match method.as_str() {
                        "GET" => match &s.object {
                            Some((b, e)) => ("200 OK", Some(e.clone()), b.clone()),
                            None => (
                                "404 Not Found",
                                None,
                                b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
                            ),
                        },
                        _ => {
                            let current = s.object.as_ref().map(|(_, e)| e.clone());
                            let ok = match (headers.get("if-none-match"), headers.get("if-match")) {
                                (Some(_), _) => current.is_none(),
                                (_, Some(m)) => current.as_deref() == Some(m.as_str()),
                                _ => true,
                            };
                            if ok {
                                s.next += 1;
                                let e = format!("\"v{}\"", s.next);
                                s.object = Some((body, e.clone()));
                                ("200 OK", Some(e), Vec::new())
                            } else {
                                (
                                    "412 Precondition Failed",
                                    None,
                                    b"<Error><Code>PreconditionFailed</Code></Error>".to_vec(),
                                )
                            }
                        }
                    };
                    drop(s);
                    let etag = etag.map(|e| format!("ETag: {e}\r\n")).unwrap_or_default();
                    let head = format!(
                        "HTTP/1.1 {status}\r\n{etag}Content-Length: {}\r\nConnection: close\r\n\r\n",
                        out.len()
                    );
                    let _ = conn.write_all(head.as_bytes());
                    let _ = conn.write_all(&out);
                }
            });
            FakeS3 { endpoint, state }
        }
    }
}
