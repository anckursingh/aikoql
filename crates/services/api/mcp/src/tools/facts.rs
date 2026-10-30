//! AG-12a: remember_facts — the calling agent extracts facts from its
//! conversation (it is the model, with the conversation in context); the
//! server judges each against what is already known: a duplicate is not
//! stored twice, a contradiction becomes a Conflict KO (never an
//! overwrite), an explicit update supersedes, and every stored fact carries
//! authority and provenance (the conversation and its message range).

use crate::session::*;
use crate::{json, Kernel, KnowledgeObject, Value, J};

const FACT_TYPE: &str = "Fact";
/// The most facts one call may carry.
const MAX_FACTS: usize = 200;

fn text(ko: &KnowledgeObject, key: &str) -> Option<String> {
    match ko.properties.get(key) {
        Some(Value::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

/// A statement compared by meaning-preserving form: case, spacing and
/// trailing punctuation do not make a new fact.
fn normalized(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!', '?', ';', ','])
        .to_lowercase()
}

pub(crate) fn tool_remember_facts(k: &Kernel, args: &J) -> Result<J, String> {
    let conversation = args
        .get("conversation_id")
        .and_then(J::as_str)
        .filter(|c| !c.trim().is_empty())
        .ok_or("missing argument: conversation_id")?;
    let facts = args
        .get("facts")
        .and_then(J::as_array)
        .filter(|f| !f.is_empty())
        .ok_or("missing argument: facts (a non-empty array)")?;
    if facts.len() > MAX_FACTS {
        return Err(format!(
            "at most {MAX_FACTS} facts per call, got {}",
            facts.len()
        ));
    }
    // shortcut: one scan of the current facts per call, add a key/statement
    // index when a store holds hundreds of thousands of facts
    let subject = subject_of(args);
    let mut known: Vec<KnowledgeObject> = k
        .scan_by_type(&subject, FACT_TYPE)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|ko| ko.epistemic_status() != crate::EpistemicStatus::Superseded)
        .collect();
    let results: Vec<J> = facts
        .iter()
        .map(
            |fact| match judge(k, args, conversation, fact, &mut known) {
                Ok(r) => r,
                Err(e) => {
                    json!({"status": "error", "error": e, "statement": fact.get("statement")})
                }
            },
        )
        .collect();
    Ok(json!({ "conversation_id": conversation, "results": results }))
}

/// Decide one fact: superseded, duplicate, conflict or created.
fn judge(
    k: &Kernel,
    args: &J,
    conversation: &str,
    fact: &J,
    known: &mut Vec<KnowledgeObject>,
) -> Result<J, String> {
    let statement = fact
        .get("statement")
        .and_then(J::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or("each fact needs a statement")?;
    let field = |name: &str| fact.get(name).and_then(J::as_str).filter(|v| !v.is_empty());
    let (key, value) = (field("key"), field("value"));
    let location = match fact
        .get("messages")
        .and_then(J::as_array)
        .map(|m| m.as_slice())
    {
        Some([a, b]) => format!("messages {}-{}", a, b),
        Some([a]) => format!("message {a}"),
        _ => "conversation".into(),
    };
    let evidence = json!([{"source_artifact": format!("conversation:{conversation}"),
        "method": "llm_inference", "location": location}]);
    let mut props = json!({"statement": statement, "conversation_id": conversation});
    for name in ["key", "value", "speaker"] {
        if let Some(v) = field(name) {
            props[name] = json!(v);
        }
    }
    let mut req = args.clone();
    for name in ["facts", "conversation_id"] {
        req.as_object_mut().map(|o| o.remove(name));
    }
    req["evidence"] = evidence;
    req["properties"] = props;
    req["authority"] = json!(field("authority").unwrap_or("llm_inferred"));

    // an explicit update: the agent says this replaces a known fact
    if let Some(old) = field("supersedes") {
        req["old"] = json!(old);
        req["type_name"] = json!(FACT_TYPE);
        req["reason"] = json!(field("reason").unwrap_or("updated in conversation"));
        let r = crate::tools::tool_supersede(k, &req)?;
        known.retain(|ko| ko.koid.to_hex() != old);
        remember_known(k, args, &r["new"], known);
        return Ok(
            json!({"status": "superseded", "koid": r["new"], "old": old, "statement": statement}),
        );
    }
    let norm = normalized(statement);
    if let Some(same) = known
        .iter()
        .find(|ko| text(ko, "statement").is_some_and(|s| normalized(&s) == norm))
    {
        return Ok(
            json!({"status": "duplicate", "koid": same.koid.to_hex(), "statement": statement}),
        );
    }
    if let Some(key) = key {
        if let Some(other) = known
            .iter()
            .find(|ko| text(ko, "key").as_deref() == Some(key))
        {
            if text(other, "value").as_deref() == value {
                return Ok(
                    json!({"status": "duplicate", "koid": other.koid.to_hex(), "statement": statement}),
                );
            }
            // never an overwrite: the counter-claim and a Conflict KO, both
            // kept until a resolution decision
            req["claim"] = json!(other.koid.to_hex());
            req["counter_type"] = json!(FACT_TYPE);
            let r = crate::tools::tool_contradict(k, &req)?;
            return Ok(
                json!({"status": "conflict", "koid": r["counter"], "conflict": r["conflict"],
                "contradicts": other.koid.to_hex(), "statement": statement}),
            );
        }
    }
    req["type_name"] = json!(FACT_TYPE);
    let r = crate::tools::tool_assert_knowledge(k, &req)?;
    remember_known(k, args, &r["koid"], known);
    Ok(json!({"status": "created", "koid": r["koid"], "statement": statement}))
}

/// A fact this call created joins `known`, so a later fact in the same call
/// is judged against it too.
fn remember_known(k: &Kernel, args: &J, koid: &J, known: &mut Vec<KnowledgeObject>) {
    if let Some(id) = koid.as_str().and_then(|h| crate::KOID::from_hex(h).ok()) {
        if let Ok(ko) = k.get(subject_of(args), &id) {
            known.push(ko);
        }
    }
}
