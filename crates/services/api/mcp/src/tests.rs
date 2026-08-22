use crate::*;

#[test]
fn model_store_dir_flag_wins() {
    let p = model_store_dir(Some("C:/tmp/models"));
    assert_eq!(p, std::path::PathBuf::from("C:/tmp/models"));
}

#[test]
fn model_store_dir_default_ends_in_aikoql_models() {
    let p = model_store_dir(None);
    let mut comps = p.components().rev();
    assert_eq!(
        comps.next().map(|c| c.as_os_str()),
        Some(std::ffi::OsStr::new("models"))
    );
    assert_eq!(
        comps.next().map(|c| c.as_os_str()),
        Some(std::ffi::OsStr::new(".aikoql"))
    );
}

#[test]
fn semantic_status_roundtrip() {
    set_semantic_status("unavailable", "no model installed");
    let s = semantic_status_snapshot();
    assert_eq!(s.state, "unavailable");
    assert_eq!(s.detail, "no model installed");
    set_semantic_status("ready", "live");
    assert_eq!(semantic_status_snapshot().state, "ready");
}

// R1 (review round 3): plaintext TCP is loopback-only — a non-loopback bind
// is rejected fail-closed (the bearer token must not travel unencrypted).

#[test]
fn listen_remote_without_tls_rejected() {
    for bad in ["0.0.0.0:9090", "192.168.1.5:9090"] {
        let err = validate_listen(bad).unwrap_err();
        assert!(
            err.contains("non-loopback"),
            "remote {bad} must be rejected, got: {err}"
        );
    }
}

#[test]
fn listen_loopback_allowed() {
    assert_eq!(validate_listen("127.0.0.1:9090").unwrap(), "127.0.0.1:9090");
    assert_eq!(validate_listen("[::1]:9090").unwrap(), "[::1]:9090");
}

#[test]
fn listen_empty_host_maps_to_loopback() {
    assert_eq!(validate_listen(":9090").unwrap(), "127.0.0.1:9090");
}

#[test]
fn listen_invalid_address_rejected() {
    assert!(validate_listen("not an address").is_err());
}
use crate::http::truncate;

#[test]
fn truncate_never_splits_multibyte_chars() {
    // 25 x 'a' + '—' (bytes 25..28) + 'zzzz' = 32 bytes. max 30 → end 27
    // lands inside the em dash and must back off to a char boundary.
    let s = "aaaaaaaaaaaaaaaaaaaaaaaaa—zzzz";
    let t = truncate(s, 30);
    assert!(t.ends_with("..."));
    assert_eq!(&t[t.len() - 4..], "a...");
}

#[test]
fn truncate_passthrough_short_strings() {
    assert_eq!(truncate("hi", 10), "hi");
}

#[test]
fn enrich_file_contains_adds_file_entities_and_relations() {
    use aikoql_ingestion::{EntityCandidate, Evidence, KnowledgeIr};
    let mut ir = KnowledgeIr {
        entities: vec![
            EntityCandidate {
                name: "graph_api".into(),
                type_hint: Some("Function".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: Evidence {
                    document_id: Some("src/main.rs".into()),
                    ..Default::default()
                },
            },
            EntityCandidate {
                name: "retry_loop".into(),
                type_hint: Some("Function".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: Evidence {
                    document_id: Some("src/main.rs".into()),
                    ..Default::default()
                },
            },
            // doc == name fallback path entity: no duplicate File entity,
            // no self-contains relation.
            EntityCandidate {
                name: "src/lib.rs".into(),
                type_hint: Some("file".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: Evidence {
                    document_id: Some("src/lib.rs".into()),
                    ..Default::default()
                },
            },
        ],
        ..Default::default()
    };
    crate::ingest::enrich_file_contains(&mut ir);
    let files: Vec<&str> = ir
        .entities
        .iter()
        .filter(|e| e.type_hint.as_deref() == Some("file"))
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(files, vec!["src/lib.rs", "src/main.rs"]);
    let contains: Vec<(&str, &str, &str)> = ir
        .relations
        .iter()
        .map(|r| (r.subject.as_str(), r.predicate.as_str(), r.object.as_str()))
        .collect();
    assert_eq!(contains.len(), 2);
    assert!(contains.contains(&("src/main.rs", "contains", "graph_api")));
    assert!(contains.contains(&("src/main.rs", "contains", "retry_loop")));
}

#[test]
fn semantic_scores_parses_caches_and_scores() {
    // Regression check for the EMB_CACHE self-deadlock: the cache-insert
    // branch used to re-lock the mutex it already held via a match
    // scrutinee temporary, wedging the first request (and every request
    // after it) forever. This test walks both branches: parse+insert,
    // then cache-hit.
    let db = std::env::temp_dir().join(format!("mnemo-sem-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let engine = crate::RedbEngine::open(db.to_str().unwrap()).expect("open store");
    let k = crate::Kernel::open(
        std::sync::Arc::new(engine),
        std::sync::Arc::new(crate::SystemClock),
        0,
    )
    .expect("open kernel");

    let mut props = crate::PropertyMap::new();
    props.insert(
        "entity_embeddings".into(),
        crate::Value::Text(r#"{"a::b":[1.0,0.0]}"#.into()),
    );
    let r = k
        .remember(crate::RememberRequest {
            context: crate::KnowledgeContext::from(&crate::Subject::with_roles("test", &["admin"])),
            koid: None,
            expected_version: Some(0),
            idempotency_key: Some("sem-scores-test".into()),
            metadata: crate::Metadata {
                type_name: "aikoql:ingested-directory".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: crate::ExtensionMap::new(),
            origin: crate::Origin::Human,
            note: None,
            referential_policy: crate::ReferentialPolicy::Permissive,
        })
        .expect("remember");
    let args = serde_json::json!({"koid": r.koid.to_hex(), "subject": "test", "roles": ["admin"]});

    let scores = crate::tools::semantic_scores(&k, &args, &[1.0, 0.0]).expect("scores");
    assert!((scores["a::b"] - 1.0).abs() < 1e-6);
    let cached = crate::tools::semantic_scores(&k, &args, &[1.0, 0.0]).expect("cached hit");
    assert_eq!(cached.len(), 1);

    drop(k);
    let _ = std::fs::remove_file(&db);
}
