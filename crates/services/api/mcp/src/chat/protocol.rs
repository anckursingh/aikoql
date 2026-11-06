//! Tenant-chat model protocol: prompts and strict reply parsers.
//! Port of `training/src/aikoql_training/inference.py` (the source of truth;
//! prompts must stay byte-identical, pinned by `tests/fixtures/chat/prompts.json`).

pub(crate) fn build_query_prompt(_question: &str) -> String {
    String::new()
}

pub(crate) fn build_answer_prompt(_question: &str, _statements: &[String]) -> String {
    String::new()
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

pub(crate) fn parse_query_reply(_text: &str) -> Result<QueryReply, String> {
    Err("unimplemented".into())
}

pub(crate) fn parse_answer_reply(_text: &str) -> Result<AnswerReply, String> {
    Err("unimplemented".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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
            "{", "}", "[", "]", "\"", ":", ",", "null", "true", "query", "answer", "claims",
            "grounded", "refusal_reason", "evidence_ids", "statement", "\\", "\0", "é", "中",
            "\u{1F600}", " ", "\n", "```", "UNKNOWN:", "0", "-1e999",
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
            let t: String = (0..n).map(|_| TOK[(next() % TOK.len() as u64) as usize]).collect();
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
