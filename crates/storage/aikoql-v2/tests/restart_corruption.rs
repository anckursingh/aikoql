//! L-06 (TDD-009/010) — restart-index corruption matrix + self-consistency.
//!
//! TDD-009: every corruption class (with the block checksum RE-STAMPED, so
//! the STRUCTURAL validation is what's exercised — the checksum would mask
//! the class as plain damage) must fail closed: Corrupt/Unsupported from
//! open, get, and scan — never a wrong answer.
//!
//! TDD-010: `debug_restart_metadata` must match an independent decode of
//! the file bytes (the test replicates the format spec: table bytes +
//! restart key bytes + restart count, summed over data blocks).

mod common;

use aikoql_storage_v2::format::{checksum8, FormatError};
use aikoql_storage_v2::identity::ReplicaId;
use aikoql_storage_v2::segment::{SegmentEntry, SegmentReader, SegmentWriter, FLAG_PUT};
use common::dir;
use std::path::Path;

const BLOCK_MAGIC: &[u8; 4] = b"AKBL";
const BLOCK_HEADER_LEN: usize = 28;
const FOOTER_MAGIC: &[u8; 4] = b"AKFT";
const BLOCK_DATA: u8 = 0;
const BLOCK_INDEX: u8 = 1;
const BLOCK_BLOOM: u8 = 2;

fn entry(key: &str, value_len: usize, seq: u64, rid: u64) -> SegmentEntry {
    SegmentEntry {
        key: key.as_bytes().to_vec(),
        value: vec![b'v'; value_len],
        seq,
        flags: FLAG_PUT,
        replica_id: ReplicaId(rid),
    }
}

/// A v2 segment: 50 keys x 1 version (~6 KiB, one block, 4 restarts at
/// the 16-entry cadence) — plenty of restart table to corrupt.
fn publish_v2(tag: &str) -> Vec<u8> {
    let path = dir(tag).join("SEGMENT-001.log");
    let mut w = SegmentWriter::new_v2(16 << 10);
    for i in 0..50u32 {
        w.push(entry(&format!("k{i:02}"), 100, i as u64 + 1, 0));
    }
    w.publish_with_anchors(&path).unwrap();
    std::fs::read(&path).unwrap()
}

/// A v4 segment: identity rows, so the dense cadence table exists.
fn publish_v4(tag: &str) -> Vec<u8> {
    let path = dir(tag).join("SEGMENT-001.log");
    let mut w = SegmentWriter::new_v4(16 << 10);
    for i in 0..40u32 {
        w.push(entry(&format!("k{i:02}"), 100, i as u64 + 1, 7));
        w.push(entry(&format!("k{i:02}"), 100, i as u64 + 1000, 8));
    }
    w.publish_with_anchors(&path).unwrap();
    std::fs::read(&path).unwrap()
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// Location of the first DATA block (the index/bloom blocks follow it, so
/// the first AKBL is the data block).
fn block_off(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .position(|w| w == BLOCK_MAGIC)
        .expect("data block magic")
}

fn payload_len(bytes: &[u8]) -> usize {
    u32_at(bytes, block_off(bytes) + 12) as usize
}

/// Re-stamp BOTH checksums after a mutation: the data block's (header
/// [..20] + payload) and the footer skeleton's (which covers the block
/// headers — see restamp_footer). The corruption must survive both to
/// reach the structural validation.
fn restamp(bytes: &mut [u8]) {
    let bo = block_off(bytes);
    let pl = payload_len(bytes);
    let mut sk = Vec::with_capacity(20 + pl);
    sk.extend_from_slice(&bytes[bo..bo + 20]);
    sk.extend_from_slice(&bytes[bo + BLOCK_HEADER_LEN..bo + BLOCK_HEADER_LEN + pl]);
    let ck = checksum8(&sk);
    bytes[bo + 20..bo + 28].copy_from_slice(&ck);
    restamp_footer(bytes);
}

/// Re-stamp the footer skeleton checksum after a mutation. The skeleton
/// covers the header, every 28-byte data-block header, the contiguous
/// index..bloom span, and the footer's own magic/version/entry_count — so
/// a block re-stamp ALONE invalidates the footer and open fails on the
/// skeleton before any structural validation runs (TDD-009's checksum
/// mask, one layer up). The corruption must survive BOTH checksums to
/// reach the structural arms.
fn restamp_footer(bytes: &mut [u8]) {
    // Header: magic(4) version(2) block_count(4) entry_count(8)
    // key_min_len(4) | key_min | key_max_len(4) | key_max | 24-byte tail.
    let key_min_len = u32_at(bytes, 18) as usize;
    let header_len = 22 + key_min_len + 4 + u32_at(bytes, 22 + key_min_len) as usize + 24;
    let mut skeleton = Vec::new();
    skeleton.extend_from_slice(&bytes[..header_len]);
    let mut cur = header_len;
    let mut index_header = None;
    let mut bloom_end = None;
    while &bytes[cur..cur + 4] != FOOTER_MAGIC {
        let kind = bytes[cur + 6];
        let compressed = u32_at(bytes, cur + 12) as usize; // magic4 ver2 kind1 comp1 entries4
        if kind == BLOCK_DATA {
            skeleton.extend_from_slice(&bytes[cur..cur + BLOCK_HEADER_LEN]);
        } else if kind == BLOCK_INDEX {
            index_header = Some(cur);
        }
        cur += BLOCK_HEADER_LEN + compressed;
        if kind == BLOCK_BLOOM {
            bloom_end = Some(cur);
        }
    }
    let footer_start = cur;
    // The index..bloom span is contiguous in the file — one slice.
    skeleton.extend_from_slice(&bytes[index_header.unwrap()..bloom_end.unwrap()]);
    skeleton.extend_from_slice(&bytes[footer_start..footer_start + 14]);
    let ck = checksum8(&skeleton);
    bytes[footer_start + 14..footer_start + 22].copy_from_slice(&ck);
}

fn write_segment(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
    let d = dir(tag);
    let p = d.join("SEGMENT-001.log");
    std::fs::write(&p, bytes).unwrap();
    p
}

/// Every probed surface must fail closed with Corrupt/Unsupported — a
/// successful read on ANY surface is a wrong answer.
fn assert_fails_closed(path: &Path) {
    match SegmentReader::open(path) {
        Err(e) => {
            assert!(
                matches!(e, FormatError::Corrupt(_) | FormatError::Unsupported(_)),
                "open must fail closed, got {e:?}"
            );
        }
        Ok(r) => {
            let g = r.get(b"k00");
            assert!(
                matches!(
                    g,
                    Err(FormatError::Corrupt(_)) | Err(FormatError::Unsupported(_))
                ),
                "get must fail closed, got {g:?}"
            );
            let s = r.scan(b"", b"~");
            assert!(
                matches!(
                    s,
                    Err(FormatError::Corrupt(_)) | Err(FormatError::Unsupported(_))
                ),
                "scan must fail closed, got {s:?}"
            );
        }
    }
}

/// The sharper pin: both checksums (block + footer) are re-stamped, so
/// open MUST succeed and only the STRUCTURAL validation may fail — a
/// fail-closed Err from get or scan on clean code is the guard doing its
/// job. A wrong answer on every surface (or an open-time failure) is a
/// mask, not a pin.
fn closed<T>(e: &Result<T, FormatError>) -> bool {
    match e {
        Err(e) => matches!(e, FormatError::Corrupt(_) | FormatError::Unsupported(_)),
        Ok(_) => false,
    }
}

fn assert_fails_closed_structural(path: &Path) {
    let r = SegmentReader::open(path)
        .unwrap_or_else(|e| panic!("open masked the structural arm: {e:?}"));
    let g = r.get(b"k00");
    let s = r.scan(b"", b"~");
    assert!(
        closed(&g) || closed(&s),
        "no structural arm failed closed: get={g:?} scan={s:?}"
    );
}

/// Walk the v2+ entries of a block from `start`, recording each entry's
/// offset and the prefix-chain key at that offset — the independent
/// decoder (also used to find non-restart entries and their fields).
fn walk_entries(payload: &[u8], start: usize, v3: bool) -> Vec<(usize, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pos = start;
    let mut scratch: Vec<u8> = Vec::new();
    while pos + 4 <= payload.len() {
        let at = pos;
        let shared = u16_at(payload, pos) as usize;
        pos += 2;
        let suffix_len = u16_at(payload, pos) as usize;
        pos += 2;
        if pos + suffix_len > payload.len() {
            break; // truncated — the reader reports Corrupt, we stop
        }
        scratch.truncate(shared);
        scratch.extend_from_slice(&payload[pos..pos + suffix_len]);
        pos += suffix_len;
        out.push((at, scratch.clone()));
        if pos + 4 > payload.len() {
            break;
        }
        let value_len = u32_at(payload, pos) as usize;
        pos += 4 + value_len + 8 + 1;
        if v3 {
            pos += 8;
        }
        if pos > payload.len() {
            break;
        }
    }
    out
}

#[test]
fn every_corruption_class_fails_closed() {
    // v2 legs: the restart table classes.
    let base = publish_v2("restart-corrupt-v2");
    let bo = block_off(&base);
    let pl = payload_len(&base);
    let table = bo + BLOCK_HEADER_LEN;
    let n = u32_at(&base, table + 2) as usize;
    assert!(n >= 4, "50 entries need >=4 restarts, got {n}");

    // 1. restart_count overruns the payload.
    let mut b = base.clone();
    put_u32(&mut b, table + 2, (pl / 4 + 10) as u32);
    restamp(&mut b);
    assert_fails_closed_structural(&write_segment("restart-count-overrun", &b));

    // 2. an offset past the payload end.
    let mut b = base.clone();
    put_u32(&mut b, table + 6, (pl + 64) as u32);
    restamp(&mut b);
    assert_fails_closed_structural(&write_segment("restart-offset-oob", &b));

    // 3. an offset inside the table itself.
    let mut b = base.clone();
    put_u32(&mut b, table + 6, 0);
    restamp(&mut b);
    assert_fails_closed_structural(&write_segment("restart-offset-in-table", &b));

    // 4. an offset into the middle of another entry (its shared-prefix
    //    bytes read nonzero — restart entries must carry shared = 0).
    let mut b = base.clone();
    let o1 = u32_at(&b, table + 10) as usize;
    put_u32(&mut b, table + 6, (o1 + 2) as u32);
    restamp(&mut b);
    assert_fails_closed_structural(&write_segment("restart-offset-mid-entry", &b));

    // 5. swapped offsets — restart keys must be strictly increasing.
    let mut b = base.clone();
    let (x, y) = (u32_at(&b, table + 6), u32_at(&b, table + 10));
    put_u32(&mut b, table + 6, y);
    put_u32(&mut b, table + 10, x);
    restamp(&mut b);
    assert_fails_closed_structural(&write_segment("restart-keys-descending", &b));

    // 6. duplicate offsets — equal restart keys.
    let mut b = base.clone();
    let o0 = u32_at(&b, table + 6);
    put_u32(&mut b, table + 10, o0);
    restamp(&mut b);
    assert_fails_closed_structural(&write_segment("restart-keys-equal", &b));

    // 7. an entry's shared prefix exceeds the previous key.
    let payload = &base[table..table + pl];
    let entries = walk_entries(payload, 6 + 4 * n, false);
    let second = entries[1].0;
    let mut b = base.clone();
    b[table + second..table + second + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
    restamp(&mut b);
    assert_fails_closed_structural(&write_segment("entry-shared-overrun", &b));

    // 8. an entry's value length overruns the payload.
    let payload = &base[table..table + pl];
    let entries = walk_entries(payload, 6 + 4 * n, false);
    let first = entries[0].0;
    let key_suffix = u16_at(payload, first + 2) as usize;
    let value_len_at = table + first + 4 + key_suffix;
    let mut b = base.clone();
    put_u32(&mut b, value_len_at, 0xFFFF_FFFF);
    restamp(&mut b);
    assert_fails_closed_structural(&write_segment("entry-value-overrun", &b));

    // v4 legs.
    let base4 = publish_v4("restart-corrupt-v4");
    let bo4 = block_off(&base4);
    let table4 = bo4 + BLOCK_HEADER_LEN;
    let n4 = u32_at(&base4, table4 + 2) as usize;
    let dense_at = table4 + 6 + 4 * n4;

    // 9. the v4 dense-cadence count overruns the payload.
    let mut b = base4.clone();
    put_u32(&mut b, dense_at, 0xFFFF_FFFF);
    restamp(&mut b);
    assert_fails_closed_structural(&write_segment("v4-dense-overrun", &b));

    // 10. an unknown block version with a valid checksum is Unsupported,
    //     not a decode (a future format).
    let mut b = base4.clone();
    let bo = block_off(&b);
    b[bo + 4..bo + 6].copy_from_slice(&99u16.to_le_bytes());
    restamp(&mut b);
    assert_fails_closed(&write_segment("block-version-unknown", &b));

    // The untouched originals still answer (the corruptions were surgical).
    assert!(
        SegmentReader::open(&write_segment("restart-pristine-v2", &base))
            .unwrap()
            .get(b"k00")
            .unwrap()
            .is_some()
    );
    assert!(
        SegmentReader::open(&write_segment("restart-pristine-v4", &base4))
            .unwrap()
            .get_by_rid(b"k00", ReplicaId(7))
            .unwrap()
            .is_some()
    );
}

/// TDD-010 — the independent decode: walk every data block's table per
/// the format spec and sum (table bytes, restart key bytes, restarts).
fn independent_metadata(bytes: &[u8]) -> (u64, u64, u64) {
    let mut table = 0u64;
    let mut keys = 0u64;
    let mut restarts = 0u64;
    let mut pos = 0;
    while let Some(rel) = bytes[pos..].windows(4).position(|w| w == BLOCK_MAGIC) {
        let bo = pos + rel;
        let version = u16_at(bytes, bo + 4);
        let kind = bytes[bo + 6];
        let pl = u32_at(bytes, bo + 12) as usize;
        if kind == 0 && version >= 2 {
            let payload = &bytes[bo + BLOCK_HEADER_LEN..bo + BLOCK_HEADER_LEN + pl];
            let n = u32_at(payload, 2) as usize;
            table += 6 + 4 * n as u64;
            let entries_start = if version == 4 {
                let dense = u32_at(payload, 6 + 4 * n) as u64;
                table += 4 + 4 * dense;
                6 + 4 * n + 4 + 4 * dense as usize
            } else {
                6 + 4 * n
            };
            let walked = walk_entries(payload, entries_start, version >= 3);
            for j in 0..n {
                let o = u32_at(payload, 6 + 4 * j) as usize;
                keys += walked
                    .iter()
                    .find(|(at, _)| *at == o)
                    .map_or(0, |(_, k)| k.len() as u64);
            }
            restarts += n as u64;
        }
        pos = bo + BLOCK_HEADER_LEN + pl;
    }
    (table, keys, restarts)
}

#[test]
fn debug_restart_metadata_matches_an_independent_decode() {
    // Multi-block corpora so the sums span several tables; through the Db
    // (the public wrapper) so the flush's own segment is the subject.
    let d = dir("restart-meta-v2");
    let db =
        aikoql_storage_v2::db::Db::open(aikoql_storage_v2::db::Config::new(d.clone())).unwrap();
    for i in 0..60u32 {
        db.put(format!("k{i:02}").as_bytes(), &vec![b'v'; 4096])
            .unwrap();
    }
    db.rotate();
    db.flush().unwrap();
    let seg = std::fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("SEGMENT")
        })
        .expect("flushed segment");
    let bytes = std::fs::read(&seg).unwrap();
    assert_eq!(
        db.debug_restart_metadata().unwrap(),
        independent_metadata(&bytes),
        "v2 metadata matches the independent decode"
    );

    // v4: dense cadence tables included, rids carried.
    let d = dir("restart-meta-v4");
    let db =
        aikoql_storage_v2::db::Db::open(aikoql_storage_v2::db::Config::new(d.clone())).unwrap();
    let oid7 = db.create_object().unwrap();
    let oid8 = db.create_object().unwrap();
    for i in 0..30u32 {
        let key = format!("k{i:02}");
        db.put_object(oid7, key.as_bytes(), &vec![b'v'; 4096])
            .unwrap();
        db.put_object(oid8, key.as_bytes(), &vec![b'v'; 4096])
            .unwrap();
    }
    db.rotate();
    db.flush().unwrap();
    let seg = std::fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("SEGMENT")
        })
        .expect("flushed segment");
    let bytes = std::fs::read(&seg).unwrap();
    assert_eq!(
        db.debug_restart_metadata().unwrap(),
        independent_metadata(&bytes),
        "v4 metadata matches the independent decode"
    );
}
