//! MVP-QA-001 MVP-SEC-004 — Sensitive Logging.
//!
//! Secrets, tokens, credentials and protected values must never appear in
//! rendered output. The R8 boundary (`filter_secrets`) must cover every IR
//! field the context compiler renders — entity names/mentions, fact
//! statements AND evidence snippets, relation endpoints, temporal and event
//! text — and the raw value must not survive the redaction itself (a
//! marker-prefixed statement that still embeds the key is not redacted).

use aikoql_ingestion::{compile_context, compile_markdown_string, filter_secrets, KnowledgeIr};

const DOC: &str = "secrets-fixture.md";
const KEY: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";

// Table-cell facts carry both a statement and an evidence snippet (raw
// source row) — the widest render surface for one secret.
const FIXTURE: &str = r#"# Integration Notes

| Setting | Value |
|---------|-------|
| API token | ghp_0123456789abcdefghijklmnopqrstuvwxyz |
"#;

/// Every renderable string in the IR (name, mention, statement, snippet,
/// endpoint, description, trigger, temporal text).
fn ir_strings(ir: &KnowledgeIr) -> Vec<String> {
    let mut out = Vec::new();
    for e in &ir.entities {
        out.push(e.name.clone());
        out.extend(e.mentions.clone());
    }
    for r in &ir.relations {
        out.push(r.subject.clone());
        out.push(r.predicate.clone());
        out.push(r.object.clone());
    }
    for f in &ir.facts {
        out.push(f.statement.clone());
        if let Some(s) = &f.snippet {
            out.push(s.clone());
        }
    }
    for ev in &ir.events {
        out.push(ev.description.clone());
        if let Some(t) = &ev.trigger {
            out.push(t.clone());
        }
    }
    for t in &ir.temporal {
        out.push(t.text.clone());
    }
    out
}

/// Every string the context compiler renders into the package sent onward
/// (entity names/mentions/justifications, fact statements/snippets/
/// justifications, relation endpoints/justifications).
fn rendered_strings(ir: &KnowledgeIr) -> Vec<String> {
    let pkg = compile_context("integration token", ir, 2000);
    let mut out = Vec::new();
    for e in &pkg.entities {
        out.push(e.name.clone());
        out.extend(e.mentions.clone());
        out.push(e.justification.clone());
    }
    for f in &pkg.facts {
        out.push(f.statement.clone());
        if let Some(s) = &f.snippet {
            out.push(s.clone());
        }
        out.push(f.justification.clone());
    }
    for r in &pkg.relations {
        out.push(r.subject.clone());
        out.push(r.predicate.clone());
        out.push(r.object.clone());
        out.push(r.justification.clone());
    }
    out
}

#[test]
fn mvp_sec_004_raw_secret_never_survives_redaction_or_rendering() {
    let raw = compile_markdown_string(FIXTURE, Some(DOC.into())).unwrap();
    // The fixture secret must actually be detected — a pass without a
    // finding would be a detection gap, not a redaction pass.
    let (redacted, findings) = filter_secrets(&raw);
    assert!(
        !findings.is_empty(),
        "fixture key must be detected by the secret filter"
    );

    let mut leaks: Vec<String> = Vec::new();
    for s in ir_strings(&redacted) {
        if s.contains(KEY) {
            leaks.push(format!("IR field leaks raw key: {}", s));
        }
    }
    for s in rendered_strings(&redacted) {
        if s.contains(KEY) {
            leaks.push(format!("rendered context leaks raw key: {}", s));
        }
    }
    assert!(leaks.is_empty(), "raw secret survived redaction:\n{}", leaks.join("\n"));

    // The knowledge survives, marked: at least one fact statement carries
    // the redaction marker (strip-not-drop — content loss is not the fix).
    assert!(
        redacted
            .facts
            .iter()
            .any(|f| f.statement.contains("REDACTED")),
        "secret-bearing knowledge must be marked, not silently dropped"
    );
}
