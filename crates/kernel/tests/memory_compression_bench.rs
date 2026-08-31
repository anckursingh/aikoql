//! §40 — Memory compression: a measurement, not a feature. The substrate
//! ships no LLM (2026-08-25 directive), so no compressor gets built — the
//! deterministic building blocks already exist (summarize_conversation,
//! retention/expiry) and this instrument is the yardstick the row demanded.
//!
//! Scenario: a 100-message transcript the agent would otherwise carry raw.
//! Two treatments, both fully mechanical:
//! - **Raw**: the full transcript text — the context payload as-is.
//! - **Structured**: `summarize_conversation` + retention-filtered recall
//!   (memories past their horizon are excluded at the retrieval boundary,
//!   RET-CHAT-001, so they bill zero context budget).
//!
//! Measured per treatment: estimated tokens (len/4, the G12 convention) and
//! wall latency.
//!
//! **Honest verdict (measured):** the summary stage does NOT compress the
//! transcript — it is a verbatim re-format. Every sentence lands in at least
//! one bucket (unbucketed sentences fall back to facts), so the summary-side
//! ratio is ≈1.0 plus provenance overhead; the test asserts that no-drop
//! property rather than a compression claim it would fail. The real budget
//! saving is the retention boundary: expired memories drop out of recall and
//! bill zero tokens (measured here: 20 working memories → 10 delivered).
//! Paraphrase compression would need a lossy step — an LLM compressor —
//! which is out of substrate scope by directive. Answer-accuracy is likewise
//! unmeasured: grading answers is a chatbot-layer task.

use aikoql_kernel::*;
use std::sync::Arc;
use std::time::Instant;

fn mk_kernel() -> (Kernel, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0x5EC38).unwrap();
    (k, clock)
}

fn msg(speaker: &str, ts_ms: u64, text: &str) -> ConversationMessage {
    ConversationMessage {
        speaker: speaker.into(),
        timestamp_ms: ts_ms,
        text: text.into(),
    }
}

fn alice() -> KnowledgeContext {
    Subject::new("alice").into()
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn transcript_text(msgs: &[ConversationMessage]) -> String {
    let mut s = String::new();
    for m in msgs {
        s.push_str(&m.speaker);
        s.push_str(": ");
        s.push_str(&m.text);
        s.push('\n');
    }
    s
}

/// The payload the agent receives from the summary: speaker + verbatim item
/// text per bucket, in fixed bucket order (deterministic).
fn render_summary(ko: &KnowledgeObject) -> String {
    let mut out = String::new();
    for name in [
        "facts",
        "decisions",
        "actions",
        "open_issues",
        "constraints",
        "outcomes",
        "entities",
    ] {
        if let Some(Value::List(items)) = ko.properties.get(name) {
            for it in items {
                if let Value::Map(m) = it {
                    if let (Some(Value::Text(t)), Some(Value::Text(s))) =
                        (m.get("text"), m.get("speaker"))
                    {
                        out.push_str(s);
                        out.push_str(": ");
                        out.push_str(t);
                        out.push('\n');
                    }
                }
            }
        }
    }
    out
}

#[test]
fn sec40_memory_compression_measurement() {
    let (k, clock) = mk_kernel();

    // Corpus: 100 messages cycling the seven t_sum1-proven cue sentences
    // (one per bucket) plus chatty filler.
    let cues = [
        "The API lives at /v2 and the db holds |n| customer rows.",
        "We decided to ship the |n|th backend service.",
        "TODO: update the runbook for region |n| before Friday.",
        "Is the migration window for service |n| still open?",
        "Every write to queue |n| must be idempotent.",
        "The load test for service |n| finished within budget.",
        "We now run workload |n| on Acme Cloud.",
    ];
    let filler = [
        "Everyone agreed the current approach is fine for now, though some details still need a second look before the next planning meeting.",
        "The standup ran longer than usual because several people asked follow-up questions about the migration timeline and the new rollout process.",
        "Someone pasted a link to the incident report and the channel went quiet for a while until the on-call shared the post-mortem notes.",
    ];
    let msgs: Vec<ConversationMessage> = (0..100)
        .map(|i| {
            let cue = cues[i % 7].replace("|n|", &i.to_string());
            let speaker = ["alice", "bob", "carol"][i % 3];
            msg(
                speaker,
                (1_000 + i as u64) * 10,
                &format!(
                    "{cue} {} {} {}",
                    filler[i % 3],
                    filler[(i + 1) % 3],
                    filler[(i + 2) % 3]
                ),
            )
        })
        .collect();
    let raw_text = transcript_text(&msgs);

    // Side A — the raw transcript as context.
    let t0 = Instant::now();
    let raw_tokens = raw_text.len() / 4;
    let raw_us = t0.elapsed().as_micros();

    // Side B — structured memory: summary + retention-filtered recall.
    // The no-drop property: the verbatim summary keeps every transcript
    // sentence in at least one bucket (unbucketed sentences fall back to
    // facts). This is why the summary stage cannot compress below the raw
    // transcript — it only adds provenance overhead.
    let sentence_count: usize = msgs
        .iter()
        .map(|m| {
            m.text
                .split_inclusive(['.', '!', '?'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .count()
        })
        .sum();
    let t0 = Instant::now();
    let req = SummarizeConversationRequest {
        context: alice(),
        conversation_id: "conv-40".into(),
        messages: msgs,
        evidence: vec![Evidence::new("chat-log", EvidenceMethod::TestObservation)],
        note: None,
    };
    let summary = k.summarize_conversation(req).unwrap();
    let summary_ko = k.get(alice(), &summary.koid).unwrap();
    let summary_text = render_summary(&summary_ko);
    let item_count: usize = [
        "facts",
        "decisions",
        "actions",
        "open_issues",
        "constraints",
        "outcomes",
    ]
    .iter()
    .filter_map(|b| match summary_ko.properties.get(*b) {
        Some(Value::List(items)) => Some(items.len()),
        _ => None,
    })
    .sum();
    assert!(
        item_count >= sentence_count,
        "every transcript sentence must be retained verbatim: {item_count} items \
         vs {sentence_count} sentences"
    );

    // Working recall: 20 session memories, half inside the retention
    // horizon, half expired — the retrieval boundary drops the expired half,
    // so it bills zero tokens. This is the one real budget saver §40 has.
    for i in 0..20 {
        let mut req = RememberRequest::create(alice(), meta("session_memory"));
        req.properties.insert(
            "text".into(),
            Value::Text(format!(
                "Working memory note {i}: deployment target and rollback steps for region {}",
                ["east", "west"][i % 2]
            )),
        );
        let horizon = if i < 10 { 1_000 } else { 60_000 };
        k.remember_retained(req, horizon).unwrap();
    }
    clock.tick(5_000);
    let recall = k
        .find_similar(SimilarityQuery {
            context: alice(),
            filter: None,
            text: Some("deployment rollback".into()),
            vector: None,
            embedding_model: None,
            k: 20,
            fusion: Fusion::TextOnly,
        })
        .unwrap();
    assert_eq!(
        recall.len(),
        10,
        "expired memories must not bill the context budget"
    );
    let recall_text: String = recall
        .iter()
        .filter_map(|r| match r.ko.properties.get("text") {
            Some(Value::Text(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let structured_tokens = (summary_text.len() + recall_text.len()) / 4;
    let structured_us = t0.elapsed().as_micros();

    let summary_tokens = summary_text.len() / 4;
    let recall_tokens = recall_text.len() / 4;
    let ratio = structured_tokens as f64 / raw_tokens as f64;
    println!(
        "§40 memory compression — raw: {raw_tokens} tokens ({raw_us} us), structured: \
         {structured_tokens} tokens ({structured_us} us) = summary {summary_tokens} + \
         recall {recall_tokens}, ratio {ratio:.2}"
    );

    // Measured verdict: the verbatim summary keeps every sentence, so the
    // summary stage cannot compress below the transcript — only provenance
    // overhead on top. Guard against silent ballooning, not honest overhead.
    assert!(
        ratio < 1.5,
        "structured memory overhead exploded: ratio {ratio:.2}"
    );
    // Sanity ceiling only — catches pathological regressions, not perf tuning.
    assert!(
        structured_us < 5_000_000,
        "structured memory assembly took {structured_us} us"
    );
}
