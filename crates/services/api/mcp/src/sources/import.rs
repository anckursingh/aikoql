//! `POST /api/v1/sources/import`: copy chosen containers of a source into the
//! knowledge base as objects typed by the physical container name.

use super::introspect::{neo4j_connector, scrub, GATE};
use super::{
    args_obj, audit, find_source, name_arg, store, tenant_arg, SourceKind, INVALID, NO_SCHEMA,
};
use crate::imports::{fresh_run_id, ImportSink};
use crate::{json, Kernel, Subject, J};
use aikoql_kernel::{KnowledgeObject, RelationshipRef, Value, KOID};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Rows of one container, capped; `true` = the container had more.
type Fetched = (Vec<KnowledgeObject>, bool);

fn fetch(
    kind: SourceKind,
    conn: &str,
    database: &str,
    ms: u64,
    container: &str,
    tenant: Option<&str>,
    cap: u64,
    deadline: std::time::Duration,
) -> Result<Fetched, String> {
    // the budget covers the open/connect too, so the driver stops when the gate would
    let started = std::time::Instant::now();
    let left = || deadline.saturating_sub(started.elapsed());
    match kind {
        SourceKind::Sqlite => {
            let c = aikoql_sqlite::SqliteConnector::open_read_only(conn, ms)?.with_deadline(left());
            let schema = c.introspect_table_with(container, false)?;
            c.import_table_limited(&schema, tenant, cap)
        }
        SourceKind::Postgres => {
            let mut c = aikoql_postgres::PostgresConnector::connect_with_timeout(conn, Some(ms))?;
            c.read_only_session()?;
            let schema = c.introspect_table_with(container, false)?;
            c.import_table_limited(&schema, tenant, cap)
        }
        SourceKind::Mongodb => {
            let c = aikoql_mongodb::MongoConnector::connect_with_timeout(conn, database, Some(ms))?
                .with_deadline(left());
            let schema = aikoql_mongodb::CollectionSchema {
                name: container.to_string(),
                document_count: 0,
                properties: Vec::new(),
            };
            c.import_collection_limited(&schema, tenant, cap)
        }
        // a graph is read whole by `fetch_neo4j` (relationships need every label), never per container
        SourceKind::Neo4j => Err("neo4j containers are imported as one graph".to_string()),
        SourceKind::Clickhouse => {
            let c = aikoql_clickhouse::ClickHouseConnector::connect_with_timeout(
                conn,
                database,
                Some(ms),
            )?;
            let t = c.introspect_table(container)?;
            c.import_table_limited(&t, tenant, cap)
        }
    }
}

/// Nodes of the chosen labels, with the relationships between them, per label.
///
/// Relationships whose other end is outside the selection (or beyond the row
/// cap) are left out: nothing may point at an object that was not imported.
/// Properties of a relationship ride on its start node as `rel:<TYPE>` lists
/// (RelationshipRef has no properties), each map tagged with the target KOID.
fn fetch_neo4j(
    conn: &str,
    ms: u64,
    labels: &[String],
    rel_types: &[String],
    tenant: Option<&str>,
    cap: u64,
) -> Result<BTreeMap<String, Fetched>, String> {
    let c = neo4j_connector(conn, ms)?;
    let mut sorted: Vec<&String> = labels.iter().collect();
    sorted.sort();
    let (mut ids, mut seen) = (HashMap::new(), HashSet::new());
    let mut by_label: BTreeMap<String, Fetched> = BTreeMap::new();
    for label in sorted {
        let (objects, id_map, cut) = c.import_nodes_limited(label, tenant, cap)?;
        for (elem, koid) in id_map {
            ids.entry(elem).or_insert(koid);
        }
        // a node under two labels is one object: the first sorted label owns it
        let kept = objects
            .into_iter()
            .filter(|o| seen.insert(o.koid))
            .collect();
        by_label.insert(label.clone(), (kept, cut));
    }
    let mut refs: HashMap<KOID, Vec<RelationshipRef>> = HashMap::new();
    let mut props: HashMap<KOID, BTreeMap<String, Vec<Value>>> = HashMap::new();
    let mut types: Vec<&String> = rel_types.iter().collect();
    types.sort();
    types.dedup();
    for rt in types {
        for (rel, src, tgt, rprops) in c.import_relationships(rt, &ids)? {
            let mut m = rprops;
            m.insert("target".to_string(), Value::Text(tgt.to_hex()));
            refs.entry(src).or_default().push(rel);
            props
                .entry(src)
                .or_default()
                .entry(format!("rel:{rt}"))
                .or_default()
                .push(Value::Map(m));
        }
    }
    for (objects, _) in by_label.values_mut() {
        for ko in objects.iter_mut() {
            ko.relationships = refs.remove(&ko.koid).unwrap_or_default();
            for (key, list) in props.remove(&ko.koid).unwrap_or_default() {
                ko.properties.insert(key, Value::List(list));
            }
        }
    }
    Ok(by_label)
}

/// The identity key the published ontology declares for each container of this
/// source kind, where it differs from what the importer already derives (the
/// source's own key): only those need their objects re-keyed. An empty map when
/// no ontology is active or none declares a key.
fn declared_keys(
    k: &Kernel,
    src: &super::SourceConfig,
    snapshot: &aikoql_ingestion::ConnectorMetadata,
) -> BTreeMap<String, Vec<String>> {
    let Some((ko, _)) = crate::http::active_ontology_for(k, src.tenant.as_deref()) else {
        return BTreeMap::new();
    };
    let Ok(def) = aikoql_kernel::OntologyDef::from_property_map(&ko.properties) else {
        return BTreeMap::new();
    };
    def.mappings
        .into_iter()
        .filter(|m| m.source == src.kind.as_str() && !m.identity.is_empty())
        .filter(|m| {
            snapshot
                .containers
                .iter()
                .find(|c| c.name == m.physical_type)
                .is_some_and(|c| aikoql_ingestion::declared_identity(c) != m.identity)
        })
        .map(|m| (m.physical_type, m.identity))
        .collect()
}

/// A stable text for a key value (what identifies the row, not how it prints).
fn key_text(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

/// 128 bits from two independent FNV-1a passes over (source, container, key values).
fn identity_koid(source: &str, container: &str, parts: &[String]) -> KOID {
    let text = format!("{source}\u{1f}{container}\u{1f}{}", parts.join("\u{1e}"));
    let fnv = |seed: u64| {
        text.bytes().fold(seed, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };
    let mut koid = [0u8; 16];
    koid[..8].copy_from_slice(&fnv(0xcbf2_9ce4_8422_2325).to_be_bytes());
    koid[8..].copy_from_slice(&fnv(0x8422_2325_cbf2_9ce4).to_be_bytes());
    KOID(koid)
}

/// Give every object the identity its key columns say it has. A key that does not
/// identify is refused whole - collapsing two rows into one object would lose data
/// without a word - so the caller writes nothing for the container.
fn rekey(
    objects: &mut [KnowledgeObject],
    source: &str,
    container: &str,
    cols: &[String],
) -> Result<(), String> {
    let mut seen: HashSet<KOID> = HashSet::new();
    for ko in objects.iter_mut() {
        let mut parts = Vec::with_capacity(cols.len());
        for c in cols {
            match ko.properties.get(c) {
                None | Some(Value::Null) => {
                    return Err(format!("a row has no value for the identity column {c}"))
                }
                Some(v) => parts.push(key_text(v)),
            }
        }
        let koid = identity_koid(source, container, &parts);
        if !seen.insert(koid) {
            return Err(format!(
                "the identity key ({}) is not unique: more than one row has {}",
                cols.join(", "),
                parts.join(", ")
            ));
        }
        ko.koid = koid;
    }
    Ok(())
}

/// `POST /api/v1/sources/import {source, containers, tenant?}`.
pub(crate) fn tool_sources_import(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let result = run(k, args);
    match &result {
        Ok(r) => audit(
            db_path,
            args,
            "imported",
            &format!(
                "source={} containers={} truncated={}",
                args.get("source").and_then(|s| s.as_str()).unwrap_or(""),
                r["imported"]
                    .as_object()
                    .map(|m| m.keys().cloned().collect::<Vec<_>>().join(","))
                    .unwrap_or_default(),
                r["truncated"]
            ),
        ),
        Err(e) => audit(
            db_path,
            args,
            "error",
            &format!("import reason={}", e.chars().take(60).collect::<String>()),
        ),
    }
    result
}

fn run(k: &Kernel, args: &J) -> Result<J, String> {
    let o = args_obj(args)?;
    let name = name_arg(o.get("source"), "source")?;
    let tenant = tenant_arg(o)?;
    let containers: Vec<String> = match o.get("containers") {
        Some(J::Array(a)) if !a.is_empty() && a.len() <= 100 => a
            .iter()
            .map(|v| match v {
                J::String(s) if !s.is_empty() && s.chars().count() <= 1_000 => Ok(s.clone()),
                _ => Err(format!(
                    "{INVALID}containers must be strings of 1-1000 characters"
                )),
            })
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(format!(
                "{INVALID}containers must be a list of 1-100 container names"
            ))
        }
    };
    let src = find_source(&name, tenant.as_deref())?;
    let importer = match src.kind {
        SourceKind::Sqlite => "sqlite-importer",
        SourceKind::Postgres => "pg-importer",
        SourceKind::Mongodb => "mongo-importer",
        SourceKind::Neo4j => "neo4j-importer",
        SourceKind::Clickhouse => "clickhouse-importer",
    };
    let snapshot = store::latest_snapshot(k, &src)?
        .ok_or_else(|| format!("source {name} has {NO_SCHEMA} - discover it first"))?;
    let known: HashSet<&str> = snapshot
        .containers
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    for c in &containers {
        if !known.contains(c.as_str()) {
            let shown: String = c.chars().take(40).flat_map(char::escape_debug).collect();
            return Err(format!(
                "{INVALID}unknown container '{shown}' - discover the source first"
            ));
        }
    }
    let keys = declared_keys(k, &src, &snapshot);
    let conn = src.connection()?;
    let mut subject = Subject::new(importer);
    if let Some(t) = &src.tenant {
        subject = subject.in_tenant(t);
    }
    let run_id = fresh_run_id();
    let sink = ImportSink::new(k, run_id.clone(), subject.clone(), vec![conn.clone()]);
    let tag = format!("src:{}", src.name);
    let (mut imported, mut unchanged) = (BTreeMap::new(), BTreeMap::new());
    let (mut truncated, mut skipped) = (Vec::new(), Vec::new());
    let mut pruned: BTreeMap<String, usize> = BTreeMap::new();
    let skip = |c: &str, why: String| json!({"container": c, "reason": why});

    // Objects are keyed by (container, primary key): never overwrite another source's.
    let mut active: Vec<&String> = Vec::new();
    for c in &containers {
        let foreign = k
            .scan_by_type(&subject, c)
            .map_err(|e| e.to_string())?
            .iter()
            .any(|ko| {
                ko.metadata.tenant.as_deref() == src.tenant.as_deref()
                    && !ko.metadata.tags.contains(&tag)
            });
        if foreign {
            skipped.push(skip(
                c,
                "this type already holds objects from another source (or a CLI import); not overwritten".into(),
            ));
        } else {
            active.push(c);
        }
    }
    let (database, ms, cap) = (
        src.database.clone().unwrap_or_default(),
        src.timeout_secs.saturating_mul(1000),
        src.import_max_rows,
    );
    // an import may legitimately run long, but not forever: a generous deadline
    let deadline = std::time::Duration::from_millis(ms.saturating_mul(20).clamp(60_000, 600_000));
    // a graph is read as a whole: relationships need every selected label's nodes
    let mut graph: Option<Result<BTreeMap<String, Fetched>, String>> = None;
    if src.kind == SourceKind::Neo4j && !active.is_empty() {
        let labels: Vec<String> = active.iter().map(|c| (*c).clone()).collect();
        let selected: HashSet<&str> = labels.iter().map(String::as_str).collect();
        let rel_types: Vec<String> = snapshot
            .references
            .iter()
            .filter(|r| {
                selected.contains(r.from_container.as_str())
                    && selected.contains(r.to_container.as_str())
            })
            .filter_map(|r| r.name.clone())
            .collect();
        let (conn_g, tenant_g) = (conn.clone(), src.tenant.clone());
        graph = Some(GATE.run(deadline, move || {
            fetch_neo4j(&conn_g, ms, &labels, &rel_types, tenant_g.as_deref(), cap)
        }));
    }
    for c in active {
        let fetched = match (&mut graph, src.kind) {
            (Some(Ok(by_label)), _) => Ok(by_label.remove(c).unwrap_or_default()),
            (Some(Err(e)), _) => Err(e.clone()),
            (None, kind) => {
                let (conn_c, container, tenant_c) = (conn.clone(), c.clone(), src.tenant.clone());
                let database = database.clone();
                GATE.run(deadline, move || {
                    fetch(
                        kind,
                        &conn_c,
                        &database,
                        ms,
                        &container,
                        tenant_c.as_deref(),
                        cap,
                        // stop on our own just before the gate gives up, so the thread returns
                        deadline.saturating_sub(std::time::Duration::from_secs(1)),
                    )
                })
            }
        };
        let (mut objects, cut) = match fetched {
            Ok(v) => v,
            Err(e) => {
                skipped.push(skip(c, scrub(&e, &conn)));
                continue;
            }
        };
        // an identity key the owner declared decides what "the same row" means
        let keyed = keys.get(c.as_str());
        if let Some(cols) = keyed {
            if let Err(why) = rekey(&mut objects, &src.name, c, cols) {
                skipped.push(skip(c, why));
                continue;
            }
        }
        let present: HashSet<KOID> = objects.iter().map(|o| o.koid).collect();
        let (mut wrote, mut same) = (0u64, 0u64);
        let mut failure = None;
        for mut ko in objects {
            ko.metadata.tags.push(tag.clone());
            match sink.put(ko, importer, "imported by source discovery") {
                Ok(true) => wrote += 1,
                Ok(false) => same += 1,
                Err(e) => {
                    failure = Some(scrub(&e.to_string(), &conn));
                    break;
                }
            }
        }
        if let Some(e) = failure {
            skipped.push(skip(c, format!("failed after {} rows: {e}", wrote + same)));
            continue;
        }
        // a complete import of a keyed container is the whole picture of the source:
        // what is no longer there is retired (this also retires the content-keyed
        // twins an earlier import left behind)
        if keyed.is_some() && !cut {
            match sink.prune_missing(c, &present) {
                Ok(n) => {
                    pruned.insert(c.clone(), n);
                }
                Err(e) => {
                    skipped.push(skip(
                        c,
                        format!(
                            "could not retire vanished rows: {}",
                            scrub(&e.to_string(), &conn)
                        ),
                    ));
                    continue;
                }
            }
        }
        imported.insert(c.clone(), wrote);
        unchanged.insert(c.clone(), same);
        if cut {
            truncated.push(c.clone());
        }
    }
    Ok(json!({
        "run_id": run_id, "imported": imported, "unchanged": unchanged,
        "pruned": pruned, "truncated": truncated, "skipped": skipped,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_deadline_counts_from_the_start_of_the_call_not_from_the_open() {
        use crate::sources::testkit::{make, tmp_db};
        let p = tmp_db("budget");
        make(
            &p,
            "CREATE TABLE seed (id INTEGER);
             CREATE VIEW v AS SELECT count(*) AS n FROM
               (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT x FROM c);",
        );
        // a writer holds the file for 900ms: the open alone uses most of a 1.5s budget
        let writer = rusqlite::Connection::open(&p).unwrap();
        writer
            .execute_batch("BEGIN EXCLUSIVE; INSERT INTO seed VALUES (1);")
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(900));
            writer.execute_batch("COMMIT").unwrap();
        });
        let conn = p.to_str().unwrap().to_string();
        let started = std::time::Instant::now();
        let r = fetch(
            SourceKind::Sqlite,
            &conn,
            "",
            5_000,
            "v",
            None,
            10,
            std::time::Duration::from_millis(1500),
        );
        let took = started.elapsed();
        release.join().unwrap();
        assert!(r.is_err(), "a runaway read must fail");
        assert!(
            took < std::time::Duration::from_millis(2100),
            "the budget restarted after the open: the call took {took:?} against a 1.5s budget"
        );
    }

    #[test]
    fn a_hung_source_call_ends_and_frees_its_gate_slot() {
        use crate::sources::introspect::Gate;
        use crate::sources::testkit::{make, tmp_db};
        // a view whose every read runs forever: the source never answers
        let p = tmp_db("hang");
        make(
            &p,
            "CREATE TABLE seed (id INTEGER);
             CREATE VIEW v AS SELECT count(*) AS n FROM
               (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT x FROM c);",
        );
        let gate = Gate::new(2);
        let conn = p.to_str().unwrap().to_string();
        let deadline = std::time::Duration::from_millis(400);
        let r = gate.run(std::time::Duration::from_millis(1500), move || {
            fetch(SourceKind::Sqlite, &conn, "", 300, "v", None, 10, deadline)
        });
        assert!(r.is_err(), "a hung source must fail the call");
        // the abandoned thread must actually end, not spin until the process dies
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while gate.stuck() > 0 && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(gate.stuck(), 0, "the gate slot was never freed");
    }

    use super::*;
    use crate::json;
    use crate::sources::service::tool_sources_discover;
    use crate::sources::testkit::*;
    use crate::sources::{with_test_sources, SourceConfig, SourceKind, INVALID, NO_SCHEMA};
    use aikoql_kernel::Value;

    fn run<R>(src: SourceConfig, db: &std::path::Path, f: impl FnOnce() -> R) -> R {
        let name = src.name.clone();
        with_test_sources(vec![src], vec![(name, conn(db))], f)
    }

    fn live(k: &Kernel, ty: &str, tenant: Option<&str>) -> Vec<aikoql_kernel::KnowledgeObject> {
        let s = crate::Subject::with_roles("test", &["admin"]);
        let s = match tenant {
            Some(t) => s.in_tenant(t),
            None => s,
        };
        k.scan_by_type(&s, ty)
            .unwrap()
            .into_iter()
            .filter(|ko| ko.metadata.tenant.as_deref() == tenant)
            .collect()
    }

    fn head(k: &Kernel) -> u64 {
        k.journal_head().unwrap().0
    }

    fn items_db(tag: &str, rows: u32) -> std::path::PathBuf {
        let p = tmp_db(tag);
        let mut sql =
            String::from("CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT); BEGIN;");
        for i in 1..=rows {
            sql.push_str(&format!("INSERT INTO items VALUES ({i}, 'item {i}');"));
        }
        sql.push_str("COMMIT;");
        make(&p, &sql);
        p
    }

    fn import(k: &Kernel, names: &[&str]) -> Result<J, String> {
        tool_sources_import(k, &json!({"source": "crm", "containers": names}), "x")
    }

    #[test]
    fn import_creates_objects_typed_by_the_physical_container_with_the_source_tag() {
        let (k, db) = (kernel(), crm_db("imp1"));
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let r = import(&k, &["customers"]).unwrap();
            assert_eq!(r["imported"]["customers"], 1);
            assert_eq!(r["truncated"], json!([]));
            let kos = live(&k, "customers", None);
            assert_eq!(kos.len(), 1);
            assert_eq!(
                kos[0].properties.get("email"),
                Some(&Value::Text("a@x.test".into()))
            );
            assert!(
                kos[0].metadata.tags.iter().any(|t| t == "src:crm"),
                "{:?}",
                kos[0].metadata.tags
            );
        });
    }

    #[test]
    fn a_tenant_sources_objects_land_in_that_tenant_only() {
        let (k, db) = (kernel(), crm_db("imp2"));
        run(sqlite_source("crm", Some("acme")), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm", "tenant": "acme"}), "x").unwrap();
            tool_sources_import(
                &k,
                &json!({"source": "crm", "tenant": "acme", "containers": ["customers"]}),
                "x",
            )
            .unwrap();
            assert_eq!(live(&k, "customers", Some("acme")).len(), 1);
            assert!(live(&k, "customers", None).is_empty());
            assert!(live(&k, "customers", Some("globex")).is_empty());
        });
    }

    #[test]
    fn unknown_undiscovered_and_malformed_containers_are_client_errors() {
        let (k, db) = (kernel(), crm_db("imp3"));
        run(sqlite_source("crm", None), &db, || {
            let e = import(&k, &["customers"]).unwrap_err();
            assert!(e.contains(NO_SCHEMA), "not discovered yet: {e}");
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let e = import(&k, &["nope"]).unwrap_err();
            assert!(e.starts_with(INVALID), "{e}");
            for bad in [
                json!({"source": "crm"}),
                json!({"source": "crm", "containers": []}),
                json!({"source": "crm", "containers": "customers"}),
                json!({"source": "crm", "containers": [1]}),
                json!({"source": "crm", "containers": (0..101).map(|i| format!("t{i}")).collect::<Vec<_>>()}),
            ] {
                let e = tool_sources_import(&k, &bad, "x").unwrap_err();
                assert_eq!(crate::sources::status_for(&e), Some(400), "{bad} -> {e}");
            }
            assert!(
                live(&k, "customers", None).is_empty(),
                "nothing was written"
            );
        });
    }

    #[test]
    fn the_row_cap_is_reported_never_silent() {
        let (k, db) = (kernel(), items_db("imp4", 5));
        let mut s = sqlite_source("crm", None);
        s.import_max_rows = 2;
        run(s, &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let r = import(&k, &["items"]).unwrap();
            assert_eq!(r["imported"]["items"], 2);
            assert_eq!(r["truncated"], json!(["items"]));
            assert_eq!(live(&k, "items", None).len(), 2);
        });
    }

    #[test]
    fn importing_twice_changes_nothing_and_a_changed_row_updates_in_place() {
        let (k, db) = (kernel(), items_db("imp5", 4));
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            import(&k, &["items"]).unwrap();
            let before = head(&k);
            let again = import(&k, &["items"]).unwrap();
            assert_eq!(again["imported"]["items"], 0);
            assert_eq!(again["unchanged"]["items"], 4);
            assert_eq!(
                head(&k),
                before,
                "an identical re-import must not move the journal"
            );
            assert_eq!(live(&k, "items", None).len(), 4);
            make(&db, "UPDATE items SET label = 'renamed' WHERE id = 2;");
            let r = import(&k, &["items"]).unwrap();
            assert_eq!(
                (
                    r["imported"]["items"].clone(),
                    r["unchanged"]["items"].clone()
                ),
                (json!(1), json!(3))
            );
            let ko = live(&k, "items", None)
                .into_iter()
                .find(|ko| ko.properties.get("id") == Some(&Value::Int(2)))
                .unwrap();
            assert_eq!(
                ko.properties.get("label"),
                Some(&Value::Text("renamed".into()))
            );
            assert_eq!(ko.version, 2);
        });
    }

    #[test]
    fn a_type_already_holding_another_sources_objects_is_refused_not_overwritten() {
        let k = kernel();
        let (a, b) = (crm_db("imp6a"), crm_db("imp6b"));
        with_test_sources(
            vec![sqlite_source("crm", None), sqlite_source("other", None)],
            vec![("crm".into(), conn(&a)), ("other".into(), conn(&b))],
            || {
                for n in ["crm", "other"] {
                    tool_sources_discover(&k, &json!({"source": n}), "x").unwrap();
                }
                import(&k, &["customers"]).unwrap();
                let r = tool_sources_import(
                    &k,
                    &json!({"source": "other", "containers": ["customers"]}),
                    "x",
                )
                .unwrap();
                assert_eq!(r["imported"].get("customers"), None);
                let skipped = r["skipped"].as_array().unwrap();
                assert_eq!(skipped.len(), 1, "{r}");
                assert!(
                    skipped[0]["reason"]
                        .as_str()
                        .unwrap()
                        .contains("another source"),
                    "{r}"
                );
                assert!(live(&k, "customers", None)[0]
                    .metadata
                    .tags
                    .iter()
                    .any(|t| t == "src:crm"));
            },
        );
    }

    // ---- declared identity keys (docs/ONTOLOGY-IN-PRACTICE.md, Phase A) ----

    fn publish_with_identity(k: &Kernel, key: &str, cols: &[&str]) {
        let p = crate::sources::ontology::tool_sources_ontology(
            k,
            &json!({"sources": ["crm"], "preview": true}),
            "x",
        )
        .unwrap();
        let r = crate::sources::ontology::tool_sources_ontology(
            k,
            &json!({"sources": ["crm"], "preview": false, "fingerprints": p["fingerprints"],
                    "identities": {key: cols}}),
            "x",
        )
        .unwrap();
        assert_eq!(r["saved"], true, "{r}");
    }

    fn visits_of(k: &Kernel, who: &str) -> Option<(i64, u64)> {
        live(k, "activity", None)
            .into_iter()
            .find(|ko| ko.properties.get("visitor") == Some(&Value::Text(who.into())))
            .and_then(|ko| match ko.properties.get("visits") {
                Some(Value::Int(n)) => Some((*n, ko.version)),
                _ => None,
            })
    }

    #[test]
    fn a_declared_identity_key_makes_a_re_import_update_in_place_and_prunes_what_vanished() {
        let (k, db) = (kernel(), tmp_db("idkey"));
        // no primary key, no unique constraint: the source says nothing about identity
        make(
            &db,
            "CREATE TABLE activity (visitor TEXT, visits INTEGER);
             INSERT INTO activity VALUES ('a', 1), ('b', 2);",
        );
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            publish_with_identity(&k, "crm.activity", &["visitor"]);
            let r = import(&k, &["activity"]).unwrap();
            assert_eq!(r["imported"]["activity"], 2, "{r}");
            assert_eq!(live(&k, "activity", None).len(), 2, "two rows, two objects");

            // the source changes one row: the SAME object gets a second version
            make(&db, "UPDATE activity SET visits = 5 WHERE visitor = 'a';");
            let r = import(&k, &["activity"]).unwrap();
            assert_eq!(
                (
                    r["imported"]["activity"].clone(),
                    r["unchanged"]["activity"].clone()
                ),
                (json!(1), json!(1)),
                "{r}"
            );
            assert_eq!(live(&k, "activity", None).len(), 2, "no duplicate object");
            assert_eq!(
                visits_of(&k, "a"),
                Some((5, 2)),
                "version 2 of the same object"
            );

            // a row that vanished from a complete import is retired, not left as a stale twin
            make(&db, "DELETE FROM activity WHERE visitor = 'b';");
            let r = import(&k, &["activity"]).unwrap();
            assert_eq!(r["pruned"]["activity"], 1, "{r}");
            assert!(visits_of(&k, "b").is_none(), "b is no longer live");
            assert_eq!(visits_of(&k, "a"), Some((5, 2)));

            // an ambiguous key is refused loudly: collapsing two rows would silently lose one
            make(&db, "INSERT INTO activity VALUES ('a', 9);");
            let before = head(&k);
            let r = import(&k, &["activity"]).unwrap();
            let why = r["skipped"][0]["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(why.contains("not unique") && why.contains("visitor"), "{r}");
            assert_eq!(
                head(&k),
                before,
                "nothing was written for the refused container"
            );
            // ...and so is a row with no value for the key
            make(
                &db,
                "DELETE FROM activity WHERE visits = 9; INSERT INTO activity VALUES (NULL, 3);",
            );
            let r = import(&k, &["activity"]).unwrap();
            let why = r["skipped"][0]["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(why.contains("no value") && why.contains("visitor"), "{r}");
        });
    }

    #[test]
    fn import_never_writes_to_the_source_file() {
        let (k, db) = (kernel(), items_db("imp7", 3));
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let before = std::fs::read(&db).unwrap();
            import(&k, &["items"]).unwrap();
            assert_eq!(std::fs::read(&db).unwrap(), before);
        });
    }

    #[test]
    fn hostile_container_names_import_as_exact_types() {
        let (k, db) = (kernel(), tmp_db("imp8"));
        make(&db, "CREATE TABLE \"we\"\"ird name\" (id INTEGER PRIMARY KEY, \"it's\" TEXT); INSERT INTO \"we\"\"ird name\" VALUES (1, 'x'); CREATE TABLE \"order\" (id INTEGER PRIMARY KEY); INSERT INTO \"order\" VALUES (9);");
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            let r = import(&k, &["we\"ird name", "order"]).unwrap();
            assert_eq!(r["imported"]["we\"ird name"], 1);
            assert_eq!(r["imported"]["order"], 1);
            assert_eq!(
                live(&k, "we\"ird name", None)[0].properties.get("it's"),
                Some(&Value::Text("x".into()))
            );
        });
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

    #[test]
    fn fuzzed_rows_import_exactly_and_reimport_is_a_no_op() {
        let iters = std::env::var("AIKOQL_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_000)
            / 100;
        for seed in 1..=iters.max(8) {
            let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (k, db) = (kernel(), tmp_db(&format!("imp-f{seed}")));
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE f (id INTEGER PRIMARY KEY, t TEXT, n INTEGER, x REAL, b BLOB)",
            )
            .unwrap();
            let rows = 1 + r.next() % 40;
            for id in 0..rows as i64 {
                let text: Option<String> = match r.next() % 6 {
                    0 => None,
                    1 => Some(String::new()),
                    2 => Some("a\u{0}b\n\"'`;--".into()),
                    3 => Some("💥\u{202e}ünï".into()),
                    4 => Some("L".repeat(100_000)),
                    _ => Some(format!("row {id}")),
                };
                let n: Option<i64> = [None, Some(i64::MIN), Some(i64::MAX), Some(0), Some(-1)]
                    [(r.next() % 5) as usize];
                let x: Option<f64> =
                    [None, Some(f64::INFINITY), Some(-0.0), Some(1.5e300)][(r.next() % 4) as usize];
                let b: Option<Vec<u8>> = r.next().is_multiple_of(3).then(|| vec![0u8, 255, 7]);
                c.execute(
                    "INSERT INTO f VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![id, text, n, x, b],
                )
                .unwrap();
            }
            drop(c);
            run(sqlite_source("crm", None), &db, || {
                tool_sources_discover(&k, &json!({"source": "crm"}), "x")
                    .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
                let r1 = import(&k, &["f"]).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
                assert_eq!(r1["imported"]["f"], rows, "seed {seed}");
                assert_eq!(live(&k, "f", None).len() as u64, rows, "seed {seed}");
                let before = head(&k);
                let r2 = import(&k, &["f"]).unwrap();
                assert_eq!(
                    (r2["imported"]["f"].clone(), r2["unchanged"]["f"].clone()),
                    (json!(0), json!(rows)),
                    "seed {seed}"
                );
                assert_eq!(head(&k), before, "seed {seed}: re-import must be a no-op");
            });
        }
    }

    #[test]
    fn the_audit_line_names_ids_only() {
        let (k, db) = (kernel(), crm_db("imp9"));
        let log = std::env::temp_dir().join(format!("aikoql-import-audit-{}", std::process::id()));
        let log = log.to_str().unwrap().to_string();
        run(sqlite_source("crm", None), &db, || {
            tool_sources_discover(&k, &json!({"source": "crm"}), "x").unwrap();
            tool_sources_import(
                &k,
                &json!({"source": "crm", "containers": ["customers"], "subject": "alice"}),
                &log,
            )
            .unwrap();
        });
        let path = crate::audit::audit_log_path(&log);
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(
            text.contains("imported") && text.contains("customers"),
            "{text}"
        );
        assert!(
            !text.contains("a@x.test") && !text.contains(db.to_str().unwrap()),
            "no row values or paths: {text}"
        );
    }
}
