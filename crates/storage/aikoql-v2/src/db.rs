//! SE2-M2/M6 — Db: WAL → memtable → flush → segment (design §7–§10, §19).
//!
//! Commit pipeline (the KSE-13 120a order, ported): assign seq → append
//! WAL frame → durability boundary → apply memtable → ack. One frame =
//! one batch = one sequence number.
//!
//! Durability modes (§7): Sync is the default and fsyncs every batch;
//! Async skips the durability boundary. GroupCommit (SE2-M6) runs a
//! committer thread: batches submitted through `Db::writer()` handles
//! queue up and commit as groups — one fsync per group, bounded by
//! max_batch_ops / max_batch_bytes / max_wait_duration — applied and
//! acked in submission order (ack only after apply, so acked == durable
//! AND visible). Sync mode remains the correctness baseline: its WAL
//! bytes are what group commit must reproduce exactly. No mode may
//! silently downgrade — Sync is the Default.
//!
//! One-writer policy (§19): `LOCK` holds an OS file lock for the database
//! lifetime; a second open fails closed (FormatError::Locked).
//!
//! Flush is synchronous in M2 — deterministic correctness over lock-free
//! sophistication, the doc's own call. rotate() makes the active memtable
//! immutable (reads keep seeing it); flush() publishes each immutable as a
//! segment. Publication order makes every crash window recoverable:
//! segment files → manifest → CURRENT → WAL truncate. A crash before the
//! manifest leaves orphan segments plus the full WAL (replay recovers);
//! before CURRENT the old pair is consistent; after CURRENT the replay of
//! the not-yet-truncated WAL is idempotent (same (key, seq) → same value).
//!
//! Drop does NOT flush — recovery is the WAL's job.

use crate::cache::{BlockCache, CacheStats};
use crate::compaction::{merge, CompactStats, KeepAll, RetentionPolicy};
use crate::format::{
    checksum8, verify_pair, Current, FormatError, Manifest, SegmentRecord, FORMAT_VERSION,
};
use crate::memtable::Memtable;
use crate::segment::{SegmentEntry, SegmentReader, SegmentWriter, FLAG_DELETE, FLAG_PUT};
use crate::wal::{encode_frame, replay_frames, Op};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

pub const LOCK_FILE: &str = "LOCK";
pub const WAL_FILE: &str = "WAL-000001.log";

const DEFAULT_MEMTABLE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_BLOCK_TARGET: usize = 64 * 1024;
const DEFAULT_GROUP_BATCH_OPS: usize = 4096;
const DEFAULT_GROUP_BATCH_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_CACHE_BYTES: usize = 8 * 1024 * 1024;

pub fn manifest_path(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("MANIFEST-{generation:06}"))
}

pub fn segment_path(dir: &Path, segment_id: u64) -> PathBuf {
    dir.join(format!("SEGMENT-{segment_id:06}.seg"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurabilityMode {
    #[default]
    Sync,
    GroupCommit,
    Async,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub dir: PathBuf,
    pub memtable_bytes: usize,
    pub block_target: usize,
    pub durability: DurabilityMode,
    /// Group commit caps (SE2-M6): a group never exceeds these (a single
    /// batch larger than a cap commits alone) and waits at most
    /// `max_wait_duration` for company — ZERO commits as soon as the
    /// queue has what it has.
    pub max_batch_ops: usize,
    pub max_batch_bytes: usize,
    pub max_wait_duration: Duration,
    /// SE2-M7 — decoded block cache cap in bytes; 0 disables. The cache
    /// only skips repeat block reads — it can never change an answer
    /// (pinned by `cache_never_changes_answers`).
    pub cache_bytes: usize,
}

impl Config {
    pub fn new(dir: PathBuf) -> Self {
        Config {
            dir,
            memtable_bytes: DEFAULT_MEMTABLE_BYTES,
            block_target: DEFAULT_BLOCK_TARGET,
            durability: DurabilityMode::default(),
            max_batch_ops: DEFAULT_GROUP_BATCH_OPS,
            max_batch_bytes: DEFAULT_GROUP_BATCH_BYTES,
            max_wait_duration: Duration::ZERO,
            cache_bytes: DEFAULT_CACHE_BYTES,
        }
    }
}

#[derive(Debug)]
struct State {
    active: Memtable,
    immutables: Vec<Memtable>,
    segments: Vec<SegmentReader>, // manifest order, oldest first
    segment_records: Vec<SegmentRecord>,
    next_seq: u64,
    next_segment_id: u64,
    generation: u64,
}

/// One queued batch waiting on its group: the ops plus the ack channel
/// (a fresh bounded(1) per batch — std has no oneshot).
type Batch = (Vec<Op>, mpsc::SyncSender<Result<u64, FormatError>>);

pub struct Db {
    config: Config,
    /// Held forever — the OS lock (dropping the file releases it).
    _lock: File,
    /// Append-only handle; truncated at each flush publication. Shared:
    /// in GroupCommit mode the committer thread appends and flush may
    /// truncate — one mutex, always taken alone (never nested), so a
    /// flush can never interleave a group's append-and-apply window.
    wal: Arc<Mutex<File>>,
    state: Arc<RwLock<State>>,
    /// GroupCommit mode only: the Db's own sender (dropping it makes the
    /// queue disconnect and lets the committer exit) and the committer
    /// thread itself, joined on drop.
    queue_tx: Option<mpsc::Sender<Batch>>,
    committer: Option<std::thread::JoinHandle<()>>,
    /// Commit fsyncs so far — one per batch (Sync) or one per group
    /// (GroupCommit); flush truncation syncs are not counted.
    fsyncs: Arc<AtomicU64>,
    /// SE2-M7 — shared block cache (None when cache_bytes = 0). Readers
    /// consult and feed it; it never changes an answer.
    cache: Option<Arc<BlockCache>>,
}

impl Db {
    pub fn open(config: Config) -> Result<Db, FormatError> {
        let lock = lock_directory(&config.dir)?;
        let current_path = config.dir.join("CURRENT");
        let current = match Current::read(&current_path) {
            Ok(c) => c,
            Err(FormatError::Io(_)) => {
                // Fresh database: publish the empty pair FIRST, so the
                // manifest always exists before the WAL can record a batch.
                let manifest = Manifest {
                    format_version: FORMAT_VERSION,
                    generation: 1,
                    segments: vec![],
                    wal_ids: vec![],
                };
                Manifest::publish(&manifest_path(&config.dir, 1), &manifest)?;
                let current = Current::new(FORMAT_VERSION, 1);
                Current::publish(&current_path, &current)?;
                current
            }
            Err(e) => return Err(e),
        };
        let manifest = Manifest::read(&manifest_path(&config.dir, current.manifest_generation))?;
        verify_pair(&current, &manifest)?;
        // Orphan segments (a crash between segment publication and
        // manifest/CURRENT, or compaction leftovers): reported and ignored.
        // They are unreferenced data — a later flush may reuse the id,
        // which is safe because nothing references the orphan.
        for id in orphan_segments(&config.dir, &manifest) {
            eprintln!("aikoql-v2: orphan segment SEGMENT-{id:06}.seg ignored (not in manifest)");
        }

        // Referenced segments must open (fail closed on missing/corrupt).
        // SE2-M7: when the block cache is on, every reader the Db opens
        // shares it (reopened segments get a fresh identity — the cache is
        // per-Db, in-memory).
        let cache = (config.cache_bytes > 0).then(|| BlockCache::new(config.cache_bytes));
        let mut segments = Vec::with_capacity(manifest.segments.len());
        for rec in &manifest.segments {
            let path = segment_path(&config.dir, rec.segment_id);
            let reader = match &cache {
                Some(c) => SegmentReader::open_with_cache(&path, Arc::clone(c))?,
                None => SegmentReader::open(&path)?,
            };
            segments.push(reader);
        }

        // Replay the active WAL (create it if this is the first open).
        // No append mode: on Windows FILE_APPEND_DATA handles cannot
        // SetEndOfFile, and flush truncates the WAL. Single-writer is
        // enforced anyway (OS lock + &mut self), so writes seek to the end.
        let wal_path = config.dir.join(WAL_FILE);
        let mut wal = OpenOptions::new()
            .create(true)
            .truncate(false) // the WAL holds acked batches — never truncate on open
            .read(true)
            .write(true)
            .open(&wal_path)
            .map_err(|e| FormatError::Io(format!("open WAL {}: {e}", wal_path.display())))?;
        let mut wal_bytes = Vec::new();
        wal.seek(SeekFrom::Start(0))
            .map_err(|e| FormatError::Io(format!("WAL seek: {e}")))?;
        wal.read_to_end(&mut wal_bytes)
            .map_err(|e| FormatError::Io(format!("WAL read: {e}")))?;
        let (frames, consumed) = replay_frames(&wal_bytes)?;
        if consumed != wal_bytes.len() {
            // torn tail: drop the partial final frame (it was never acked)
            wal.set_len(consumed as u64)
                .map_err(|e| FormatError::Io(format!("WAL truncate: {e}")))?;
            wal.sync_all()
                .map_err(|e| FormatError::Io(format!("WAL sync: {e}")))?;
        }

        // Replay bypasses the durability boundary — the frames are already
        // fsynced — but preserves every sequence number.
        let mut active = Memtable::new();
        let mut replay_max = 0;
        for frame in &frames {
            for op in &frame.ops {
                match op {
                    Op::Put(k, v) => active.apply(k.clone(), frame.seq, Some(v.clone())),
                    Op::Delete(k) => active.apply(k.clone(), frame.seq, None),
                }
            }
            replay_max = replay_max.max(frame.seq);
        }
        let segment_max = manifest
            .segments
            .iter()
            .map(|s| s.seq_hi)
            .max()
            .unwrap_or(0);
        let next_seq = replay_max.max(segment_max) + 1;
        let next_segment_id = manifest
            .segments
            .iter()
            .map(|s| s.segment_id)
            .max()
            .unwrap_or(0)
            + 1;

        let wal = Arc::new(Mutex::new(wal));
        let state = Arc::new(RwLock::new(State {
            active,
            immutables: vec![],
            segments,
            segment_records: manifest.segments,
            next_seq,
            next_segment_id,
            generation: manifest.generation,
        }));
        let fsyncs = Arc::new(AtomicU64::new(0));
        let (queue_tx, committer) = if config.durability == DurabilityMode::GroupCommit {
            let (tx, rx) = mpsc::channel();
            let handle = {
                let wal = Arc::clone(&wal);
                let state = Arc::clone(&state);
                let config = config.clone();
                let fsyncs = Arc::clone(&fsyncs);
                let cache = cache.clone();
                std::thread::spawn(move || committer_loop(rx, wal, state, config, fsyncs, cache))
            };
            (Some(tx), Some(handle))
        } else {
            (None, None)
        };
        Ok(Db {
            config,
            _lock: lock,
            wal,
            state,
            queue_tx,
            committer,
            fsyncs,
            cache,
        })
    }

    /// One batch, one sequence (design refinement: sequence is per-batch).
    /// GroupCommit mode routes through the commit queue — the same
    /// pipeline, executed by the committer thread one group at a time.
    pub fn write(&mut self, ops: &[Op]) -> Result<u64, FormatError> {
        if ops.is_empty() {
            return Err(FormatError::Invalid("empty write batch".into()));
        }
        if self.config.durability == DurabilityMode::GroupCommit {
            return self.writer()?.write(ops);
        }
        let seq = {
            let mut state = self.state.write().unwrap();
            let seq = state.next_seq;
            state.next_seq += 1;
            seq
        };
        let frame = encode_frame(seq, ops)?;
        {
            let mut wal = self.wal.lock().unwrap();
            wal.seek(SeekFrom::End(0))
                .map_err(|e| FormatError::Io(format!("WAL seek: {e}")))?;
            wal.write_all(&frame)
                .map_err(|e| FormatError::Io(format!("WAL append: {e}")))?;
            if self.config.durability == DurabilityMode::Sync {
                wal.sync_all()
                    .map_err(|e| FormatError::Io(format!("WAL sync: {e}")))?;
                self.fsyncs.fetch_add(1, Ordering::SeqCst);
            }
        }
        let mut state = self.state.write().unwrap();
        for op in ops {
            match op {
                Op::Put(k, v) => state.active.apply(k.clone(), seq, Some(v.clone())),
                Op::Delete(k) => state.active.apply(k.clone(), seq, None),
            }
        }
        if state.active.bytes() >= self.config.memtable_bytes {
            Self::flush_locked_impl(&self.config, &self.wal, &mut state, &self.cache)?;
        }
        Ok(seq)
    }

    /// A shared writer handle for group commit. Only GroupCommit mode has
    /// a commit queue — anything else returns Invalid. Drop every handle
    /// before dropping the Db: the committer exits (and the Db's drop
    /// joins it) only once no sender remains.
    pub fn writer(&self) -> Result<CommitWriter, FormatError> {
        match &self.queue_tx {
            Some(tx) => Ok(CommitWriter { tx: tx.clone() }),
            None => Err(FormatError::Invalid(
                "writer handles require DurabilityMode::GroupCommit".into(),
            )),
        }
    }

    /// Commit fsyncs so far: one per batch (Sync) or one per group
    /// (GroupCommit); none in Async. Flush truncation syncs are not
    /// counted.
    pub fn fsync_count(&self) -> u64 {
        self.fsyncs.load(Ordering::SeqCst)
    }

    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<u64, FormatError> {
        self.write(&[Op::Put(key.to_vec(), value.to_vec())])
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<u64, FormatError> {
        self.write(&[Op::Delete(key.to_vec())])
    }

    /// Newest layer wins: active → immutables → segments (all newest
    /// first). A tombstone in a newer layer shadows an older value.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, FormatError> {
        let state = self.state.read().unwrap();
        if let Some(e) = state.active.get(key) {
            return Ok(e.value.clone());
        }
        for mem in state.immutables.iter().rev() {
            if let Some(e) = mem.get(key) {
                return Ok(e.value.clone());
            }
        }
        for seg in state.segments.iter().rev() {
            // SE2-M7 — bloom pre-check: false positives possible, false
            // negatives never (M1 pin), so skipping a segment the bloom
            // rejects is answer-preserving; it just saves the probe.
            if !seg.bloom_may_contain(key) {
                continue;
            }
            if let Some(e) = seg.get(key)? {
                return Ok(if e.flags & FLAG_DELETE != 0 {
                    None // tombstone: shadow everything older
                } else {
                    Some(e.value)
                });
            }
        }
        Ok(None)
    }

    /// SE2-M7 — block cache metrics; all zeros when the cache is off
    /// (cache_bytes = 0).
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.as_ref().map(|c| c.stats()).unwrap_or_default()
    }

    /// Flush's first half, public so the visibility contract is testable:
    /// the active memtable becomes immutable (reads keep seeing it) and a
    /// fresh active takes new writes. flush() = rotate + publish.
    pub fn rotate(&mut self) {
        let mut state = self.state.write().unwrap();
        if state.active.is_empty() {
            return;
        }
        let fresh = std::mem::take(&mut state.active);
        state.immutables.push(fresh);
    }

    pub fn flush(&mut self) -> Result<(), FormatError> {
        let mut state = self.state.write().unwrap();
        Self::flush_locked_impl(&self.config, &self.wal, &mut state, &self.cache)
    }

    /// Publication order (every crash window recoverable — see module doc):
    /// segment files → manifest → CURRENT → WAL truncate. Shared with the
    /// group-commit committer — it takes the pieces, not the Db.
    fn flush_locked_impl(
        config: &Config,
        wal: &Arc<Mutex<File>>,
        state: &mut State,
        cache: &Option<Arc<BlockCache>>,
    ) -> Result<(), FormatError> {
        if !state.active.is_empty() {
            let fresh = std::mem::take(&mut state.active);
            state.immutables.push(fresh);
        }
        if state.immutables.is_empty() {
            return Ok(());
        }
        let mut new_segments = Vec::with_capacity(state.immutables.len());
        for mem in state.immutables.drain(..) {
            let id = state.next_segment_id;
            state.next_segment_id += 1;
            let path = segment_path(&config.dir, id);
            let mut writer = SegmentWriter::new(config.block_target);
            for (key, seq, e) in mem.entries() {
                let flags = if e.value.is_some() {
                    FLAG_PUT
                } else {
                    FLAG_DELETE
                };
                writer.push(SegmentEntry {
                    key: key.to_vec(),
                    value: e.value.clone().unwrap_or_default(),
                    seq,
                    flags,
                });
            }
            writer.publish(&path)?;
            let reader = match cache {
                Some(c) => SegmentReader::open_with_cache(&path, Arc::clone(c))?,
                None => SegmentReader::open(&path)?,
            };
            let file_bytes = std::fs::read(&path)
                .map_err(|e| FormatError::Io(format!("read segment {}: {e}", path.display())))?;
            let record = SegmentRecord {
                segment_id: id,
                level: 0,
                key_min: reader.key_min().to_vec(),
                key_max: reader.key_max().to_vec(),
                seq_lo: reader.seq_lo(),
                seq_hi: reader.seq_hi(),
                record_count: reader.entry_count(),
                file_size: file_bytes.len() as u64,
                checksum: u64::from_le_bytes(checksum8(&file_bytes)),
            };
            state.segment_records.push(record);
            new_segments.push(reader);
        }
        state.generation += 1;
        let manifest = Manifest {
            format_version: FORMAT_VERSION,
            generation: state.generation,
            segments: state.segment_records.clone(),
            wal_ids: vec![],
        };
        Manifest::publish(&manifest_path(&config.dir, state.generation), &manifest)?;
        Current::publish(
            &config.dir.join("CURRENT"),
            &Current::new(FORMAT_VERSION, state.generation),
        )?;
        {
            let wal = wal.lock().unwrap();
            wal.set_len(0)
                .map_err(|e| FormatError::Io(format!("WAL truncate: {e}")))?;
            wal.sync_all()
                .map_err(|e| FormatError::Io(format!("WAL sync: {e}")))?;
        }
        state.segments.extend(new_segments);
        Ok(())
    }

    /// SE2-M4 — L0 → L1 compaction: merge ALL segments (L0 + L1) into one
    /// L1 segment, per key only the newest entry survives, a tombstone
    /// drops the key (L1 is the bottom level). Synchronous — deterministic
    /// correctness over a background thread, the doc's own call for flush;
    /// a trigger threshold arrives when measurements justify one.
    /// Publication order mirrors flush (segment → manifest → CURRENT →
    /// delete obsolete) so every crash window recovers the SAME logical
    /// state — compaction is state-preserving. Memtables are not
    /// compaction material: they are newer than every segment and read
    /// first anyway.
    pub fn compact(&mut self) -> Result<CompactStats, FormatError> {
        self.compact_with(&KeepAll)
    }

    /// Compact with a retention policy (SE2-M5): KEEP/DROP/ARCHIVE per
    /// key class. The policy is an input — the caller asserts which rows
    /// are genuinely obsolete (superseded heads, tombstoned keys); the
    /// engine stays key-space-generic. ARCHIVE rows land in
    /// `archive/ARCHIVE-{id:06}.seg` and leave the live key space.
    pub fn compact_with(
        &mut self,
        policy: &dyn RetentionPolicy,
    ) -> Result<CompactStats, FormatError> {
        let mut state = self.state.write().unwrap();
        if state.segments.is_empty() {
            return Ok(CompactStats::default());
        }
        let id = state.next_segment_id;
        state.next_segment_id += 1;
        let archive_id = state.next_segment_id;
        state.next_segment_id += 1;
        let out_path = segment_path(&self.config.dir, id);
        let archive_path = self
            .config
            .dir
            .join("archive")
            .join(format!("ARCHIVE-{archive_id:06}.seg"));
        let (stats, out_reader) = merge(
            &state.segments,
            self.config.block_target,
            &out_path,
            &archive_path,
            policy,
        )?;
        crash_park("AIKOQL_V2_COMPACT_PARK", &self.config.dir, "after_segment");

        let old_paths: Vec<PathBuf> = state
            .segment_records
            .iter()
            .map(|r| segment_path(&self.config.dir, r.segment_id))
            .collect();
        let mut new_records = Vec::new();
        let mut new_segments = Vec::new();
        if let Some(reader) = out_reader {
            // ponytail: whole-file read for the record checksum (the flush
            // idiom) — O(file) at compact time; stream it when the M4
            // measurement says so.
            let file_bytes = std::fs::read(&out_path).map_err(|e| {
                FormatError::Io(format!("read segment {}: {e}", out_path.display()))
            })?;
            new_records.push(SegmentRecord {
                segment_id: id,
                level: 1,
                key_min: reader.key_min().to_vec(),
                key_max: reader.key_max().to_vec(),
                seq_lo: reader.seq_lo(),
                seq_hi: reader.seq_hi(),
                record_count: reader.entry_count(),
                file_size: file_bytes.len() as u64,
                checksum: u64::from_le_bytes(checksum8(&file_bytes)),
            });
            new_segments.push(reader);
        }
        state.generation += 1;
        let manifest = Manifest {
            format_version: FORMAT_VERSION,
            generation: state.generation,
            segments: new_records.clone(),
            wal_ids: vec![],
        };
        Manifest::publish(
            &manifest_path(&self.config.dir, state.generation),
            &manifest,
        )?;
        crash_park("AIKOQL_V2_COMPACT_PARK", &self.config.dir, "after_manifest");
        Current::publish(
            &self.config.dir.join("CURRENT"),
            &Current::new(FORMAT_VERSION, state.generation),
        )?;
        crash_park("AIKOQL_V2_COMPACT_PARK", &self.config.dir, "after_current");

        // Swap readers before deleting: handles open with share-delete, so
        // Windows marks the files delete-pending and any reader that still
        // references an obsolete segment keeps its data alive (the
        // Arc<Segment> lifetime guarantee, via the OS).
        state.segments = new_segments;
        state.segment_records = new_records;
        for p in &old_paths {
            if let Err(e) = std::fs::remove_file(p) {
                // Not fatal: the segment is unreferenced — a leftover is
                // reported and ignored at the next open.
                eprintln!(
                    "aikoql-v2: obsolete segment {} not removed: {e}",
                    p.display()
                );
            }
        }
        Ok(stats)
    }
}

/// SE2-M4 crash-matrix hook: parks forever only when the env names this
/// stage, so the child-kill harness can kill the process mid-compaction
/// and pin the §25 windows. Unset in production — a no-op.
impl Drop for Db {
    fn drop(&mut self) {
        // GroupCommit mode: drop the Db's own sender so the queue
        // disconnects once every CommitWriter handle is gone, let the
        // committer commit what is still pending, and join it — a reopen
        // of the same directory must never race the old committer's last
        // group. (CommitWriter's doc spells out the drop-order rule.)
        self.queue_tx = None;
        if let Some(handle) = self.committer.take() {
            let _ = handle.join();
        }
    }
}

/// Park forever when `var` names this stage — the crash-window harness
/// (no-op unset). The marker file tells the parent the park was reached.
fn crash_park(var: &str, dir: &Path, stage: &str) {
    if std::env::var(var).ok().as_deref() != Some(stage) {
        return;
    }
    std::fs::write(dir.join(stage), b"1").ok();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

/// A shared writer handle (GroupCommit mode): batches submitted through
/// handles queue up and commit as groups — one fsync per group, applied
/// and acked in submission order. Clone cheaply for one handle per
/// writer thread; drop every handle before dropping the Db.
#[derive(Clone)]
pub struct CommitWriter {
    tx: mpsc::Sender<Batch>,
}

impl CommitWriter {
    /// Submit one batch and block until its group commits: the returned
    /// seq is assigned in submission order, and the ack fires only after
    /// the batch is durable (group fsync) AND visible (applied).
    pub fn write(&self, ops: &[Op]) -> Result<u64, FormatError> {
        if ops.is_empty() {
            return Err(FormatError::Invalid("empty write batch".into()));
        }
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.tx
            .send((ops.to_vec(), ack_tx))
            .map_err(|_| FormatError::Io("commit queue closed".into()))?;
        match ack_rx.recv() {
            Ok(result) => result,
            Err(_) => Err(FormatError::Io("commit queue closed".into())),
        }
    }
}

fn batch_ops_of(b: &Batch) -> usize {
    b.0.len()
}

/// The engine's byte accounting for the cap: the sum over ops of
/// key+value bytes (a Delete carries only its key).
fn batch_bytes_of(b: &Batch) -> usize {
    b.0.iter()
        .map(|op| match op {
            Op::Put(k, v) => k.len() + v.len(),
            Op::Delete(k) => k.len(),
        })
        .sum()
}

/// The committer: drain the queue into groups bounded by the caps and
/// the wait window, commit each group with ONE fsync, apply, ack. Exits
/// when every sender is gone and nothing is pending.
fn committer_loop(
    rx: mpsc::Receiver<Batch>,
    wal: Arc<Mutex<File>>,
    state: Arc<RwLock<State>>,
    config: Config,
    fsyncs: Arc<AtomicU64>,
    cache: Option<Arc<BlockCache>>,
) {
    let wait = config.max_wait_duration;
    let mut carry: Option<Batch> = None;
    loop {
        let first = match carry.take().or_else(|| rx.recv().ok()) {
            Some(b) => b,
            None => return, // all senders dropped, nothing pending
        };
        let mut group = vec![first];
        let deadline = Instant::now() + wait;
        loop {
            // Sum over the whole group — groups are small; exact-fit caps.
            let (ops_n, bytes_n) = group.iter().fold((0usize, 0usize), |(o, b), batch| {
                (o + batch_ops_of(batch), b + batch_bytes_of(batch))
            });
            if ops_n >= config.max_batch_ops || bytes_n >= config.max_batch_bytes {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(batch) => {
                    if ops_n + batch_ops_of(&batch) > config.max_batch_ops
                        || bytes_n + batch_bytes_of(&batch) > config.max_batch_bytes
                    {
                        carry = Some(batch); // exact fit: leads the next group
                        break;
                    }
                    group.push(batch);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        commit_group(&group, &wal, &state, &config, &fsyncs, &cache);
    }
}

/// Commit one group: assign seqs, append every frame, ONE fsync, apply,
/// ack — all under one state write-lock (the same exclusivity Sync's
/// write() gets via &mut self), so a flush can never interleave the
/// append-and-apply window. Lock order is always state → wal, and the
/// wal lock is never held across a flush.
fn commit_group(
    group: &[Batch],
    wal: &Arc<Mutex<File>>,
    state: &Arc<RwLock<State>>,
    config: &Config,
    fsyncs: &Arc<AtomicU64>,
    cache: &Option<Arc<BlockCache>>,
) {
    let mut st = state.write().unwrap();
    let mut seqs: Vec<u64> = Vec::with_capacity(group.len());
    let mut outcome: Result<(), FormatError> = Ok(());
    {
        let mut wal = wal.lock().unwrap();
        for (ops, _) in group {
            let seq = st.next_seq;
            st.next_seq += 1;
            seqs.push(seq);
            let frame = match encode_frame(seq, ops) {
                Ok(f) => f,
                Err(e) => {
                    outcome = Err(e);
                    break;
                }
            };
            let appended = wal
                .seek(SeekFrom::End(0))
                .and_then(|_| wal.write_all(&frame));
            if let Err(e) = appended {
                outcome = Err(FormatError::Io(format!("WAL append: {e}")));
                break;
            }
        }
        if outcome.is_ok() {
            if let Err(e) = wal.sync_all() {
                outcome = Err(FormatError::Io(format!("WAL sync: {e}")));
            }
        }
    }
    if outcome.is_ok() {
        fsyncs.fetch_add(1, Ordering::SeqCst);
    }
    crash_park("AIKOQL_V2_GROUP_PARK", &config.dir, "after_fsync");
    if outcome.is_ok() {
        for ((ops, _), seq) in group.iter().zip(&seqs) {
            for op in ops {
                match op {
                    Op::Put(k, v) => st.active.apply(k.clone(), *seq, Some(v.clone())),
                    Op::Delete(k) => st.active.apply(k.clone(), *seq, None),
                }
            }
        }
        if st.active.bytes() >= config.memtable_bytes {
            if let Err(e) = Db::flush_locked_impl(config, wal, &mut st, cache) {
                outcome = Err(e);
            }
        }
    }
    crash_park("AIKOQL_V2_GROUP_PARK", &config.dir, "after_apply");
    drop(st);
    for ((_, ack_tx), seq) in group.iter().zip(&seqs) {
        let _ = ack_tx.send(outcome.clone().map(|()| *seq));
    }
    crash_park("AIKOQL_V2_GROUP_PARK", &config.dir, "after_ack");
}

/// SEGMENT-*.seg files the manifest does not reference. Reported and
/// ignored at open — unreferenced data; a future flush may reuse the id,
/// which is safe because nothing references the orphan.
pub fn orphan_segments(dir: &Path, manifest: &Manifest) -> Vec<u64> {
    let mut orphans = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return orphans,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(stem) = name
            .strip_prefix("SEGMENT-")
            .and_then(|s| s.strip_suffix(".seg"))
        else {
            continue;
        };
        let Ok(id) = stem.parse::<u64>() else {
            continue;
        };
        if !manifest.segments.iter().any(|r| r.segment_id == id) {
            orphans.push(id);
        }
    }
    orphans.sort_unstable();
    orphans
}

fn lock_directory(dir: &Path) -> Result<File, FormatError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| FormatError::Io(format!("create {}: {e}", dir.display())))?;
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false) // the lock file is just a lock handle — never truncate
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| FormatError::Io(format!("open LOCK {}: {e}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(_) => Err(FormatError::Locked(format!(
            "database directory is held by another process: {}",
            dir.display()
        ))),
    }
}
