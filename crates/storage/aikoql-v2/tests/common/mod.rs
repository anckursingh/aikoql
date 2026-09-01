//! Shared SE2 test helpers (docs/TESTING-PLAN-V2.md).
// Each test binary compiles this module but uses a subset — the unused
// helper would be dead code there, so the module opts out wholesale.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use aikoql_storage_v2::segment::SegmentEntry;

/// Parallel tests in one binary share a pid, so a plain tag+pid path would
/// collide between tests using the same tag — every call gets its own
/// counter suffix instead.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The 3-entry fixture segment (keys "a1"/"a2"/"a3", PUT/VERSION/DELETE).
pub fn entry(key: &str, value: &str, seq: u64, flags: u8) -> SegmentEntry {
    SegmentEntry {
        key: key.as_bytes().to_vec(),
        value: value.as_bytes().to_vec(),
        seq,
        flags,
    }
}

/// A unique scratch FILE path under the OS temp dir: tag + pid so parallel
/// test binaries never collide; any stale file is removed so reruns are clean.
pub fn tmp(tag: &str) -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("aikoql-v2-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// A fresh, empty scratch DIRECTORY under the OS temp dir (same tag+pid
/// scheme); any stale directory is wiped first.
pub fn dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("aikoql-v2-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Golden fixtures are hex — this is the only format-drift surface left to
/// eyeballs.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
