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
#[cfg(test)]
use crate::s3::{amz_date, authorization, Creds};
use crate::s3::{describe, S3};
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

/// R5a-2 — the lease as one S3 object; the version is its ETag and every
/// write is conditional (`If-None-Match: *` to create, `If-Match: <etag>`
/// to renew or take over). Requests are SigV4-signed (`crate::s3`).
pub(crate) struct S3Lease {
    s3: S3,
    key: String,
}

impl S3Lease {
    /// `endpoint`: an S3-compatible store (path-style); `None` is AWS
    /// (virtual-hosted). `timeout` bounds every request — a renewal must
    /// not hang past its period.
    #[cfg(test)]
    pub(crate) fn new(
        endpoint: Option<String>,
        bucket: String,
        key: String,
        region: String,
        creds: Creds,
        timeout: Duration,
    ) -> Self {
        Self {
            s3: S3::new(endpoint, bucket, region, creds, timeout),
            key,
        }
    }

    /// `s3://bucket/key`, with the standard AWS credential and region
    /// variables; `AIKOQL_S3_ENDPOINT` selects an S3-compatible store.
    pub(crate) fn from_env(url: &str, timeout: Duration) -> Result<Self, String> {
        let (bucket, key) =
            parse_s3_url(url).ok_or_else(|| format!("{url:?} is not s3://BUCKET/KEY"))?;
        Ok(Self {
            s3: S3::from_env(bucket, timeout)?,
            key,
        })
    }

    fn send(
        &self,
        method: &str,
        body: &[u8],
        condition: Option<(&str, &str)>,
    ) -> Result<ureq::Response, (Option<u16>, String)> {
        self.s3.send(method, &self.key, &[], body, condition)
    }
}

impl LeaseStore for S3Lease {
    fn read(&self) -> Result<Option<(Lease, String)>, String> {
        match self.send("GET", b"", None) {
            Ok(resp) => {
                let etag = resp
                    .header("etag")
                    .ok_or("S3 GET without an ETag")?
                    .to_string();
                let mut body = Vec::new();
                std::io::Read::read_to_end(
                    &mut std::io::Read::take(resp.into_reader(), 64 << 10),
                    &mut body,
                )
                .map_err(|e| format!("read the lease: {e}"))?;
                let lease =
                    serde_json::from_slice(&body).map_err(|e| format!("unreadable lease: {e}"))?;
                Ok(Some((lease, etag)))
            }
            Err((Some(404), _)) => Ok(None),
            Err((_, e)) => Err(e),
        }
    }

    fn write(&self, lease: &Lease, expected: Option<&str>) -> Result<String, LeaseError> {
        let body = serde_json::to_vec(lease).map_err(|e| LeaseError::Other(e.to_string()))?;
        let condition = expected.map_or(("if-none-match", "*"), |v| ("if-match", v));
        match self.send("PUT", &body, Some(condition)) {
            Ok(resp) => resp
                .header("etag")
                .map(str::to_string)
                .ok_or_else(|| LeaseError::Other("S3 PUT without an ETag".into())),
            // 412: the condition failed; 409: a concurrent conditional write
            Err((Some(412 | 409), _)) => Err(LeaseError::Conflict),
            Err((_, e)) => Err(LeaseError::Other(e)),
        }
    }
}

/// R5b-1 — the lease as a Kubernetes `Lease` (coordination.k8s.io/v1).
/// The version is `metadata.resourceVersion`: create is POST (409 when it
/// exists), renew / take over is PUT carrying that resourceVersion (409
/// when it moved). The record rides the `aikoql.io/lease` annotation;
/// `spec.holderIdentity` names the holder for kubectl.
pub(crate) struct K8sLease {
    base: String,
    token: String,
    namespace: String,
    name: String,
    agent: ureq::Agent,
}

const ANNOTATION: &str = "aikoql.io/lease";

impl K8sLease {
    pub(crate) fn new(
        base: String,
        token: String,
        namespace: String,
        name: String,
        agent: ureq::Agent,
    ) -> Self {
        Self {
            base,
            token,
            namespace,
            name,
            agent,
        }
    }

    /// `k8s://NAMESPACE/NAME` from inside a pod: the service account's
    /// token and CA, the API server from `KUBERNETES_SERVICE_HOST/PORT`.
    pub(crate) fn in_cluster(url: &str, timeout: Duration) -> Result<Self, String> {
        const SA: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
        let (namespace, name) =
            parse_k8s_url(url).ok_or_else(|| format!("{url:?} is not k8s://NAMESPACE/NAME"))?;
        let host = std::env::var("KUBERNETES_SERVICE_HOST").map_err(|_| {
            "a k8s:// lease runs in a pod (KUBERNETES_SERVICE_HOST unset)".to_string()
        })?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let read = |f: &str| crate::tls::read(&format!("{SA}/{f}"));
        let token = String::from_utf8_lossy(&read("token")?).trim().to_string();
        let tls =
            crate::tls::client_config(&read("ca.crt")?).map_err(|e| format!("cluster CA: {e}"))?;
        let agent = ureq::AgentBuilder::new()
            .timeout(timeout)
            .tls_config(tls)
            .build();
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        Ok(Self::new(
            format!("https://{host}:{port}"),
            token,
            namespace,
            name,
            agent,
        ))
    }

    fn url(&self, named: bool) -> String {
        let name = if named {
            format!("/{}", self.name)
        } else {
            String::new()
        };
        format!(
            "{}/apis/coordination.k8s.io/v1/namespaces/{}/leases{name}",
            self.base, self.namespace
        )
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

/// (record, resourceVersion) out of a Lease object.
fn lease_object(v: &serde_json::Value) -> Result<(Lease, String), String> {
    let rv = v["metadata"]["resourceVersion"]
        .as_str()
        .ok_or("a Lease without metadata.resourceVersion")?;
    let rec = v["metadata"]["annotations"][ANNOTATION]
        .as_str()
        .ok_or_else(|| {
            format!("the Lease carries no {ANNOTATION} annotation — not an aikoql lease")
        })?;
    let lease = serde_json::from_str(rec).map_err(|e| format!("unreadable lease: {e}"))?;
    Ok((lease, rv.to_string()))
}

impl LeaseStore for K8sLease {
    fn read(&self) -> Result<Option<(Lease, String)>, String> {
        match self
            .agent
            .get(&self.url(true))
            .set("authorization", &self.bearer())
            .call()
        {
            Ok(resp) => {
                let v: serde_json::Value = resp
                    .into_json()
                    .map_err(|e| format!("read the Lease: {e}"))?;
                lease_object(&v).map(Some)
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(describe(e).1),
        }
    }

    fn write(&self, lease: &Lease, expected: Option<&str>) -> Result<String, LeaseError> {
        let record = serde_json::to_string(lease).map_err(|e| LeaseError::Other(e.to_string()))?;
        let mut metadata =
            serde_json::json!({ "name": self.name, "annotations": { ANNOTATION: record } });
        if let Some(rv) = expected {
            metadata["resourceVersion"] = serde_json::json!(rv);
        }
        let object = serde_json::json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": metadata,
            "spec": { "holderIdentity": lease.holder },
        });
        let req = match expected {
            None => self.agent.post(&self.url(false)),
            Some(_) => self.agent.put(&self.url(true)),
        };
        match req.set("authorization", &self.bearer()).send_json(object) {
            Ok(resp) => {
                let v: serde_json::Value = resp
                    .into_json()
                    .map_err(|e| LeaseError::Other(format!("read the Lease: {e}")))?;
                lease_object(&v)
                    .map(|(_, rv)| rv)
                    .map_err(LeaseError::Other)
            }
            // AlreadyExists on create, a moved resourceVersion on replace
            Err(ureq::Error::Status(409, _)) => Err(LeaseError::Conflict),
            Err(e) => Err(LeaseError::Other(describe(e).1)),
        }
    }
}

pub(crate) fn parse_k8s_url(url: &str) -> Option<(String, String)> {
    let (ns, name) = url.strip_prefix("k8s://")?.split_once('/')?;
    (!ns.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| (ns.to_string(), name.to_string()))
}

pub(crate) fn parse_s3_url(url: &str) -> Option<(String, String)> {
    let (bucket, key) = url.strip_prefix("s3://")?.split_once('/')?;
    (!bucket.is_empty() && !key.is_empty()).then(|| (bucket.to_string(), key.to_string()))
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
    /// R3c — trust for the leader's replication certificate.
    pub(crate) tls: Option<Arc<rustls::ClientConfig>>,
    pub(crate) dir: PathBuf,
}

/// The renewer's late-bound fence target: the storage admin surface,
/// set once the kernel has opened.
pub(crate) type AdminSlot = Arc<OnceLock<Arc<dyn StorageAdminApi>>>;

/// Where `begin` leaves this node.
pub(crate) enum Begun {
    /// It holds the lease: the directory is claimed at the lease's term and
    /// the renewer runs (it started before the kernel opens — a slow open
    /// must not let the lease lapse). Serve as the leader.
    Lead(AdminSlot),
    /// Another node holds it: serve reads as its standby (`stand_by`).
    Standby(Arc<HaConfig>),
}

/// The start-up decision. No lease: try to win one. This node's own lease
/// — from before a restart, or won just before a re-exec at a newer term
/// than its directory — is claimed if needed and resumed by an immediate
/// CAS renewal (the fence timer starts from a renewal that landed, never
/// from the restart). Any other holder: a standby. An unreadable store, or
/// a lost race, is retried.
pub(crate) fn begin(cfg: HaConfig) -> Result<Begun, String> {
    let cfg = Arc::new(cfg);
    loop {
        let (term, _) = replication::load_term(&cfg.dir).map_err(|e| e.to_string())?;
        match cfg.store.read() {
            Err(e) => warn!("ha: read the lease: {e}"),
            Ok(None) => {
                if let Some(won) = try_win(&cfg, None, term + 1)? {
                    return Ok(Begun::Lead(won));
                }
            }
            Ok(Some((rec, ver))) if rec.holder == cfg.node && rec.term >= term => {
                if rec.term > term
                    || replication::load_term(&cfg.dir).map(|t| t.1).ok() != Some(Role::Leader)
                {
                    replication::claim_term(&cfg.dir, rec.term)
                        .map_err(|e| format!("claim term {}: {e}", rec.term))?;
                }
                info!(term = rec.term, "ha: resuming the lease this node holds");
                if let Some(slot) = resume(&cfg, rec, &ver) {
                    return Ok(Begun::Lead(slot));
                }
            }
            Ok(Some(_)) => return Ok(Begun::Standby(cfg)),
        }
        std::thread::sleep(cfg.ttl / 5);
    }
}

/// Renew this node's lease now (CAS); the renewer takes over from there.
fn resume(cfg: &Arc<HaConfig>, rec: Lease, ver: &str) -> Option<AdminSlot> {
    let started = Instant::now();
    let next = Lease {
        beat: rec.beat + 1,
        ..rec
    };
    match cfg.store.write(&next, Some(ver)) {
        Ok(v) => Some(start_renewer(cfg.clone(), next, v, started)),
        Err(LeaseError::Conflict) => None, // taken meanwhile: decide again
        Err(LeaseError::Other(e)) => {
            warn!("ha: renew the lease: {e}");
            None
        }
    }
}

/// The serving standby (standby reads): follow the holder into this
/// node's open, fenced engine. Once the record has stayed unchanged for a
/// full TTL on this node's monotonic clock — and this node holds at least
/// the holder's last renewed seq — win the lease at the next term and
/// re-execute this process: the fresh process claims the directory and
/// leads (`begin`).
pub(crate) fn stand_by(cfg: Arc<HaConfig>, replica: Arc<dyn StorageAdminApi>) {
    let mut seen: Option<(String, Instant)> = None;
    let mut following: Option<Follower> = None;
    let mut behind_warned = false;
    loop {
        std::thread::sleep(cfg.ttl / 5);
        let term = replication::load_term(&cfg.dir).map_or(0, |t| t.0);
        match cfg.store.read() {
            Err(e) => warn!("ha: read the lease: {e}"),
            Ok(None) => win(&cfg, None, term + 1, &replica),
            Ok(Some((rec, ver))) => {
                if seen.as_ref().map(|(v, _)| v) != Some(&ver) {
                    seen = Some((ver.clone(), Instant::now()));
                }
                let lead_addr = (rec.holder != cfg.node).then_some(&rec.repl);
                if following.as_ref().map(|f| &f.leader) != lead_addr {
                    stop(&mut following);
                    following =
                        lead_addr.map(|a| Follower::start(&cfg, a.clone(), replica.clone()));
                }
                let stale = seen.as_ref().is_some_and(|(_, at)| at.elapsed() >= cfg.ttl);
                if stale {
                    let last = replica.last_seq();
                    if last >= rec.seq {
                        let next = term.max(rec.term) + 1;
                        // logged BEFORE the CAS: an operator (and the R5c
                        // cert) can order it against the old leader's
                        // "fenced" line — one writer per term
                        info!(
                            holder = %rec.holder,
                            term = next,
                            "ha: the lease lapsed — taking over"
                        );
                        win(&cfg, Some(&ver), next, &replica);
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
    }
}

/// One CAS for `term` from the serving standby; a win re-executes.
fn win(cfg: &HaConfig, expected: Option<&str>, term: u64, replica: &Arc<dyn StorageAdminApi>) {
    let lease = Lease {
        holder: cfg.node.clone(),
        term,
        repl: cfg.repl.clone(),
        client: cfg.client.clone(),
        seq: replica.last_seq(),
        beat: 0,
    };
    match cfg.store.write(&lease, expected) {
        Ok(_) => {
            info!(term, "ha: won the lease — re-executing as the leader");
            reexec();
        }
        Err(LeaseError::Conflict) => {}
        Err(LeaseError::Other(e)) => warn!("ha: write the lease: {e}"),
    }
}

/// Replace this process with a fresh one of the same command line (unix:
/// same pid, same stdio; the lease it just won is claimed by `begin`).
/// Elsewhere — or if exec fails — exit for the supervisor to restart it.
fn reexec() -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::env::current_exe().map(|exe| {
            std::process::Command::new(exe)
                .args(std::env::args_os().skip(1))
                .exec()
        });
        error!("ha: re-exec as the leader failed ({err:?}) — exiting for the supervisor");
    }
    std::process::exit(FENCED_EXIT);
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
    let _ = replication::store_term(&cfg.dir, term, Role::Fenced);
    error!(
        term,
        "ha: {why} — fenced; exiting for the supervisor to restart this node as a standby"
    );
    // best-effort NOT_LEADER hint, last: a partitioned store makes this
    // read wait out its timeout, and nothing above may wait for it
    if let Ok(Some((rec, _))) = cfg.store.read() {
        if let Some(c) = rec.client.filter(|c| replication::valid_addr(c)) {
            let _ = replication::store_leader(&cfg.dir, &c);
        }
    }
    std::process::exit(FENCED_EXIT);
}

/// The standby's stream into its open engine, on a thread, stoppable.
struct Follower {
    leader: String,
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl Follower {
    fn start(cfg: &Arc<HaConfig>, leader: String, replica: Arc<dyn StorageAdminApi>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (c, l, st) = (cfg.clone(), leader.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            // R3b: shipped index checkpoints land where the promoted
            // process resumes them
            let ckpt = PathBuf::from(format!("{}.ckpt", c.dir.display()));
            let followed = replication::follow_into(
                &l,
                &c.token,
                &replica,
                &c.dir,
                Some(&ckpt),
                &st,
                c.tls.as_ref(),
            );
            if let Err(e) = followed {
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

    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
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
            "",
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

    type Shared = Arc<std::sync::Mutex<FakeState>>;
    type Headers = std::collections::HashMap<String, String>;

    /// A one-request-per-connection HTTP/1.1 server: `handle` gets
    /// (state, method, path, headers, body) and answers (status line,
    /// extra headers, body). Every request's path and authorization land
    /// in the state.
    fn fake_http(
        handle: impl Fn(&mut FakeState, &str, &str, &Headers, Vec<u8>) -> (&'static str, String, Vec<u8>)
            + Send
            + 'static,
    ) -> (String, Shared) {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state: Shared = Arc::default();
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
                let mut headers = Headers::new();
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
                    s.paths.push(path.clone());
                }
                let (status, extra, out) = handle(&mut s, &method, &path, &headers, body);
                drop(s);
                let head = format!(
                    "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    out.len()
                );
                let _ = conn.write_all(head.as_bytes());
                let _ = conn.write_all(&out);
            }
        });
        (endpoint, state)
    }

    /// Just enough S3: GET / PUT on one object, `If-None-Match: *` and
    /// `If-Match` → 412 when they fail, quoted ETags.
    struct FakeS3 {
        endpoint: String,
        state: Shared,
    }

    impl FakeS3 {
        fn start() -> Self {
            let (endpoint, state) = fake_http(|s, method, _, headers, body| match method {
                "GET" => match &s.object {
                    Some((b, e)) => ("200 OK", format!("ETag: {e}\r\n"), b.clone()),
                    None => (
                        "404 Not Found",
                        String::new(),
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
                        ("200 OK", format!("ETag: {e}\r\n"), Vec::new())
                    } else {
                        (
                            "412 Precondition Failed",
                            String::new(),
                            b"<Error><Code>PreconditionFailed</Code></Error>".to_vec(),
                        )
                    }
                }
            });
            FakeS3 { endpoint, state }
        }
    }

    const LEASES: &str = "/apis/coordination.k8s.io/v1/namespaces/aikoql/leases";

    /// Just enough of the Kubernetes API for one Lease: POST creates (409
    /// if it exists), PUT replaces iff `metadata.resourceVersion` matches
    /// (409 otherwise), GET reads (404 if absent).
    fn fake_k8s() -> (String, Shared) {
        fake_http(|s, method, path, _, body| {
            let named = format!("{LEASES}/primary");
            let current = s.object.as_ref().map(|(_, rv)| rv.clone());
            let store = |s: &mut FakeState, body: Vec<u8>| {
                s.next += 1;
                let rv = s.next.to_string();
                let mut obj: serde_json::Value = serde_json::from_slice(&body).unwrap();
                obj["metadata"]["resourceVersion"] = serde_json::json!(rv);
                let out = serde_json::to_vec(&obj).unwrap();
                s.object = Some((out.clone(), rv));
                out
            };
            match (method, path) {
                ("GET", p) if p == named => match &s.object {
                    Some((b, _)) => ("200 OK", String::new(), b.clone()),
                    None => ("404 Not Found", String::new(), b"{}".to_vec()),
                },
                ("POST", LEASES) if current.is_none() => {
                    ("201 Created", String::new(), store(s, body))
                }
                ("POST", LEASES) => ("409 Conflict", String::new(), b"{}".to_vec()),
                ("PUT", p) if p == named => {
                    let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    if sent["metadata"]["resourceVersion"].as_str() == current.as_deref() {
                        ("200 OK", String::new(), store(s, body))
                    } else {
                        ("409 Conflict", String::new(), b"{}".to_vec())
                    }
                }
                _ => ("400 Bad Request", String::new(), b"{}".to_vec()),
            }
        })
    }

    /// R5b-1 — the Kubernetes Lease store: the same CAS contract on
    /// resourceVersion, bearer-authenticated, holder visible to kubectl.
    #[test]
    fn a_k8s_lease_is_a_compare_and_swap() {
        let (endpoint, state) = fake_k8s();
        let store = K8sLease::new(
            endpoint,
            "tok".into(),
            "aikoql".into(),
            "primary".into(),
            ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .build(),
        );
        cas_contract(&store);
        let s = state.lock().unwrap();
        assert!(s.auth.iter().all(|a| a == "Bearer tok"), "{:?}", s.auth);
        let obj: serde_json::Value = serde_json::from_slice(&s.object.as_ref().unwrap().0).unwrap();
        assert_eq!(obj["kind"], "Lease");
        assert_eq!(obj["spec"]["holderIdentity"], "a");
    }

    #[test]
    fn k8s_lease_urls_parse() {
        assert_eq!(
            parse_k8s_url("k8s://aikoql/primary"),
            Some(("aikoql".to_string(), "primary".to_string()))
        );
        assert_eq!(parse_k8s_url("k8s://primary"), None);
        assert_eq!(parse_k8s_url("s3://b/k"), None);
    }

    fn ha_cfg(store: Arc<dyn LeaseStore>, dir: PathBuf) -> HaConfig {
        HaConfig {
            store,
            node: "a".into(),
            ttl: Duration::from_secs(60), // no background renewal during the test
            repl: "127.0.0.1:1".into(),
            client: None,
            token: "t".into(),
            tls: None,
            dir,
        }
    }

    /// Resuming its own lease after a restart, a leader renews it at once
    /// (a CAS): the fence timer must start from a renewal that landed, not
    /// from the restart — a lease last renewed long ago could lapse while
    /// the restarted node already writes.
    #[test]
    fn resuming_a_lease_renews_it_at_once() {
        let dir = tmp("resume-db");
        std::fs::create_dir_all(&dir).unwrap();
        crate::replication::store_term(&dir, 1, Role::Leader).unwrap();
        let store: Arc<dyn LeaseStore> = Arc::new(FileLease::new(tmp("resume").join("lease.json")));
        store.write(&lease("a", 1, 0), None).unwrap();
        assert!(matches!(
            begin(ha_cfg(store.clone(), dir)).unwrap(),
            Begun::Lead(_)
        ));
        let (rec, _) = store.read().unwrap().unwrap();
        assert_eq!(
            (rec.holder.as_str(), rec.term, rec.beat),
            ("a", 1, 1),
            "not renewed at resume"
        );
    }

    /// R5 standby reads — a standby that won the lease re-executes itself;
    /// the fresh process finds the lease its own at a newer term than its
    /// directory, claims the directory at that term and leads.
    #[test]
    fn a_lease_won_before_a_restart_is_claimed_and_led() {
        let dir = tmp("won-db");
        std::fs::create_dir_all(&dir).unwrap();
        crate::replication::store_term(&dir, 1, Role::Standby).unwrap();
        let store: Arc<dyn LeaseStore> = Arc::new(FileLease::new(tmp("won").join("lease.json")));
        store.write(&lease("a", 2, 0), None).unwrap();
        assert!(matches!(
            begin(ha_cfg(store.clone(), dir.clone())).unwrap(),
            Begun::Lead(_)
        ));
        assert_eq!(
            crate::replication::load_term(&dir).unwrap(),
            (2, Role::Leader)
        );
        assert_eq!(store.read().unwrap().unwrap().0.beat, 1);
    }

    /// A live lease held by another node: this node serves as a standby.
    #[test]
    fn another_holder_makes_this_node_a_standby() {
        let dir = tmp("other-db");
        std::fs::create_dir_all(&dir).unwrap();
        let store: Arc<dyn LeaseStore> = Arc::new(FileLease::new(tmp("other").join("lease.json")));
        store.write(&lease("b", 1, 0), None).unwrap();
        assert!(matches!(
            begin(ha_cfg(store, dir)).unwrap(),
            Begun::Standby(_)
        ));
    }

    /// A real S3-compatible store (MinIO in dev/CI, `scripts/s3-it.sh`):
    /// AIKOQL_TEST_S3_ENDPOINT + AIKOQL_TEST_S3_BUCKET + the AWS_* creds.
    /// None — the test is skipped — when unset.
    fn real_s3() -> Option<(String, String, Creds)> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Some((
            env("AIKOQL_TEST_S3_ENDPOINT")?,
            env("AIKOQL_TEST_S3_BUCKET")?,
            Creds {
                akid: env("AWS_ACCESS_KEY_ID")?,
                secret: env("AWS_SECRET_ACCESS_KEY")?,
                token: env("AWS_SESSION_TOKEN"),
            },
        ))
    }

    fn unique(tag: &str) -> String {
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("it-{tag}-{}-{ns}", std::process::id())
    }

    /// The conditional-write contract on a real store — the open R5a risk
    /// ("S3-compatible stores must each be checked").
    #[test]
    fn real_s3_lease_is_a_compare_and_swap() {
        let Some((endpoint, bucket, creds)) = real_s3() else {
            eprintln!("skipped: AIKOQL_TEST_S3_ENDPOINT unset");
            return;
        };
        let store = S3Lease::new(
            Some(endpoint.clone()),
            bucket.clone(),
            format!("{}/lease.json", unique("cas")),
            "us-east-1".into(),
            creds,
            Duration::from_secs(10),
        );
        cas_contract(&store);
        // and under contention: exactly one creator wins
        let key = format!("{}/lease.json", unique("race"));
        let wins: usize = (0..8)
            .map(|i| {
                let (endpoint, bucket, key, creds) = (
                    endpoint.clone(),
                    bucket.clone(),
                    key.clone(),
                    real_s3().unwrap().2,
                );
                std::thread::spawn(move || {
                    let s = S3Lease::new(
                        Some(endpoint),
                        bucket,
                        key,
                        "us-east-1".into(),
                        creds,
                        Duration::from_secs(10),
                    );
                    s.write(&lease(&format!("n{i}"), 1, 0), None).is_ok() as usize
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .sum();
        assert_eq!(wins, 1, "conditional create let {wins} writers win");
    }
}
