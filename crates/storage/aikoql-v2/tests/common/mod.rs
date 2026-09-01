//! Shared SE2 test helpers (docs/TESTING-PLAN-V2.md).
// Each test binary compiles this module but uses a subset — the unused
// helper would be dead code there, so the module opts out wholesale.

#![allow(dead_code)]

use std::path::PathBuf;

/// A unique scratch FILE path under the OS temp dir: tag + pid so parallel
/// test binaries never collide; any stale file is removed so reruns are clean.
pub fn tmp(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("aikoql-v2-{tag}-{}", std::process::id()));
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
