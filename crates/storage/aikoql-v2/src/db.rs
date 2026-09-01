//! SE2-M2 — Db: WAL → memtable → flush → segment (design §7–§10, §19).
//!
//! Commit pipeline (the KSE-13 120a order, ported): assign seq → append
//! WAL frame → durability boundary → apply memtable → ack. One frame =
//! one batch = one sequence number.
//!
//! Durability modes (§7): Sync is the default and fsyncs every batch;
//! GroupCommit/Async skip the per-batch fsync (the real group-commit
//! machinery is SE2-M6). No mode may silently downgrade — Sync is the
//! Default.
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

use crate::format::{
    checksum8, verify_pair, Current, FormatError, Manifest, SegmentRecord, FORMAT_VERSION,
};
use crate::memtable::Memtable;
use crate::segment::{SegmentEntry, SegmentReader, SegmentWriter, FLAG_DELETE, FLAG_PUT};
use crate::wal::{encode_frame, replay_frames, Op};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

pub const LOCK_FILE: &str = "LOCK";
pub const WAL_FILE: &str = "WAL-000001.log";

const DEFAULT_MEMTABLE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_BLOCK_TARGET: usize = 64 * 1024;

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
}

impl Config {
    pub fn new(dir: PathBuf) -> Self {
        Config {
            dir,
            memtable_bytes: DEFAULT_MEMTABLE_BYTES,
            block_target: DEFAULT_BLOCK_TARGET,
            durability: DurabilityMode::default(),
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

pub struct Db {
    config: Config,
    /// Held forever — the OS lock (dropping the file releases it).
    _lock: File,
    /// Append-only handle; truncated at each flush publication.
    wal: File,
    state: RwLock<State>,
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

        // Referenced segments must open (fail closed on missing/corrupt).
        let mut segments = Vec::with_capacity(manifest.segments.len());
        for rec in &manifest.segments {
            segments.push(SegmentReader::open(&segment_path(
                &config.dir,
                rec.segment_id,
            ))?);
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

        Ok(Db {
            config,
            _lock: lock,
            wal,
            state: RwLock::new(State {
                active,
                immutables: vec![],
                segments,
                segment_records: manifest.segments,
                next_seq,
                next_segment_id,
                generation: manifest.generation,
            }),
        })
    }

    /// One batch, one sequence (design refinement: sequence is per-batch).
    pub fn write(&mut self, ops: &[Op]) -> Result<u64, FormatError> {
        if ops.is_empty() {
            return Err(FormatError::Invalid("empty write batch".into()));
        }
        let seq = {
            let mut state = self.state.write().unwrap();
            let seq = state.next_seq;
            state.next_seq += 1;
            seq
        };
        let frame = encode_frame(seq, ops)?;
        self.wal
            .seek(SeekFrom::End(0))
            .map_err(|e| FormatError::Io(format!("WAL seek: {e}")))?;
        self.wal
            .write_all(&frame)
            .map_err(|e| FormatError::Io(format!("WAL append: {e}")))?;
        if self.config.durability == DurabilityMode::Sync {
            self.wal
                .sync_all()
                .map_err(|e| FormatError::Io(format!("WAL sync: {e}")))?;
        }
        let mut state = self.state.write().unwrap();
        for op in ops {
            match op {
                Op::Put(k, v) => state.active.apply(k.clone(), seq, Some(v.clone())),
                Op::Delete(k) => state.active.apply(k.clone(), seq, None),
            }
        }
        if state.active.bytes() >= self.config.memtable_bytes {
            self.flush_locked(&mut state)?;
        }
        Ok(seq)
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
            if let Some(e) = seg.get(key)? {
                return Ok(Some(e.value));
            }
        }
        Ok(None)
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
        self.flush_locked(&mut state)
    }

    /// Publication order (every crash window recoverable — see module doc):
    /// segment files → manifest → CURRENT → WAL truncate.
    fn flush_locked(&self, state: &mut State) -> Result<(), FormatError> {
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
            let path = segment_path(&self.config.dir, id);
            let mut writer = SegmentWriter::new(self.config.block_target);
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
            let reader = SegmentReader::open(&path)?;
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
        Manifest::publish(
            &manifest_path(&self.config.dir, state.generation),
            &manifest,
        )?;
        Current::publish(
            &self.config.dir.join("CURRENT"),
            &Current::new(FORMAT_VERSION, state.generation),
        )?;
        self.wal
            .set_len(0)
            .map_err(|e| FormatError::Io(format!("WAL truncate: {e}")))?;
        self.wal
            .sync_all()
            .map_err(|e| FormatError::Io(format!("WAL sync: {e}")))?;
        state.segments.extend(new_segments);
        Ok(())
    }
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
