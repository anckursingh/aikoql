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

pub fn encode_frame(seq: u64, ops: &[Op]) -> Result<Vec<u8>, FormatError> {
    if ops.is_empty() {
        return Err(FormatError::Invalid("WAL frame with no ops".into()));
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(&(ops.len() as u32).to_le_bytes());
    for op in ops {
        match op {
            Op::Put(k, v) => {
                payload.push(OP_PUT);
                payload.extend_from_slice(&(k.len() as u32).to_le_bytes());
                payload.extend_from_slice(k);
                payload.extend_from_slice(&(v.len() as u32).to_le_bytes());
                payload.extend_from_slice(v);
            }
            Op::Delete(k) => {
                payload.push(OP_DELETE);
                payload.extend_from_slice(&(k.len() as u32).to_le_bytes());
                payload.extend_from_slice(k);
            }
            Op::CreateObject {
                oid,
                lid,
                rid,
                pgen,
            } => {
                payload.push(OP_CREATE_OBJECT);
                payload.extend_from_slice(oid.as_bytes());
                payload.extend_from_slice(&lid.to_bytes());
                payload.extend_from_slice(&rid.to_bytes());
                payload.extend_from_slice(&pgen.to_le_bytes());
            }
            Op::PutObject(rid, k, v) => {
                payload.push(OP_PUT_OBJECT);
                payload.extend_from_slice(&rid.to_bytes());
                payload.extend_from_slice(&(k.len() as u32).to_le_bytes());
                payload.extend_from_slice(k);
                payload.extend_from_slice(&(v.len() as u32).to_le_bytes());
                payload.extend_from_slice(v);
            }
            Op::DeleteObject(rid, k) => {
                payload.push(OP_DELETE_OBJECT);
                payload.extend_from_slice(&rid.to_bytes());
                payload.extend_from_slice(&(k.len() as u32).to_le_bytes());
                payload.extend_from_slice(k);
            }
        }
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len() + 8);
    frame.extend_from_slice(WAL_MAGIC);
    frame.extend_from_slice(&WAL_FORMAT_VERSION.to_le_bytes());
    frame.push(FRAME_BATCH);
    frame.extend_from_slice(&seq.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
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

/// Decode the op entries from a frame payload — the shared tail of
/// `decode_frame` (in-memory replay path) and `validate_frame_at` (the
/// streamed snapshot path); one definition keeps the two byte-identical.
fn decode_payload(payload: &[u8]) -> Result<Vec<Op>, FormatError> {
    let mut pcur = Cursor::new(payload);
    let count = pcur.u32()? as usize;
    if count == 0 {
        return Err(FormatError::Corrupt("WAL frame with zero entries".into()));
    }
    let mut ops = Vec::with_capacity(count);
    for _ in 0..count {
        let op = pcur.u8()?;
        match op {
            OP_PUT => {
                let key = pcur.vec()?;
                let value = pcur.vec()?;
                ops.push(Op::Put(key, value));
            }
            OP_DELETE => {
                let key = pcur.vec()?;
                ops.push(Op::Delete(key));
            }
            OP_CREATE_OBJECT => {
                let oid = ObjectId::from_bytes(pcur.take(16)?.try_into().expect("16-byte slice"));
                let lid = LogicalId::from_bytes(pcur.take(8)?.try_into().expect("8-byte slice"));
                let rid = ReplicaId::from_bytes(pcur.take(8)?.try_into().expect("8-byte slice"));
                let pgen = pcur.u64()?;
                ops.push(Op::CreateObject {
                    oid,
                    lid,
                    rid,
                    pgen,
                });
            }
            OP_PUT_OBJECT => {
                let rid = ReplicaId::from_bytes(pcur.take(8)?.try_into().expect("8-byte slice"));
                let key = pcur.vec()?;
                let value = pcur.vec()?;
                ops.push(Op::PutObject(rid, key, value));
            }
            OP_DELETE_OBJECT => {
                let rid = ReplicaId::from_bytes(pcur.take(8)?.try_into().expect("8-byte slice"));
                let key = pcur.vec()?;
                ops.push(Op::DeleteObject(rid, key));
            }
            other => {
                return Err(FormatError::Unsupported(format!("WAL op byte {other}")));
            }
        }
    }
    if !pcur.is_empty() {
        return Err(FormatError::Corrupt("WAL payload trailing bytes".into()));
    }
    Ok(ops)
}

/// Walk frames from the start. Returns the valid prefix and the byte count
/// it covers. A decode failure with nothing valid after it is a torn tail
/// (the caller truncates the WAL there); damage followed by a valid frame
/// is Corrupt — the WAL must not be trusted. Sequences must strictly
/// increase.
pub fn replay_frames(bytes: &[u8]) -> Result<(Vec<WalFrame>, usize), FormatError> {
    let mut frames = Vec::new();
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
                frames.push(frame);
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
                return Ok((frames, pos));
            }
        }
    }
    Ok((frames, pos))
}

/// Validate ONE complete frame at `pos`: header, then payload + stored
/// checksum as one contiguous read (so the sha256-8 is byte-identical to
/// `decode_frame`'s), then the op decode. Memory is bounded by this one
/// frame. Ok(None): no valid frame starts at `pos` (bad magic/version/type,
/// truncation, checksum or op failure — every case `replay_frames` would
/// probe past). Err: a real read failure.
fn validate_frame_at(
    r: &mut (impl Read + Seek),
    pos: u64,
    end: u64,
) -> Result<Option<(u64, usize)>, FormatError> {
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
    let mut body = vec![0u8; total];
    body[..FRAME_HEADER_LEN].copy_from_slice(&head);
    if let Err(e) = r.read_exact(&mut body[FRAME_HEADER_LEN..]) {
        return if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Ok(None)
        } else {
            Err(FormatError::Io(format!("WAL read at {pos}: {e}")))
        };
    }
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
