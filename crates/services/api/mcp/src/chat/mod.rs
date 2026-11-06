pub(crate) mod client;
pub(crate) mod context;
pub(crate) mod grounding;
pub(crate) mod protocol;
#[cfg(test)]
pub(crate) mod stub;

use crate::{json, Kernel};
use client::Generate;
use context::Fact;
use protocol::{AnswerReply, QueryReply};

const NOT_SUPPORTED: &str = "answer not supported by the retrieved facts";

/// Model query (read-only, caller-scoped) -> facts, or None to fall back.
fn facts_from_model_query(
    k: &Kernel,
    model: &dyn Generate,
    subject: &str,
    roles: &[String],
    tenant: Option<&str>,
    question: &str,
) -> Result<(Option<String>, Vec<Fact>), String> {
    let reply = model.generate(&protocol::build_query_prompt(question))?;
    let Ok(QueryReply::Query(query)) = protocol::parse_query_reply(&reply) else {
        return Ok((None, Vec::new()));
    };
    // The compile path also accepts CREATE/UPDATE/DELETE/INGEST: a model
    // must only ever read.
    let is_read = matches!(
        aikoql_compiler::parser::parse(&query),
        Ok(aikoql_compiler::parser::ast::Statement::Match(_))
    );
    if !is_read {
        return Ok((Some(query), Vec::new()));
    }
    let rows = crate::tools::query::execute_readonly(k, &query, subject, roles, tenant);
    let facts = rows
        .map(|r| context::facts_from_rows(&r))
        .unwrap_or_default();
    Ok((Some(query), facts))
}

fn answer_from_model(
    model: &dyn Generate,
    question: &str,
    facts: &[Fact],
) -> Result<AnswerReply, String> {
    let statements: Vec<String> = facts.iter().map(|f| f.statement.clone()).collect();
    let reply = model.generate(&protocol::build_answer_prompt(question, &statements))?;
    let answer = protocol::parse_answer_reply(&reply)
        .map_err(|e| format!("model answer was malformed: {e}"))?;
    grounding::verify(&answer, facts).map_err(|_| NOT_SUPPORTED.to_string())?;
    Ok(answer)
}

pub(crate) const NOT_CONFIGURED: &str =
    "chat is not configured — add a [chat] table to aikoql.toml";

fn question_of(args: &crate::J) -> Result<&str, String> {
    args.get("question")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|q| (1..=2000).contains(&q.chars().count()))
        .ok_or_else(|| "question must be 1-2000 characters".to_string())
}

/// ASCII-only and short: `audit_log` cuts its detail at 200 bytes with a
/// raw slice, which would panic inside a multi-byte character.
fn audit_safe(s: &str) -> String {
    s.chars().filter(char::is_ascii_graphic).take(40).collect()
}

/// REST/MCP entry: validates the question, then runs the flow against the
/// configured model and writes one id-only audit line.
pub(crate) fn tool_chat(k: &Kernel, args: &crate::J, db_path: &str) -> Result<crate::J, String> {
    question_of(args)?;
    let cfg = client::CHAT_CONFIG.get().ok_or(NOT_CONFIGURED)?;
    tool_chat_with(
        k,
        &client::HttpModel::new(cfg.clone()),
        &cfg.model,
        args,
        db_path,
    )
}

/// `{enabled, model, reachable}` for the Studio Ask panel.
pub(crate) fn chat_status() -> crate::J {
    match client::CHAT_CONFIG.get() {
        None => json!({"enabled": false, "model": null, "reachable": null}),
        Some(c) => json!({
            "enabled": true,
            "model": c.model,
            "reachable": client::reachable(&c.url, std::time::Duration::from_secs(2)),
        }),
    }
}

pub(crate) fn tool_chat_with(
    k: &Kernel,
    model: &dyn Generate,
    model_id: &str,
    args: &crate::J,
    db_path: &str,
) -> Result<crate::J, String> {
    let question = question_of(args)?;
    let who = crate::session::subject_of(args);
    let record = run_chat(
        k,
        model,
        model_id,
        &who.name,
        &who.roles,
        who.tenant.as_deref(),
        question,
    );
    let reason = record["refusal_reason"].as_str().unwrap_or("");
    let outcome = if reason.starts_with("model ") {
        "error"
    } else if record["refused"] == true {
        "refused"
    } else {
        "answered"
    };
    let koids: Vec<&str> = record["retrieved"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    let detail = format!(
        "context={} ms={} model={} tenant={} koids={}",
        record["context_source"].as_str().unwrap_or(""),
        record["ms"],
        audit_safe(model_id),
        audit_safe(who.tenant.as_deref().unwrap_or("-")),
        koids.join(","),
    );
    crate::audit::audit_log(db_path, &who.name, "chat", outcome, &detail);
    Ok(record)
}

/// The whole flow: model query -> scoped read -> retrieval fallback ->
/// model answer -> server-side grounding check. Returns the response record.
pub(crate) fn run_chat(
    k: &Kernel,
    model: &dyn Generate,
    model_id: &str,
    subject: &str,
    roles: &[String],
    tenant: Option<&str>,
    question: &str,
) -> serde_json::Value {
    let t0 = std::time::Instant::now();
    let mut query: Option<String> = None;
    let mut source = "none";
    let mut facts: Vec<Fact> = Vec::new();

    let outcome = (|| -> Result<AnswerReply, String> {
        let (q, from_query) = facts_from_model_query(k, model, subject, roles, tenant, question)?;
        query = q;
        facts = from_query;
        if !facts.is_empty() {
            source = "model_query";
        } else {
            facts = context::retrieve_fallback(k, subject, roles, tenant, question);
            if !facts.is_empty() {
                source = "retrieval";
            }
        }
        if facts.is_empty() {
            return Err("no relevant facts were found".into());
        }
        answer_from_model(model, question, &facts)
    })();

    let (answer, claims, refusal) = match outcome {
        Ok(a) if a.refusal_reason.is_none() => (a.answer, a.claims, None),
        Ok(a) => (a.answer, Vec::new(), a.refusal_reason),
        Err(reason) => (format!("UNKNOWN: {reason}"), Vec::new(), Some(reason)),
    };
    json!({
        "question": question,
        "answer": answer,
        "grounded": refusal.is_none(),
        "refused": refusal.is_some(),
        "refusal_reason": refusal,
        "claims": claims.iter().map(|c| json!({
            "statement": c.statement, "evidence_ids": c.evidence_ids,
        })).collect::<Vec<_>>(),
        "query": query,
        "context_source": source,
        "retrieved": facts.iter().map(|f| f.koid.clone()).collect::<Vec<_>>(),
        "statements": facts.iter().map(|f| f.statement.clone()).collect::<Vec<_>>(),
        "model": model_id,
        "ms": t0.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stub::{kernel, seed, StubModel};

    const STMT: &str = "Note Zebra policy: body=Zebras are allowed; title=Zebra policy";

    fn q(query: &str) -> Result<String, String> {
        Ok(json!({"query": query, "refusal_reason": null}).to_string())
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
        assert_eq!(koids(&r), std::slice::from_ref(&acme));
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

    fn tmp_db(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("aikoql-chat-{tag}-{}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn m10_audit_line_has_ids_and_no_answer_text() {
        let (k, acme) = setup();
        let db = tmp_db("audit");
        let m = StubModel::new(vec![q("MATCH Note RETURN *"), answer(STMT, &acme)]);
        let args = json!({"question": "zebra policy?", "subject": "alice", "tenant": "acme"});
        let r = tool_chat_with(&k, &m, "stub", &args, &db).unwrap();
        assert_eq!(r["refused"], false, "{r}");
        let log = std::fs::read_to_string(crate::audit::audit_log_path(&db)).unwrap();
        let _ = std::fs::remove_file(crate::audit::audit_log_path(&db));
        let line = log.lines().last().unwrap();
        assert!(line.contains("\"tool\":\"chat\""), "{line}");
        assert!(line.contains("\"outcome\":\"answered\""), "{line}");
        assert!(line.contains(&acme), "{line}");
        assert!(!line.contains("Zebras are allowed"), "{line}");
    }

    #[test]
    fn m10_audit_survives_a_multibyte_tenant() {
        let (k, _) = setup();
        let db = tmp_db("audit-mb");
        let m = StubModel::new(vec![Err("model unreachable".into())]);
        let t = "é".repeat(150);
        let args = json!({"question": "q", "subject": "alice", "tenant": t});
        let r = tool_chat_with(&k, &m, "stub", &args, &db).unwrap();
        let _ = std::fs::remove_file(crate::audit::audit_log_path(&db));
        assert_eq!(r["refused"], true);
    }

    #[test]
    fn tool_chat_scopes_to_the_tenant_argument() {
        let (k, acme) = setup();
        let db = tmp_db("scope");
        let m = StubModel::new(vec![q_refuse(), answer(STMT, &acme)]);
        let args = json!({"question": "zebra", "subject": "alice", "tenant": "acme"});
        let r = tool_chat_with(&k, &m, "stub", &args, &db).unwrap();
        let _ = std::fs::remove_file(crate::audit::audit_log_path(&db));
        assert_eq!(koids(&r), std::slice::from_ref(&acme), "{r}");
    }

    #[test]
    fn question_length_is_validated_before_anything_else() {
        let (k, _) = setup();
        let m = StubModel::new(vec![]);
        for bad in [json!(""), json!("   "), json!("x".repeat(2001)), json!(5)] {
            let e = tool_chat_with(&k, &m, "stub", &json!({"question": bad}), "x").unwrap_err();
            assert!(e.starts_with("question must be"), "{e}");
        }
        assert!(m.prompts.lock().unwrap().is_empty());
    }
}
