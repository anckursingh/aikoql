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

/// A label as a safe Cypher identifier body: every backtick doubled, so the
/// result can sit inside ``(n:`…`)`` without ending the identifier.
pub fn quote_label(label: &str) -> String {
    label.to_string()
}

/// One property of a label: observed value types and whether every node has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelProp {
    pub name: String,
    pub types: Vec<String>,
    pub mandatory: bool,
}

/// Property schema from a bounded sample of `properties(n)` maps (the fallback
/// when `db.schema.nodeTypeProperties()` is unavailable).
pub fn label_props_from_sample(_rows: &[serde_json::Value]) -> Vec<LabelProp> {
    Vec::new()
}

pub struct Neo4jConnector {
    base_url: String,
    auth: String, // "Basic <base64>"
    agent: ureq::Agent,
}

impl Neo4jConnector {
    /// Connect to Neo4j HTTP API.
    /// `uri`: e.g. "http://localhost:7474"
    /// `user`: e.g. "neo4j"
    /// `password`: e.g. "password"
    pub fn connect(uri: &str, user: &str, password: &str) -> Result<Self, String> {
        Self::connect_with_timeout(uri, user, password, None)
    }

    /// Connect with an optional timeout (CON-005): ureq has NO default
    /// timeout — a hung neo4j hangs forever — so `--timeout-ms` must bound
    /// every request.
    pub fn connect_with_timeout(
        uri: &str,
        user: &str,
        password: &str,
        timeout_ms: Option<u64>,
    ) -> Result<Self, String> {
        let base_url = uri.trim_end_matches('/').to_string();
        let agent = match timeout_ms {
            Some(ms) => ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_millis(ms))
                .build(),
            None => ureq::Agent::new(),
        };
        // Verify connection.
        let resp = agent
            .get(&base_url)
            .set("Authorization", &basic_auth(user, password))
            .call()
            .map_err(|e| format!("Neo4j connection failed: {}", e))?;
        if resp.status() >= 400 {
            return Err(format!("Neo4j returned HTTP {}", resp.status()));
        }
        Ok(Neo4jConnector {
            base_url,
            auth: basic_auth(user, password),
            agent,
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
        let resp = self
            .agent
            .post(&format!("{}/db/neo4j/tx/commit", self.base_url))
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

    /// Property names of a label (ONT-001 schema discovery).
    /// ponytail: samples ONE node — heterogeneous labels under-report props;
    /// switch to a per-label properties UNION scan if discovery completeness
    /// ever matters.
    pub fn introspect_label_props(&self, label: &str) -> Result<Vec<String>, String> {
        let rows = self.cypher(&format!(
            "MATCH (n:`{}`) RETURN properties(n) LIMIT 1",
            label
        ))?;
        let mut props: Vec<String> = rows
            .first()
            .and_then(|v| v.as_object())
            .map(|obj| obj.keys().cloned().collect())
            .unwrap_or_default();
        props.sort();
        Ok(props)
    }

    /// Property schema of a label: union over the whole label via
    /// `db.schema.nodeTypeProperties()`, else over a bounded sample.
    pub fn introspect_label_schema(
        &self,
        _label: &str,
        _sample_limit: u32,
    ) -> Result<Vec<LabelProp>, String> {
        Ok(Vec::new())
    }

    /// Distinct (relationship type, from label, to label) patterns, at most `limit`.
    pub fn introspect_rel_patterns(
        &self,
        _limit: u32,
    ) -> Result<Vec<(String, String, String)>, String> {
        Ok(Vec::new())
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

    #[test]
    fn sample_props_union_types_and_mandatory() {
        let rows = vec![
            serde_json::json!({"name": "a", "age": 30}),
            serde_json::json!({"name": "b", "age": "old", "nick": null}),
            serde_json::json!({"name": "c"}),
        ];
        let props = label_props_from_sample(&rows);
        let get = |n: &str| {
            props
                .iter()
                .find(|p| p.name == n)
                .unwrap_or_else(|| panic!("no {n}: {props:?}"))
        };
        assert!(get("name").mandatory && get("name").types == ["String"]);
        assert!(!get("age").mandatory);
        assert_eq!(get("age").types, ["Long", "String"]);
        assert!(!get("nick").mandatory);
        assert!(label_props_from_sample(&[]).is_empty());
    }

    #[test]
    fn quote_label_doubles_backticks_so_it_cannot_leave_the_identifier() {
        assert_eq!(quote_label("Person"), "Person");
        assert_eq!(quote_label("a`b"), "a``b");
        let hostile = "x`) DETACH DELETE (m //";
        let q = format!("MATCH (n:`{}`) RETURN n", quote_label(hostile));
        // after removing escaped pairs, the only backticks left are the two delimiters
        assert_eq!(q.replace("``", "").matches('`').count(), 2, "{q}");
    }

    /// Seeded xorshift: reproducible, no unseeded RNG (env-hygiene gate).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
            &xs[(self.next() % xs.len() as u64) as usize]
        }
    }

    fn fuzz_iters() -> u64 {
        std::env::var("AIKOQL_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2_000)
    }

    fn random_json(r: &mut Rng, depth: u32) -> serde_json::Value {
        match r.next() % if depth > 4 { 5 } else { 7 } {
            0 => serde_json::Value::Null,
            1 => serde_json::json!(r.next() % 2 == 0),
            2 => serde_json::json!(r.next() as i64),
            3 => serde_json::json!((r.next() % 1000) as f64 / 7.0),
            4 => serde_json::json!(*r.pick(&["", "a", "\u{0}", "x`y", "ünï", "💥"])),
            5 => serde_json::Value::Array(
                (0..r.next() % 4)
                    .map(|_| random_json(r, depth + 1))
                    .collect(),
            ),
            _ => serde_json::Value::Object(
                (0..r.next() % 4)
                    .map(|_| {
                        (
                            r.pick(&["a", "b", "c", "k`", "", "ü"]).to_string(),
                            random_json(r, depth + 1),
                        )
                    })
                    .collect(),
            ),
        }
    }

    #[test]
    fn label_props_fuzz_keeps_its_invariants() {
        for seed in 1..=fuzz_iters() {
            let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let rows: Vec<serde_json::Value> =
                (0..r.next() % 6).map(|_| random_json(&mut r, 0)).collect();
            let props = label_props_from_sample(&rows); // must not panic
            let objects: Vec<&serde_json::Map<String, serde_json::Value>> =
                rows.iter().filter_map(|v| v.as_object()).collect();
            for p in &props {
                assert!(!p.types.iter().any(|t| t == "Null"), "seed {seed}: {p:?}");
                let mut sorted = p.types.clone();
                sorted.sort();
                sorted.dedup();
                assert_eq!(sorted, p.types, "seed {seed}: types sorted and unique");
                if p.mandatory {
                    assert!(
                        !objects.is_empty()
                            && objects
                                .iter()
                                .all(|o| o.get(&p.name).is_some_and(|v| !v.is_null())),
                        "seed {seed}: mandatory {p:?} over {rows:?}"
                    );
                }
            }
        }
    }
}
