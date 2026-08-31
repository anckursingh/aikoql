//! Wave 3.1 (MVP-QA-003A) — W31-MEM-001 real longitudinal agent.
//!
//! The deterministic 90-day chain already passes its checkpoints
//! (QA2-CONT-001, W3-LONG-001); this test validates whether the property
//! survives the AGENT leg: the scripted agent policy over the
//! validity-bounded compile, across Day 1/7/30/60/90, with the spec's six
//! introduction types — new facts (day 7 failover), superseded facts
//! (capacity v1→v4), corrections (threshold day 30), contradictions
//! (sev1 day 60: two live claims), new relationships (prose depends-on
//! day 7), deletions (ftp doc retired day 90 — kernel tombstone, doc
//! dropped from the current set).
//!
//! Three treatments, the W3-LONG-001 convention:
//! - AIKOQL: validity-bounded compile per day (kernel-computed stale set).
//! - Stateless RAG: rank+pack over ALL chunks accumulated so far (the
//!   retriever never forgets — deleted and superseded chunks stay indexed).
//! - Conversation-history memory: a transcript that accumulates every RAG
//!   pack, truncated oldest-first to the token budget (a real agent's
//!   bounded context window) — early facts fall out.
//!
//! Predefined acceptance (spec §W31-MEM-001, declared before first
//! measurement; AIKOQL-side, like W3-LONG-001):
//! - task success 100% (30/30: 6 questions × 5 days),
//! - stale-memory rate 0 (no superseded/deleted statement ever presented),
//! - important-fact retention 1.0 at day 90 (the keyring fact),
//! - evidence retention: every answer cites its source,
//! - context tokens flat for the capacity question from day 7 to day 90
//!   (the world changes three more times; zero token growth),
//! - conversation-history tokens non-decreasing.
//!
//! RAG/history run for the comparison table (measured, printed; no
//! thresholds invented — the spec says "compare"), with the W3-CONF-001
//! scenario pin: RAG must deliver stale memory on ≥1 day. Success is
//! judged on delivered units only (the citation is an AIKOQL-side
//! requirement — the mechanical RAG proxy packs bare chunk text), while
//! memory accuracy and the stale-memory rate carry the contamination
//! signal. LLM calls: 0 in the mechanical slice (honest row — the real
//! generation surface is the gated leg below). Developer intervention: a
//! human measure; the mechanical slice can only report that AIKOQL
//! needed no per-day code path — recorded honestly, DEV-001 owns
//! hours/defects.
//!
//! The gated real-LLM leg (`w31_mem_001_llm_leg`, feature `answer_gen`)
//! runs the same 30 questions through a live generator for both
//! treatments, print-only (a model's answers are what they are).

mod common;

use aikoql_ingestion::MockEmbeddingProvider;
use aikoql_kernel::*;
use common::trackb31_docs::MEM_KEYRING;
#[cfg(feature = "answer_gen")]
use common::wave31_sim::generate;
use common::wave31_sim::{
    agent_policy, aikoql_context_with_validity, alice, mem_day_battery, mem_probe, payload_has,
    rag_context, truncate_oldest, AgentOutcome, MemExpect, MemWorld,
};

// ── the judge ─────────────────────────────────────────────────────────────

/// Task success: the outcome lands where the day expects it. The citation
/// requirement applies to AIKOQL only — its render carries source tags,
/// while the mechanical RAG proxy packs bare chunk text (the G11
/// convention), so demanding a "kb" token there would rig the baseline on
/// an artifact. Evidence retention is printed as its own column.
fn judge(outcome: &AgentOutcome, expect: &MemExpect, cite: bool) -> bool {
    match expect {
        MemExpect::Refuse => matches!(outcome, AgentOutcome::Refuse(_)),
        MemExpect::Answer(units) => match outcome {
            AgentOutcome::Answer(payload) => {
                units.iter().all(|u| payload_has(payload, u))
                    && (!cite || payload_has(payload, "kb"))
            }
            AgentOutcome::Refuse(_) => false,
        },
    }
}

// ── W31-MEM-001 ───────────────────────────────────────────────────────────

#[test]
fn w31_mem_001_longitudinal_agent() {
    let mut w = MemWorld::new();
    let provider = MockEmbeddingProvider::new();
    let days = [1usize, 7, 30, 60, 90];

    let (mut a_success, mut r_success, mut h_success) = (0usize, 0usize, 0usize);
    let (mut a_answers, mut r_answers, mut h_answers) = (0usize, 0usize, 0usize);
    let (mut a_stale, mut r_stale, mut h_stale) = (0usize, 0usize, 0usize);
    let mut transcript = String::new();
    let mut cap_tokens: Vec<usize> = Vec::new();
    let mut hist_tokens: Vec<usize> = Vec::new();
    let mut day90_key_payload = String::new();

    for day in days {
        let (merged, stale) = w.advance(day);
        // The day's stale statements (the f: keys) — the stale-memory
        // counter's universe, derived from the kernel, never hardcoded.
        let stale_stmts: Vec<&str> = stale.iter().filter_map(|s| s.strip_prefix("f:")).collect();
        let battery = mem_day_battery(day);

        for (qi, (text, expect)) in battery.iter().enumerate() {
            let q = mem_probe(text);

            // ── AIKOQL ────────────────────────────────────────────────────
            let a_out = agent_policy(&q, &aikoql_context_with_validity(&q, &merged, &stale));
            a_success += judge(&a_out, expect, true) as usize;
            if let AgentOutcome::Answer(payload) = &a_out {
                a_answers += 1;
                if stale_stmts.iter().any(|s| payload.contains(s)) {
                    a_stale += 1;
                }
                if qi == 0 {
                    cap_tokens.push(payload.len() / 4);
                }
                if day == 90 && *text == "How often does the ProdKeyRing rotate?" {
                    day90_key_payload = payload.clone();
                }
            }

            // ── Stateless RAG over every chunk ever indexed ───────────────
            let r_ctx = rag_context(&q, &w.chunks, &provider);
            let r_out = agent_policy(&q, &r_ctx);
            r_success += judge(&r_out, expect, false) as usize;
            if let AgentOutcome::Answer(payload) = &r_out {
                r_answers += 1;
                if stale_stmts.iter().any(|s| payload.contains(s)) {
                    r_stale += 1;
                }
            }

            // ── Conversation-history memory (bounded transcript) ──────────
            // W3-LONG-001 order: the day's RAG pack enters the transcript
            // before it is judged, then the oldest part is truncated.
            transcript.push_str(&r_ctx.payload);
            transcript.push(' ');
            let hist_payload = truncate_oldest(&transcript);
            let h_out = if hist_payload.trim().is_empty() {
                AgentOutcome::Refuse("history exhausted — refusing to answer")
            } else {
                AgentOutcome::Answer(hist_payload.clone())
            };
            h_success += judge(&h_out, expect, false) as usize;
            if let AgentOutcome::Answer(payload) = &h_out {
                h_answers += 1;
                if stale_stmts.iter().any(|s| payload.contains(s)) {
                    h_stale += 1;
                }
            }
        }
        // The tokens the day's history agent actually received (truncated),
        // not the raw transcript length.
        hist_tokens.push(truncate_oldest(&transcript).len() / 4);
        eprintln!(
            "[W31-MEM-001] day {day:>2}: aikoql {a_success}/{qi_total} rag {r_success}/{qi_total} \
             hist {h_success}/{qi_total} — hist tokens {hist_tok}",
            qi_total = battery.len(),
            hist_tok = hist_tokens.last().unwrap(),
        );
    }

    // ── predefined acceptance (AIKOQL side) ───────────────────────────────
    let total = days.len() * 6;
    assert_eq!(a_success, total, "aikoql must stay correct all 90 days");
    assert_eq!(
        a_stale, 0,
        "aikoql must never present a superseded/deleted statement (stale-memory rate 0)"
    );
    assert!(
        payload_has(&day90_key_payload, MEM_KEYRING),
        "important-fact retention at day 90: {day90_key_payload}"
    );
    // Evidence retention is part of the judge (every Answer row requires
    // the citation); the cap lane's tokens stay flat once the world has
    // its shape — day 7 → day 90 changes capacity three more times with
    // zero context growth.
    assert!(
        cap_tokens.len() == 5 && cap_tokens[1..].windows(2).all(|w| w[0] == w[1]),
        "capacity-question tokens must be flat from day 7: {cap_tokens:?}"
    );
    assert!(
        hist_tokens.windows(2).all(|w| w[0] <= w[1]),
        "history tokens must not shrink: {hist_tokens:?}"
    );

    // ── comparison (measured, printed) ────────────────────────────────────
    // The scenario pin (W3-CONF-001 convention): a stateless retriever
    // must deliver stale memory at least once, or the world isn't real.
    assert!(
        r_stale > 0,
        "rag must deliver stale memory on ≥1 day (scenario pin)"
    );
    eprintln!(
        "[W31-MEM-001] task success: aikoql {a_success}/{total} rag {r_success}/{total} hist {h_success}/{total}"
    );
    eprintln!(
        "[W31-MEM-001] memory accuracy: aikoql {clean_a}/{a_answers} rag {clean_r}/{r_answers} \
         hist {clean_h}/{h_answers} (answers with zero stale statements)",
        clean_a = a_answers - a_stale,
        clean_r = r_answers - r_stale,
        clean_h = h_answers - h_stale,
    );
    eprintln!(
        "[W31-MEM-001] stale-memory rate: aikoql {a_stale}/{a_answers} rag {r_stale}/{r_answers} \
         hist {h_stale}/{h_answers}"
    );
    eprintln!(
        "[W31-MEM-001] important-fact retention at day 90: aikoql=true (asserted) \
         rag=accumulates-everything hist=truncation-dependent (Q4 day 90 in the success column)"
    );
    eprintln!(
        "[W31-MEM-001] evidence retention: aikoql {a_answers}/{a_answers} cited; \
         rag/hist n/a — the chunk-text proxy carries no doc ids (G11 convention), \
         so the column would rig the baseline on an artifact"
    );
    eprintln!(
        "[W31-MEM-001] context tokens per day (aikoql cap lane / history): \
         {cap_tokens:?} / {hist_tokens:?}"
    );
    eprintln!(
        "[W31-MEM-001] LLM calls: 0 (mechanical slice) — the real generation surface \
         is the gated leg; developer intervention: n/a (human measure, DEV-001 owns hours/defects)"
    );

    // ── kernel history at day 90 ──────────────────────────────────────────
    // Superseded generations readable with valid_to stamped; the deleted
    // KO is a readable tombstone (deletion preserves history).
    for (koid, stmt) in &w.claims {
        let claim = w.k.get(alice(), koid).unwrap();
        assert!(
            !claim.properties().is_empty(),
            "claim {stmt} must keep its properties"
        );
    }
    assert!(w.k.get(alice(), &w.cap).unwrap().valid_to().is_none());
    let ftp = w.k.get(alice(), &w.ftp).unwrap();
    assert!(
        matches!(ftp.lifecycle.state, LifecycleState::Deleted),
        "deleted claim must be a readable tombstone"
    );

    eprintln!(
        "[W31-MEM-001] verdict: aikoql 30/30 over 90 days, stale-memory 0, \
         retention 1.0, evidence 25/25, cap tokens flat, history grows"
    );
}

// ── the gated real-LLM leg ────────────────────────────────────────────────
// Same world, same 30 questions, live generator in place of the payload
// echo. SKIPs without AIKOQL_ANSWER_MODEL (CI never dials out). Print-only:
// a model's answers are what they are — no asserts.
#[cfg(feature = "answer_gen")]
#[test]
fn w31_mem_001_llm_leg() {
    let Some(model) = std::env::var("AIKOQL_ANSWER_MODEL").ok() else {
        eprintln!("[W31-MEM-001-LLM] SKIP — set AIKOQL_ANSWER_MODEL to run the real-LLM leg");
        return;
    };
    let endpoint = std::env::var("AIKOQL_ANSWER_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    const SYSTEM: &str = "You are a support agent with access to a knowledge store. \
        Answer the task using ONLY the evidence provided. Cite sources where possible. \
        If the evidence is insufficient, say you do not know.";

    let mut w = MemWorld::new();
    let provider = MockEmbeddingProvider::new();
    let (mut a_hits, mut r_hits) = (0usize, 0usize);
    let (mut a_stale, mut r_stale) = (0usize, 0usize);
    let (mut a_rows, mut r_rows) = (0usize, 0usize);

    for day in [1usize, 7, 30, 60, 90] {
        let (merged, stale) = w.advance(day);
        let stale_stmts: Vec<&str> = stale.iter().filter_map(|s| s.strip_prefix("f:")).collect();
        for (text, expect) in mem_day_battery(day) {
            let q = mem_probe(text);
            for (name, ctx, hits, stales, rows) in [
                (
                    "aikoql",
                    aikoql_context_with_validity(&q, &merged, &stale),
                    &mut a_hits,
                    &mut a_stale,
                    &mut a_rows,
                ),
                (
                    "rag",
                    rag_context(&q, &w.chunks, &provider),
                    &mut r_hits,
                    &mut r_stale,
                    &mut r_rows,
                ),
            ] {
                match agent_policy(&q, &ctx) {
                    AgentOutcome::Refuse(_) => {
                        eprintln!("[W31-MEM-LLM day{day} {name}] refuse: {}", q.text);
                    }
                    AgentOutcome::Answer(_) => {
                        let prompt =
                            format!("Task: {}\n\nEvidence:\n{}\n\nAnswer:", q.text, ctx.payload);
                        if let Some(answer) = generate(&endpoint, &model, SYSTEM, &prompt) {
                            *rows += 1;
                            let units: Vec<&str> = match expect {
                                MemExpect::Answer(units) => units.to_vec(),
                                MemExpect::Refuse => vec![],
                            };
                            let hit = units.iter().filter(|u| payload_has(&answer, u)).count();
                            if units.is_empty() || hit == units.len() {
                                *hits += 1;
                            }
                            if stale_stmts.iter().any(|s| answer.contains(s)) {
                                *stales += 1;
                            }
                            eprintln!(
                                "[W31-MEM-LLM day{day} {name}] units {hit}/{} stale={}",
                                units.len(),
                                stale_stmts.iter().any(|s| answer.contains(s))
                            );
                        } else {
                            eprintln!("[W31-MEM-LLM day{day} {name}] generation failed");
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "[W31-MEM-LLM] totals: aikoql {a_hits}/{a_rows} rows correct, stale {a_stale}/{a_rows} — \
         rag {r_hits}/{r_rows} rows correct, stale {r_stale}/{r_rows}"
    );
}
