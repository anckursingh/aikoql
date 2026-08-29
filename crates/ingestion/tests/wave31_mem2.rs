//! Wave 3.1 (MVP-QA-003A) — W31-MEM-002 memory compression.
//!
//! Three memory treatments over the same 90-day world (MemWorld — shared
//! with MEM-001):
//! - **raw conversation history**: the agent's own dialogue transcript,
//!   bounded oldest-first to the token budget;
//! - **summarized memory**: the kernel's `summarize_conversation` op over
//!   that transcript (verbatim seven-bucket extraction, §38/39);
//! - **AIKOQL structured memory**: the kernel claims themselves — the
//!   current view validity-bounded, history readable via `get()`.
//!
//! Spec measures (day 90): tokens, fact retention (the day-1 fact set),
//! relationship retention (the day-7 depends_on), conflict retention
//! (the day-60 sev1 pair), provenance retention, task success (the
//! day-90 battery) — primary metric: correct task completion per
//! retained token. The spec's measure list is measured, not thresholded:
//! asserts pin the structural properties (kernel history readable, the
//! summary op's contract), the comparison itself is printed honestly.
//!
//! Memory-size definitions (day 90): raw = the bounded transcript;
//! summarized = the rendered summary buckets; structured = the tokens
//! the kernel served for the day's six tasks (the agent's query-scoped
//! view — the kernel never hands the agent its whole history).

mod common;

use aikoql_kernel::*;
use common::trackb31_docs::{
    MEM_CAP_100, MEM_DEPENDS, MEM_FTP, MEM_KEYRING, MEM_SEV1_A, MEM_SEV1_B, MEM_THRESH_V1,
};
use common::wave31_sim::{
    agent_policy, aikoql_context_with_validity, alice, ev, mem_day_battery, mem_probe, payload_has,
    truncate_oldest, AgentOutcome, MemExpect, MemWorld,
};

/// Task success without the citation requirement — provenance retention is
/// MEM-002's own measured column, not part of the success judge.
fn judge_units(outcome: &AgentOutcome, expect: &MemExpect) -> bool {
    match expect {
        MemExpect::Refuse => matches!(outcome, AgentOutcome::Refuse(_)),
        MemExpect::Answer(units) => match outcome {
            AgentOutcome::Answer(payload) => units.iter().all(|u| payload_has(payload, u)),
            AgentOutcome::Refuse(_) => false,
        },
    }
}

/// The rendered text of a summary KO's seven buckets.
fn render_summary(ko: &KnowledgeObject) -> String {
    let mut t = String::new();
    for key in [
        "facts",
        "decisions",
        "actions",
        "open_issues",
        "constraints",
        "outcomes",
        "entities",
    ] {
        if let Some(Value::List(items)) = ko.properties.get(key) {
            for item in items {
                if let Value::Map(m) = item {
                    if let Some(Value::Text(text)) = m.get("text") {
                        t.push_str(text);
                        t.push(' ');
                    }
                }
            }
        }
    }
    t
}

/// A treatment's day-90 memory: the text the agent holds, and the answer
/// it gives (echo the memory, refuse only when it is empty).
fn memory_answer(memory: &str) -> AgentOutcome {
    if memory.trim().is_empty() {
        AgentOutcome::Refuse("memory exhausted — refusing to answer")
    } else {
        AgentOutcome::Answer(memory.to_string())
    }
}

#[test]
fn w31_mem_002_memory_compression() {
    let mut w = MemWorld::new();

    // The agent's dialogue over 90 days — every exchange recorded as a
    // conversation message (question → payload answer / refusal).
    let mut transcript: Vec<ConversationMessage> = Vec::new();
    let mut day90_view = None;
    for day in [1usize, 7, 30, 60, 90] {
        let (merged, stale) = w.advance(day);
        for (text, _expect) in mem_day_battery(day) {
            let q = mem_probe(text);
            transcript.push(ConversationMessage {
                speaker: "user".into(),
                timestamp_ms: day as u64 * 1_000,
                text: text.into(),
            });
            let reply = match agent_policy(&q, &aikoql_context_with_validity(&q, &merged, &stale)) {
                AgentOutcome::Answer(p) => p,
                AgentOutcome::Refuse(r) => r.to_string(),
            };
            transcript.push(ConversationMessage {
                speaker: "agent".into(),
                timestamp_ms: day as u64 * 1_000 + 1,
                text: reply,
            });
        }
        if day == 90 {
            day90_view = Some((merged, stale));
        }
    }
    let (day90_merged, day90_stale) = day90_view.unwrap();

    // ── the three memories at day 90 ───────────────────────────────────
    let raw = {
        let mut t = String::new();
        for m in &transcript {
            t.push_str(&m.text);
            t.push(' ');
        }
        truncate_oldest(&t)
    };

    let summary_req = SummarizeConversationRequest {
        context: alice(),
        conversation_id: "mem2".into(),
        messages: transcript.clone(),
        evidence: vec![ev("kb-transcript")],
        note: None,
    };
    let summary = w.k.summarize_conversation(summary_req).unwrap();
    let summary_ko = w.k.get(alice(), &summary.koid).unwrap();
    let summarized = render_summary(&summary_ko);

    // ── day-90 battery through each treatment ──────────────────────────
    let (mut raw_ok, mut sum_ok, mut struct_ok) = (0usize, 0usize, 0usize);
    let mut struct_tokens = 0usize;
    let mut struct_cited = 0usize;
    for (text, expect) in mem_day_battery(90) {
        let q = mem_probe(text);
        raw_ok += judge_units(&memory_answer(&raw), &expect) as usize;
        sum_ok += judge_units(&memory_answer(&summarized), &expect) as usize;
        let ctx = aikoql_context_with_validity(&q, &day90_merged, &day90_stale);
        let out = agent_policy(&q, &ctx);
        struct_ok += judge_units(&out, &expect) as usize;
        if let AgentOutcome::Answer(payload) = &out {
            struct_tokens += payload.len() / 4;
            if payload_has(payload, "kb") {
                struct_cited += 1;
            }
        }
    }

    // ── retention columns (day 90) ─────────────────────────────────────
    let day1_facts = [MEM_CAP_100, MEM_KEYRING, MEM_FTP, MEM_THRESH_V1];
    let raw_retain = day1_facts.iter().filter(|s| raw.contains(**s)).count();
    let sum_retain = day1_facts
        .iter()
        .filter(|s| summarized.contains(**s))
        .count();
    // Structured: the kernel keeps every claim readable — the day-1 fact
    // set (by statement, not claim order) must all be readable with
    // their properties.
    let day1_claims: Vec<_> = w
        .claims
        .iter()
        .filter(|(_, s)| day1_facts.contains(s))
        .collect();
    let struct_retain = day1_claims
        .iter()
        .filter(|(koid, _)| {
            w.k.get(alice(), koid)
                .is_ok_and(|ko| !ko.properties().is_empty())
        })
        .count();
    let raw_rel = raw.contains(MEM_DEPENDS);
    let sum_rel = summarized.contains(MEM_DEPENDS);
    let depends_claims: Vec<_> = w.claims.iter().filter(|(_, s)| *s == MEM_DEPENDS).collect();
    let struct_rel = depends_claims.len() == 1
        && depends_claims.iter().all(|(koid, _)| {
            let ko = w.k.get(alice(), koid).unwrap();
            !ko.properties().is_empty() && ko.valid_to().is_none()
        });
    let raw_conf = raw.contains(MEM_SEV1_A) && raw.contains(MEM_SEV1_B);
    let sum_conf = summarized.contains(MEM_SEV1_A) && summarized.contains(MEM_SEV1_B);
    let sev1_claims: Vec<_> = w
        .claims
        .iter()
        .filter(|(_, s)| *s == MEM_SEV1_A || *s == MEM_SEV1_B)
        .collect();
    let struct_conf = sev1_claims.len() == 2
        && sev1_claims.iter().all(|(koid, _)| {
            let ko = w.k.get(alice(), koid).unwrap();
            !ko.properties().is_empty() && ko.valid_to().is_none()
        });
    // Provenance: raw — the transcript's agent messages carry the aikoql
    // payloads' citations; summarized — doc-level citations survive only
    // as sentence fragments (measured honestly); structured — the kernel
    // evidence trail rides every current answer.
    let raw_prov: usize = transcript
        .iter()
        .filter(|m| m.speaker == "agent")
        .filter(|m| payload_has(&m.text, "kb"))
        .count();
    let raw_agent_msgs = transcript.iter().filter(|m| m.speaker == "agent").count();
    let sum_prov = render_summary(&summary_ko)
        .split(' ')
        .filter(|w| *w == "kb")
        .count();

    // ── structural asserts (the spec's measures are printed, not
    // thresholded — these pin the known guarantees) ────────────────────
    assert_eq!(
        struct_ok, 6,
        "structured memory must stay correct at day 90"
    );
    assert_eq!(
        struct_retain, 4,
        "kernel history must keep the day-1 fact set readable"
    );
    assert!(
        struct_rel,
        "the day-7 relationship must stay readable and current in the kernel"
    );
    assert!(
        struct_conf,
        "the day-60 conflict pair must stay readable and current in the kernel"
    );
    assert_eq!(
        summary_ko.properties.get("message_count"),
        Some(&Value::Int(transcript.len() as i64)),
        "the summary must record the transcript length"
    );
    // The summary extracted real content (the transcript is full of
    // capitalized entity names) — the op's entities bucket must not be
    // empty, or the summarized treatment measured an empty op.
    match summary_ko.properties.get("entities") {
        Some(Value::List(items)) => assert!(!items.is_empty(), "summary entities bucket empty"),
        other => panic!("expected entities bucket, got {other:?}"),
    }

    // ── the comparison table (measured, printed) ───────────────────────
    let raw_tokens = raw.len() / 4;
    let sum_tokens = summarized.len() / 4;
    let primary = |ok: usize, tokens: usize| -> f32 {
        if tokens == 0 {
            0.0
        } else {
            ok as f32 / tokens as f32 * 1000.0
        }
    };
    eprintln!(
        "[W31-MEM-002] day-90 memory size (tokens): raw {raw_tokens} summarized {sum_tokens} \
         structured {struct_tokens} (the six tasks' served context)"
    );
    eprintln!(
        "[W31-MEM-002] task success (day-90 battery): raw {raw_ok}/6 summarized {sum_ok}/6 \
         structured {struct_ok}/6"
    );
    eprintln!(
        "[W31-MEM-002] fact retention (day-1 facts): raw {raw_retain}/4 summarized {sum_retain}/4 \
         structured {struct_retain}/4 (kernel-readable history)"
    );
    eprintln!(
        "[W31-MEM-002] relationship retention (depends_on): raw {raw_rel} summarized {sum_rel} \
         structured {struct_rel}"
    );
    eprintln!(
        "[W31-MEM-002] conflict retention (sev1 pair): raw {raw_conf} summarized {sum_conf} \
         structured {struct_conf}"
    );
    eprintln!(
        "[W31-MEM-002] provenance retention: raw {raw_prov}/{raw_agent_msgs} agent messages \
         cite a doc — summarized {sum_prov} doc-citation fragments (conversation-level \
         provenance is structural: every item carries speaker/msg-range/ts) — structured \
         {struct_cited}/6 answers cited"
    );
    eprintln!(
        "[W31-MEM-002] primary metric (correct tasks per 1000 retained tokens): \
         raw {:.1} summarized {:.1} structured {:.1}",
        primary(raw_ok, raw_tokens),
        primary(sum_ok, sum_tokens),
        primary(struct_ok, struct_tokens),
    );
    eprintln!(
        "[W31-MEM-002] verdict: measured, no thresholds invented — the compression \
         direction is the table's job; the kernel-history guarantees are asserted"
    );
}
