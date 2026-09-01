//! SE2-M1 — immutable segments (docs/IMPLEMENTATION-PLAN-V2.md SE2-M1,
//! docs/TESTING-PLAN-V2.md row V2-M1).
//!
//! On-disk layout (all integers little-endian; pinned byte-exact by the
//! golden fixture in tests/segment_golden.rs):
//!
//! SEGMENT = header | data blocks | index block | bloom block | footer
//!
//! Header: `AKSE | version u16 | data_block_count u32 | entry_count u64 |
//! key_min_len u32 | key_min | key_max_len u32 | key_max | seq_lo u64 |
//! seq_hi u64 | sha256-8(everything before)`
//!
//! Block: 28-byte header `AKBL | version u16 | type u8 | compression u8 |
//! entry_count u32 | compressed_size u32 | uncompressed_size u32 |
//! sha256-8(20-byte header + payload)` + payload.
//! Types: 0 data, 1 index, 2 bloom. Compression 0 = none.
//!
//! Entry: `shared_prefix_len u16 | key_suffix_len u16 | key_suffix |
//! value_len u32 | value | seq u64 | flags u8`. Entries are sorted
//! (key asc, seq desc); a key's head is its first version. The first entry
//! of a block carries its full key (shared = 0).
//!
//! Index payload: per data block `first_key_len u16 | first_key |
//! last_key_len u16 | last_key | block_offset u64 | entry_count u32`.
//!
//! Bloom payload: `m u32 | bits (ceil(m/8), lsb-first)` with m = 10·n and
//! k = 4 probes, double hashing h1 + i·h2 mod m over sha256(key).
//!
//! Footer: `AKFT | version u16 | entry_count u64 | sha256-8(skeleton)`.
//! The skeleton covers the header, every 28-byte block header, the index
//! and bloom blocks whole, and the footer fields — but not data payloads,
//! so open() stays O(block count) no matter the file size. Torn segments
//! are impossible (atomic publication); data payloads are validated lazily
//! on the read that touches the block. Structural damage fails at open,
//! payload damage fails on access.

use crate::format::{checksum8, publish_atomic, Cursor, FormatError};
use aikoql_kernel::knowledge::kom::sha256;
use std::fs::File;
use std::ops::Range;
#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub const SEGMENT_VERSION: u16 = 1;
pub const FLAG_PUT: u8 = 1;
pub const FLAG_DELETE: u8 = 2;
pub const FLAG_VERSION: u8 = 4;

const SEGMENT_MAGIC: &[u8; 4] = b"AKSE";
const BLOCK_MAGIC: &[u8; 4] = b"AKBL";
const FOOTER_MAGIC: &[u8; 4] = b"AKFT";
const BLOCK_HEADER_LEN: usize = 28;
const FOOTER_LEN: usize = 22;
const BLOCK_DATA: u8 = 0;
const BLOCK_INDEX: u8 = 1;
const BLOCK_BLOOM: u8 = 2;
/// Smallest possible entry: shared + suffix + value_len + value + seq + flags.
const MIN_ENTRY_LEN: usize = 2 + 2 + 4 + 8 + 1;
const BLOOM_BITS_PER_KEY: usize = 10;
const BLOOM_PROBES: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentEntry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub seq: u64,
    pub flags: u8,
}

/// Buffers entries and writes them as one immutable segment.
pub struct SegmentWriter {
    target_block_bytes: usize,
    entries: Vec<SegmentEntry>,
}

impl SegmentWriter {
    pub fn new(target_block_bytes: usize) -> Self {
        SegmentWriter {
            target_block_bytes,
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: SegmentEntry) {
        self.entries.push(entry);
    }

    /// Sort (key asc, seq desc), split into target-sized data blocks, and
    /// write the segment atomically. Caller misuse — empty input, duplicate
    /// (key, seq), zero block target — is Invalid, never written to disk.
    pub fn publish(&self, path: &Path) -> Result<(), FormatError> {
        if self.target_block_bytes == 0 {
            return Err(FormatError::Invalid("target block size must be > 0".into()));
        }
        if self.entries.is_empty() {
            return Err(FormatError::Invalid(
                "cannot publish an empty segment".into(),
            ));
        }
        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| a.key.cmp(&b.key).then(b.seq.cmp(&a.seq)));
        if entries
            .windows(2)
            .any(|w| w[0].key == w[1].key && w[0].seq == w[1].seq)
        {
            return Err(FormatError::Invalid("duplicate (key, seq) pair".into()));
        }

        // Split into data blocks: encode as we go, start a new block when
        // the next entry would push the payload past the target (a block
        // always holds at least one entry, even one bigger than the target).
        struct Pending {
            payload: Vec<u8>,
            first: Vec<u8>,
            last: Vec<u8>,
            count: u32,
            prev: Option<Vec<u8>>,
        }
        let mut blocks: Vec<Pending> = Vec::new();
        for e in &entries {
            let split = match blocks.last() {
                Some(b) if !b.payload.is_empty() => {
                    let shared = shared_of(&b.prev, e);
                    let len = 2 + 2 + (e.key.len() - shared) + 4 + e.value.len() + 8 + 1;
                    b.payload.len() + len > self.target_block_bytes
                }
                _ => false,
            };
            if blocks.is_empty() || split {
                blocks.push(Pending {
                    payload: Vec::new(),
                    first: e.key.clone(),
                    last: e.key.clone(),
                    count: 0,
                    prev: None,
                });
            }
            let b = blocks.last_mut().expect("block pushed above");
            let shared = shared_of(&b.prev, e);
            b.payload.extend_from_slice(&(shared as u16).to_le_bytes());
            b.payload
                .extend_from_slice(&((e.key.len() - shared) as u16).to_le_bytes());
            b.payload.extend_from_slice(&e.key[shared..]);
            b.payload
                .extend_from_slice(&(e.value.len() as u32).to_le_bytes());
            b.payload.extend_from_slice(&e.value);
            b.payload.extend_from_slice(&e.seq.to_le_bytes());
            b.payload.push(e.flags);
            b.last = e.key.clone();
            b.count += 1;
            b.prev = Some(e.key.clone());
        }

        let block_count = blocks.len() as u32;
        let entry_count = entries.len() as u64;
        let key_min = &entries[0].key;
        let key_max = &entries[entries.len() - 1].key;
        let seq_lo = entries.iter().map(|e| e.seq).min().expect("non-empty");
        let seq_hi = entries.iter().map(|e| e.seq).max().expect("non-empty");

        // Header.
        let mut out = Vec::new();
        out.extend_from_slice(SEGMENT_MAGIC);
        out.extend_from_slice(&SEGMENT_VERSION.to_le_bytes());
        out.extend_from_slice(&block_count.to_le_bytes());
        out.extend_from_slice(&entry_count.to_le_bytes());
        out.extend_from_slice(&(key_min.len() as u32).to_le_bytes());
        out.extend_from_slice(key_min);
        out.extend_from_slice(&(key_max.len() as u32).to_le_bytes());
        out.extend_from_slice(key_max);
        out.extend_from_slice(&seq_lo.to_le_bytes());
        out.extend_from_slice(&seq_hi.to_le_bytes());
        out.extend_from_slice(&checksum8(&out));

        // Data blocks + index payload (offsets are final-file positions).
        let mut index_payload = Vec::new();
        let mut offset = out.len() as u64;
        let mut data_blocks = Vec::with_capacity(blocks.len());
        for b in &blocks {
            index_payload.extend_from_slice(&(b.first.len() as u16).to_le_bytes());
            index_payload.extend_from_slice(&b.first);
            index_payload.extend_from_slice(&(b.last.len() as u16).to_le_bytes());
            index_payload.extend_from_slice(&b.last);
            index_payload.extend_from_slice(&offset.to_le_bytes());
            index_payload.extend_from_slice(&b.count.to_le_bytes());
            let block = encode_block(BLOCK_DATA, b.count, &b.payload);
            offset += block.len() as u64;
            data_blocks.push(block);
        }

        // Bloom: m = 10·n bits, 4 probes, double hashing over sha256(key).
        let m = BLOOM_BITS_PER_KEY as u64 * entry_count;
        let mut bits = vec![0u8; m.div_ceil(8) as usize];
        for e in &entries {
            let d = sha256(&e.key);
            let h1 = u64::from_le_bytes(d[..8].try_into().expect("sha256 len"));
            let h2 = u64::from_le_bytes(d[8..16].try_into().expect("sha256 len"));
            for i in 0..BLOOM_PROBES {
                let bit = ((h1 as u128 + i as u128 * h2 as u128) % m as u128) as usize;
                bits[bit / 8] |= 1 << (bit % 8);
            }
        }
        let mut bloom_payload = Vec::with_capacity(4 + bits.len());
        bloom_payload.extend_from_slice(&(m as u32).to_le_bytes());
        bloom_payload.extend_from_slice(&bits);

        let index_block = encode_block(BLOCK_INDEX, block_count, &index_payload);
        let bloom_block = encode_block(BLOCK_BLOOM, entry_count as u32, &bloom_payload);

        // Footer checksum over the skeleton: header, all block headers, the
        // index and bloom blocks whole, and the footer fields. Data payloads
        // are excluded so open() never hashes the whole file.
        let mut skeleton = Vec::with_capacity(
            out.len()
                + BLOCK_HEADER_LEN * blocks.len()
                + index_block.len()
                + bloom_block.len()
                + 14,
        );
        skeleton.extend_from_slice(&out);
        for b in &data_blocks {
            skeleton.extend_from_slice(&b[..BLOCK_HEADER_LEN]);
        }
        skeleton.extend_from_slice(&index_block);
        skeleton.extend_from_slice(&bloom_block);
        skeleton.extend_from_slice(FOOTER_MAGIC);
        skeleton.extend_from_slice(&SEGMENT_VERSION.to_le_bytes());
        skeleton.extend_from_slice(&entry_count.to_le_bytes());

        for b in &data_blocks {
            out.extend_from_slice(b);
        }
        out.extend_from_slice(&index_block);
        out.extend_from_slice(&bloom_block);
        out.extend_from_slice(FOOTER_MAGIC);
        out.extend_from_slice(&SEGMENT_VERSION.to_le_bytes());
        out.extend_from_slice(&entry_count.to_le_bytes());
        out.extend_from_slice(&checksum8(&skeleton));
        publish_atomic(path, &out)
    }
}

/// Common prefix of the previous key and this one — the entry stores only
/// the suffix (0 when there is no previous key, e.g. the first of a block).
fn shared_of(prev: &Option<Vec<u8>>, e: &SegmentEntry) -> usize {
    match prev {
        Some(p) => common_prefix(p, &e.key),
        None => 0,
    }
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

fn encode_block(kind: u8, entries: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(BLOCK_HEADER_LEN + payload.len());
    out.extend_from_slice(BLOCK_MAGIC);
    out.extend_from_slice(&SEGMENT_VERSION.to_le_bytes());
    out.push(kind);
    out.push(0); // compression: none
    out.extend_from_slice(&entries.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let mut sk = Vec::with_capacity(20 + payload.len());
    sk.extend_from_slice(&out);
    sk.extend_from_slice(payload);
    out.extend_from_slice(&checksum8(&sk));
    out.extend_from_slice(payload);
    out
}

/// A read-only handle on a published segment. Open reads only the skeleton
/// (header, block headers, index, bloom, footer) — O(block count), never
/// O(file size) — and defers data-block payloads to the read that touches
/// the block, which validates that block's checksum on first touch.
#[derive(Debug)]
pub struct SegmentReader {
    file: File,
    file_len: u64,
    block_count: u32,
    entry_count: u64,
    key_min: Vec<u8>,
    key_max: Vec<u8>,
    seq_lo: u64,
    seq_hi: u64,
    data: Vec<DataBlock>,
    /// Index payload — per-block key ranges point into this buffer.
    index: Vec<u8>,
    /// Bloom payload (m u32 + bits).
    bloom: Vec<u8>,
    bloom_m: u32,
}

#[derive(Debug)]
struct DataBlock {
    /// File offset of the 28-byte block header (payload follows at +28).
    header: u64,
    payload_len: usize,
    entries: u32,
    /// Key ranges into the reader's index payload.
    first: Range<usize>,
    last: Range<usize>,
    /// Atomic so concurrent readers on one segment are safe (SE2-M4) — a
    /// benign race re-validates a block's deterministic checksum twice.
    validated: AtomicBool,
}

/// Bounded positional read: short reads are the same Corrupt truncation
/// class the whole-file Cursor used to report (so the M1 pins hold), real
/// I/O failures stay Io. `read_at` has no shared seek position — concurrent
/// readers on one segment never race payload loads.
fn read_segment_at(
    file: &File,
    file_len: u64,
    offset: u64,
    n: usize,
) -> Result<Vec<u8>, FormatError> {
    if file_len - offset < n as u64 {
        return Err(FormatError::Corrupt(format!(
            "truncated: need {n} bytes at offset {offset}, {} remain",
            file_len - offset
        )));
    }
    let mut buf = vec![0u8; n];
    #[cfg(unix)]
    file.read_exact_at(&mut buf, offset)
        .map_err(|e| FormatError::Io(format!("segment read at {offset}: {e}")))?;
    #[cfg(windows)]
    file.seek_read(&mut buf, offset)
        .map_err(|e| FormatError::Io(format!("segment read at {offset}: {e}")))?;
    Ok(buf)
}

impl SegmentReader {
    pub fn open(path: &Path) -> Result<Self, FormatError> {
        let file = File::open(path)
            .map_err(|e| FormatError::Io(format!("open segment {}: {e}", path.display())))?;
        let file_len = file
            .metadata()
            .map_err(|e| FormatError::Io(format!("segment metadata {}: {e}", path.display())))?
            .len();

        // Header: magic(4) version(2) data_block_count(4) entry_count(8)
        // key_min_len(4) key_min key_max_len(4) key_max seq_lo(8) seq_hi(8)
        // checksum(8). Read in pieces — the variable key ranges are tiny.
        let prefix = read_segment_at(&file, file_len, 0, 22)?;
        let mut cur = Cursor::new(&prefix);
        if cur.take(4)? != SEGMENT_MAGIC {
            return Err(FormatError::Corrupt("segment bad magic".into()));
        }
        let version = cur.u16()?;
        if version != SEGMENT_VERSION {
            // A newer format whose v1-shaped header checksum still validates
            // is Unsupported, not damaged; anything else fails closed.
            if file_len >= 54 {
                let head = read_segment_at(&file, file_len, 0, 54)?;
                if checksum8(&head[..46]) == head[46..54] {
                    return Err(FormatError::Unsupported(format!(
                        "segment format version {version} (this build: {SEGMENT_VERSION})"
                    )));
                }
            }
            return Err(FormatError::Corrupt(format!(
                "segment version {version} damaged"
            )));
        }
        let block_count = cur.u32()?;
        let entry_count = cur.u64()?;
        let key_min_len = cur.u32()? as usize;
        let mut header = Vec::with_capacity(22 + key_min_len + 4 + 32);
        header.extend_from_slice(&prefix);
        let key_min = read_segment_at(&file, file_len, header.len() as u64, key_min_len)?;
        header.extend_from_slice(&key_min);
        let key_max_len_b = read_segment_at(&file, file_len, header.len() as u64, 4)?;
        let key_max_len =
            u32::from_le_bytes(key_max_len_b[..4].try_into().expect("u32 slice")) as usize;
        header.extend_from_slice(&key_max_len_b);
        let key_max = read_segment_at(&file, file_len, header.len() as u64, key_max_len)?;
        header.extend_from_slice(&key_max);
        let tail = read_segment_at(&file, file_len, header.len() as u64, 24)?; // seq_lo | seq_hi | checksum
        let mut tcur = Cursor::new(&tail);
        let seq_lo = tcur.u64()?;
        let seq_hi = tcur.u64()?;
        let stored: [u8; 8] = tcur.take(8)?.try_into().expect("8-byte checksum");
        header.extend_from_slice(&tail[..16]);
        header.extend_from_slice(&stored);
        if checksum8(&header[..header.len() - 8]) != stored {
            return Err(FormatError::Corrupt(
                "segment header checksum mismatch".into(),
            ));
        }
        if seq_lo > seq_hi {
            return Err(FormatError::Corrupt(format!(
                "seq_lo {seq_lo} > seq_hi {seq_hi}"
            )));
        }

        // Walk blocks until the footer, skipping payloads by size (a
        // payload that happens to start with "AKFT" is never misread).
        let mut data: Vec<DataBlock> = Vec::new();
        let mut block_headers: Vec<u8> = Vec::new();
        let mut last_kind: Option<u8> = None;
        let mut index_block: Option<(u64, usize)> = None;
        let mut bloom_block: Option<(u64, usize)> = None;
        let mut cur = header.len() as u64;
        loop {
            let remaining = file_len - cur;
            let footer = remaining >= 4
                && read_segment_at(&file, file_len, cur, 4)?.as_slice() == FOOTER_MAGIC;
            if footer {
                break;
            }
            if remaining < BLOCK_HEADER_LEN as u64 {
                return Err(FormatError::Corrupt(format!(
                    "truncated: need a block or footer at offset {cur}, {remaining} bytes remain"
                )));
            }
            let header_off = cur;
            let hdr = read_segment_at(&file, file_len, cur, BLOCK_HEADER_LEN)?;
            let mut hcur = Cursor::new(&hdr);
            if hcur.take(4)? != BLOCK_MAGIC {
                return Err(FormatError::Corrupt("block bad magic".into()));
            }
            if hcur.u16()? != SEGMENT_VERSION {
                return Err(FormatError::Corrupt("block version".into()));
            }
            let kind = hcur.u8()?;
            let compression = hcur.u8()?;
            if compression != 0 {
                return Err(FormatError::Unsupported(format!(
                    "block compression {compression}"
                )));
            }
            if kind > BLOCK_BLOOM || last_kind.is_some_and(|k| kind < k) {
                return Err(FormatError::Corrupt("block types out of order".into()));
            }
            last_kind = Some(kind);
            let entries = hcur.u32()?;
            let compressed = hcur.u32()? as usize;
            hcur.u32()?; // uncompressed size (same: compression 0)
            hcur.take(8)?; // block checksum field — data payloads validate on first touch
            cur += BLOCK_HEADER_LEN as u64;
            if file_len - cur < compressed as u64 {
                return Err(FormatError::Corrupt(format!(
                    "truncated: need {compressed} bytes at offset {cur}, {} remain",
                    file_len - cur
                )));
            }
            match kind {
                BLOCK_DATA => {
                    if entries as usize > compressed / MIN_ENTRY_LEN {
                        return Err(FormatError::Corrupt(format!(
                            "{entries} entries cannot fit in {compressed} bytes"
                        )));
                    }
                    block_headers.extend_from_slice(&hdr);
                    data.push(DataBlock {
                        header: header_off,
                        payload_len: compressed,
                        entries,
                        first: 0..0,
                        last: 0..0,
                        validated: AtomicBool::new(false),
                    });
                }
                BLOCK_INDEX => {
                    if index_block.is_some() {
                        return Err(FormatError::Corrupt("two index blocks".into()));
                    }
                    index_block = Some((header_off, compressed));
                }
                BLOCK_BLOOM => {
                    if bloom_block.is_some() {
                        return Err(FormatError::Corrupt("two bloom blocks".into()));
                    }
                    bloom_block = Some((header_off, compressed));
                }
                _ => unreachable!("kind checked above"),
            }
            cur += compressed as u64;
        }
        let footer_start = cur;
        if file_len - footer_start != FOOTER_LEN as u64 {
            return Err(FormatError::Corrupt(format!(
                "footer must be exactly {FOOTER_LEN} bytes at the end, {} remain",
                file_len - footer_start
            )));
        }
        let footer = read_segment_at(&file, file_len, footer_start, FOOTER_LEN)?;
        let mut fcur = Cursor::new(&footer);
        if fcur.take(4)? != FOOTER_MAGIC {
            return Err(FormatError::Corrupt("footer bad magic".into()));
        }
        if fcur.u16()? != SEGMENT_VERSION {
            return Err(FormatError::Corrupt("footer version".into()));
        }
        if fcur.u64()? != entry_count {
            return Err(FormatError::Corrupt("footer entry_count mismatch".into()));
        }
        let (index_header, index_len) =
            index_block.ok_or_else(|| FormatError::Corrupt("missing index block".into()))?;
        let (bloom_header, bloom_len) =
            bloom_block.ok_or_else(|| FormatError::Corrupt("missing bloom block".into()))?;
        if data.is_empty() || data.len() != block_count as usize {
            return Err(FormatError::Corrupt(format!(
                "header says {block_count} data blocks, found {}",
                data.len()
            )));
        }
        let index = read_segment_at(
            &file,
            file_len,
            index_header + BLOCK_HEADER_LEN as u64,
            index_len,
        )?;
        let bloom = read_segment_at(
            &file,
            file_len,
            bloom_header + BLOCK_HEADER_LEN as u64,
            bloom_len,
        )?;

        // Index payload: per-block key range, offset, entry count. Key
        // ranges point into the index buffer; the stored offset is the
        // file-absolute block header position.
        let mut icur = Cursor::new(&index);
        let mut total = 0u64;
        for db in &mut data {
            let len = icur.u16()? as usize;
            let start = icur.pos();
            icur.take(len)?;
            db.first = start..icur.pos();
            let len = icur.u16()? as usize;
            let start = icur.pos();
            icur.take(len)?;
            db.last = start..icur.pos();
            let offset = icur.u64()?;
            let count = icur.u32()?;
            if offset != db.header {
                return Err(FormatError::Corrupt(format!(
                    "index says block at {offset}, found at {}",
                    db.header
                )));
            }
            if count != db.entries {
                return Err(FormatError::Corrupt(format!(
                    "index says {count} entries, block header says {}",
                    db.entries
                )));
            }
            total += count as u64;
        }
        if !icur.is_empty() {
            return Err(FormatError::Corrupt("index trailing bytes".into()));
        }
        if total != entry_count {
            return Err(FormatError::Corrupt(format!(
                "data blocks hold {total} entries, header says {entry_count}"
            )));
        }

        // Bloom payload: m u32 + ceil(m/8) bytes of bits.
        let mut bcur = Cursor::new(&bloom);
        let bloom_m = bcur.u32()?;
        if bcur.remaining() as u32 != bloom_m.div_ceil(8) {
            return Err(FormatError::Corrupt(format!(
                "bloom: m = {bloom_m} needs {} bit-bytes, {} present",
                bloom_m.div_ceil(8),
                bcur.remaining()
            )));
        }

        // Index + bloom block headers must agree with the header counts
        // (block header: magic 4 | version 2 | type 1 | compression 1 |
        // entry_count u32 — so the count sits at offset 8).
        let idx_entries = read_segment_at(&file, file_len, index_header + 8, 4)?;
        let idx_entries = u32::from_le_bytes(idx_entries[..4].try_into().expect("u32 slice"));
        if idx_entries != block_count {
            return Err(FormatError::Corrupt(
                "index block entry count mismatch".into(),
            ));
        }
        let blm_entries = read_segment_at(&file, file_len, bloom_header + 8, 4)?;
        let blm_entries = u32::from_le_bytes(blm_entries[..4].try_into().expect("u32 slice"));
        if blm_entries as u64 != entry_count {
            return Err(FormatError::Corrupt(
                "bloom block entry count mismatch".into(),
            ));
        }

        // Footer checksum over the skeleton (the index header + index
        // payload + bloom header + bloom payload are contiguous in the
        // file, so one bounded read covers that span).
        let mut skeleton = Vec::with_capacity(
            header.len()
                + data.len() * BLOCK_HEADER_LEN
                + (bloom_header + BLOCK_HEADER_LEN as u64 + bloom_len as u64 - index_header)
                    as usize
                + 14,
        );
        skeleton.extend_from_slice(&header);
        skeleton.extend_from_slice(&block_headers);
        skeleton.extend_from_slice(&read_segment_at(
            &file,
            file_len,
            index_header,
            (bloom_header + BLOCK_HEADER_LEN as u64 + bloom_len as u64 - index_header) as usize,
        )?);
        skeleton.extend_from_slice(&footer[..14]);
        let stored: [u8; 8] = footer[14..].try_into().expect("8-byte checksum");
        if checksum8(&skeleton) != stored {
            return Err(FormatError::Corrupt(
                "footer skeleton checksum mismatch".into(),
            ));
        }

        Ok(SegmentReader {
            file,
            file_len,
            block_count,
            entry_count,
            key_min,
            key_max,
            seq_lo,
            seq_hi,
            data,
            index,
            bloom,
            bloom_m,
        })
    }

    pub fn entry_count(&self) -> u64 {
        self.entry_count
    }

    pub fn block_count(&self) -> u32 {
        self.block_count
    }

    pub fn key_min(&self) -> &[u8] {
        &self.key_min
    }

    pub fn key_max(&self) -> &[u8] {
        &self.key_max
    }

    pub fn seq_lo(&self) -> u64 {
        self.seq_lo
    }

    pub fn seq_hi(&self) -> u64 {
        self.seq_hi
    }

    /// False positives possible, false negatives never: a false answer means
    /// the key is definitely not in the segment.
    pub fn bloom_may_contain(&self, key: &[u8]) -> bool {
        let d = sha256(key);
        let h1 = u64::from_le_bytes(d[..8].try_into().expect("sha256 len"));
        let h2 = u64::from_le_bytes(d[8..16].try_into().expect("sha256 len"));
        let bits = &self.bloom[4..];
        for i in 0..BLOOM_PROBES {
            let bit = ((h1 as u128 + i as u128 * h2 as u128) % self.bloom_m as u128) as usize;
            if bits[bit / 8] & (1 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
    }

    /// The head version of `key` (highest seq — entries sort seq-descending).
    pub fn get(&self, key: &[u8]) -> Result<Option<SegmentEntry>, FormatError> {
        let Some(i) = self.locate(key) else {
            return Ok(None);
        };
        let entries = self.block_entries(i)?;
        Ok(entries.into_iter().find(|e| e.key.as_slice() == key))
    }

    /// Every version of `key`, seq-descending. Versions may straddle a block
    /// boundary, so this walks blocks while their first key ≤ the target.
    pub fn versions(&self, key: &[u8]) -> Result<Vec<SegmentEntry>, FormatError> {
        let Some(mut i) = self.locate(key) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        while i < self.data.len() && self.block_key(&self.data[i].first) <= key {
            let entries = self.block_entries(i)?;
            out.extend(entries.into_iter().filter(|e| e.key.as_slice() == key));
            i += 1;
        }
        Ok(out)
    }

    /// Keys in [start, end) byte order, versions seq-descending within a key.
    pub fn scan(&self, start: &[u8], end: &[u8]) -> Result<Vec<SegmentEntry>, FormatError> {
        let first = self
            .data
            .partition_point(|b| self.block_key(&b.last) < start);
        let mut out = Vec::new();
        for (i, b) in self.data[first..].iter().enumerate() {
            if self.block_key(&b.first) >= end {
                break;
            }
            let entries = self.block_entries(first + i)?;
            out.extend(
                entries
                    .into_iter()
                    .filter(|e| e.key.as_slice() >= start && e.key.as_slice() < end),
            );
        }
        Ok(out)
    }

    /// First block whose key range covers `key`, if any.
    fn locate(&self, key: &[u8]) -> Option<usize> {
        let i = self.data.partition_point(|b| self.block_key(&b.last) < key);
        (i < self.data.len() && self.block_key(&self.data[i].first) <= key).then_some(i)
    }

    fn block_key(&self, r: &Range<usize>) -> &[u8] {
        &self.index[r.clone()]
    }

    /// Decode a data block, loading the payload from the file and
    /// validating its checksum on first touch (lazy: open() must stay
    /// O(block count), not O(file size)).
    fn block_entries(&self, i: usize) -> Result<Vec<SegmentEntry>, FormatError> {
        let b = &self.data[i];
        let raw = read_segment_at(
            &self.file,
            self.file_len,
            b.header,
            BLOCK_HEADER_LEN + b.payload_len,
        )?;
        if !b.validated.load(Ordering::Relaxed) {
            let mut sk = Vec::with_capacity(20 + b.payload_len);
            sk.extend_from_slice(&raw[..20]);
            sk.extend_from_slice(&raw[BLOCK_HEADER_LEN..]);
            if checksum8(&sk) != raw[20..BLOCK_HEADER_LEN] {
                return Err(FormatError::Corrupt(format!(
                    "data block {i} checksum mismatch"
                )));
            }
            b.validated.store(true, Ordering::Relaxed);
        }
        let mut cur = Cursor::new(&raw[BLOCK_HEADER_LEN..]);
        let mut out = Vec::with_capacity(b.entries as usize);
        let mut prev: Vec<u8> = Vec::new();
        for _ in 0..b.entries {
            let shared = cur.u16()? as usize;
            if shared > prev.len() {
                return Err(FormatError::Corrupt(format!(
                    "entry shared prefix {shared} exceeds previous key {}",
                    prev.len()
                )));
            }
            let suffix_len = cur.u16()? as usize;
            let suffix = cur.take(suffix_len)?.to_vec();
            let mut key = prev[..shared].to_vec();
            key.extend_from_slice(&suffix);
            let value = cur.vec()?;
            let seq = cur.u64()?;
            let flags = cur.u8()?;
            prev = key.clone();
            out.push(SegmentEntry {
                key,
                value,
                seq,
                flags,
            });
        }
        if !cur.is_empty() {
            return Err(FormatError::Corrupt(format!(
                "data block {i} trailing bytes"
            )));
        }
        Ok(out)
    }
}

/// Streaming iterator over every entry in key order — compaction's k-way
/// merge pulls one entry at a time, so the merge is O(k) memory, not
/// O(dataset). Blocks load (and validate) as the cursor reaches them.
pub struct SegmentIter<'a> {
    reader: &'a SegmentReader,
    block: usize,
    entries: std::vec::IntoIter<SegmentEntry>,
}

impl<'a> Iterator for SegmentIter<'a> {
    type Item = Result<SegmentEntry, FormatError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(e) = self.entries.next() {
                return Some(Ok(e));
            }
            let i = self.block;
            if i >= self.reader.data.len() {
                return None;
            }
            match self.reader.block_entries(i) {
                Ok(v) => {
                    self.entries = v.into_iter();
                    self.block += 1;
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl SegmentReader {
    pub fn iter(&self) -> SegmentIter<'_> {
        SegmentIter {
            reader: self,
            block: 0,
            entries: Vec::new().into_iter(),
        }
    }
}
