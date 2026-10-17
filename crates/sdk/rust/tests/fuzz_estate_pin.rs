//! The F-04 pin for the Rust slice of the D-16 fuzz estate: the eight §15
//! target names must exist as cargo-fuzz targets wired to the SDK's
//! doc(hidden) `fuzz` checks, and the engine crate must declare the
//! cargo-fuzz contract. Removing any of them is a detected coverage loss.

use std::path::Path;

/// The frozen §15 names (the PR #7 fuzz review).
const TARGETS: [&str; 8] = [
    "fuzz_rpc_frame",
    "fuzz_native_frame",
    "fuzz_error_frame",
    "fuzz_stream_frame",
    "fuzz_protocol_version",
    "fuzz_request_decoder",
    "fuzz_response_decoder",
    "fuzz_auth_frame",
];

/// The pure checks the targets call (each is the real frozen wire logic,
/// not a copy — the client itself uses the same extractions).
const CHECKS: [&str; 8] = [
    "check_rpc_frame",
    "check_native_frame",
    "check_error_frame",
    "check_stream_frame",
    "check_protocol_version",
    "check_request_decoder",
    "check_response_decoder",
    "check_auth_frame",
];

/// The pure extractions the client and the checks share.
const EXTRACTIONS: [&str; 5] = [
    "fn decode_response",
    "fn map_rpc_error",
    "fn decode_notify",
    "fn classify_id",
    "fn map_native_error",
];

fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "fuzz estate missing {rel} ({}): the D-16 Rust slice is absent",
            e
        )
    })
}

#[test]
fn fuzz_estate_pin() {
    let manifest = read("fuzz/Cargo.toml");
    assert!(
        manifest.contains("cargo-fuzz = true"),
        "fuzz/Cargo.toml must declare the cargo-fuzz contract"
    );
    assert!(
        manifest.contains("libfuzzer-sys"),
        "fuzz/Cargo.toml must depend on libfuzzer-sys"
    );
    assert!(
        manifest.contains("aikoql-sdk"),
        "fuzz/Cargo.toml must depend on the SDK it fuzzes"
    );

    let surface = read("src/fuzz.rs");
    for check in CHECKS {
        assert!(
            surface.contains(&format!("pub fn {check}")),
            "src/fuzz.rs must expose {check} — the target's entry point"
        );
    }
    for extraction in EXTRACTIONS {
        assert!(
            surface.contains(extraction),
            "src/fuzz.rs must carry the {extraction} extraction"
        );
    }

    for (target, check) in TARGETS.iter().zip(CHECKS.iter()) {
        let body = read(&format!("fuzz/fuzz_targets/{target}.rs"));
        assert!(
            body.contains("fuzz_target!"),
            "{target}.rs must be a cargo-fuzz target"
        );
        assert!(
            body.contains(&format!("fuzz::{check}")),
            "{target}.rs must call the SDK's {check} check"
        );
    }
}
