//! L-08 (TDD-015) — WAL encode boundaries: a payload whose length cannot
//! be written into the format's u32 length fields must FAIL SAFELY —
//! Invalid, never a silently truncated frame (a truncated payload_len
//! decodes as checksum-Corrupt, so the batch the engine acked would be
//! unreadable on recovery). The encode path's length arithmetic is checked
//! end to end: per-op sums, the payload sum, and every u32 conversion.

use aikoql_storage_v2::format::FormatError;
use aikoql_storage_v2::wal::{decode_frame, encode_frame, Op};

#[test]
fn oversized_value_fails_safely() {
    // 4 GiB + 1 — one past the u32 length prefix. encode must refuse
    // (Invalid), never Ok with a truncated payload_len. Peak memory is the
    // value alone: the check must fire BEFORE the frame buffer is sized.
    let v = vec![0u8; u32::MAX as usize + 1];
    match encode_frame(1, &[Op::Put(b"k".to_vec(), v)]) {
        Err(FormatError::Invalid(_)) => {}
        other => panic!("an oversized value must fail safely as Invalid, got {other:?}"),
    }
}

#[test]
fn boundary_payload_still_round_trips() {
    // The guard is not a hair-trigger: a frame that FITS the u32 fields
    // still encodes, and the length round-trips exactly.
    let v = vec![0u8; 1 << 20]; // 1 MiB — well inside the format
    let frame = encode_frame(1, &[Op::Put(b"key".to_vec(), v.clone())]).unwrap();
    let (decoded, len) = decode_frame(&frame).unwrap();
    assert_eq!(len, frame.len());
    assert_eq!(decoded.ops, vec![Op::Put(b"key".to_vec(), v)]);
}
