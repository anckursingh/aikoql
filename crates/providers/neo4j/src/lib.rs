//! Neo4j Import Connector — cross-db-support milestone 4.
#![allow(dead_code)]
//!
//! Connects to Neo4j via HTTP JSON API, imports nodes as KnowledgeObjects
//! and relationships as RelationshipRefs. Uses `ureq` for sync HTTP.
//!
//! Design:
//! - Node labels → type_name (multi-label nodes get primary label)
//! - Node properties → PropertyMap
//! - Node elementId → deterministic KOID
//! - Relationships → RelationshipRef with rel_type, source KOID, target KOID
//!
//! ponytail: HTTP API over Bolt driver — avoids native driver + tokio deps.

use aikoql_kernel::knowledge::kom::*;
use serde::Deserialize;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Neo4j HTTP JSON types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Neo4jResponse {
    results: Vec<Neo4jResult>,
    errors: Vec<Neo4jError>,
}

#[derive(Debug, Deserialize)]
struct Neo4jResult {
    columns: Vec<String>,
    data: Vec<Neo4jData>,
}

#[derive(Debug, Deserialize)]
struct Neo4jData {
    row: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Neo4jError {
    message: String,
    code: String,
}

// ---------------------------------------------------------------------------
// Connector
// ---------------------------------------------------------------------------

pub struct Neo4jConnector {
    base_url: String,
    auth: String, // "Basic <base64>"
}

impl Neo4jConnector {
    /// Connect to Neo4j HTTP API.
    /// `uri`: e.g. "http://localhost:7474"
    /// `user`: e.g. "neo4j"
    /// `password`: e.g. "password"
    pub fn connect(uri: &str, user: &str, password: &str) -> Result<Self, String> {
        let base_url = uri.trim_end_matches('/').to_string();
        // Verify connection.
        let resp = ureq::get(&base_url)
            .set("Authorization", &basic_auth(user, password))
            .call()
            .map_err(|e| format!("Neo4j connection failed: {}", e))?;
        if resp.status() >= 400 {
            return Err(format!("Neo4j returned HTTP {}", resp.status()));
        }
        Ok(Neo4jConnector {
            base_url,
            auth: basic_auth(user, password),
        })
    }

    /// Execute a Cypher query and return results.
    ///
    /// Neo4j 5 removed the legacy /db/data/transaction/commit alias — the
    /// db-scoped /db/neo4j/tx/commit endpoint is the only one that answers
    /// (verified live: legacy 404, db-scoped 200).
    fn cypher(&self, statement: &str) -> Result<Vec<serde_json::Value>, String> {
        let body = serde_json::json!({
            "statements": [{"statement": statement}]
        });
        let resp = ureq::post(&format!("{}/db/neo4j/tx/commit", self.base_url))
            .set("Authorization", &self.auth)
            .set("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|e| format!("cypher request: {}", e))?;
        let neo: Neo4jResponse = resp
            .into_json()
            .map_err(|e| format!("parse response: {}", e))?;
        if !neo.errors.is_empty() {
            return Err(format!("{}: {}", neo.errors[0].code, neo.errors[0].message));
        }
        let mut rows = Vec::new();
        for result in neo.results {
            for data in result.data {
                for val in data.row {
                    rows.push(val);
                }
            }
        }
        Ok(rows)
    }

    // ------------------------------------------------------------------
    // Schema discovery
    // ------------------------------------------------------------------

    /// List all node labels in the database.
    pub fn list_labels(&self) -> Result<Vec<String>, String> {
        let rows = self.cypher("CALL db.labels()")?;
        Ok(rows
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    /// List all relationship types.
    pub fn list_rel_types(&self) -> Result<Vec<String>, String> {
        let rows = self.cypher("CALL db.relationshipTypes()")?;
        Ok(rows
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    /// Get count of nodes for a label.
    pub fn count_nodes(&self, label: &str) -> Result<u64, String> {
        let rows = self.cypher(&format!("MATCH (n:`{}`) RETURN count(n)", label))?;
        rows.first()
            .and_then(|v| v.as_u64())
            .ok_or_else(|| "unexpected count result".to_string())
    }

    // ------------------------------------------------------------------
    // Import
    // ------------------------------------------------------------------

    /// Import all nodes with a given label.
    /// Each node becomes one KnowledgeObject.
    pub fn import_nodes(
        &self,
        label: &str,
        tenant: Option<&str>,
    ) -> Result<(Vec<KnowledgeObject>, HashMap<String, KOID>), String> {
        let stmt = format!(
            "MATCH (n:`{}`) RETURN elementId(n), properties(n), labels(n)",
            label
        );
        let body = serde_json::json!({ "statements": [{"statement": stmt}] });
        let resp = ureq::post(&format!("{}/db/neo4j/tx/commit", self.base_url))
            .set("Authorization", &self.auth)
            .set("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|e| format!("import nodes: {}", e))?;
        let neo: Neo4jResponse = resp.into_json().map_err(|e| format!("parse: {}", e))?;
        if !neo.errors.is_empty() {
            return Err(format!("{}: {}", neo.errors[0].code, neo.errors[0].message));
        }

        let mut objects = Vec::new();
        let mut id_map: HashMap<String, KOID> = HashMap::new(); // elementId → KOID

        for result in &neo.results {
            for data in &result.data {
                if data.row.len() < 3 {
                    continue;
                }
                let elem_id = data.row[0].as_str().unwrap_or("").to_string();
                let props_val = &data.row[1];
                let labels_val = &data.row[2];

                let mut props = PropertyMap::new();
                if let Some(obj) = props_val.as_object() {
                    for (k, v) in obj {
                        props.insert(k.clone(), neo4j_json_to_value(v));
                    }
                }
                // Full label list, sorted. The runner iterates labels sorted,
                // so the first sorted label is the KO's type_name — the list
                // here keeps the other labels visible on the KO.
                let mut labels: Vec<String> = labels_val
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                labels.sort();
                props.insert(
                    "labels".into(),
                    Value::List(labels.into_iter().map(Value::Text).collect()),
                );

                // Label-independent KOID: the same node matched under two
                // label passes must yield the SAME KOID, or the runner would
                // import it once per label (type explosion).
                let koid = deterministic_koid("neo4j", &elem_id);
                id_map.insert(elem_id, koid);

                objects.push(KnowledgeObject {
                    koid,
                    version: 0,
                    commit_ts: 0,
                    metadata: Metadata {
                        type_name: label.to_string(),
                        tenant: tenant.map(String::from),
                        schema_version: 1,
                        tags: vec!["imported".into(), "source:neo4j".into()],
                    },
                    properties: props,
                    semantic: None,
                    relationships: vec![],
                    event_refs: vec![],
                    security: SecurityDescriptor {
                        owner: "neo4j-importer".into(),
                        acl: vec![],
                        classification: None,
                    },
                    lifecycle: Lifecycle {
                        state: LifecycleState::Draft,
                        origin: Origin::Human,
                    },
                    extensions: ExtensionMap::new(),
                });
            }
        }
        Ok((objects, id_map))
    }

    /// Import all relationships of a given type.
    /// Requires `id_map` from node import to resolve elementIds to KOIDs.
    /// Returns (ref, source KOID, target KOID, relationship properties) —
    /// the ref is pinned on the STORED start node.
    pub fn import_relationships(
        &self,
        rel_type: &str,
        id_map: &HashMap<String, KOID>,
    ) -> Result<Vec<(RelationshipRef, KOID, KOID, PropertyMap)>, String> {
        // startNode(r)/endNode(r) pin the stored direction — a Cypher arrow
        // in MATCH does NOT, the endpoints come back in elementId order and
        // inverted rels would land on the wrong KO. The undirected match
        // returns each relationship in BOTH orientations, so the WHERE keeps
        // exactly one row per rel; ORDER BY makes the order deterministic so
        // re-imports are byte-identical.
        let stmt = format!(
            "MATCH (a)-[r:`{}`]-(b) WHERE elementId(a) < elementId(b) RETURN elementId(startNode(r)), elementId(endNode(r)), type(r), properties(r) ORDER BY elementId(startNode(r)), elementId(endNode(r))",
            rel_type
        );
        let body = serde_json::json!({ "statements": [{"statement": stmt}] });
        let resp = ureq::post(&format!("{}/db/neo4j/tx/commit", self.base_url))
            .set("Authorization", &self.auth)
            .set("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|e| format!("import rels: {}", e))?;
        let neo: Neo4jResponse = resp.into_json().map_err(|e| format!("parse: {}", e))?;
        if !neo.errors.is_empty() {
            return Err(format!("{}: {}", neo.errors[0].code, neo.errors[0].message));
        }

        let mut rels = Vec::new();
        for result in &neo.results {
            for data in &result.data {
                if data.row.len() < 4 {
                    continue;
                }
                let src_id = data.row[0].as_str().unwrap_or("");
                let tgt_id = data.row[1].as_str().unwrap_or("");
                let rtype = data.row[2].as_str().unwrap_or(rel_type);

                let src_koid = match id_map.get(src_id) {
                    Some(k) => *k,
                    None => continue,
                };
                let tgt_koid = match id_map.get(tgt_id) {
                    Some(k) => *k,
                    None => continue,
                };

                let mut rprops = PropertyMap::new();
                if let Some(obj) = data.row[3].as_object() {
                    for (k, v) in obj {
                        rprops.insert(k.clone(), neo4j_json_to_value(v));
                    }
                }

                rels.push((
                    RelationshipRef {
                        rel_type: rtype.to_string(),
                        target: tgt_koid,
                        direction: Direction::Outbound,
                    },
                    src_koid,
                    tgt_koid,
                    rprops,
                ));
            }
        }
        Ok(rels)
    }
}

// ---------------------------------------------------------------------------
// Neo4j JSON → Aikoql Value
// ---------------------------------------------------------------------------

fn neo4j_json_to_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => Value::Text(s.clone()),
        serde_json::Value::Array(arr) => Value::List(arr.iter().map(neo4j_json_to_value).collect()),
        serde_json::Value::Object(obj) => {
            let mut m = std::collections::BTreeMap::new();
            for (k, v) in obj {
                m.insert(k.clone(), neo4j_json_to_value(v));
            }
            Value::Map(m)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn basic_auth(user: &str, pass: &str) -> String {
    let creds = format!("{}:{}", user, pass);
    let encoded = base64_encode(&creds);
    format!("Basic {}", encoded)
}

/// pub so the connector-certification harness can build its seed requests
/// with the same auth encoding the provider uses.
pub fn base64_encode(input: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn deterministic_koid(label: &str, elem_id: &str) -> KOID {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in label.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash ^= 0x3a;
    hash = hash.wrapping_mul(0x100000001b3);
    for b in elem_id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let mut k = [0u8; 16];
    k[..8].copy_from_slice(&hash.to_be_bytes());
    KOID(k)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encode_works() {
        assert_eq!(base64_encode("neo4j:password"), "bmVvNGo6cGFzc3dvcmQ=");
        assert_eq!(base64_encode("f"), "Zg==");
        assert_eq!(base64_encode("fo"), "Zm8=");
        assert_eq!(base64_encode("foo"), "Zm9v");
    }

    #[test]
    fn basic_auth_format() {
        let auth = basic_auth("neo4j", "secret");
        assert!(auth.starts_with("Basic "));
    }

    #[test]
    fn json_scalars_to_value() {
        assert_eq!(neo4j_json_to_value(&serde_json::json!(42)), Value::Int(42));
        assert_eq!(
            neo4j_json_to_value(&serde_json::json!(std::f64::consts::PI)),
            Value::Float(std::f64::consts::PI)
        );
        assert_eq!(
            neo4j_json_to_value(&serde_json::json!("hi")),
            Value::Text("hi".into())
        );
        assert_eq!(
            neo4j_json_to_value(&serde_json::json!(true)),
            Value::Bool(true)
        );
        assert_eq!(neo4j_json_to_value(&serde_json::json!(null)), Value::Null);
    }

    #[test]
    fn json_array_to_list() {
        let v = neo4j_json_to_value(&serde_json::json!([1, "two", true]));
        match v {
            Value::List(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Value::Int(1));
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn json_object_to_map() {
        let v = neo4j_json_to_value(&serde_json::json!({"name": "Alice", "age": 30}));
        match v {
            Value::Map(m) => {
                assert_eq!(m.get("name"), Some(&Value::Text("Alice".into())));
                assert_eq!(m.get("age"), Some(&Value::Int(30)));
            }
            other => panic!("expected Map, got {:?}", other),
        }
    }

    #[test]
    fn deterministic_koid_consistent() {
        assert_eq!(
            deterministic_koid("Person", "4:abc:123"),
            deterministic_koid("Person", "4:abc:123")
        );
    }
}
