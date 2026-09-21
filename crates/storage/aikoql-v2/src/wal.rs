//! SE2-M2 — WAL v2 frames (design §7: MAGIC, FORMAT_VERSION, FRAME_TYPE,
//! SEQUENCE, PAYLOAD_LENGTH, PAYLOAD, CRC — the checksum is the crate's
//! established sha256-8, not a CRC32).
//!
//! Frame layout (all little-endian):
//! `AKWF | format_version u16 | frame_type u8 | seq u64 | payload_len u32 |
//!  payload | sha256-8(everything before)`
//! payload: `entry_count u32 | per entry: op u8 | key_len u32 | key |
//!  (value_len u32 | value if Put)`
//! op: 1 = Put, 2 = Delete. One frame = one batch = one sequence number
//! (design refinement: sequence is per-batch, not per-op — the kernel
//! commits one atomic batch per transaction).
//!
//! Decode order is magic → version → type → checksum, so an
//! unknown-but-clean version or frame type is Unsupported (a newer-format
//! file is not damaged); damaged bytes are Corrupt. Replay walks frames
//! with the KSE-082B classifier shape: a failure with nothing valid after
//! it is a torn tail (kill mid-append — the caller truncates there);
//! damage followed by a valid frame fails closed.

use crate::format::{checksum8, Cursor, FormatError};
use crate::identity::{LogicalId, ObjectId, ReplicaId};
use std::io::{Read, Seek, SeekFrom};

pub const WAL_FORMAT_VERSION: u16 = 1;
pub const FRAME_BATCH: u8 = 1;
pub const OP_PUT: u8 = 1;
pub const OP_DELETE: u8 = 2;
/// SE2-M30 — allocate a new object identity (spec §14): the frame carries
/// the (ObjectId, LogicalId, ReplicaId) triple; replay and the live apply
/// both rebuild the identity directories from it. SE2-M32 — plus the
/// placement generation: the op is self-describing, so a replayed create
/// reproduces the EXACT placement record the live apply produced (the
/// merge gate needs it — a re-derived generation would regress a flushed
/// Segment placement back to Memtable in the §24 state-D window).
pub const OP_CREATE_OBJECT: u8 = 3;
/// SE2-M33 — an object put/delete carries the ReplicaId (spec §17/§18: rid
/// is the persisted relocation handle), so replay restores the memtable
/// entry's identity without consulting the directory maps. Payload after
/// the op byte: `rid u64 | key_len u32 | key | (value_len u32 | value)`.
pub const OP_PUT_OBJECT: u8 = 4;
pub const OP_DELETE_OBJECT: u8 = 5;

const WAL_MAGIC: &[u8; 4] = b"AKWF";
const FRAME_HEADER_LEN: usize = 19; // magic 4 + version 2 + type 1 + seq 8 + payload_len 4

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    /// `oid 16 | lid 8 | rid 8 | pgen 8` after the op byte — fixed width,
    /// no length prefixes (a CreateObject has no key or value). `pgen` is
    /// the placement generation the live apply assigned (SE2-M32), so
    /// replay reproduces the exact placement record.
    CreateObject {
        oid: ObjectId,
        lid: LogicalId,
        rid: ReplicaId,
        pgen: u64,
    },
    /// SE2-M33 — write a value under the object's own ReplicaId (§14 write
    /// path): the memtable entry carries the rid, the identity read path
    /// filters on it.
    PutObject(ReplicaId, Vec<u8>, Vec<u8>),
    /// SE2-M33 — tombstone carrying the owning rid (§16): identity
    /// metadata survives the delete.
    DeleteObject(ReplicaId, Vec<u8>),
}

impl Op {
    pub fn key(&self) -> &[u8] {
        match self {
            Op::Put(k, _) | Op::Delete(k) | Op::PutObject(_, k, _) | Op::DeleteObject(_, k) => k,
            Op::CreateObject { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalFrame {
    pub seq: u64,
    pub ops: Vec<Op>,
}

/// PERF-4 — the encoded byte length of one op (the `encoded_len` idiom the
/// directory records use). One definition of the layout lengths, so an
/// under-counted arm shows up as a realloc in the pin instead of a
/// truncated frame.
fn op_encoded_len(op: &Op) -> usize {
    match op {
        Op::Put(k, v) => 1 + 4 + k.len() + 4 + v.len(),
        Op::Delete(k) => 1 + 4 + k.len(),
        Op::CreateObject { .. } => 1 + 16 + 8 + 8 + 8,
        Op::PutObject(_, k, v) => 1 + 8 + 4 + k.len() + 4 + v.len(),
        Op::DeleteObject(_, k) => 1 + 8 + 4 + k.len(),
    }
}

pub fn encode_frame(seq: u64, ops: &[Op]) -> Result<Vec<u8>, FormatError> {
    if ops.is_empty() {
        return Err(FormatError::Invalid("WAL frame with no ops".into()));
    }
    // PERF-4 — one allocation, sized exactly: payload = entry_count u32 +
    // the ops, then the frame is the header + payload + checksum written
    // straight into the final buffer (no intermediate payload Vec to copy).
    // Byte-identical to the old two-buffer build (wal_golden pins it).
    let payload_len = 4 + ops.iter().map(op_encoded_len).sum::<usize>();
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload_len + 8);
    frame.extend_from_slice(WAL_MAGIC);
    frame.extend_from_slice(&WAL_FORMAT_VERSION.to_le_bytes());
    frame.push(FRAME_BATCH);
    frame.extend_from_slice(&seq.to_le_bytes());
    frame.extend_from_slice(&(payload_len as u32).to_le_bytes());
    frame.extend_from_slice(&(ops.len() as u32).to_le_bytes());
    for op in ops {
        match op {
            Op::Put(k, v) => {
                frame.push(OP_PUT);
                frame.extend_from_slice(&(k.len() as u32).to_le_bytes());
                frame.extend_from_slice(k);
                frame.extend_from_slice(&(v.len() as u32).to_le_bytes());
                frame.extend_from_slice(v);
            }
            Op::Delete(k) => {
                frame.push(OP_DELETE);
                frame.extend_from_slice(&(k.len() as u32).to_le_bytes());
                frame.extend_from_slice(k);
            }
            Op::CreateObject {
                oid,
                lid,
                rid,
                pgen,
            } => {
                frame.push(OP_CREATE_OBJECT);
                frame.extend_from_slice(oid.as_bytes());
                frame.extend_from_slice(&lid.to_bytes());
                frame.extend_from_slice(&rid.to_bytes());
                frame.extend_from_slice(&pgen.to_le_bytes());
            }
            Op::PutObject(rid, k, v) => {
                frame.push(OP_PUT_OBJECT);
                frame.extend_from_slice(&rid.to_bytes());
                frame.extend_from_slice(&(k.len() as u32).to_le_bytes());
                frame.extend_from_slice(k);
                frame.extend_from_slice(&(v.len() as u32).to_le_bytes());
                frame.extend_from_slice(v);
            }
            Op::DeleteObject(rid, k) => {
                frame.push(OP_DELETE_OBJECT);
                frame.extend_from_slice(&rid.to_bytes());
                frame.extend_from_slice(&(k.len() as u32).to_le_bytes());
                frame.extend_from_slice(k);
            }
        }
    }
    frame.extend_from_slice(&checksum8(&frame));
    Ok(frame)
}

/// Decode ONE frame from the front of `bytes`; returns the frame and its
/// encoded length. Bytes after the frame are not an error here —
/// `replay_frames` owns the stream-level judgement (torn tail vs damage).
pub fn decode_frame(bytes: &[u8]) -> Result<(WalFrame, usize), FormatError> {
    let mut cur = Cursor::new(bytes);
    if cur.take(4)? != WAL_MAGIC {
        return Err(FormatError::Corrupt("WAL frame bad magic".into()));
    }
    let version = cur.u16()?;
    if version != WAL_FORMAT_VERSION {
        return Err(FormatError::Unsupported(format!(
            "WAL frame format version {version} (this build: {WAL_FORMAT_VERSION})"
        )));
    }
    let frame_type = cur.u8()?;
    if frame_type != FRAME_BATCH {
        return Err(FormatError::Unsupported(format!(
            "WAL frame type {frame_type} (this build: {FRAME_BATCH})"
        )));
    }
    let seq = cur.u64()?;
    let payload_len = cur.u32()? as usize;
    let payload = cur.take(payload_len)?; // bounds-checked: an impossible
                                          // length fails here, before the
                                          // checksum (it IS truncation)
    let stored_ck = cur.take(8)?;
    let total = FRAME_HEADER_LEN + payload_len + 8;
    if checksum8(&bytes[..total - 8]) != stored_ck {
        return Err(FormatError::Corrupt("WAL frame checksum mismatch".into()));
    }

    let ops = decode_payload(payload)?;
    Ok((WalFrame { seq, ops }, total))
}

/// M28 (P1-01) — walk the op entries of a frame payload, decoding each op
/// straight into `sink` — the shared tail of `decode_frame` (collecting),
/// `replay_reader` (straight into the apply callback, no per-frame
/// Vec<Op>), and `validate_frame_at` (via the collecting wrapper). One
/// definition keeps every path byte-identical.
fn walk_payload(
    payload: &[u8],
    mut sink: impl FnMut(Op) -> Result<(), FormatError>,
) -> Result<(), FormatError> {
    let mut pcur = Cursor::new(payload);
    let count = pcur.u32()? as usize;
    if count == 0 {
        return Err(FormatError::Corrupt("WAL frame with zero entries".into()));
    }
    for _ in 0..count {
        let op = pcur.u8()?;
        match op {
            OP_PUT => {
                let key = pcur.vec()?;
                let value = pcur.vec()?;
                sink(Op::Put(key, value))?;
            }
            OP_DELETE => {
                let key = pcur.vec()?;
                sink(Op::Delete(key))?;
            }
            OP_CREATE_OBJECT => {
                let oid = ObjectId::from_bytes(pcur.take(16)?.try_into().expect("16-byte slice"));
                let lid = LogicalId::from_bytes(pcur.take(8)?.try_into().expect("8-byte slice"));
                let rid = ReplicaId::from_bytes(pcur.take(8)?.try_into().expect("8-byte slice"));
                let pgen = pcur.u64()?;
                sink(Op::CreateObject {
                    oid,
                    lid,
                    rid,
                    pgen,
                })?;
            }
            OP_PUT_OBJECT => {
                let rid = ReplicaId::from_bytes(pcur.take(8)?.try_into().expect("8-byte slice"));
                let key = pcur.vec()?;
                let value = pcur.vec()?;
                sink(Op::PutObject(rid, key, value))?;
            }
            OP_DELETE_OBJECT => {
                let rid = ReplicaId::from_bytes(pcur.take(8)?.try_into().expect("8-byte slice"));
                let key = pcur.vec()?;
                sink(Op::DeleteObject(rid, key))?;
            }
            other => {
                return Err(FormatError::Unsupported(format!("WAL op byte {other}")));
            }
        }
    }
    if !pcur.is_empty() {
        return Err(FormatError::Corrupt("WAL payload trailing bytes".into()));
    }
    Ok(())
}

/// Decode the op entries from a frame payload into a Vec — the collecting
/// wrapper over `walk_payload`. The PERF-4 exact-capacity idiom is
/// preserved: the count is peeked from the first 4 bytes for
/// `with_capacity` (a short payload fails inside the walk itself).
fn decode_payload(payload: &[u8]) -> Result<Vec<Op>, FormatError> {
    let count = payload
        .get(..4)
        .map(|b| u32::from_le_bytes(b.try_into().expect("4-byte slice")) as usize)
        .unwrap_or(0);
    let mut ops = Vec::with_capacity(count);
    walk_payload(payload, |op| {
        ops.push(op);
        Ok(())
    })?;
    Ok(ops)
}

/// Walk frames from the start, applying each to `f` as it is decoded —
/// PERF-1: recovery consumes this so the frames are never materialized
/// (peak memory is one frame, not a second copy of the WAL). Returns the
/// byte count of the valid prefix. A decode failure with nothing valid
/// after it is a torn tail (the caller truncates the WAL there); damage
/// followed by a valid frame is Corrupt — the WAL must not be trusted.
/// Sequences must strictly increase.
pub fn replay_frames_streaming<F>(bytes: &[u8], mut f: F) -> Result<usize, FormatError>
where
    F: FnMut(WalFrame) -> Result<(), FormatError>,
{
    let mut pos = 0;
    let mut last_seq: Option<u64> = None;
    while pos < bytes.len() {
        match decode_frame(&bytes[pos..]) {
            Ok((frame, len)) => {
                if let Some(prev) = last_seq {
                    if frame.seq <= prev {
                        return Err(FormatError::Corrupt(format!(
                            "WAL sequence must increase: {frame:?} after {prev}"
                        )));
                    }
                }
                last_seq = Some(frame.seq);
                f(frame)?;
                pos += len;
            }
            Err(_) => {
                // ponytail: O(n²) probe for a valid frame after the damage —
                // the active WAL is bounded, linear resync would be M3 polish.
                for probe in pos + 1..bytes.len() {
                    if decode_frame(&bytes[probe..]).is_ok() {
                        return Err(FormatError::Corrupt(format!(
                            "WAL damage at offset {pos} with valid frames after"
                        )));
                    }
                }
                return Ok(pos);
            }
        }
    }
    Ok(pos)
}

/// Walk frames from the start into a Vec — the collecting wrapper over
/// `replay_frames_streaming` (one definition of the stream semantics; the
/// recovery path streams, tests and the damage corpus collect).
pub fn replay_frames(bytes: &[u8]) -> Result<(Vec<WalFrame>, usize), FormatError> {
    let mut frames = Vec::new();
    let consumed = replay_frames_streaming(bytes, |frame| {
        frames.push(frame);
        Ok(())
    })?;
    Ok((frames, consumed))
}

/// M28 — read ONE frame's bytes at `pos` into memory: the header, then the
/// payload + stored checksum contiguous in one Vec (so the sha256-8 is
/// byte-identical to `decode_frame`'s). Ok(None): no frame starts at `pos`
/// (bad magic/version/type, truncation — every case the in-memory replay
/// probes past). Err: a real read failure. The checksum is NOT verified
/// here — the callers own that (the probe needs only byte completeness,
/// and the two full paths verify before use).
fn read_frame_bytes(
    r: &mut (impl Read + Seek),
    pos: u64,
    end: u64,
) -> Result<Option<(u64, Vec<u8>)>, FormatError> {
    if pos + FRAME_HEADER_LEN as u64 > end {
        return Ok(None); // torn header
    }
    r.seek(SeekFrom::Start(pos))
        .map_err(|e| FormatError::Io(format!("WAL seek to {pos}: {e}")))?;
    let mut head = [0u8; FRAME_HEADER_LEN];
    if let Err(e) = r.read_exact(&mut head) {
        return if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Ok(None) // the WAL shrank under us (concurrent flush) — torn
        } else {
            Err(FormatError::Io(format!("WAL read at {pos}: {e}")))
        };
    }
    if &head[..4] != WAL_MAGIC {
        return Ok(None);
    }
    let version = u16::from_le_bytes(head[4..6].try_into().expect("2-byte slice"));
    if version != WAL_FORMAT_VERSION || head[6] != FRAME_BATCH {
        return Ok(None);
    }
    let seq = u64::from_le_bytes(head[7..15].try_into().expect("8-byte slice"));
    let payload_len = u32::from_le_bytes(head[15..19].try_into().expect("4-byte slice")) as usize;
    let total = FRAME_HEADER_LEN + payload_len + 8;
    if pos + total as u64 > end {
        return Ok(None); // truncated payload/checksum
    }
    let mut body = Vec::with_capacity(total);
    body.extend_from_slice(&head);
    body.resize(total, 0);
    if let Err(e) = r.read_exact(&mut body[FRAME_HEADER_LEN..]) {
        return if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Ok(None)
        } else {
            Err(FormatError::Io(format!("WAL read at {pos}: {e}")))
        };
    }
    Ok(Some((seq, body)))
}

/// Validate ONE complete frame at `pos` — `read_frame_bytes` plus the
/// checksum and the op decode. Memory is bounded by this one frame.
/// Ok(None): no valid frame starts at `pos` (bad magic/version/type,
/// truncation or checksum — every case `replay_frames` would probe past).
/// Err: a real read failure, or a checksum-valid frame whose ops fail to
/// decode (an acked frame this build cannot understand — never silently
/// skipped).
fn validate_frame_at(
    r: &mut (impl Read + Seek),
    pos: u64,
    end: u64,
) -> Result<Option<(u64, usize)>, FormatError> {
    let Some((seq, body)) = read_frame_bytes(r, pos, end)? else {
        return Ok(None);
    };
    let total = body.len();
    if checksum8(&body[..total - 8]) != body[total - 8..] {
        return Ok(None);
    }
    decode_payload(&body[FRAME_HEADER_LEN..total - 8])?;
    Ok(Some((seq, total)))
}

/// PR6-R3-003 — the streamed, bounded-memory WAL scan: the byte length of
/// the torn-safe prefix, with EXACTLY `replay_frames`' semantics (a decode
/// failure with nothing valid after it is a torn tail; damage followed by a
/// valid frame is Corrupt; sequences must strictly increase). Reads one
/// frame's bytes into memory at a time at most. The caller holds the wal
/// mutex, so the file cannot grow between the probe and its use.
pub fn valid_prefix_len(r: &mut (impl Read + Seek)) -> Result<u64, FormatError> {
    let end = r
        .seek(SeekFrom::End(0))
        .map_err(|e| FormatError::Io(format!("WAL seek end: {e}")))?;
    let mut pos = 0u64;
    let mut last_seq: Option<u64> = None;
    while pos < end {
        match validate_frame_at(r, pos, end)? {
            Some((seq, total)) => {
                if let Some(prev) = last_seq {
                    if seq <= prev {
                        return Err(FormatError::Corrupt(format!(
                            "WAL sequence must increase: {seq} after {prev}"
                        )));
                    }
                }
                last_seq = Some(seq);
                pos += total as u64;
            }
            None => {
                // The same probe as replay_frames, per-offset: O(n) seeks,
                // memory bounded — the active WAL is small.
                for probe in pos + 1..end {
                    if validate_frame_at(r, probe, end)?.is_some() {
                        return Err(FormatError::Corrupt(format!(
                            "WAL damage at offset {pos} with valid frames after"
                        )));
                    }
                }
                return Ok(pos);
            }
        }
    }
    Ok(pos)
}

/// M28 — the bounded-memory "is there a valid frame after `pos`" probe:
/// the torn-tail vs damage classifier for the reader replay, where the
/// tail can be arbitrarily long (a 100 MB torn tail would mean ~100M
/// per-offset decodes). Scan 64 KiB chunks for the magic, overlapping 3
/// bytes so a magic straddling a chunk boundary is found; only magic hits
/// are fully validated (header + checksum + ops — the same predicate
/// `decode_frame` accepts). Verdict-identical to validating every offset:
/// any offset `decode_frame` could accept starts with a valid magic, and
/// every offset with a valid magic is scanned.
fn find_valid_frame_after(
    r: &mut (impl Read + Seek),
    pos: u64,
    end: u64,
) -> Result<Option<u64>, FormatError> {
    const CHUNK: usize = 64 << 10;
    let mut buf = vec![0u8; CHUNK];
    let mut at = pos;
    while at < end {
        let want = ((end - at) as usize).min(CHUNK);
        if want < 4 {
            break; // not even a magic — nothing valid can start here
        }
        r.seek(SeekFrom::Start(at))
            .map_err(|e| FormatError::Io(format!("WAL seek to {at}: {e}")))?;
        let mut n = 0;
        while n < want {
            match r.read(&mut buf[n..want]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        break; // the WAL shrank — the rest is gone
                    }
                    return Err(FormatError::Io(format!("WAL read at {at}: {e}")));
                }
            }
        }
        if n < 4 {
            break;
        }
        for i in 0..=n - 4 {
            if &buf[i..i + 4] != WAL_MAGIC {
                continue;
            }
            let cand = at + i as u64;
            if let Some((_, bytes)) = read_frame_bytes(r, cand, end)? {
                if checksum8(&bytes[..bytes.len() - 8]) == bytes[bytes.len() - 8..]
                    && decode_payload(&bytes[FRAME_HEADER_LEN..bytes.len() - 8]).is_ok()
                {
                    return Ok(Some(cand));
                }
            }
        }
        if n < want {
            break; // short read — nothing beyond
        }
        at += (want - 3) as u64;
    }
    Ok(None)
}

/// M28 (P0-01 + P1-01) — the reader-based replay: recovery consumes THIS,
/// so the WAL is never materialized — one frame's bytes in memory at a
/// time, and each op decodes straight into the apply callback (no
/// per-frame Vec<Op>). EXACTLY `replay_frames`' semantics: a decode
/// failure with nothing valid after it is a torn tail — Ok((prefix, end)),
/// the caller truncates at `prefix`; damage followed by a valid frame is
/// Corrupt; sequences must strictly increase. One deliberate divergence: a
/// checksum-VALID frame whose ops fail to decode is a hard error (an acked
/// frame this build cannot understand — never truncated), matching
/// `validate_frame_at`; unreachable by raw byte damage, so the corpus
/// verdicts stay identical. Returns (consumed prefix, file size at open).
pub fn replay_reader<R, F>(r: &mut R, mut f: F) -> Result<(u64, u64), FormatError>
where
    R: Read + Seek,
    F: FnMut(u64, Op) -> Result<(), FormatError>,
{
    let end = r
        .seek(SeekFrom::End(0))
        .map_err(|e| FormatError::Io(format!("WAL seek end: {e}")))?;
    let mut pos = 0u64;
    let mut last_seq: Option<u64> = None;
    while pos < end {
        let Some((seq, bytes)) = read_frame_bytes(r, pos, end)? else {
            // Header-level failure (bad magic/version/type, truncation) —
            // probe past it, exactly like the in-memory replay.
            return match find_valid_frame_after(r, pos, end)? {
                Some(after) => Err(FormatError::Corrupt(format!(
                    "WAL damage at offset {pos} with valid frames after (next at {after})"
                ))),
                None => Ok((pos, end)),
            };
        };
        if checksum8(&bytes[..bytes.len() - 8]) != bytes[bytes.len() - 8..] {
            // Checksum mismatch — the torn-tail signal (a torn final
            // frame), or mid-WAL damage if a valid frame follows.
            return match find_valid_frame_after(r, pos, end)? {
                Some(after) => Err(FormatError::Corrupt(format!(
                    "WAL damage at offset {pos} with valid frames after (next at {after})"
                ))),
                None => Ok((pos, end)),
            };
        }
        if let Some(prev) = last_seq {
            if seq <= prev {
                return Err(FormatError::Corrupt(format!(
                    "WAL sequence must increase: {seq} after {prev}"
                )));
            }
        }
        last_seq = Some(seq);
        // P1-01 — ops decode straight into the apply callback; a
        // checksum-valid frame whose ops fail to decode hard-errors here
        // (see the doc comment above).
        walk_payload(&bytes[FRAME_HEADER_LEN..bytes.len() - 8], |op| f(seq, op))?;
        pos += bytes.len() as u64;
    }
    Ok((pos, end))
}
