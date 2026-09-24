//! P5-M37 (P1-10 + P1-11) — the comparative-harness contract schema.
//! Single source of truth: the M7 harness emits rows through
//! [`ROW_KEYS`], and artifacts validated here. Hand-rolled on the
//! harness's fixed-format JSON (no serde in the v2 tree) — the checks
//! are key-exact against the format the harness writes.

/// The row contract: every required field, with the unit named in the
/// field (the old harness carried nanoseconds in fields named `*_us`).
pub const ROW_KEYS: [&str; 8] = [
    "label",
    "ops",
    "wall_ms",
    "p50_ns",
    "p95_ns",
    "p99_ns",
    "read_bytes",
    "written_bytes",
];

/// The old `*_us` spellings, which carried `as_nanos()` values — a
/// unit mismatch the contract rejects (bench002).
const UNIT_MISMATCH_KEYS: [&str; 3] = ["p50_us", "p95_us", "p99_us"];

/// Required top-level sections of an artifact.
const SECTION_KEYS: [&str; 6] = [
    "suite",
    "generated",
    "environment",
    "dataset",
    "backends",
    "gates",
];

/// Required environment keys (the harness records them per run).
const ENV_KEYS: [&str; 5] = ["git_sha", "rustc", "os", "arch", "build"];

/// Required dataset keys.
const DATASET_KEYS: [&str; 2] = ["seed", "n"];

/// Fixed-format JSON key probe: the harness writes `"key":` verbatim.
fn has_key(s: &str, key: &str) -> bool {
    s.contains(&format!("\"{key}\":"))
}

/// A row must carry every required field with the unit-named spelling.
pub fn validate_row(row: &str) -> Result<(), Vec<String>> {
    let mut errs = Vec::new();
    for k in ROW_KEYS {
        if !has_key(row, k) {
            errs.push(format!("row missing required field `{k}`"));
        }
    }
    for k in UNIT_MISMATCH_KEYS {
        if has_key(row, k) {
            errs.push(format!(
                "row carries `{k}` — nanoseconds in a *_us field (unit mismatch; emit the *_ns spelling)"
            ));
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// An artifact must carry every required section and environment /
/// dataset key, every backend row must validate, and the gate verdict
/// must be a string — PASS/FAIL/NOT_EVIDENCED, never null (a
/// single-backend run is NOT_EVIDENCED, stated, not a bare null).
pub fn validate_artifact(s: &str) -> Result<(), Vec<String>> {
    let mut errs = Vec::new();
    for k in SECTION_KEYS {
        if !has_key(s, k) {
            errs.push(format!("artifact missing required section `{k}`"));
        }
    }
    for k in ENV_KEYS {
        if !has_key(s, k) {
            errs.push(format!("environment missing required key `{k}`"));
        }
    }
    for k in DATASET_KEYS {
        if !has_key(s, k) {
            errs.push(format!("dataset missing required key `{k}`"));
        }
    }
    let backends = s.matches("\"name\":").count(); // harness format: one name per backend
    if backends == 0 {
        errs.push("artifact has no backends".into());
    }
    for row in row_sections(s) {
        if let Err(row_errs) = validate_row(row) {
            errs.extend(row_errs);
        }
    }
    if !has_key(s, "verdict") {
        errs.push("gates missing required key `verdict`".into());
    } else if s.contains("\"verdict\": null") {
        errs.push("null gate verdict — emit the verdict string (PASS/FAIL/NOT_EVIDENCED)".into());
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// The harness writes each row as a `{ ... }` object inside a
/// backend's `"rows": [ ... ]` list — extract those objects only (a
/// top-level balanced scan would swallow the whole artifact).
fn row_sections(s: &str) -> Vec<&str> {
    let mut rows = Vec::new();
    let mut rest = s;
    while let Some(pos) = rest.find("\"rows\": [") {
        let after = &rest[pos + 9..];
        let mut start = None;
        let mut depth = 0i32;
        for (i, b) in after.bytes().enumerate() {
            match b {
                b'{' => {
                    if depth == 0 {
                        start = Some(i);
                    }
                    depth += 1;
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(st) = start.take() {
                            rows.push(&after[st..=i]);
                        }
                    }
                }
                b']' if depth == 0 => break,
                _ => {}
            }
        }
        rest = after;
    }
    rows
}
