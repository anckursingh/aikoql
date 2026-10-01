//! D-12: the shared conformance runner, rust arm (§7, §23). The runner
//! executes the §23 canonical workload and the §7 category vectors from
//! tests/sdk-conformance/ (plus the frozen protocol/test-vectors/) against
//! a real server through this SDK, with the same expected results every
//! language must produce. This test pins only the CLI contract; the
//! vectors carry the semantics.

use std::path::PathBuf;
use std::process::Command;

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
    let out = Command::new("bash")
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
