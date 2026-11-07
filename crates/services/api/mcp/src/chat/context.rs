//! Retrieved facts: rendering, secret scrubbing, bounded context, and the
//! tenant-scoped retrieval fallback.

use crate::{json, Kernel};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fact {
    pub koid: String,
    pub statement: String,
}

pub(crate) const MAX_FACTS: usize = 8;
pub(crate) const MAX_CONTEXT_CHARS: usize = 2000;
/// One property value never takes more than this, so a long `mentions` dump
/// cannot swallow the whole budget.
const MAX_VALUE_CHARS: usize = 200;

/// Ingestion bookkeeping that tells a model nothing about the subject.
fn is_internal(key: &str) -> bool {
    matches!(key, "confidence" | "type_hint") || key.starts_with("evidence_")
}

/// Render result rows as facts. Rows without a koid or any content are
/// skipped; every statement is secret-scrubbed; the total is bounded.
pub(crate) fn facts_from_rows(rows: &serde_json::Value) -> Vec<Fact> {
    let mut out = Vec::new();
    let mut budget = MAX_CONTEXT_CHARS;
    for row in rows.as_array().into_iter().flatten() {
        if out.len() == MAX_FACTS || budget == 0 {
            break;
        }
        let Some(koid) = row.get("koid").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(statement) = render(row) else {
            continue;
        };
        let statement: String = aikoql_ingestion::redact_text(&statement)
            .chars()
            .take(budget)
            .collect();
        budget -= statement.chars().count();
        out.push(Fact {
            koid: koid.to_string(),
            statement,
        });
    }
    out
}

fn render(row: &serde_json::Value) -> Option<String> {
    let ty = row.get("type_name").and_then(|v| v.as_str()).unwrap_or("");
    let props = row
        .get("properties")?
        .as_object()
        .filter(|p| !p.is_empty())?;
    let text = |v: &serde_json::Value| {
        let t = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if t.chars().count() > MAX_VALUE_CHARS {
            format!("{}…", t.chars().take(MAX_VALUE_CHARS).collect::<String>())
        } else {
            t
        }
    };
    let label = ["name", "title", "label"]
        .iter()
        .find_map(|k| props.get(*k).and_then(|v| v.as_str()));
    if label.is_none() {
        if let Some(t) = props.get("text").and_then(|v| v.as_str()) {
            return Some(format!("{ty}: {t}"));
        }
    }
    let mut body: Vec<String> = props
        .iter()
        .filter(|(k, _)| !is_internal(k))
        .map(|(k, v)| format!("{k}={}", text(v)))
        .collect();
    body.sort();
    Some(match label {
        Some(l) => format!("{ty} {l}: {}", body.join("; ")),
        None => format!("{ty}: {}", body.join("; ")),
    })
}

/// Text/vector search over what the caller may see, confined to `tenant`
/// (None = unscoped, as `execute_readonly`). The kernel search is not
/// tenant-aware, so hits are over-fetched and filtered here.
pub(crate) fn retrieve_fallback(
    k: &Kernel,
    subject: &str,
    roles: &[String],
    tenant: Option<&str>,
    question: &str,
) -> Vec<Fact> {
    let ctx =
        crate::session::subject_of(&json!({"subject": subject, "roles": roles, "tenant": tenant}));
    let hits = k.find_similar(crate::SimilarityQuery {
        context: ctx.into(),
        filter: None,
        text: Some(question.to_string()),
        vector: None,
        embedding_model: None,
        k: MAX_FACTS * 4,
        fusion: crate::helpers::parse_fusion(&json!({})),
    });
    let Ok(hits) = hits else {
        return Vec::new();
    };
    let rows: Vec<serde_json::Value> = hits
        .iter()
        .filter(|h| h.score > 0.0)
        .map(|h| &h.ko)
        .filter(|ko| !ko.metadata.type_name.starts_with("aikoql:"))
        .filter(|ko| tenant.is_none_or(|t| ko.metadata.tenant.as_deref() == Some(t)))
        .map(|ko| {
            json!({
                "koid": ko.koid.to_hex(),
                "type_name": ko.metadata.type_name,
                "properties": ko.properties.iter()
                    .map(|(k, v)| (k.clone(), crate::helpers::value_to_json(v)))
                    .collect::<serde_json::Map<_, _>>(),
            })
        })
        .collect();
    facts_from_rows(&json!(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        json, ExtensionMap, Metadata, Origin, PropertyMap, ReferentialPolicy, RememberRequest,
        Value,
    };
    use aikoql_kernel::{ManualClock, MemoryEngine, TokenTextIndex};
    use std::sync::Arc;
    use std::time::Duration;

    fn row(koid: &str, ty: &str, props: serde_json::Value) -> serde_json::Value {
        json!({"koid": koid, "type_name": ty, "properties": props})
    }

    #[test]
    fn facts_render_type_label_and_properties() {
        let f = facts_from_rows(&json!([row(
            "k1",
            "Person",
            json!({"name": "Ada", "city": "London"})
        )]));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].koid, "k1");
        assert_eq!(f[0].statement, "Person Ada: city=London; name=Ada");
    }

    #[test]
    fn facts_render_chunk_text() {
        let f = facts_from_rows(&json!([row(
            "k1",
            "DocChunk",
            json!({"text": "Refunds take 5 days."})
        )]));
        assert_eq!(f[0].statement, "DocChunk: Refunds take 5 days.");
    }

    #[test]
    fn facts_drop_internal_metadata_and_cap_long_values() {
        let long = "word ".repeat(200);
        let f = facts_from_rows(&json!([row(
            "k1",
            "Thing",
            json!({"name": "Ada", "confidence": 0.85, "type_hint": "Person",
                   "evidence_model": "rules-v1", "evidence_document": "/x/y",
                   "mentions": long})
        )]));
        let st = &f[0].statement;
        for noise in ["confidence", "type_hint", "evidence_"] {
            assert!(!st.contains(noise), "{st}");
        }
        assert!(st.starts_with("Thing Ada: "), "{st}");
        assert!(st.contains("mentions=word word"), "{st}");
        assert!(st.chars().count() < 260, "{} chars", st.chars().count());
        assert!(st.contains('…'), "a cut value is marked: {st}");
    }

    #[test]
    fn facts_are_scrubbed() {
        let f = facts_from_rows(&json!([row(
            "k1",
            "Note",
            json!({"body": "AKIAIOSFODNN7EXAMPLE", "region": "Asia Pacific"})
        )]));
        assert!(!f[0].statement.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(f[0].statement.contains("Asia Pacific"));
    }

    #[test]
    fn facts_are_capped_to_8_and_2000_chars() {
        let big = "é".repeat(500);
        let rows: Vec<_> = (0..20)
            .map(|i| row(&format!("k{i}"), "Note", json!({"text": big})))
            .collect();
        let f = facts_from_rows(&json!(rows));
        assert!(f.len() <= MAX_FACTS && !f.is_empty());
        let total: usize = f.iter().map(|x| x.statement.chars().count()).sum();
        assert!(total <= MAX_CONTEXT_CHARS, "total {total}");
    }

    #[test]
    fn facts_skip_rows_without_koid() {
        let f = facts_from_rows(&json!([
            {"properties": {"a": "b"}},
            row("k2", "Note", json!({"a": "b"}))
        ]));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].koid, "k2");
    }

    fn mk() -> (Arc<Kernel>, String, String) {
        let k = Arc::new(
            Kernel::open(
                Arc::new(MemoryEngine::new()),
                Arc::new(ManualClock::new(10_000)),
                0xBEEF,
            )
            .unwrap(),
        );
        let m = aikoql_scheduler::IndexMaintainer::start(
            &k,
            Arc::new(aikoql_kernel::BruteForceVectorIndex::new()),
            Arc::new(TokenTextIndex::new()),
        )
        .unwrap();
        k.attach_indexes(m.clone());
        let mut ids = Vec::new();
        for tenant in ["acme", "globex"] {
            let mut props = PropertyMap::new();
            props.insert("text".into(), Value::Text("zebra policy applies".into()));
            let r = k
                .remember(RememberRequest {
                    context: crate::session::subject_of(&json!({"subject": "alice"})).into(),
                    koid: None,
                    expected_version: Some(0),
                    idempotency_key: None,
                    metadata: Metadata {
                        type_name: "Note".into(),
                        tenant: Some(tenant.into()),
                        schema_version: 1,
                        tags: vec![],
                    },
                    properties: props,
                    semantic: None,
                    relationships: vec![],
                    security: None,
                    extensions: ExtensionMap::new(),
                    origin: Origin::Human,
                    note: None,
                    referential_policy: ReferentialPolicy::default(),
                })
                .unwrap();
            ids.push(r.koid.to_hex());
        }
        m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
        (k, ids[0].clone(), ids[1].clone())
    }

    #[test]
    fn fallback_finds_a_tenant_fact_and_never_another_tenants() {
        let (k, acme, globex) = mk();
        let a = retrieve_fallback(&k, "alice", &[], Some("acme"), "zebra policy");
        assert_eq!(
            a.iter().map(|f| f.koid.clone()).collect::<Vec<_>>(),
            std::slice::from_ref(&acme)
        );
        let g = retrieve_fallback(&k, "alice", &[], Some("globex"), "zebra policy");
        assert_eq!(
            g.iter().map(|f| f.koid.clone()).collect::<Vec<_>>(),
            std::slice::from_ref(&globex)
        );
        let all = retrieve_fallback(&k, "alice", &["admin".to_string()], None, "zebra policy");
        let ids: Vec<_> = all.iter().map(|f| f.koid.clone()).collect();
        assert!(ids.contains(&acme) && ids.contains(&globex), "{ids:?}");
    }
}
