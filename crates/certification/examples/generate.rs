//! P5-M14 (ND-14) — publish the five DB-* certification reports to
//! `docs/certification/` (run-date + commit hash are stamped inside each
//! result.json by the runner). Re-run after meaningful changes:
//!
//!     cargo run -p aikoql-certification --example generate

use std::path::Path;

use aikoql_certification::{run_suite, SUITES};

fn main() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/certification");
    for suite in SUITES {
        match run_suite(suite, &out) {
            Ok(path) => println!("{suite}: {}", path.display()),
            Err(e) => {
                eprintln!("{suite}: FAILED — {e}");
                std::process::exit(1);
            }
        }
    }
}
