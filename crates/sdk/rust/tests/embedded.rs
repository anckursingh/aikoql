//! D-12 embedded smoke (§25): open the kernel in-process (no server, no
//! MCP), run the canonical CRUD + vector + lineage ops with an admin
//! subject (ACL parity with the server's TOKEN::admin).

use aikoql_sdk::tools::{FindSimilarParams, RememberParams};
use aikoql_sdk::Embedded;
use serde_json::json;

fn props(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn embedded_crud_vector_lineage_roundtrip() {
    let dir = std::env::temp_dir().join(format!("aikoql-sdk-embedded-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("db.redb");
    let emb = Embedded::open(db.to_str().unwrap()).unwrap();

    // CRUD: create, get, update, tombstone.
    let created = emb
        .remember(RememberParams {
            type_name: "person".into(),
            properties: Some(props(json!({"name": "ada", "age": 36}))),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(created.version, 1);

    let ko = emb.get(&created.koid).await.unwrap();
    assert_eq!(ko.koid, created.koid);
    // The kernel creates in draft (the wire returns "draft" too — only
    // forget is state-asserted by the vectors).
    assert_eq!(ko.state.as_deref(), Some("draft"));
    assert_eq!(ko.properties["name"], "ada");

    let updated = emb
        .remember(RememberParams {
            type_name: "person".into(),
            koid: Some(created.koid.clone()),
            properties: Some(props(json!({"name": "ada", "age": 37}))),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(updated.version, 2);
    assert_eq!(emb.get(&created.koid).await.unwrap().properties["age"], 37);

    // Vector: text recall finds the remembered KO.
    let hits = emb
        .find_similar(FindSimilarParams {
            text: Some("ada".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(hits.iter().any(|h| h.koid == created.koid));

    // Lineage: explain + trace agree on the koid.
    let expl = emb.explain(&created.koid, None).await.unwrap();
    assert_eq!(expl["koid"], created.koid);
    let lineage = emb.trace(&created.koid).await.unwrap();
    assert_eq!(lineage["koid"], created.koid);
    assert!(lineage["versions"].as_u64().unwrap() >= 1);

    // Tombstone: forget, then get returns the tombstoned head (the wire
    // shape — crud/vector.json asserts state "deleted" the same way).
    emb.forget(&created.koid, "tombstone").await.unwrap();
    let gone = emb.get(&created.koid).await.unwrap();
    assert_eq!(gone.state.as_deref(), Some("deleted"));

    let _ = std::fs::remove_dir_all(&dir);
}
