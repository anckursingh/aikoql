//! G13 §38–39 — Conversation summarization: a structured summary KO that
//! preserves facts/decisions/actions/open issues/entities/constraints/
//! outcomes, never invents facts (verbatim extraction only), and carries
//! per-item provenance (conversation, message range, speaker, timestamp).

use aikoql_kernel::*;
use std::sync::Arc;

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

fn summarize(k: &Kernel, conversation_id: &str, msgs: Vec<ConversationMessage>) -> KnowledgeObject {
    let req = SummarizeConversationRequest {
        context: Subject::new("alice").into(),
        conversation_id: conversation_id.into(),
        messages: msgs,
        evidence: vec![Evidence::new("chat-log", EvidenceMethod::TestObservation)],
        note: None,
    };
    let r = k.summarize_conversation(req).unwrap();
    k.get(Subject::new("alice"), &r.koid).unwrap()
}

fn bucket(ko: &KnowledgeObject, name: &str) -> Vec<Value> {
    match ko.properties.get(name) {
        Some(Value::List(items)) => items.clone(),
        other => panic!("expected List bucket {name}, got {other:?}"),
    }
}

fn item_text(v: &Value) -> String {
    match v {
        Value::Map(m) => match m.get("text") {
            Some(Value::Text(t)) => t.clone(),
            other => panic!("expected text in item, got {other:?}"),
        },
        other => panic!("expected Map item, got {other:?}"),
    }
}

#[test]
fn t_sum1_preserves_all_seven_categories() {
    let (k, _c) = mk_kernel();
    let msgs = vec![
        msg(
            "alice",
            1_000,
            "The API lives at /v2. The db holds customer rows.",
        ),
        msg("bob", 2_000, "We decided to ship the Rust backend."),
        msg("carol", 3_000, "TODO: update the runbook before Friday."),
        msg("alice", 4_000, "Is the migration window still open?"),
        msg("bob", 5_000, "Every write must be idempotent."),
        msg("carol", 6_000, "The load test finished within budget."),
        msg("alice", 7_000, "We now run on Acme Cloud."),
    ];
    let ko = summarize(&k, "conv-1", msgs);

    assert_eq!(ko.metadata.type_name, "aikoql:conversation_summary");
    assert_eq!(
        ko.properties.get("conversation_id"),
        Some(&Value::Text("conv-1".into()))
    );
    assert_eq!(ko.properties.get("message_count"), Some(&Value::Int(7)));

    let texts = |name: &str| bucket(&ko, name).iter().map(item_text).collect::<Vec<_>>();
    assert!(texts("facts")
        .iter()
        .any(|t| t.contains("API lives at /v2")));
    assert!(texts("decisions")
        .iter()
        .any(|t| t.contains("ship the Rust backend")));
    assert!(texts("actions")
        .iter()
        .any(|t| t.contains("update the runbook")));
    assert!(texts("open_issues")
        .iter()
        .any(|t| t.contains("migration window")));
    assert!(texts("constraints")
        .iter()
        .any(|t| t.contains("idempotent")));
    assert!(texts("outcomes")
        .iter()
        .any(|t| t.contains("load test finished")));
    assert!(texts("entities").iter().any(|t| t == "Acme Cloud"));
}

#[test]
fn t_sum2_never_invents_facts() {
    let (k, _c) = mk_kernel();
    let msgs = vec![
        msg(
            "alice",
            1_000,
            "The build is green. We decided to freeze the API. TODO: write changelog.",
        ),
        msg(
            "bob",
            2_000,
            "Deploy must not happen on Friday. The canary passed.",
        ),
    ];
    let ko = summarize(&k, "conv-2", msgs.clone());

    for name in [
        "facts",
        "decisions",
        "actions",
        "open_issues",
        "constraints",
        "outcomes",
        "entities",
    ] {
        for item in bucket(&ko, name) {
            let t = item_text(&item);
            assert!(
                msgs.iter().any(|m| m.text.contains(&t)),
                "bucket {name} item {t:?} is not verbatim from the transcript"
            );
        }
    }
}

#[test]
fn t_sum3_items_carry_provenance_and_evidence_is_mandatory() {
    let (k, _c) = mk_kernel();
    let msgs = vec![msg("bob", 2_000, "We decided to freeze the API.")];
    let ko = summarize(&k, "conv-3", msgs.clone());

    let items = bucket(&ko, "decisions");
    assert_eq!(items.len(), 1);
    match &items[0] {
        Value::Map(m) => {
            assert_eq!(m.get("speaker"), Some(&Value::Text("bob".into())));
            assert_eq!(
                m.get("msg_range"),
                Some(&Value::List(vec![Value::Int(0), Value::Int(0)]))
            );
            assert_eq!(m.get("ts_ms"), Some(&Value::Int(2_000)));
        }
        other => panic!("expected Map item, got {other:?}"),
    }

    // evidence is mandatory — a summary nobody can trace to a transcript
    // is not knowledge (same rule as every other knowledge op).
    let no_ev = SummarizeConversationRequest {
        context: Subject::new("alice").into(),
        conversation_id: "conv-3b".into(),
        messages: msgs,
        evidence: vec![],
        note: None,
    };
    assert!(matches!(
        k.summarize_conversation(no_ev).unwrap_err(),
        KError::InvalidObject(_)
    ));

    // an empty transcript is not summarizable either
    let empty = SummarizeConversationRequest {
        context: Subject::new("alice").into(),
        conversation_id: "conv-3c".into(),
        messages: vec![],
        evidence: vec![Evidence::new("chat-log", EvidenceMethod::TestObservation)],
        note: None,
    };
    assert!(matches!(
        k.summarize_conversation(empty).unwrap_err(),
        KError::InvalidObject(_)
    ));
}

#[test]
fn t_sum4_100_message_transcript_is_deterministic() {
    let (k, _c) = mk_kernel();
    let msgs: Vec<ConversationMessage> = (0..100)
        .map(|i| {
            let text = if i % 4 == 0 {
                format!("Milestone {i} was completed by the team.")
            } else if i % 4 == 1 {
                format!("Task T{i} must finish before demo day.")
            } else if i % 4 == 2 {
                format!("The service {i} reported no errors.")
            } else {
                format!("TODO: review plan {i}.")
            };
            msg("bot", (1_000 + i as u64) * 10, &text)
        })
        .collect();

    let ko1 = summarize(&k, "conv-100", msgs.clone());
    assert_eq!(ko1.properties.get("message_count"), Some(&Value::Int(100)));
    let ko2 = summarize(&k, "conv-100", msgs);
    assert_eq!(
        ko1.properties, ko2.properties,
        "summary must be deterministic"
    );
}
