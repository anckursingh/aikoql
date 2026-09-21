//! P5-M37 (P1-10 + P1-11) — the comparative-harness contract schema.
//! bench001: an artifact missing a required field fails the schema
//! check (the harness must emit through the contract). bench002: the
//! current harness rows carry nanoseconds in fields named `*_us` (the
//! timed() pass pushes `as_nanos()`; the M7 artifact labels it µs) —
//! a unit-mismatch regression must fail the check, so the fields get
//! renamed to `*_ns` (the contract names the unit in the field).
//!
//! RED reason: E0432 — `common::contract` does not exist yet (the
//! schema module the harness will emit through in GREEN).

mod common;

use common::contract::{validate_artifact, validate_row};

/// The contract-conforming row shape the GREEN harness will emit.
const GOOD_ROW: &str = r#"{"label": "KO get (W1)", "ops": 100, "wall_ms": 12.5, "p50_ns": 12500, "p95_ns": 21000, "p99_ns": 30000, "read_bytes": 4096, "written_bytes": 0}"#;

#[test]
fn bench001_contract_rejects_missing_fields() {
    // A row missing a required field must name it.
    let no_read = r#"{"label": "KO get (W1)", "ops": 100, "wall_ms": 12.5, "p50_ns": 12500, "p95_ns": 21000, "p99_ns": 30000, "written_bytes": 0}"#;
    let err = validate_row(no_read).unwrap_err();
    assert!(
        err.iter().any(|e| e.contains("read_bytes")),
        "missing field must be named: {err:?}"
    );
    assert!(validate_row(GOOD_ROW).is_ok(), "the contract row validates");

    // An artifact missing a required top-level section must name it.
    let artifact = format!(
        r#"{{ "suite": "x", "generated": "2026-09-21", "dataset": {{ "seed": 1, "n": 1000 }}, "backends": [ {{ "name": "aikoql-v2", "rows": [ {GOOD_ROW} ] }} ], "gates": {{}} }}"#
    );
    let err = validate_artifact(&artifact).unwrap_err();
    assert!(
        err.iter().any(|e| e.contains("environment")),
        "missing section must be named: {err:?}"
    );
}

#[test]
fn bench002_contract_rejects_unit_mismatch() {
    // The CURRENT harness row: nanoseconds in fields named `*_us`.
    let mismatched = r#"{"label": "KO get (W1)", "ops": 100, "wall_ms": 12.5, "p50_us": 12500, "p95_us": 21000, "p99_us": 30000, "read_bytes": 4096, "written_bytes": 0}"#;
    let err = validate_row(mismatched).unwrap_err();
    assert!(
        err.iter().any(|e| e.contains("p50_us")),
        "unit mismatch must be named (ns in a *_us field): {err:?}"
    );
}
