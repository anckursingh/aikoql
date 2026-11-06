//! The held-out entity corpus is the official extraction target (decision
//! 2026-11-06): the rules must not be tuned against it.
//!
//! - The corpus is frozen: its SHA-256 is pinned in `results.json`. Changing
//!   the corpus is a deliberate act (bump the pin and say why in the commit).
//! - The official gates are the `*_holdout` metrics, named in `results.json`.
//! - No held-out entity name may appear as a literal in the rule tables
//!   (`src/ner.rs`) — that is what "tuned against the held-out text" looks
//!   like in code.

use serde_json::Value;
use sha2::{Digest, Sha256};

const DIR: &str = "tests/fixtures/extraction-bench";

fn results() -> Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{DIR}/results.json")).unwrap()).unwrap()
}

fn sha(file: &str) -> String {
    // Line endings normalised: a Windows checkout (autocrlf) must hash the same.
    let b: Vec<u8> = std::fs::read(format!("{DIR}/{file}"))
        .unwrap()
        .into_iter()
        .filter(|&c| c != b'\r')
        .collect();
    Sha256::digest(&b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

#[test]
fn ho001_holdout_corpus_is_frozen_by_hash() {
    let r = results();
    let pins = r["holdout_sha256"]
        .as_object()
        .expect("holdout_sha256 pin missing");
    for f in ["holdout-entities.txt", "holdout-entities.txt.gt.json"] {
        assert_eq!(
            pins.get(f).and_then(Value::as_str),
            Some(sha(f).as_str()),
            "{f} changed: the held-out corpus is frozen; bump the pin deliberately"
        );
    }
}

#[test]
fn ho002_official_gates_are_the_holdout_metrics() {
    let r = results();
    let official: Vec<&str> = r["official"]
        .as_array()
        .expect("`official` metric list missing")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for m in ["entity_precision_holdout", "entity_recall_holdout"] {
        assert!(official.contains(&m), "{m} must be an official gate");
        assert!(r["floor"].get(m).is_some(), "{m} has no floor");
    }
}

#[test]
fn ho003_no_holdout_entity_name_is_hardcoded_in_the_rules() {
    let gt: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{DIR}/holdout-entities.txt.gt.json")).unwrap(),
    )
    .unwrap();
    let rules = std::fs::read_to_string("src/ner.rs").unwrap();
    // Only the production rules, not the unit tests at the bottom of the file.
    let rules = rules.split("#[cfg(test)]").next().unwrap().to_lowercase();
    // Generic vocabulary (legal forms, given names, big cities) that was
    // already in the tables when the corpus was frozen. Anything NEW fails.
    let known: Vec<String> = results()["holdout_known_vocab_overlap"]
        .as_array()
        .expect("holdout_known_vocab_overlap missing")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_lowercase))
        .collect();
    let mut leaks = Vec::new();
    for e in gt["entities"].as_array().unwrap() {
        if !matches!(
            e["type"].as_str(),
            Some("Organization" | "Person" | "Location")
        ) {
            continue;
        }
        for part in e["text"].as_str().unwrap().split_whitespace() {
            if known.contains(&part.to_lowercase()) {
                continue;
            }
            let lit = format!("\"{}\"", part.to_lowercase());
            if rules.contains(&lit) {
                leaks.push(format!("{} -> {part}", e["text"]));
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "held-out names hard-coded in the rules: {leaks:?}"
    );
}
