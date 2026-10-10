//! D-15: the Aikoql native wire protocol (§5/§6, ECO-2) — the framed binary
//! transport codec, shared by the server listener and the SDKs. Zero
//! dependencies: the checksum is a const-evaluated CRC32 table (IEEE 802.3,
//! poly 0xEDB88320, little-endian bytes on the wire).
//!
//! Frame layout (frozen by `crates/sdk/rust/tests/native.rs` — the wire
//! oracle; see docs/NATIVE-PROTOCOL.md):
//!
//! ```text
//! magic "AKQL" (4B) | protocol_version u16 BE | flags u16 BE
//! | request_id u64 BE | msg_type u16 BE | payload_length u32 BE
//! | payload (JSON, UTF-8) | crc32 u32 LE (over everything before it)
//! ```
//!
//! Header = 22 bytes. `MAX_PAYLOAD` = 1 MiB (mirrors the MCP
//! MAX_FRAME_BYTES cap). This crate only owns the codec — the server and
//! the SDKs build their own read/write loops on top of it.

use std::fmt;

pub const MAGIC: &[u8; 4] = b"AKQL";
pub const PROTOCOL_VERSION: u16 = 1;
/// Bit 0 of the flags: set on every server response frame.
pub const FLAG_RESPONSE: u16 = 0b1;
pub const HEADER_LEN: usize = 22;
/// Mirrors the MCP MAX_FRAME_BYTES cap (§6 invariant 3: length-bounded).
pub const MAX_PAYLOAD: usize = 1024 * 1024;

// The 15 §6 message classes (14 + ERROR).
pub const HELLO: u16 = 1;
pub const AUTH: u16 = 2;
pub const PING: u16 = 3;
pub const BEGIN: u16 = 4;
pub const COMMIT: u16 = 5;
pub const ROLLBACK: u16 = 6;
pub const PREPARE: u16 = 7;
pub const EXECUTE: u16 = 8;
pub const QUERY: u16 = 9;
pub const QUERY_CHUNK: u16 = 10;
pub const QUERY_END: u16 = 11;
pub const CANCEL: u16 = 12;
pub const CLOSE: u16 = 13;
pub const ERROR: u16 = 14;

/// A decoded 22-byte frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub flags: u16,
    pub request_id: u64,
    pub msg_type: u16,
    pub payload_len: usize,
}

/// Decode-time rejection: the peer misbehaved at the frame level (§6
/// invariants 4, 5, 7) — the receiver closes the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    BadMagic,
    BadVersion { version: u16 },
    Oversized { claimed: usize },
    Truncated,
    BadChecksum,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::BadMagic => write!(f, "bad magic"),
            DecodeError::BadVersion { version } => {
                write!(f, "unsupported protocol version {version}")
            }
            DecodeError::Oversized { claimed } => {
                write!(
                    f,
                    "payload length {claimed} exceeds the {MAX_PAYLOAD} byte cap"
                )
            }
            DecodeError::Truncated => write!(f, "truncated frame"),
            DecodeError::BadChecksum => write!(f, "checksum mismatch"),
        }
    }
}

/// Builds a 22-byte header (big-endian fields, per the frozen layout).
pub fn header_bytes(
    flags: u16,
    request_id: u64,
    msg_type: u16,
    payload_len: u32,
) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[0..4].copy_from_slice(MAGIC);
    h[4..6].copy_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    h[6..8].copy_from_slice(&flags.to_be_bytes());
    h[8..16].copy_from_slice(&request_id.to_be_bytes());
    h[16..18].copy_from_slice(&msg_type.to_be_bytes());
    h[18..22].copy_from_slice(&payload_len.to_be_bytes());
    h
}

/// Parses a 22-byte header. Rejects bad magic, wrong version, and
/// over-cap payload lengths before a byte of payload is read (§6
/// invariants 4 + 7 — the decoder rejects oversized, version mismatch is
/// explicit at the HELLO level too).
pub fn parse_header(bytes: &[u8; HEADER_LEN]) -> Result<Header, DecodeError> {
    if &bytes[0..4] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    if version != PROTOCOL_VERSION {
        return Err(DecodeError::BadVersion { version });
    }
    let flags = u16::from_be_bytes([bytes[6], bytes[7]]);
    let request_id = u64::from_be_bytes(bytes[8..16].try_into().expect("8 bytes")); // justified: fixed slice
    let msg_type = u16::from_be_bytes([bytes[16], bytes[17]]);
    let payload_len = u32::from_be_bytes(bytes[18..22].try_into().expect("4 bytes")) as usize; // justified: fixed slice
    if payload_len > MAX_PAYLOAD {
        return Err(DecodeError::Oversized {
            claimed: payload_len,
        });
    }
    Ok(Header {
        flags,
        request_id,
        msg_type,
        payload_len,
    })
}

/// CRC32 (IEEE 802.3): const-evaluated table, byte-at-a-time update.
const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

/// The frame checksum over header + payload bytes (the wire carries the
/// little-endian u32).
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc = CRC_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// Verifies a received checksum against header + payload.
pub fn verify(header: &[u8], payload: &[u8], crc: u32) -> bool {
    let mut buf = Vec::with_capacity(header.len() + payload.len());
    buf.extend_from_slice(header);
    buf.extend_from_slice(payload);
    crc32(&buf) == crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_known_check_vector() {
        // The canonical CRC-32/IEEE check value (RocksDB's own unit vector).
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn header_roundtrip() {
        let h = header_bytes(FLAG_RESPONSE, 42, QUERY, 7);
        let parsed = parse_header(&h).unwrap();
        assert_eq!(
            parsed,
            Header {
                flags: FLAG_RESPONSE,
                request_id: 42,
                msg_type: QUERY,
                payload_len: 7,
            }
        );
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let mut h = header_bytes(0, 1, PING, 0);
        h[0] = b'X';
        assert_eq!(parse_header(&h), Err(DecodeError::BadMagic));
    }

    #[test]
    fn parse_rejects_bad_version() {
        let mut h = header_bytes(0, 1, PING, 0);
        h[4..6].copy_from_slice(&2u16.to_be_bytes());
        assert_eq!(
            parse_header(&h),
            Err(DecodeError::BadVersion { version: 2 })
        );
    }

    #[test]
    fn parse_rejects_oversized_before_any_payload_read() {
        let h = header_bytes(0, 1, QUERY, MAX_PAYLOAD as u32 + 1);
        assert_eq!(
            parse_header(&h),
            Err(DecodeError::Oversized {
                claimed: MAX_PAYLOAD + 1
            })
        );
    }

    #[test]
    fn verify_catches_a_flipped_payload_byte() {
        let h = header_bytes(0, 1, EXECUTE, 3);
        let payload = b"abc";
        let good = crc32(&{
            let mut b = h.to_vec();
            b.extend_from_slice(payload);
            b
        });
        assert!(verify(&h, payload, good));
        assert!(!verify(&h, payload, good ^ 1));
    }
}
