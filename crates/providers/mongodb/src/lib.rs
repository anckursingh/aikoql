//! MongoDB Import Connector — cross-db-support milestone 3.
//!
//! Connects to MongoDB, lists collections, samples documents to infer
//! properties, and imports BSON documents as Knowledge Objects.
//!
//! Design:
//! - Collection → type_name
//! - _id (ObjectId) → deterministic KOID
//! - BSON fields → PropertyMap (sub-docs → Map, arrays → List)
//! - Schemaless: properties auto-discovered from documents

use aikoql_kernel::knowledge::kom::*;
use mongodb::bson::{doc, Bson, Document};
use mongodb::Client;
use std::collections::BTreeMap;
use tokio::runtime::Runtime;

#[derive(Clone, Debug)]
pub struct CollectionSchema {
    pub name: String,
    pub document_count: u64,
    pub properties: Vec<String>,
}

pub struct MongoConnector {
    rt: Runtime,
    #[allow(dead_code)]
    client: Client, // held for lifetime; db borrows from it
    db: mongodb::Database,
}

impl MongoConnector {
    pub fn connect(uri: &str, database: &str) -> Result<Self, String> {
        Self::connect_with_timeout(uri, database, None)
    }

    /// Connect with an optional timeout (CON-005): the driver's default
    /// server selection waits 30s — `--timeout-ms` must bound it.
    pub fn connect_with_timeout(
        uri: &str,
        database: &str,
        timeout_ms: Option<u64>,
    ) -> Result<Self, String> {
        let rt = Runtime::new().map_err(|e| format!("tokio runtime: {}", e))?;
        // Client construction needs a tokio reactor context (mongodb 3.8) —
        // parse + with_options stay inside block_on.
        let (client, db) = rt.block_on(async {
            let mut opts = mongodb::options::ClientOptions::parse(uri)
                .await
                .map_err(|e| format!("MongoDB connection failed: {}", e))?;
            if let Some(ms) = timeout_ms {
                opts.server_selection_timeout = Some(std::time::Duration::from_millis(ms));
            }
            let client = Client::with_options(opts)
                .map_err(|e| format!("MongoDB connection failed: {}", e))?;
            let db = client.database(database);
            Ok::<_, String>((client, db))
        })?;
        Ok(MongoConnector { rt, client, db })
    }

    pub fn list_collections(&self) -> Result<Vec<String>, String> {
        self.rt
            .block_on(async { self.db.list_collection_names().await })
            .map_err(|e| format!("list collections: {}", e))
    }

    pub fn introspect_collection(&self, name: &str) -> Result<CollectionSchema, String> {
        self.rt
            .block_on(async {
                let coll = self.db.collection::<Document>(name);
                let count = coll
                    .count_documents(doc! {})
                    .await
                    .map_err(|e| format!("count {}: {}", name, e))?;
                let mut props: Vec<String> = Vec::new();
                let opts = mongodb::options::FindOptions::builder().limit(100).build();
                let mut cursor = coll
                    .find(doc! {})
                    .with_options(opts)
                    .await
                    .map_err(|e| format!("sample {}: {}", name, e))?;
                use futures_util::StreamExt;
                while let Some(Ok(d)) = cursor.next().await {
                    collect_keys("", &d, &mut props);
                }
                props.sort();
                props.dedup();
                Ok(CollectionSchema {
                    name: name.to_string(),
                    document_count: count,
                    properties: props,
                })
            })
            .map_err(|e: String| e)
    }

    pub fn introspect_all(&self) -> Result<Vec<CollectionSchema>, String> {
        let names = self.list_collections()?;
        let mut schemas = Vec::new();
        for n in &names {
            schemas.push(self.introspect_collection(n)?);
        }
        Ok(schemas)
    }

    pub fn import_collection(
        &self,
        schema: &CollectionSchema,
        tenant: Option<&str>,
    ) -> Result<Vec<KnowledgeObject>, String> {
        self.rt
            .block_on(async {
                let coll = self.db.collection::<Document>(&schema.name);
                let mut cursor = coll
                    .find(doc! {})
                    .await
                    .map_err(|e| format!("import {}: {}", schema.name, e))?;
                use futures_util::StreamExt;
                let mut objects = Vec::new();
                while let Some(Ok(doc)) = cursor.next().await {
                    let (props, koid) = document_to_ko(&doc, &schema.name);
                    objects.push(KnowledgeObject {
                        koid,
                        version: 0,
                        commit_ts: 0,
                        metadata: Metadata {
                            type_name: schema.name.clone(),
                            tenant: tenant.map(String::from),
                            schema_version: 1,
                            tags: vec!["imported".into(), "source:mongodb".into()],
                        },
                        properties: props,
                        semantic: None,
                        relationships: vec![],
                        event_refs: vec![],
                        security: SecurityDescriptor {
                            // Must equal the runner subject "mongo-importer" —
                            // a mismatch makes every commit ACCESS_DENIED and
                            // the whole import a silent no-op (item 2 lesson).
                            owner: "mongo-importer".into(),
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
                Ok(objects)
            })
            .map_err(|e: String| e)
    }
}

// ---------------------------------------------------------------------------
// BSON → Aikoql Value
// ---------------------------------------------------------------------------

fn document_to_ko(doc: &Document, collection: &str) -> (PropertyMap, KOID) {
    let mut props = PropertyMap::new();
    let mut id_str: Option<String> = None;
    for (key, value) in doc.iter() {
        if key == "_id" {
            id_str = Some(bson_id_to_string(value));
        } else {
            props.insert(key.clone(), bson_to_value(value));
        }
    }
    // ponytail: the fallback key is non-deterministic across re-imports, but
    // it cannot occur in practice — MongoDB always assigns _id on insert.
    let pk = vec![id_str.unwrap_or_else(|| format!("{}:{}", collection, props.len()))];
    (props, deterministic_koid(collection, &pk))
}

fn bson_to_value(bson: &Bson) -> Value {
    match bson {
        Bson::Double(f) => Value::Float(*f),
        Bson::String(s) => Value::Text(s.clone()),
        Bson::Array(arr) => Value::List(arr.iter().map(bson_to_value).collect()),
        Bson::Document(doc) => {
            let mut m = BTreeMap::new();
            for (k, v) in doc.iter() {
                m.insert(k.clone(), bson_to_value(v));
            }
            Value::Map(m)
        }
        Bson::Boolean(b) => Value::Bool(*b),
        Bson::Null => Value::Null,
        Bson::Int32(i) => Value::Int(*i as i64),
        Bson::Int64(i) => Value::Int(*i),
        Bson::Binary(bin) => Value::Bytes(bin.bytes.clone()),
        Bson::ObjectId(oid) => Value::Text(oid.to_hex()),
        Bson::DateTime(dt) => Value::Text(dt.to_string()),
        Bson::RegularExpression(re) => Value::Text(re.pattern.clone()),
        _ => Value::Text(format!("{:?}", bson)),
    }
}

fn bson_id_to_string(bson: &Bson) -> String {
    match bson {
        Bson::ObjectId(oid) => oid.to_hex(),
        Bson::String(s) => s.clone(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        other => format!("{:?}", other),
    }
}

fn collect_keys(prefix: &str, doc: &Document, keys: &mut Vec<String>) {
    for (k, v) in doc.iter() {
        let fk = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{}.{}", prefix, k)
        };
        keys.push(fk.clone());
        if let Bson::Document(sub) = v {
            collect_keys(&fk, sub, keys);
        }
    }
}

fn deterministic_koid(table: &str, parts: &[String]) -> KOID {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in table.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash ^= 0x3a;
    hash = hash.wrapping_mul(0x100000001b3);
    for p in parts {
        for b in p.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0x2f;
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
    fn bson_scalars_to_value() {
        assert_eq!(bson_to_value(&Bson::Int32(42)), Value::Int(42));
        assert_eq!(
            bson_to_value(&Bson::Double(std::f64::consts::PI)),
            Value::Float(std::f64::consts::PI)
        );
        assert_eq!(
            bson_to_value(&Bson::String("hi".into())),
            Value::Text("hi".into())
        );
        assert_eq!(bson_to_value(&Bson::Boolean(true)), Value::Bool(true));
        assert_eq!(bson_to_value(&Bson::Null), Value::Null);
    }

    #[test]
    fn bson_array_to_value() {
        let arr = Bson::Array(vec![Bson::Int32(1), Bson::String("x".into())]);
        match bson_to_value(&arr) {
            Value::List(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], Value::Int(1));
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn bson_document_to_map() {
        let doc = Bson::Document(doc! { "k": "v", "n": 1 });
        match bson_to_value(&doc) {
            Value::Map(m) => {
                assert_eq!(m.get("k"), Some(&Value::Text("v".into())));
            }
            other => panic!("expected Map, got {:?}", other),
        }
    }

    #[test]
    fn bson_objectid_to_hex() {
        let oid = mongodb::bson::oid::ObjectId::new();
        let hex = oid.to_hex();
        assert_eq!(bson_to_value(&Bson::ObjectId(oid)), Value::Text(hex));
    }

    #[test]
    fn deterministic_koid_consistent() {
        assert_eq!(
            deterministic_koid("c", &["x".into()]),
            deterministic_koid("c", &["x".into()])
        );
    }

    #[test]
    fn collect_keys_flattens_nested() {
        let doc = doc! { "name": "A", "addr": { "city": "B" } };
        let mut keys = Vec::new();
        collect_keys("", &doc, &mut keys);
        assert!(keys.contains(&"name".into()));
        assert!(keys.contains(&"addr.city".into()));
    }
}
