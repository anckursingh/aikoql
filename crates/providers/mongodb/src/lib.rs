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

/// What a sample of documents showed about one (dotted) field.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldStats {
    pub name: String,
    /// Observed BSON type names, sorted (`Null` included when seen).
    pub types: Vec<String>,
    pub present: u64,
    pub nulls: u64,
    pub samples: u64,
}

impl FieldStats {
    pub fn presence_ratio(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.present as f64 / self.samples as f64
        }
    }
    pub fn null_ratio(&self) -> f64 {
        if self.present == 0 {
            0.0
        } else {
            self.nulls as f64 / self.present as f64
        }
    }
}

/// Field statistics over a slice of documents (pure; no server needed).
pub fn profile_from_docs(docs: &[Document]) -> Vec<FieldStats> {
    use std::collections::{BTreeSet, HashSet};
    /// Deeper nesting is reported as a plain `Document`, not descended into.
    const MAX_DEPTH: usize = 64;
    #[derive(Default)]
    struct Acc {
        types: BTreeSet<String>,
        present: u64,
        nulls: u64,
    }
    fn walk(
        prefix: &str,
        doc: &Document,
        depth: usize,
        acc: &mut BTreeMap<String, Acc>,
        once: &mut HashSet<String>,
    ) {
        for (k, v) in doc.iter() {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            let a = acc.entry(path.clone()).or_default();
            a.types.insert(bson_type_name(v));
            if once.insert(path.clone()) {
                a.present += 1;
                if matches!(v, Bson::Null) {
                    a.nulls += 1;
                }
            }
            if let Bson::Document(sub) = v {
                if depth < MAX_DEPTH {
                    walk(&path, sub, depth + 1, acc, once);
                }
            }
        }
    }
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
    for d in docs {
        walk("", d, 0, &mut acc, &mut HashSet::new());
    }
    acc.into_iter()
        .map(|(name, a)| FieldStats {
            name,
            types: a.types.into_iter().collect(),
            present: a.present,
            nulls: a.nulls,
            samples: docs.len() as u64,
        })
        .collect()
}

fn bson_type_name(b: &Bson) -> String {
    match b {
        Bson::Document(_) => "Document".into(),
        Bson::Null => "Null".into(),
        other => format!("{:?}", other.element_type()),
    }
}

pub struct MongoConnector {
    rt: Runtime,
    #[allow(dead_code)]
    client: Client, // held for lifetime; db borrows from it
    db: mongodb::Database,
    /// Absolute time after which this connector abandons its own work.
    deadline: Option<std::time::Instant>,
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
        Ok(MongoConnector {
            rt,
            client,
            db,
            deadline: None,
        })
    }

    /// Give every operation of this connector until `d` from now; later ones fail
    /// with "timed out" and the in-flight future is dropped, so the caller's thread
    /// returns instead of waiting on an unresponsive server.
    pub fn with_deadline(mut self, d: std::time::Duration) -> Self {
        self.deadline = Some(std::time::Instant::now() + d);
        self
    }

    fn block<T>(
        &self,
        fut: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let Some(deadline) = self.deadline else {
            return self.rt.block_on(fut);
        };
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        // the timer must be created inside the runtime, hence the async block
        self.rt
            .block_on(async { tokio::time::timeout(left, fut).await })
            .unwrap_or_else(|_| Err("timed out".to_string()))
    }

    pub fn list_collections(&self) -> Result<Vec<String>, String> {
        self.block(async {
            self.db
                .list_collection_names()
                .await
                .map_err(|e| format!("list collections: {}", e))
        })
    }

    pub fn introspect_collection(&self, name: &str) -> Result<CollectionSchema, String> {
        self.block(async {
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

    /// Field statistics from a random `$sample` of up to `sample_limit` documents.
    pub fn introspect_collection_sampled(
        &self,
        name: &str,
        sample_limit: u32,
    ) -> Result<Vec<FieldStats>, String> {
        let docs: Vec<Document> = self.block(async {
            use futures_util::StreamExt;
            let coll = self.db.collection::<Document>(name);
            let mut cursor = coll
                .aggregate(vec![
                    doc! { "$sample": { "size": sample_limit.clamp(1, 10_000) as i64 } },
                ])
                .await
                .map_err(|e| format!("sample {name}: {e}"))?;
            let mut out = Vec::new();
            while let Some(d) = cursor.next().await {
                out.push(d.map_err(|e| format!("sample {name}: {e}"))?);
            }
            Ok::<_, String>(out)
        })?;
        Ok(profile_from_docs(&docs))
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
        self.import_collection_limited(schema, tenant, u64::MAX)
            .map(|(objects, _)| objects)
    }

    /// At most `max_rows` documents; the flag says the collection had more.
    pub fn import_collection_limited(
        &self,
        schema: &CollectionSchema,
        tenant: Option<&str>,
        max_rows: u64,
    ) -> Result<(Vec<KnowledgeObject>, bool), String> {
        let limit = max_rows.saturating_add(1).min(i64::MAX as u64) as i64;
        self.block(async {
            let coll = self.db.collection::<Document>(&schema.name);
            let mut cursor = coll
                .find(doc! {})
                .limit(limit)
                .await
                .map_err(|e| format!("import {}: {}", schema.name, e))?;
            let (docs, truncated) = drain_documents(&mut cursor, &schema.name, max_rows).await?;
            let mut objects = Vec::with_capacity(docs.len());
            for doc in &docs {
                let (props, koid) = document_to_ko(doc, &schema.name);
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
            Ok((objects, truncated))
        })
        .map_err(|e: String| e)
    }
}

// ---------------------------------------------------------------------------
// BSON → Aikoql Value
// ---------------------------------------------------------------------------

/// Every document of a cursor, at most `max_rows`; the flag says there were more.
/// A cursor error (killed cursor, dropped connection, bad BSON) FAILS the import:
/// ending the loop at the first `Err` would report a partial copy as complete.
async fn drain_documents<S, E>(
    mut stream: S,
    collection: &str,
    max_rows: u64,
) -> Result<(Vec<Document>, bool), String>
where
    S: futures_util::Stream<Item = Result<Document, E>> + Unpin,
    E: std::fmt::Display,
{
    use futures_util::StreamExt;
    let mut docs = Vec::new();
    while let Some(item) = stream.next().await {
        docs.push(item.map_err(|e| format!("import {collection}: {e}"))?);
        if docs.len() as u64 > max_rows {
            break;
        }
    }
    let truncated = docs.len() as u64 > max_rows;
    docs.truncate(usize::try_from(max_rows).unwrap_or(usize::MAX));
    Ok((docs, truncated))
}

fn document_to_ko(doc: &Document, collection: &str) -> (PropertyMap, KOID) {
    let mut props = PropertyMap::new();
    let mut id_str: Option<String> = None;
    for (key, value) in doc.iter() {
        if key == "_id" {
            id_str = Some(bson_id_to_string(value));
        }
        // `_id` stays a property too: the source identity must be readable
        // from the object, not only recoverable through the KOID hash.
        props.insert(key.clone(), bson_to_value(value));
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
        Bson::DateTime(dt) => Value::Text(
            dt.try_to_rfc3339_string()
                .unwrap_or_else(|_| dt.timestamp_millis().to_string()),
        ),
        Bson::Decimal128(d) => Value::Text(d.to_string()),
        Bson::Timestamp(t) => Value::Map(BTreeMap::from([
            ("t".to_string(), Value::Int(i64::from(t.time))),
            ("i".to_string(), Value::Int(i64::from(t.increment))),
        ])),
        Bson::RegularExpression(re) => Value::Text(format!("/{}/{}", re.pattern, re.options)),
        Bson::Symbol(s) | Bson::JavaScriptCode(s) => Value::Text(s.clone()),
        // remaining exotic types (MinKey, MaxKey, Undefined, DBPointer, scoped
        // JS): their canonical extended-JSON text, never the Rust Debug form
        other => Value::Text(other.clone().into_canonical_extjson().to_string()),
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
    fn an_operation_that_never_returns_fails_at_the_deadline() {
        let c = MongoConnector::connect_with_timeout("mongodb://127.0.0.1:1", "d", Some(500))
            .unwrap()
            .with_deadline(std::time::Duration::from_millis(300));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r: Result<(), String> = c.block(std::future::pending());
            let _ = tx.send(r);
        });
        let r = rx.recv_timeout(std::time::Duration::from_secs(5));
        let err = r
            .expect("still blocked 5s after a 300ms deadline")
            .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
    }

    #[test]
    fn a_connector_without_a_deadline_is_unbounded_by_it() {
        let c =
            MongoConnector::connect_with_timeout("mongodb://127.0.0.1:1", "d", Some(500)).unwrap();
        let r: Result<u8, String> = c.block(async { Ok(7) });
        assert_eq!(r, Ok(7));
    }

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
    fn bson_decimal_dates_timestamps_and_regex_are_readable_values() {
        use mongodb::bson::{DateTime, Decimal128, Regex, Timestamp};
        let dec: Decimal128 = "12345.6789".parse().unwrap();
        assert_eq!(
            bson_to_value(&Bson::Decimal128(dec)),
            Value::Text("12345.6789".into())
        );
        let dt = DateTime::parse_rfc3339_str("2026-01-02T03:04:05Z").unwrap();
        assert_eq!(
            bson_to_value(&Bson::DateTime(dt)),
            Value::Text("2026-01-02T03:04:05Z".into())
        );
        let ts = Bson::Timestamp(Timestamp {
            time: 1,
            increment: 2,
        });
        assert_eq!(
            bson_to_value(&ts),
            Value::Map(BTreeMap::from([
                ("t".to_string(), Value::Int(1)),
                ("i".to_string(), Value::Int(2)),
            ]))
        );
        let re = Bson::RegularExpression(Regex {
            pattern: "^a".into(),
            options: "i".into(),
        });
        assert_eq!(bson_to_value(&re), Value::Text("/^a/i".into()));
        // nothing is ever rendered with the Rust Debug form
        for b in [
            Bson::MinKey,
            Bson::MaxKey,
            Bson::Undefined,
            Bson::Symbol("s".into()),
        ] {
            let v = bson_to_value(&b);
            assert!(!format!("{v:?}").contains("Symbol("), "{b:?} -> {v:?}");
        }
    }

    #[test]
    fn the_source_id_is_kept_as_a_property() {
        let (props, _) = document_to_ko(&doc! { "_id": 7_i32, "a": 1 }, "c");
        assert_eq!(props.get("_id"), Some(&Value::Int(7)));
        let oid = mongodb::bson::oid::ObjectId::new();
        let (props, _) = document_to_ko(&doc! { "_id": oid, "a": 1 }, "c");
        assert_eq!(props.get("_id"), Some(&Value::Text(oid.to_hex())));
    }

    #[test]
    fn a_cursor_error_fails_the_import_instead_of_ending_it_quietly() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let items: Vec<Result<Document, String>> = vec![
            Ok(doc! { "_id": 1 }),
            Err("cursor killed".into()),
            Ok(doc! { "_id": 2 }),
        ];
        let err = rt
            .block_on(drain_documents(
                futures_util::stream::iter(items),
                "c",
                u64::MAX,
            ))
            .unwrap_err();
        assert!(err.contains("cursor killed") && err.contains("c"), "{err}");
        let ok: Vec<Result<Document, String>> = vec![Ok(doc! { "_id": 1 }), Ok(doc! { "_id": 2 })];
        let (docs, truncated) = rt
            .block_on(drain_documents(futures_util::stream::iter(ok), "c", 1))
            .unwrap();
        assert_eq!(
            (docs.len(), truncated),
            (1, true),
            "the cap is reported, not an error"
        );
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

    #[test]
    fn profile_reports_types_presence_and_nulls() {
        let docs = vec![
            doc! { "_id": 1, "age": 30, "name": "a", "addr": { "city": "X" } },
            doc! { "_id": 2, "age": Bson::Null, "name": "b" },
            doc! { "_id": 3, "age": "unknown", "addr": { "city": "Y" } },
            doc! { "_id": 4 },
        ];
        let stats = profile_from_docs(&docs);
        let get = |n: &str| {
            stats
                .iter()
                .find(|s| s.name == n)
                .unwrap_or_else(|| panic!("no {n}: {stats:?}"))
        };
        let age = get("age");
        assert_eq!(age.types, ["Int32", "Null", "String"]);
        assert_eq!((age.present, age.nulls, age.samples), (3, 1, 4));
        assert!((age.presence_ratio() - 0.75).abs() < 1e-9);
        assert!((age.null_ratio() - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(get("name").types, ["String"]);
        assert_eq!(get("addr.city").present, 2);
        assert_eq!(get("addr").types, ["Document"]);
        assert_eq!(get("_id").presence_ratio(), 1.0);
        assert!(profile_from_docs(&[]).is_empty());
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn fuzz_iters() -> u64 {
        std::env::var("AIKOQL_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2_000)
    }

    fn random_bson(r: &mut Rng, depth: u32) -> Bson {
        match r.next() % if depth > 5 { 6 } else { 8 } {
            0 => Bson::Null,
            1 => Bson::Int32(r.next() as i32),
            2 => Bson::Int64(r.next() as i64),
            3 => Bson::Double(
                *[f64::NAN, f64::INFINITY, -0.0, 1.5]
                    .get((r.next() % 4) as usize)
                    .unwrap_or(&0.0),
            ),
            4 => Bson::String(["", "a\u{0}b", "💥", "x.y"][(r.next() % 4) as usize].to_string()),
            5 => Bson::Boolean(r.next().is_multiple_of(2)),
            6 => Bson::Array(
                (0..r.next() % 3)
                    .map(|_| random_bson(r, depth + 1))
                    .collect(),
            ),
            _ => Bson::Document(random_doc(r, depth + 1)),
        }
    }

    fn random_doc(r: &mut Rng, depth: u32) -> Document {
        let mut d = Document::new();
        for _ in 0..r.next() % 5 {
            let key = ["a", "b", "c", "x.y", "", "k$", "ü"][(r.next() % 7) as usize];
            d.insert(key, random_bson(r, depth));
        }
        d
    }

    #[test]
    fn profile_fuzz_keeps_its_invariants() {
        for seed in 1..=fuzz_iters() {
            let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let docs: Vec<Document> = (0..r.next() % 8).map(|_| random_doc(&mut r, 0)).collect();
            let stats = profile_from_docs(&docs); // must not panic
            for s in &stats {
                assert_eq!(s.samples, docs.len() as u64, "seed {seed}");
                assert!(
                    s.present <= s.samples && s.nulls <= s.present,
                    "seed {seed}: {s:?}"
                );
                assert!(
                    (0.0..=1.0).contains(&s.presence_ratio())
                        && (0.0..=1.0).contains(&s.null_ratio())
                );
                assert!(
                    !s.types.is_empty() && s.types.windows(2).all(|w| w[0] < w[1]),
                    "seed {seed}: {s:?}"
                );
            }
            let mut names: Vec<&str> = stats.iter().map(|s| s.name.as_str()).collect();
            let n = names.len();
            names.sort();
            names.dedup();
            assert_eq!(n, names.len(), "seed {seed}: one entry per field");
        }
    }

    #[test]
    fn profile_survives_200_levels_of_nesting_and_bounds_the_keys() {
        let mut inner = doc! { "leaf": 1 };
        for _ in 0..200 {
            inner = doc! { "n": inner };
        }
        let stats = profile_from_docs(&[inner]);
        assert!(!stats.is_empty());
        assert!(
            stats.len() <= 66,
            "depth is capped, got {} fields",
            stats.len()
        );
    }
}
