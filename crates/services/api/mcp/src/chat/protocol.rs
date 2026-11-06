//! Tenant-chat model protocol: prompts and strict reply parsers.
//! Port of `training/src/aikoql_training/inference.py` (the source of truth;
//! prompts must stay byte-identical, pinned by `tests/fixtures/chat/prompts.json`).

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

pub(crate) fn build_query_prompt(question: &str) -> String {
    format!(
        "Question: {question}\n\
         Reply with exactly one JSON object, no other text:\n\
         {{\"query\": \"<aikoql query>\"}}\n\
         If the question cannot be answered from a knowledge graph, reply:\n\
         {{\"query\": null, \"refusal_reason\": \"<short reason>\"}}\n"
    )
}

pub(crate) fn build_answer_prompt(question: &str, statements: &[String]) -> String {
    let lines = statements
        .iter()
        .map(|s| format!("- {s}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Question: {question}\n\n\
         Knowledge context:\n{lines}\n\n\
         Answer the question from the context above. Reply with exactly one JSON object, no other text:\n\
         {{\"answer\": \"<answer>\", \"grounded\": true,\n \
         \"claims\": [{{\"statement\": \"<a context statement supporting the answer>\", \"evidence_ids\": [\"<evidence id>\"]}}],\n \
         \"refusal_reason\": null}}\n\
         If the context does not contain the answer, reply:\n\
         {{\"answer\": \"UNKNOWN: <short reason>\", \"grounded\": false, \"claims\": [], \"refusal_reason\": \"<short reason>\"}}\n"
    )
}

#[derive(Debug, PartialEq)]
pub(crate) enum QueryReply {
    Query(String),
    Refusal(String),
}

#[derive(Debug, PartialEq)]
pub(crate) struct Claim {
    pub statement: String,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct AnswerReply {
    pub answer: String,
    pub grounded: bool,
    pub claims: Vec<Claim>,
    pub refusal_reason: Option<String>,
}

const UNKNOWN_PREFIX: &str = "UNKNOWN:";

/// A JSON value whose objects reject duplicate keys at any depth. serde_json's
/// built-in recursion limit (128) turns absurd nesting into an `Err`, not a stack overflow.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Value;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
                Ok(Value::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
                Ok(v.into())
            }
            fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
                Ok(v.into())
            }
            fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
                Ok(v.into())
            }
            fn visit_str<E>(self, v: &str) -> Result<Value, E> {
                Ok(Value::String(v.to_owned()))
            }
            fn visit_string<E>(self, v: String) -> Result<Value, E> {
                Ok(Value::String(v))
            }
            fn visit_unit<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
                let mut out = Vec::new();
                while let Some(Strict(v)) = a.next_element()? {
                    out.push(v);
                }
                Ok(Value::Array(out))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
                let mut out = serde_json::Map::new();
                while let Some((k, Strict(v))) = a.next_entry::<String, Strict>()? {
                    if out.insert(k.clone(), v).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate key {k:?}")));
                    }
                }
                Ok(Value::Object(out))
            }
        }
        d.deserialize_any(V).map(Strict)
    }
}

/// The first JSON object in the reply; prose or a code fence around it is tolerated.
fn json_object(text: &str) -> Result<serde_json::Map<String, Value>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty model reply".into());
    }
    let start = text.find('{').ok_or("model reply carries no JSON object")?;
    let mut de = serde_json::Deserializer::from_str(&text[start..]);
    match Strict::deserialize(&mut de) {
        Ok(Strict(Value::Object(m))) => Ok(m),
        Ok(_) => Err("model reply is not a JSON object".into()),
        Err(e) => Err(format!("model reply is not valid JSON: {e}")),
    }
}

fn check_fields(obj: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(k) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("unknown field in reply: {k:?}"));
    }
    if let Some(k) = allowed.iter().find(|k| !obj.contains_key(**k)) {
        return Err(format!("missing field in reply: {k:?}"));
    }
    Ok(())
}

/// `Some(non-blank string)` or `None` for null; anything else is an error.
fn opt_text(v: &Value, name: &str) -> Result<Option<String>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if !s.trim().is_empty() => Ok(Some(s.clone())),
        _ => Err(format!("{name} must be a non-empty string or null")),
    }
}

pub(crate) fn parse_query_reply(text: &str) -> Result<QueryReply, String> {
    let obj = json_object(text)?;
    check_fields(&obj, &["query", "refusal_reason"])?;
    match (
        opt_text(&obj["query"], "query")?,
        opt_text(&obj["refusal_reason"], "refusal_reason")?,
    ) {
        (Some(q), None) => Ok(QueryReply::Query(q)),
        (None, Some(r)) => Ok(QueryReply::Refusal(r)),
        _ => Err("exactly one of query and refusal_reason must be set".into()),
    }
}

fn parse_claim(v: &Value) -> Result<Claim, String> {
    let o = v.as_object().ok_or("malformed claim")?;
    check_fields(o, &["statement", "evidence_ids"]).map_err(|e| format!("malformed claim: {e}"))?;
    let statement = opt_text(&o["statement"], "claim statement")?
        .ok_or("claim statement must be a non-empty string")?;
    let evidence_ids = o["evidence_ids"]
        .as_array()
        .ok_or("claim evidence_ids must be a list")?
        .iter()
        .map(|i| opt_text(i, "evidence id")?.ok_or_else(|| "evidence id must be a string".into()))
        .collect::<Result<_, String>>()?;
    Ok(Claim {
        statement,
        evidence_ids,
    })
}

pub(crate) fn parse_answer_reply(text: &str) -> Result<AnswerReply, String> {
    let obj = json_object(text)?;
    check_fields(&obj, &["answer", "grounded", "claims", "refusal_reason"])?;
    let answer = opt_text(&obj["answer"], "answer")?.ok_or("answer must be a non-empty string")?;
    let grounded = obj["grounded"]
        .as_bool()
        .ok_or("grounded must be a boolean")?;
    let claims = obj["claims"]
        .as_array()
        .ok_or("claims must be a list")?
        .iter()
        .map(parse_claim)
        .collect::<Result<Vec<_>, _>>()?;
    let refusal_reason = opt_text(&obj["refusal_reason"], "refusal_reason")?;
    if refusal_reason.is_some() != answer.starts_with(UNKNOWN_PREFIX) {
        return Err("refusal_reason and the UNKNOWN: answer must agree".into());
    }
    if refusal_reason.is_some() && grounded {
        return Err("a refusal cannot be grounded".into());
    }
    if grounded != !claims.is_empty() {
        return Err("grounded must agree with the claims".into());
    }
    Ok(AnswerReply {
        answer,
        grounded,
        claims,
        refusal_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = include_str!("../../tests/fixtures/chat/prompts.json");

    #[test]
    fn prompts_equal_the_python_fixtures() {
        let f: Value = serde_json::from_str(FIXTURES).unwrap();
        assert!(f["query"].as_array().unwrap().len() >= 4);
        for c in f["query"].as_array().unwrap() {
            assert_eq!(
                build_query_prompt(c["question"].as_str().unwrap()),
                c["prompt"].as_str().unwrap()
            );
        }
        for c in f["answer"].as_array().unwrap() {
            let st: Vec<String> = c["statements"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect();
            assert_eq!(
                build_answer_prompt(c["question"].as_str().unwrap(), &st),
                c["prompt"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn query_reply_accepts_query_or_refusal() {
        assert_eq!(
            parse_query_reply(r#"{"query":"MATCH X RETURN a","refusal_reason":null}"#),
            Ok(QueryReply::Query("MATCH X RETURN a".into()))
        );
        assert_eq!(
            parse_query_reply(r#"{"query":null,"refusal_reason":"no graph"}"#),
            Ok(QueryReply::Refusal("no graph".into()))
        );
    }

    #[test]
    fn query_reply_rejects() {
        for bad in [
            r#"{"query":"x","refusal_reason":"y"}"#,
            r#"{"query":null,"refusal_reason":null}"#,
            r#"{"query":"x","refusal_reason":null,"extra":1}"#,
            r#"{"query":"a","query":"b","refusal_reason":null}"#,
            r#"{"query":"x"}"#,
            r#"{"query":42,"refusal_reason":null}"#,
            r#"{"query":"  ","refusal_reason":null}"#,
            r#"[1,2]"#,
            "",
            "I cannot help with that.",
        ] {
            assert!(parse_query_reply(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn reply_tolerates_prose_and_fence() {
        let t = "Sure!\n```json\n{\"query\":\"Q\",\"refusal_reason\":null}\n```";
        assert_eq!(parse_query_reply(t), Ok(QueryReply::Query("Q".into())));
        let a = "Answer:\n```json\n{\"answer\":\"A\",\"grounded\":true,\"claims\":[{\"statement\":\"s\",\"evidence_ids\":[\"e1\"]}],\"refusal_reason\":null}\n```";
        assert!(parse_answer_reply(a).unwrap().grounded);
    }

    fn ans(answer: &str, grounded: bool, claims: &str, reason: &str) -> String {
        format!(
            r#"{{"answer":"{answer}","grounded":{grounded},"claims":{claims},"refusal_reason":{reason}}}"#
        )
    }
    const CLAIM: &str = r#"[{"statement":"s","evidence_ids":["e1"]}]"#;

    #[test]
    fn answer_reply_cross_constraints() {
        let r = parse_answer_reply(&ans("UNKNOWN: nope", false, "[]", r#""nope""#)).unwrap();
        assert_eq!(r.refusal_reason.as_deref(), Some("nope"));
        assert!(r.claims.is_empty());
        let g = parse_answer_reply(&ans("Alice", true, CLAIM, "null")).unwrap();
        assert_eq!(g.claims[0].evidence_ids, vec!["e1".to_string()]);
        for bad in [
            ans("Alice", true, "[]", "null"),
            ans("Alice", false, CLAIM, "null"),
            ans("Alice", false, "[]", r#""nope""#),
            ans("UNKNOWN: x", false, "[]", "null"),
            ans("UNKNOWN: x", true, CLAIM, r#""x""#),
            ans("UNKNOWN: x", false, CLAIM, r#""x""#),
        ] {
            assert!(parse_answer_reply(&bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn answer_reply_rejects_wrong_types() {
        for bad in [
            r#"{"answer":1,"grounded":false,"claims":[],"refusal_reason":null}"#,
            r#"{"answer":"a","grounded":"yes","claims":[{"statement":"s","evidence_ids":["e"]}],"refusal_reason":null}"#,
            r#"{"answer":"a","grounded":true,"claims":{},"refusal_reason":null}"#,
            r#"{"answer":"a","grounded":true,"claims":[{"statement":"s"}],"refusal_reason":null}"#,
            r#"{"answer":"a","grounded":true,"claims":[{"statement":"s","evidence_ids":[1]}],"refusal_reason":null}"#,
            r#"{"answer":"a","grounded":true,"claims":[{"statement":"s","evidence_ids":["e"],"x":1}],"refusal_reason":null}"#,
            r#"{"answer":"a","grounded":true,"claims":[{"statement":"s","evidence_ids":["e"]}],"refusal_reason":null,"score":1}"#,
            r#"{"answer":"a","grounded":true,"claims":[{"statement":"s","evidence_ids":["e"]}]}"#,
            r#"{"answer":"a","grounded":true,"claims":[{"statement":"s","evidence_ids":["e"],"statement":"t"}],"refusal_reason":null}"#,
            "The answer is Alice.",
        ] {
            assert!(parse_answer_reply(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn parsers_never_panic_on_random_text() {
        const TOK: &[&str] = &[
            "{",
            "}",
            "[",
            "]",
            "\"",
            ":",
            ",",
            "null",
            "true",
            "query",
            "answer",
            "claims",
            "grounded",
            "refusal_reason",
            "evidence_ids",
            "statement",
            "\\",
            "\0",
            "é",
            "中",
            "\u{1F600}",
            " ",
            "\n",
            "```",
            "UNKNOWN:",
            "0",
            "-1e999",
        ];
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..2000 {
            let n = (next() % 40) as usize;
            let t: String = (0..n)
                .map(|_| TOK[(next() % TOK.len() as u64) as usize])
                .collect();
            let _ = parse_query_reply(&t);
            let _ = parse_answer_reply(&t);
        }
        let deep = "{".repeat(10_000);
        assert!(parse_query_reply(&deep).is_err());
        assert!(parse_answer_reply(&deep).is_err());
        let deep = format!("{}{}", "{\"a\":".repeat(10_000), "1");
        assert!(parse_answer_reply(&deep).is_err());
    }
}
