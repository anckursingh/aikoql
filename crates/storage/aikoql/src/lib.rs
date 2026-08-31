//! AIKOQL-native storage engine (MRFC-KSE-001).
//!
//! Experimental backend behind the kernel's `StorageEngine` trait — never the
//! production default unless the measured adoption gate passes (TDD doc §29).
//!
//! KSE-1 skeleton: an append-only write-ahead log over the kernel's
//! `MemoryEngine` reference semantics. Each batch is serialized to one
//! enveloped log record (magic/format-version/checksum — KSE-3), fsynced,
//! then applied to the in-memory map — durable before visible,
//! all-or-nothing. Open replays the log; a torn tail record (crash
//! mid-append) is truncated, corruption fails closed.
//! ponytail: the log grows unbounded (no compaction/checkpoint) — KSE-4
//! brings the block format that makes compaction possible.

use aikoql_kernel::knowledge::kom::{KError, KResult};
use aikoql_kernel::storage::store::{MemoryEngine, StorageEngine, WriteBatch};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;

mod envelope;

fn se(e: impl std::fmt::Display) -> KError {
    KError::Store(format!("aikoql-storage: {}", e))
}

fn corrupt(what: &str) -> KError {
    KError::Store(format!("aikoql-storage: corrupt log: {}", what))
}

fn poisoned() -> KError {
    KError::Store("aikoql-storage: log lock poisoned".into())
}

/// AIKOQL-native engine: WAL file + in-memory sorted map.
pub struct AikoqlStorageEngine {
    log: Mutex<File>,
    mem: MemoryEngine,
}

// --- batch payload codec (inside the envelope; see envelope.rs) ---
//
// Payload: [u16 n_puts] (u32 klen, k, u32 vlen, v)* [u16 n_dels] (u32 klen, k)*

fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn encode_batch(batch: &WriteBatch) -> Vec<u8> {
    let mut p = Vec::new();
    push_u16(&mut p, batch.puts.len() as u16);
    for (k, v) in &batch.puts {
        push_u32(&mut p, k.len() as u32);
        p.extend_from_slice(k);
        push_u32(&mut p, v.len() as u32);
        p.extend_from_slice(v);
    }
    push_u16(&mut p, batch.dels.len() as u16);
    for k in &batch.dels {
        push_u32(&mut p, k.len() as u32);
        p.extend_from_slice(k);
    }
    p
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> KResult<&'a [u8]> {
        if self.b.len() - self.pos < n {
            return Err(corrupt("record truncated"));
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u16(&mut self) -> KResult<u16> {
        let raw: [u8; 2] = self.take(2)?.try_into().unwrap();
        Ok(u16::from_le_bytes(raw))
    }

    fn u32(&mut self) -> KResult<u32> {
        let raw: [u8; 4] = self.take(4)?.try_into().unwrap();
        Ok(u32::from_le_bytes(raw))
    }
}

fn decode_batch(payload: &[u8]) -> KResult<WriteBatch> {
    let mut c = Cursor { b: payload, pos: 0 };
    let mut batch = WriteBatch::new();
    for _ in 0..c.u16()? {
        let klen = c.u32()? as usize;
        let k = c.take(klen)?.to_vec();
        let vlen = c.u32()? as usize;
        let v = c.take(vlen)?.to_vec();
        batch.put(k, v);
    }
    for _ in 0..c.u16()? {
        let klen = c.u32()? as usize;
        batch.del(c.take(klen)?.to_vec());
    }
    if c.pos != payload.len() {
        return Err(corrupt("trailing bytes in record"));
    }
    Ok(batch)
}

/// Replay `bytes` into a fresh map; returns the offset of the last complete
/// record. A torn tail (crash mid-append) is left out — it was never
/// acknowledged to a caller. Corruption (bad magic, checksum mismatch,
/// unknown type/version) fails closed with a deterministic error.
fn replay(bytes: &[u8], mem: &MemoryEngine) -> KResult<usize> {
    let mut pos = 0usize;
    while pos < bytes.len() {
        match envelope::parse_at(bytes, pos)? {
            envelope::ParseOutcome::Complete { payload, end, .. } => {
                mem.write_batch(&decode_batch(&payload)?)?;
                pos = end;
            }
            envelope::ParseOutcome::TornTail => break,
        }
    }
    Ok(pos)
}

impl AikoqlStorageEngine {
    /// Open (or create) a durable store at `path`. Replays the WAL; a torn
    /// tail record is truncated, anything else malformed fails closed.
    pub fn open(path: impl AsRef<Path>) -> KResult<Self> {
        let p = path.as_ref();
        // 1. Read the whole log under a transient read handle. A missing file
        //    is a fresh store.
        let mut bytes = Vec::new();
        match File::open(p) {
            Ok(mut f) => {
                f.read_to_end(&mut bytes)
                    .map_err(|e| se(format!("read: {e}")))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(se(format!("read: {e}"))),
        }
        // 2. Replay into memory.
        let mem = MemoryEngine::new();
        let last_good = replay(&bytes, &mem)?;
        // 3. Drop a torn tail with a transient plain-write handle — on
        //    Windows SetEndOfFile needs FILE_WRITE_DATA, which the append
        //    WAL handle (below) cannot request.
        if last_good != bytes.len() {
            OpenOptions::new()
                .write(true)
                .open(p)
                .and_then(|f| f.set_len(last_good as u64))
                .map_err(|e| se(format!("truncate: {e}")))?;
        }
        // 4. The WAL handle itself: append-only (all reads happened in step 1).
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .map_err(|e| se(format!("open: {e}")))?;
        Ok(AikoqlStorageEngine {
            log: Mutex::new(log),
            mem,
        })
    }
}

impl StorageEngine for AikoqlStorageEngine {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>> {
        self.mem.get(key)
    }

    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.mem.scan(prefix)
    }

    fn write_batch(&self, batch: &WriteBatch) -> KResult<()> {
        if batch.is_empty() {
            return Ok(()); // KSE-005: no state change, no log record
        }
        let payload = encode_batch(batch);
        let record = envelope::encode_record(envelope::TYPE_BATCH, &payload);
        // WAL: the record is durable before the state change is visible.
        let mut log = self.log.lock().map_err(|_| poisoned())?;
        log.write_all(&record).map_err(se)?;
        log.sync_data().map_err(se)?;
        drop(log);
        // MemoryEngine applies puts before dels — the shared KSE-006 semantics.
        self.mem.write_batch(batch)
    }
}
