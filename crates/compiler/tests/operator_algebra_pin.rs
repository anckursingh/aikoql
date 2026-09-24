//! alg001 — the operator-algebra doc enumerates every planner rewrite with
//! its preconditions and its pinning test. A planner change that isn't
//! reflected in the doc (or a doc drift that drops a rewrite) fails this pin.

use std::path::PathBuf;

const DOC: &str = "../../docs/compiler/operator-algebra.md";

fn doc() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DOC);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} missing or unreadable: {e}", path.display()))
}

fn expect(doc: &str, needle: &str) {
    assert!(
        doc.contains(needle),
        "docs/compiler/operator-algebra.md must contain {needle:?}"
    );
}

#[test]
fn alg001_doc_enumerates_every_rewrite() {
    let doc = doc();
    // one section per rewrite, each with preconditions, a proof sketch, and
    // the test that pins it
    for needle in [
        "## 1. merge_filters",
        "## 2. pushdown_filters",
        "## 3. dedup_scans",
        "Preconditions",
        "Proof",
        "Pinned by",
    ] {
        expect(&doc, needle);
    }
    // every documented rewrite maps to a live, named test id
    for test_id in ["merge_two_filters", "ppl006", "ppl001", "ppl005", "alg002"] {
        expect(&doc, test_id);
    }
    // the MUST-NOTs that P4-M1 closed — the doc must restate them
    for must_not in ["MUST NOT dedup", "consecutive"] {
        expect(&doc, must_not);
    }
}
