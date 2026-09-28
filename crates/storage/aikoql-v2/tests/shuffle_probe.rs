//! TDD-033 (L-22): a DELIBERATE order dependency, armed only by the
//! shuffle proof (`scripts/shuffle-proof.sh`). Unarmed —
//! `AIKOQL_SHUFFLE_PROBE_DIR` unset — both tests are no-ops, so the
//! normal suites run them in any order (and any thread schedule)
//! unaffected.
//!
//! Armed, the pair models the P1-5 interference class the nightly
//! shuffle exists to catch out-of-band: the writer "leaks" a marker
//! file (the leaked-temp-dir class); the reader fails when it runs
//! first. The proof runs the pair through the real nextest shuffle
//! with two pinned seeds — one seed breaks the pair (the catch), one
//! does not (the control) — and each probe records its own name in the
//! shared order log, so the proof asserts the order nextest actually
//! ran, not an assumption.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

fn probe_dir() -> Option<PathBuf> {
    env::var_os("AIKOQL_SHUFFLE_PROBE_DIR").map(PathBuf::from)
}

fn record_order(dir: &std::path::Path, who: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("order"))
        .unwrap();
    writeln!(f, "{who}").unwrap();
}

#[test]
fn tdd033_writer_leaks_the_marker() {
    let Some(dir) = probe_dir() else { return };
    std::fs::write(dir.join("marker"), b"leaked").unwrap();
    record_order(&dir, "writer");
}

#[test]
fn tdd033_reader_fails_when_first() {
    let Some(dir) = probe_dir() else { return };
    record_order(&dir, "reader");
    if !dir.join("marker").exists() {
        panic!("TDD-033: order dependency caught — reader ran before writer");
    }
}
