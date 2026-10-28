//! MRFC-0080 R6 — the continuous archive and point-in-time restore.
//!
//! Layout in an `ObjectStore` (a directory, or `s3://bucket/prefix`), one
//! directory per replication term so timelines never mix:
//!
//! ```text
//! term-{T:010}/snap/{seq:020}-{ms:013}/<snapshot files> + COMPLETE (last)
//! term-{T:010}/log/{first:020}-{last:020}-{ms:013}.frames   encoded WAL frames
//! ```
//!
//! The serving leader's `Archiver` starts its term's directory with a
//! snapshot, then uploads the retained log in batches; it holds log
//! release (an entry in `Acked`, like a standby) until a batch is up.
//! `restore` picks the newest timeline whose snapshot precedes the
//! target, restores it and replays log frames up to the target seq or
//! wall-clock instant (granularity: one log batch).

use crate::replication::{collect_files, Acked};
use aikoql_storage_v2::db::Db;
use aikoql_storage_v2::engine::StorageAdminApi;
use aikoql_storage_v2::wal::{decode_frame, encode_frame};
use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;

pub(crate) trait ObjectStore: Send + Sync {
    fn put(&self, key: &str, body: &[u8]) -> Result<(), String>;
    /// A missing key is an error, never an empty body.
    fn get(&self, key: &str) -> Result<Vec<u8>, String>;
    /// Every key starting with `prefix`, sorted.
    fn list(&self, prefix: &str) -> Result<Vec<String>, String>;
}

/// Keys as files under a root — local disks, NFS, tests.
pub(crate) struct DirStore {
    root: PathBuf,
}

impl DirStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Trust boundary: a key is plain relative components — never `..`
    /// or a root that would escape the store.
    fn path(&self, key: &str) -> Result<PathBuf, String> {
        let rel = Path::new(key);
        if key.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!("archive key {key:?} is not a plain relative path"));
        }
        Ok(self.root.join(rel))
    }
}

impl ObjectStore for DirStore {
    fn put(&self, key: &str, body: &[u8]) -> Result<(), String> {
        let path = self.path(key)?;
        let io = |e: std::io::Error| format!("archive put {key}: {e}");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        f.write_all(body).and_then(|_| f.sync_all()).map_err(io)?;
        std::fs::rename(&tmp, &path).map_err(io)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.path(key)?).map_err(|e| format!("archive get {key}: {e}"))
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        let mut files = Vec::new();
        match collect_files(&self.root, &self.root, &mut files) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("archive list: {e}")),
        }
        let mut keys: Vec<String> = files
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with(prefix) && !k.ends_with(".tmp"))
            .collect();
        keys.sort();
        Ok(keys)
    }
}

/// Keys as objects under `prefix` in one S3 bucket.
pub(crate) struct S3Store {
    s3: crate::s3::S3,
    prefix: String,
}

impl S3Store {
    pub(crate) fn new(s3: crate::s3::S3, prefix: String) -> Self {
        Self { s3, prefix }
    }

    fn full(&self, key: &str) -> String {
        if self.prefix.is_empty() {
            key.to_string()
        } else {
            format!("{}/{key}", self.prefix)
        }
    }
}

impl ObjectStore for S3Store {
    fn put(&self, key: &str, body: &[u8]) -> Result<(), String> {
        self.s3
            .send("PUT", &self.full(key), &[], body, None)
            .map(|_| ())
            .map_err(|(_, e)| e)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        match self.s3.send("GET", &self.full(key), &[], b"", None) {
            Ok(resp) => {
                let mut body = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut body)
                    .map_err(|e| format!("archive get {key}: {e}"))?;
                Ok(body)
            }
            Err((_, e)) => Err(format!("archive get {key}: {e}")),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        let full = self.full(prefix);
        let strip = if self.prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", self.prefix)
        };
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![("list-type", "2"), ("prefix", full.as_str())];
            if let Some(t) = &token {
                query.push(("continuation-token", t.as_str()));
            }
            let resp = self
                .s3
                .send("GET", "", &query, b"", None)
                .map_err(|(_, e)| format!("archive list: {e}"))?;
            let xml = resp
                .into_string()
                .map_err(|e| format!("archive list: {e}"))?;
            let (page, next) = parse_list(&xml);
            keys.extend(
                page.into_iter()
                    .filter_map(|k| k.strip_prefix(&strip).map(str::to_string)),
            );
            match next {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        keys.sort();
        Ok(keys)
    }
}

/// (keys, continuation token) out of a ListObjectsV2 response.
pub(crate) fn parse_list(xml: &str) -> (Vec<String>, Option<String>) {
    fn unescape(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    }
    let between = |s: &str, open: &str, close: &str| -> Vec<String> {
        s.split(open)
            .skip(1)
            .filter_map(|part| part.split_once(close).map(|(v, _)| unescape(v)))
            .collect()
    };
    let keys = between(xml, "<Key>", "</Key>");
    let token = between(xml, "<NextContinuationToken>", "</NextContinuationToken>")
        .into_iter()
        .next();
    (keys, token)
}

/// `s3://bucket[/prefix]`, or a directory (`file://` optional).
pub(crate) fn open_store(url: &str, timeout: Duration) -> Result<Arc<dyn ObjectStore>, String> {
    if let Some(rest) = url.strip_prefix("s3://") {
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        if bucket.is_empty() {
            return Err(format!("{url:?} is not s3://BUCKET[/PREFIX]"));
        }
        let s3 = crate::s3::S3::from_env(bucket.to_string(), timeout)?;
        return Ok(Arc::new(S3Store::new(
            s3,
            prefix.trim_matches('/').to_string(),
        )));
    }
    Ok(Arc::new(DirStore::new(PathBuf::from(
        url.strip_prefix("file://").unwrap_or(url),
    ))))
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn term_dir(term: u64) -> String {
    format!("term-{term:010}")
}

/// Frames per log object.
const LOG_BATCH: usize = 4096;
/// The archiver's entry in `Acked` (connection ids count up from 0).
const ARCHIVE_ID: u64 = u64::MAX;

/// R6 — the serving leader's continuous archive for one term.
pub(crate) struct Archiver {
    store: Arc<dyn ObjectStore>,
    source: Arc<dyn StorageAdminApi>,
    acked: Arc<Acked>,
    term: u64,
    /// where a snapshot is staged before upload
    scratch: PathBuf,
    /// the newest seq the archive holds for this term
    archived: u64,
}

impl Archiver {
    /// Resume this term's archive (its newest log object or snapshot), or
    /// start it with a snapshot. Log release is held from the first moment.
    pub(crate) fn start(
        store: Arc<dyn ObjectStore>,
        source: Arc<dyn StorageAdminApi>,
        acked: Arc<Acked>,
        term: u64,
        scratch: PathBuf,
    ) -> Result<Self, String> {
        acked.hold(ARCHIVE_ID, "archive", 0);
        let timeline = Timeline::read(&*store, term)?;
        let resume = timeline
            .logs
            .iter()
            .map(|l| l.last)
            .chain(timeline.snaps.iter().map(|s| s.seq))
            .max();
        let mut a = Self {
            store,
            source,
            acked,
            term,
            scratch,
            archived: 0,
        };
        a.archived = match resume {
            Some(seq) => seq,
            None => a.snapshot()?,
        };
        a.acked.hold(ARCHIVE_ID, "archive", a.archived);
        Ok(a)
    }

    /// Upload every frame past `archived`, then release the log to the
    /// slowest consumer. A log that no longer holds `archived` (released
    /// while the archive was away) restarts the timeline from a snapshot.
    pub(crate) fn pass(&mut self) -> Result<u64, String> {
        loop {
            let frames = match self.source.log_since(self.archived, LOG_BATCH) {
                Ok(f) => f,
                Err(e) if e.to_string().contains("gap") => {
                    warn!("archive: {e} — starting from a fresh snapshot");
                    self.archived = self.snapshot()?;
                    self.acked.hold(ARCHIVE_ID, "archive", self.archived);
                    continue;
                }
                Err(e) => return Err(format!("archive: read the log: {e}")),
            };
            let (Some(first), Some(last)) = (frames.first(), frames.last()) else {
                break;
            };
            let key = format!(
                "{}/log/{:020}-{:020}-{:013}.frames",
                term_dir(self.term),
                first.seq,
                last.seq,
                now_ms()
            );
            let mut body = Vec::new();
            for f in &frames {
                body.extend_from_slice(&encode_frame(f.seq, &f.ops).map_err(|e| e.to_string())?);
            }
            self.store.put(&key, &body)?;
            self.archived = last.seq;
            self.acked.hold(ARCHIVE_ID, "archive", self.archived);
        }
        if let Err(e) = self.source.release_log(self.acked.slowest()) {
            warn!("archive: release the log: {e}");
        }
        Ok(self.archived)
    }

    /// Snapshot the storage and upload it (files, then COMPLETE); returns
    /// the seq it holds.
    pub(crate) fn snapshot(&mut self) -> Result<u64, String> {
        let tmp = self.scratch.join("snap");
        let _ = std::fs::remove_dir_all(&tmp);
        let info = self
            .source
            .snapshot_to(&tmp)
            .map_err(|e| format!("archive snapshot: {e}"))?;
        let key = format!(
            "{}/snap/{:020}-{:013}",
            term_dir(self.term),
            info.last_seq,
            now_ms()
        );
        let mut files = Vec::new();
        collect_files(&tmp, &tmp, &mut files).map_err(|e| format!("archive snapshot: {e}"))?;
        for (rel, path) in files {
            // shortcut: one file in memory at a time; stream multipart
            // uploads once single segments outgrow memory
            let body = std::fs::read(&path).map_err(|e| format!("archive snapshot: {e}"))?;
            self.store.put(&format!("{key}/{rel}"), &body)?;
        }
        self.store.put(&format!("{key}/COMPLETE"), b"")?;
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(info.last_seq)
    }

    /// The archiver thread: a log pass every `log_every`, a snapshot every
    /// `snap_every` (bounding restore replay).
    pub(crate) fn run(mut self, log_every: Duration, snap_every: Duration) {
        let (mut last_snap, mut reported) = (Instant::now(), 0);
        loop {
            std::thread::sleep(log_every);
            match self.pass() {
                Ok(seq) if seq != reported => {
                    eprintln!("archive: log up to seq {seq}");
                    reported = seq;
                }
                Ok(_) => {}
                Err(e) => warn!("{e}"),
            }
            if last_snap.elapsed() >= snap_every {
                match self.snapshot() {
                    Ok(seq) => eprintln!("archive: snapshot at seq {seq}"),
                    Err(e) => warn!("{e}"),
                }
                last_snap = Instant::now();
            }
        }
    }
}

struct Snap {
    seq: u64,
    ms: u64,
    key: String,
}

struct Log {
    first: u64,
    last: u64,
    ms: u64,
    key: String,
}

/// One term's archive.
#[derive(Default)]
struct Timeline {
    snaps: Vec<Snap>,
    logs: Vec<Log>,
}

impl Timeline {
    fn read(store: &dyn ObjectStore, term: u64) -> Result<Self, String> {
        let all = Self::read_all(store, &format!("{}/", term_dir(term)))?;
        Ok(all.into_values().next().unwrap_or_default())
    }

    /// Every timeline under `prefix`, by term. Only a snapshot with its
    /// COMPLETE marker counts.
    fn read_all(store: &dyn ObjectStore, prefix: &str) -> Result<BTreeMap<u64, Self>, String> {
        let mut out: BTreeMap<u64, Self> = BTreeMap::new();
        for key in store.list(prefix)? {
            let mut parts = key.splitn(3, '/');
            let (Some(t), Some(kind), Some(rest)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Some(term) = t.strip_prefix("term-").and_then(|n| n.parse().ok()) else {
                continue;
            };
            let num = |s: &str| s.parse::<u64>().ok();
            match kind {
                "snap" => {
                    let Some(name) = rest.strip_suffix("/COMPLETE") else {
                        continue;
                    };
                    if let Some((seq, ms)) = name.split_once('-') {
                        if let (Some(seq), Some(ms)) = (num(seq), num(ms)) {
                            out.entry(term).or_default().snaps.push(Snap {
                                seq,
                                ms,
                                key: format!("{t}/snap/{name}"),
                            });
                        }
                    }
                }
                "log" => {
                    let Some(name) = rest.strip_suffix(".frames") else {
                        continue;
                    };
                    let f: Vec<&str> = name.split('-').collect();
                    if let [first, last, ms] = f[..] {
                        if let (Some(first), Some(last), Some(ms)) =
                            (num(first), num(last), num(ms))
                        {
                            out.entry(term).or_default().logs.push(Log {
                                first,
                                last,
                                ms,
                                key: key.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        for tl in out.values_mut() {
            tl.logs.sort_by_key(|l| l.first);
        }
        Ok(out)
    }
}

/// How far a restore replays.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Until {
    Latest,
    Seq(u64),
    /// Unix milliseconds: what the archive held by then.
    Ms(u64),
}

/// Restore the archive into `target` (must not exist or be empty):
/// the newest timeline whose snapshot precedes the target, replayed up
/// to it. Returns (seq, term).
pub(crate) fn restore(
    store: &dyn ObjectStore,
    target: &Path,
    until: Until,
    scratch: &Path,
) -> Result<(u64, u64), String> {
    let timelines = Timeline::read_all(store, "")?;
    for (term, tl) in timelines.iter().rev() {
        let fits_snap = |s: &&Snap| match until {
            Until::Latest => true,
            Until::Seq(n) => s.seq <= n,
            Until::Ms(t) => s.ms <= t,
        };
        let Some(snap) = tl.snaps.iter().filter(fits_snap).max_by_key(|s| s.seq) else {
            continue;
        };
        let logs: Vec<&Log> = tl
            .logs
            .iter()
            .filter(|l| l.last > snap.seq && !matches!(until, Until::Ms(t) if l.ms > t))
            .collect();
        let reach = logs.last().map_or(snap.seq, |l| l.last);
        let goal = match until {
            Until::Seq(n) if n > reach => continue, // not in this timeline
            Until::Seq(n) => n,
            _ => reach,
        };
        let staged = scratch.join("snap");
        let _ = std::fs::remove_dir_all(&staged);
        for key in store.list(&format!("{}/", snap.key))? {
            let rel = &key[snap.key.len() + 1..];
            if rel == "COMPLETE" {
                continue;
            }
            let path = staged.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, store.get(&key)?).map_err(|e| e.to_string())?;
        }
        let db = aikoql_storage_v2::snapshot::restore_from(&staged, target.to_path_buf())
            .map_err(|e| format!("restore snapshot {}: {e}", snap.key))?;
        let _ = std::fs::remove_dir_all(&staged);
        replay(store, &db, &logs, goal)?;
        if db.last_seq() != goal {
            return Err(format!(
                "the archive reaches seq {} of {goal}: a log object is missing",
                db.last_seq()
            ));
        }
        return Ok((goal, *term));
    }
    Err(format!("no archived snapshot covers {until:?}"))
}

/// Apply the frames of `logs` (ascending) to `db` up to `goal`.
fn replay(store: &dyn ObjectStore, db: &Db, logs: &[&Log], goal: u64) -> Result<(), String> {
    for log in logs {
        if db.last_seq() >= goal {
            break;
        }
        if log.first > db.last_seq() + 1 {
            return Err(format!(
                "archive gap: {} follows seq {}",
                log.key,
                db.last_seq()
            ));
        }
        let bytes = store.get(&log.key)?;
        let mut at = 0;
        while at < bytes.len() {
            let (f, used) = decode_frame(&bytes[at..]).map_err(|e| format!("{}: {e}", log.key))?;
            at += used;
            if f.seq > goal {
                break;
            }
            db.apply_replicated(f.seq, &f.ops)
                .map_err(|e| format!("replay frame {}: {e}", f.seq))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::Acked;
    use aikoql_storage_v2::db::{Config, Db};
    use aikoql_storage_v2::engine::StorageAdminApi;
    use aikoql_storage_v2::AikoqlStorageEngineV2;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "archive-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn engine(tag: &str) -> Arc<AikoqlStorageEngineV2> {
        engine_at(tmp(tag))
    }

    fn engine_at(dir: PathBuf) -> Arc<AikoqlStorageEngineV2> {
        let mut cfg = Config::new(dir);
        cfg.retain_log = true;
        cfg.memtable_bytes = 1024; // flushes: the archive reads across truncations
        Arc::new(AikoqlStorageEngineV2::open_with_config(cfg).unwrap())
    }

    fn put(e: &AikoqlStorageEngineV2, from: u32, to: u32) {
        for i in from..to {
            e.db()
                .put(format!("k{i:04}").as_bytes(), &[i as u8; 64])
                .unwrap();
        }
    }

    fn rows(dir: &std::path::Path) -> Vec<(Vec<u8>, Vec<u8>)> {
        Db::open(Config::new(dir.to_path_buf()))
            .unwrap()
            .scan(b"")
            .unwrap()
    }

    fn store_contract(s: &dyn ObjectStore) {
        s.put("a/1", b"one").unwrap();
        s.put("a/2", b"two").unwrap();
        s.put("b/1", b"other").unwrap();
        assert_eq!(s.list("a/").unwrap(), ["a/1", "a/2"]);
        assert_eq!(s.get("a/2").unwrap(), b"two");
        assert!(
            s.get("a/3").is_err(),
            "a missing key must not read as empty"
        );
        s.put("a/2", b"two again").unwrap();
        assert_eq!(s.get("a/2").unwrap(), b"two again");
    }

    #[test]
    fn a_dir_store_keeps_keys() {
        store_contract(&DirStore::new(tmp("dirstore")));
    }

    #[test]
    fn an_s3_store_keeps_keys_under_its_prefix() {
        let (endpoint, keys) = fake_s3();
        let s3 = crate::s3::S3::new(
            Some(endpoint),
            "bkt".into(),
            "us-east-1".into(),
            crate::s3::Creds {
                akid: "AKIDTEST".into(),
                secret: "secret".into(),
                token: None,
            },
            Duration::from_secs(5),
        );
        store_contract(&S3Store::new(s3, "cluster-1".into()));
        let mut stored: Vec<String> = keys.lock().unwrap().keys().cloned().collect();
        stored.sort();
        assert_eq!(stored, ["cluster-1/a/1", "cluster-1/a/2", "cluster-1/b/1"]);
    }

    #[test]
    fn list_xml_parses_keys_and_the_continuation() {
        let xml = "<ListBucketResult><IsTruncated>true</IsTruncated>\
            <Contents><Key>p/a&amp;b</Key></Contents><Contents><Key>p/c</Key></Contents>\
            <NextContinuationToken>tok/1=</NextContinuationToken></ListBucketResult>";
        assert_eq!(
            parse_list(xml),
            (
                vec!["p/a&b".to_string(), "p/c".to_string()],
                Some("tok/1=".to_string())
            )
        );
        let last = "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>";
        assert_eq!(parse_list(last), (vec![], None));
    }

    /// The round trip: archive a live db (base snapshot, then log passes
    /// across flush truncations), restore to the latest seq and to a seq
    /// in the middle — each byte-equal to the db at that seq.
    #[test]
    fn archive_then_restore_to_the_latest_and_to_a_seq() {
        let e = engine("rt");
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(tmp("rt-store")));
        let acked = Arc::new(Acked::default());
        put(&e, 0, 50);
        let source: Arc<dyn StorageAdminApi> = e.clone();
        let mut a =
            Archiver::start(store.clone(), source, acked.clone(), 1, tmp("rt-scratch")).unwrap();
        put(&e, 50, 100);
        assert_eq!(a.pass().unwrap(), e.db().last_seq());
        let (mid, mid_rows) = (e.db().last_seq(), e.db().scan(b"").unwrap());
        put(&e, 100, 150);
        assert_eq!(a.pass().unwrap(), e.db().last_seq());

        let latest = tmp("rt-latest");
        let (seq, term) = restore(&*store, &latest, Until::Latest, &tmp("rt-s1")).unwrap();
        assert_eq!((seq, term), (e.db().last_seq(), 1));
        assert_eq!(rows(&latest), e.db().scan(b"").unwrap());

        let at_mid = tmp("rt-mid");
        let (seq, _) = restore(&*store, &at_mid, Until::Seq(mid), &tmp("rt-s2")).unwrap();
        assert_eq!(seq, mid);
        assert_eq!(rows(&at_mid), mid_rows);
    }

    /// Point in time: a restore until a wall-clock instant gets exactly
    /// what was archived by then.
    #[test]
    fn restore_until_a_time_stops_at_what_was_archived_by_then() {
        let e = engine("ms");
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(tmp("ms-store")));
        let source: Arc<dyn StorageAdminApi> = e.clone();
        let mut a = Archiver::start(
            store.clone(),
            source,
            Arc::new(Acked::default()),
            1,
            tmp("ms-scratch"),
        )
        .unwrap();
        put(&e, 0, 40);
        a.pass().unwrap();
        let (then, then_rows) = (e.db().last_seq(), e.db().scan(b"").unwrap());
        std::thread::sleep(Duration::from_millis(20));
        let boundary = now_ms();
        std::thread::sleep(Duration::from_millis(20));
        put(&e, 40, 80);
        a.pass().unwrap();
        let out = tmp("ms-out");
        let (seq, _) = restore(&*store, &out, Until::Ms(boundary), &tmp("ms-s")).unwrap();
        assert_eq!(seq, then);
        assert_eq!(rows(&out), then_rows);
    }

    /// Timelines never mix: a new term archives into its own directory
    /// from a fresh snapshot; restore picks the newest term that covers
    /// the target, an older one for a target before the newer began.
    #[test]
    fn each_term_archives_its_own_timeline() {
        let e = engine("tl");
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(tmp("tl-store")));
        let source: Arc<dyn StorageAdminApi> = e.clone();
        let mut t1 = Archiver::start(
            store.clone(),
            source.clone(),
            Arc::new(Acked::default()),
            1,
            tmp("tl-s1"),
        )
        .unwrap();
        put(&e, 0, 30);
        t1.pass().unwrap();
        let (early, early_rows) = (e.db().last_seq(), e.db().scan(b"").unwrap());
        put(&e, 30, 40);
        let mut t2 = Archiver::start(
            store.clone(),
            source,
            Arc::new(Acked::default()),
            2,
            tmp("tl-s2"),
        )
        .unwrap();
        put(&e, 40, 60);
        t2.pass().unwrap();
        let keys = store.list("").unwrap();
        assert!(
            keys.iter().any(|k| k.starts_with("term-0000000001/")),
            "{keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.starts_with("term-0000000002/snap/")),
            "{keys:?}"
        );

        let (seq, term) = restore(&*store, &tmp("tl-latest"), Until::Latest, &tmp("tl-x")).unwrap();
        assert_eq!((seq, term), (e.db().last_seq(), 2));
        let out = tmp("tl-early");
        let (seq, term) = restore(&*store, &out, Until::Seq(early), &tmp("tl-y")).unwrap();
        assert_eq!((seq, term), (early, 1));
        assert_eq!(rows(&out), early_rows);
    }

    /// The archive holds log release: frames it has not uploaded are
    /// never dropped, even with no standby connected.
    #[test]
    fn the_archive_holds_log_release_until_it_uploads() {
        let dir = tmp("hold");
        let e = engine_at(dir.clone());
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(tmp("hold-store")));
        let source: Arc<dyn StorageAdminApi> = e.clone();
        let acked = Arc::new(Acked::default());
        let mut a = Archiver::start(store.clone(), source, acked, 1, tmp("hold-s")).unwrap();
        let base = e.db().last_seq();
        put(&e, 0, 200); // several flushes → several archive files
        assert!(
            e.db().log_since(base, usize::MAX).is_ok(),
            "frames released before upload"
        );
        a.pass().unwrap();
        let files = std::fs::read_dir(dir.join("rlog"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert!(
            files <= 1,
            "uploaded archive files were not released ({files} left)"
        );
    }

    /// Just enough S3 for a key space: PUT, GET, and ListObjectsV2
    /// (list-type=2, prefix) — no pagination.
    type Keys = Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>;

    fn fake_s3() -> (String, Keys) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let keys: Keys = Arc::default();
        let k = keys.clone();
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, target) = (
                    parts.next().unwrap().to_string(),
                    parts.next().unwrap().to_string(),
                );
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let key = path.trim_start_matches("/bkt/").to_string();
                let mut map = k.lock().unwrap();
                let (status, out) = match method.as_str() {
                    "PUT" => {
                        map.insert(key, body);
                        ("200 OK", Vec::new())
                    }
                    "GET" if query.contains("list-type=2") => {
                        let prefix = query
                            .split('&')
                            .find_map(|p| p.strip_prefix("prefix="))
                            .unwrap_or("")
                            .replace("%2F", "/");
                        let mut ks: Vec<&String> =
                            map.keys().filter(|k| k.starts_with(&prefix)).collect();
                        ks.sort();
                        let items: String = ks
                            .iter()
                            .map(|k| format!("<Contents><Key>{k}</Key></Contents>"))
                            .collect();
                        ("200 OK", format!("<ListBucketResult><IsTruncated>false</IsTruncated>{items}</ListBucketResult>").into_bytes())
                    }
                    _ => match map.get(&key) {
                        Some(b) => ("200 OK", b.clone()),
                        None => (
                            "404 Not Found",
                            b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
                        ),
                    },
                };
                drop(map);
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    out.len()
                );
                let _ = conn.write_all(head.as_bytes());
                let _ = conn.write_all(&out);
            }
        });
        (endpoint, keys)
    }

    /// A real S3-compatible store (see ha.rs `real_s3`), under a fresh prefix.
    fn real_s3_store(tag: &str) -> Option<S3Store> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let s3 = crate::s3::S3::new(
            Some(env("AIKOQL_TEST_S3_ENDPOINT")?),
            env("AIKOQL_TEST_S3_BUCKET")?,
            "us-east-1".into(),
            crate::s3::Creds {
                akid: env("AWS_ACCESS_KEY_ID")?,
                secret: env("AWS_SECRET_ACCESS_KEY")?,
                token: env("AWS_SESSION_TOKEN"),
            },
            Duration::from_secs(30),
        );
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Some(S3Store::new(
            s3,
            format!("it-{tag}-{}-{ns}", std::process::id()),
        ))
    }

    #[test]
    fn real_s3_store_keeps_keys() {
        let Some(s) = real_s3_store("keys") else {
            eprintln!("skipped: AIKOQL_TEST_S3_ENDPOINT unset");
            return;
        };
        store_contract(&s);
    }

    /// The archive on a real store: base snapshot + log batches up,
    /// listed back (ListObjectsV2) and restored byte-equal.
    #[test]
    fn real_s3_archive_round_trips() {
        let Some(s) = real_s3_store("archive") else {
            eprintln!("skipped: AIKOQL_TEST_S3_ENDPOINT unset");
            return;
        };
        let store: Arc<dyn ObjectStore> = Arc::new(s);
        let e = engine("real-s3");
        put(&e, 0, 50);
        let source: Arc<dyn StorageAdminApi> = e.clone();
        let mut a = Archiver::start(
            store.clone(),
            source,
            Arc::new(Acked::default()),
            1,
            tmp("real-s3-scratch"),
        )
        .unwrap();
        put(&e, 50, 120);
        assert_eq!(a.pass().unwrap(), e.db().last_seq());
        let out = tmp("real-s3-out");
        let (seq, _) = restore(&*store, &out, Until::Latest, &tmp("real-s3-r")).unwrap();
        assert_eq!(seq, e.db().last_seq());
        assert_eq!(rows(&out), e.db().scan(b"").unwrap());
    }
}
