//! MVP-QA-001 MVP-EXT-001 — Raw Evidence Preservation.
//!
//! The spec fixture has nine source segment kinds: normal prose, table,
//! bullet list, code block, fenced text, formula, heading, image caption,
//! artifact section. After ingest, every meaningful segment must remain
//! addressable back to the source document — resolvable from the evidence of
//! at least one candidate, even where the segment never becomes a KO.
//!
//! Addressable := some candidate's text carries the segment (verbatim from
//! the source) AND its evidence names the source document. A typed locator
//! (`EvidenceSource`) is the stronger form; the test additionally asserts the
//! IR itself is stamped with the document id.

use aikoql_ingestion::{compile_markdown_string, KnowledgeIr};

const DOC: &str = "segments-fixture.md";

// Each segment kind gets its own section so one section's classification
// cannot hide another segment's extraction gap (the mixed-layout case —
// prose under a section that also holds a code fence — classifies the whole
// section as Artifact by design; see TESTING-PLAN §9.1 note).
const FIXTURE: &str = r#"# SEGHEADING

SEGPROSE normal prose sentence carrying knowledge.

## Bullet Section

- SEGBULLET first bullet item with content

## Table Section

| ColA | ColB |
|------|------|
| SEGTABLECELL | 42 |

## Code Section

```rust
fn seg_code() { println!("SEGCODE"); }
```

## Fence Section

```text
SEGFENCE plain fenced text
```

## Formula Section

SEGFORMULA: $E = mc^2$

## Image Section

![SEGCAPTION](diagram.png)

## SEGSECTION artifact section

SEGSECTIONBODY body prose of the artifact section.
"#;

/// All candidate texts with their evidence, across every candidate kind.
fn text_and_evidence(ir: &KnowledgeIr) -> Vec<(String, &aikoql_ingestion::Evidence)> {
    let mut out = Vec::new();
    for e in &ir.entities {
        out.push((format!("{} {}", e.name, e.mentions.join(" ")), &e.evidence));
    }
    for r in &ir.relations {
        out.push((
            format!("{} {} {}", r.subject, r.predicate, r.object),
            &r.evidence,
        ));
    }
    for f in &ir.facts {
        out.push((
            format!("{} {}", f.statement, f.snippet.clone().unwrap_or_default()),
            &f.evidence,
        ));
    }
    for ev in &ir.events {
        out.push((
            format!(
                "{} {}",
                ev.description,
                ev.trigger.clone().unwrap_or_default()
            ),
            &ev.evidence,
        ));
    }
    for t in &ir.temporal {
        out.push((t.text.clone(), &t.evidence));
    }
    out
}

/// A segment is addressable when at least one candidate carries its text and
/// that candidate's evidence names the source document.
fn segment_addressable(ir: &KnowledgeIr, marker: &str) -> Result<(), String> {
    let hits: Vec<&aikoql_ingestion::Evidence> = text_and_evidence(ir)
        .iter()
        .filter(|(text, _)| text.contains(marker))
        .map(|(_, ev)| *ev)
        .collect();
    if hits.is_empty() {
        return Err(format!("no candidate carries segment text '{}'", marker));
    }
    for ev in hits {
        if ev.document_id.as_deref() != Some(DOC) {
            return Err(format!(
                "segment '{}' evidence does not name the source document",
                marker
            ));
        }
    }
    Ok(())
}

#[test]
fn mvp_ext_001_all_nine_segment_kinds_are_addressable() {
    let ir = compile_markdown_string(FIXTURE, Some(DOC.into())).unwrap();

    // IR-level stamp.
    assert_eq!(ir.document_id.as_deref(), Some(DOC));

    let mut failures = Vec::new();
    for marker in [
        "SEGHEADING",
        "SEGPROSE",
        "SEGBULLET",
        "SEGTABLECELL",
        "SEGCODE",
        "SEGFENCE",
        "SEGFORMULA",
        "SEGCAPTION",
        "SEGSECTION",
        "SEGSECTIONBODY",
    ] {
        if let Err(e) = segment_addressable(&ir, marker) {
            failures.push(e);
        }
    }
    assert!(
        failures.is_empty(),
        "unaddressable segments:\n{}",
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// MVP-EXT-002 — artifact classification must not destroy prose
// ---------------------------------------------------------------------------

#[test]
fn mvp_ext_002_prose_in_fenced_section_stays_retrievable() {
    let fixture = r#"## Artifact Prose Section

Customer revenue increased by 20%.

```text
fenced content next to the prose
```
"#;
    let ir = compile_markdown_string(fixture, Some(DOC.into())).unwrap();
    segment_addressable(&ir, "Customer revenue increased by 20%")
        .expect("artifact-section prose must stay retrievable");
}

// ---------------------------------------------------------------------------
// MVP-EXT-003 — formula preservation
// ---------------------------------------------------------------------------

#[test]
fn mvp_ext_003_formula_preserved_as_evidence_and_retrievable() {
    // Plain-text formula line and math-fenced formula — both forms must
    // survive as retrievable candidates naming the source document.
    let fixture = r#"## Formula Section

E = mc^2

```math
F = ma
```
"#;
    let ir = compile_markdown_string(fixture, Some(DOC.into())).unwrap();
    let mut failures = Vec::new();
    for marker in ["E = mc^2", "F = ma"] {
        if let Err(e) = segment_addressable(&ir, marker) {
            failures.push(e);
        }
    }
    assert!(
        failures.is_empty(),
        "formula segments:\n{}",
        failures.join("\n")
    );
}
