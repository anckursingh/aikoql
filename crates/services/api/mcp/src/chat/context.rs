//! Retrieved facts: rendering, secret scrubbing, bounded context, and the
//! tenant-scoped retrieval fallback.

use crate::Kernel;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fact {
    pub koid: String,
    pub statement: String,
}

pub(crate) const MAX_FACTS: usize = 8;
pub(crate) const MAX_CONTEXT_CHARS: usize = 2000;

pub(crate) fn facts_from_rows(_rows: &serde_json::Value) -> Vec<Fact> {
    Vec::new()
}

pub(crate) fn retrieve_fallback(
    _k: &Kernel,
    _subject: &str,
    _roles: &[String],
    _tenant: Option<&str>,
    _question: &str,
) -> Vec<Fact> {
    Vec::new()
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
            [acme.clone()]
        );
        let g = retrieve_fallback(&k, "alice", &[], Some("globex"), "zebra policy");
        assert_eq!(
            g.iter().map(|f| f.koid.clone()).collect::<Vec<_>>(),
            [globex.clone()]
        );
        let all = retrieve_fallback(&k, "alice", &[], None, "zebra policy");
        let ids: Vec<_> = all.iter().map(|f| f.koid.clone()).collect();
        assert!(ids.contains(&acme) && ids.contains(&globex), "{ids:?}");
    }
}
