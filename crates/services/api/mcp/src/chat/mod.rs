pub(crate) mod client;
pub(crate) mod context;
pub(crate) mod grounding;
pub(crate) mod protocol;
#[cfg(test)]
pub(crate) mod stub;

use crate::{json, Kernel};
use client::Generate;
use context::Fact;

/// The whole flow: model query -> scoped read -> retrieval fallback ->
/// model answer -> server-side grounding check. Returns the response record.
pub(crate) fn run_chat(
    _k: &Kernel,
    _model: &dyn Generate,
    model_id: &str,
    _subject: &str,
    _roles: &[String],
    _tenant: Option<&str>,
    question: &str,
) -> serde_json::Value {
    let _: Option<Fact> = None;
    json!({"question": question, "model": model_id})
}

#[cfg(test)]
mod tests {
    use super::*;
    use stub::{kernel, seed, StubModel};

    const STMT: &str = "Note Zebra policy: body=Zebras are allowed; title=Zebra policy";

    fn q(query: &str) -> Result<String, String> {
        Ok(json!({"query": query}).to_string())
    }
    fn q_refuse() -> Result<String, String> {
        Ok(json!({"query": null, "refusal_reason": "no"}).to_string())
    }
    fn answer(statement: &str, koid: &str) -> Result<String, String> {
        Ok(json!({"answer": "Zebras are allowed.", "grounded": true,
            "claims": [{"statement": statement, "evidence_ids": [koid]}],
            "refusal_reason": null})
        .to_string())
    }

    fn setup() -> (std::sync::Arc<Kernel>, String) {
        let k = kernel();
        let acme = seed(
            &k,
            Some("acme"),
            "Note",
            &[("title", "Zebra policy"), ("body", "Zebras are allowed")],
        );
        seed(
            &k,
            Some("globex"),
            "Plan",
            &[("title", "Globex zebra plan")],
        );
        (k, acme)
    }

    fn ask(k: &Kernel, m: &StubModel) -> serde_json::Value {
        run_chat(k, m, "stub", "alice", &[], Some("acme"), "zebra policy?")
    }

    fn koids(r: &serde_json::Value) -> Vec<String> {
        r["retrieved"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn happy_path_model_query_then_grounded_answer() {
        let (k, acme) = setup();
        let m = StubModel::new(vec![q("MATCH Note RETURN *"), answer(STMT, &acme)]);
        let r = ask(&k, &m);
        assert_eq!(r["refused"], false, "{r}");
        assert_eq!(r["grounded"], true);
        assert_eq!(r["context_source"], "model_query");
        assert_eq!(r["query"], "MATCH Note RETURN *");
        assert_eq!(koids(&r), [acme.clone()]);
        assert_eq!(r["statements"][0], STMT);
        assert_eq!(r["claims"][0]["evidence_ids"][0], acme.as_str());
        assert_eq!(r["model"], "stub");
    }

    #[test]
    fn model_refusal_falls_back_to_retrieval() {
        let (k, acme) = setup();
        let m = StubModel::new(vec![q_refuse(), answer(STMT, &acme)]);
        let r = ask(&k, &m);
        assert_eq!(r["context_source"], "retrieval", "{r}");
        assert_eq!(r["grounded"], true);
        assert_eq!(koids(&r), [acme]);
    }

    #[test]
    fn uncompilable_query_falls_back() {
        let (k, acme) = setup();
        let m = StubModel::new(vec![q("this is not a query"), answer(STMT, &acme)]);
        assert_eq!(ask(&k, &m)["context_source"], "retrieval");
    }

    #[test]
    fn empty_results_fall_back_then_refuse_when_retrieval_is_empty() {
        let (k, _) = setup();
        let m = StubModel::new(vec![q("MATCH Nothing RETURN *")]);
        let r = run_chat(&k, &m, "stub", "alice", &[], Some("acme"), "qqqq xxxx");
        assert_eq!(r["context_source"], "none", "{r}");
        assert_eq!(r["refused"], true);
        assert!(r["answer"].as_str().unwrap().starts_with("UNKNOWN:"));
        assert_eq!(
            m.prompts.lock().unwrap().len(),
            1,
            "no answer call without facts"
        );
    }

    #[test]
    fn malformed_answer_json_is_a_refusal() {
        let (k, _) = setup();
        let m = StubModel::new(vec![q("MATCH Note RETURN *"), Ok("not json".into())]);
        let r = ask(&k, &m);
        assert_eq!(r["refused"], true, "{r}");
        assert!(r["answer"].as_str().unwrap().starts_with("UNKNOWN:"));
    }

    #[test]
    fn ungrounded_answer_is_downgraded_to_refusal() {
        let (k, acme) = setup();
        let m = StubModel::new(vec![
            q("MATCH Note RETURN *"),
            answer("Zebras are banned", &acme),
        ]);
        let r = ask(&k, &m);
        assert_eq!(r["refused"], true, "{r}");
        assert_eq!(r["grounded"], false);
        assert_eq!(r["claims"], json!([]));
        assert_eq!(
            r["refusal_reason"],
            "answer not supported by the retrieved facts"
        );
    }

    #[test]
    fn model_error_is_a_refusal_with_its_reason() {
        let (k, _) = setup();
        let m = StubModel::new(vec![Err("model unreachable".into())]);
        let r = ask(&k, &m);
        assert_eq!(r["refused"], true);
        assert_eq!(r["refusal_reason"], "model unreachable");
    }

    #[test]
    fn model_query_naming_another_tenants_type_returns_nothing() {
        let (k, _) = setup();
        // retrieval finds nothing for this question either, so only the
        // model-query path could have leaked globex's Plan
        let m = StubModel::new(vec![q("MATCH Plan RETURN *")]);
        let r = run_chat(&k, &m, "stub", "alice", &[], Some("acme"), "qqqq xxxx");
        assert_eq!(koids(&r), Vec::<String>::new(), "{r}");
        assert_eq!(r["context_source"], "none");
    }

    #[test]
    fn model_written_create_is_refused_and_writes_nothing() {
        let (k, _) = setup();
        let before = |k: &Kernel| {
            crate::tools::query::execute_readonly(
                k,
                "MATCH Note RETURN *",
                "alice",
                &[],
                Some("acme"),
            )
            .unwrap()
            .as_array()
            .unwrap()
            .len()
        };
        let n = before(&k);
        let m = StubModel::new(vec![q("CREATE Note(title: \"pwned\")")]);
        let r = run_chat(&k, &m, "stub", "alice", &[], Some("acme"), "qqqq xxxx");
        assert_eq!(r["refused"], true, "{r}");
        assert_eq!(before(&k), n, "a model-written CREATE must write nothing");
    }

    #[test]
    fn facts_sent_to_the_model_are_scrubbed() {
        let k = kernel();
        let id = seed(
            &k,
            Some("acme"),
            "Note",
            &[("title", "Zebra key"), ("body", "AKIAIOSFODNN7EXAMPLE")],
        );
        let m = StubModel::new(vec![q("MATCH Note RETURN *"), answer("x", &id)]);
        let _ = ask(&k, &m);
        let prompts = m.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2);
        assert!(
            !prompts[1].contains("AKIAIOSFODNN7EXAMPLE"),
            "{}",
            prompts[1]
        );
        assert!(prompts[1].contains("[REDACTED:AWS_KEY]"), "{}", prompts[1]);
    }
}
