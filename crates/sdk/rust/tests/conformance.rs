//! D-12: the shared conformance runner, rust arm (§7, §23). The runner
//! executes the §23 canonical workload and the §7 category vectors from
//! tests/sdk-conformance/ (plus the frozen protocol/test-vectors/) against
//! a real server through this SDK, with the same expected results every
//! language must produce. This test pins only the CLI contract; the
//! vectors carry the semantics.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// The interpreter for the script. On Windows, plain "bash" resolves to
/// the WSL shim (System32) under CreateProcess even when a PATH walk
/// finds Git's bash first — and WSL bash has no cargo. where.exe does the
/// plain PATH walk, so prefer its Git hit.
#[cfg(windows)]
fn bash_exe() -> OsString {
    if let Ok(out) = Command::new("where.exe").arg("bash").output() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let hits: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if let Some(git) = hits.iter().find(|h| h.to_lowercase().contains("git")) {
            return (*git).into();
        }
        if let Some(first) = hits.first() {
            return (*first).into();
        }
    }
    "bash".into()
}

#[cfg(not(windows))]
fn bash_exe() -> OsString {
    "bash".into()
}

#[test]
fn sdk_conformance_rust() {
    // crates/sdk/rust (the test cwd) -> repo root is three levels up.
    let script = PathBuf::from("..")
        .join("..")
        .join("..")
        .join("scripts")
        .join("sdk-conformance.sh");
    if !script.exists() {
        panic!(
            "sdk-conformance runner missing at {} — D-12 RED",
            script.display()
        );
    }
    if std::env::var_os("AIKOQL_MCP_BIN").is_none() {
        eprintln!("AIKOQL_MCP_BIN not set — real-server conformance skipped");
        return;
    }
    // Hand bash a POSIX relative path — Go's spawn gets MSYS conversion,
    // Rust's doesn't (bash receives the raw backslashes). Forward slashes
    // are already the bash form; the test cwd is the crate root.
    let script = script.to_string_lossy().replace('\\', "/");
    let out = Command::new(bash_exe())
        .arg(&script)
        .arg("--language")
        .arg("rust")
        .output()
        .expect("bash must be on PATH (the Go and Python pins use it too)");
    assert!(
        out.status.success(),
        "sdk-conformance --language rust failed: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
