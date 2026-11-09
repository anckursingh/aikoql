//! The source's schema as knowledge: `aikoql:source` (one per source),
//! `aikoql:source-table` (one per container, fields and references as
//! properties) and `aikoql:schema-run` (one per run that changed something).
//! Versions give schema history through the normal MVCC; an unchanged
//! fingerprint writes nothing. No connection string is ever stored.

use super::SourceConfig;
use crate::helpers::{parse_properties, value_to_json};
use crate::{
    json, ExtensionMap, Kernel, Metadata, Origin, ReferentialPolicy, RememberRequest,
    SecurityDescriptor, Subject, Value, J,
};
use aikoql_ingestion::{
    diff_metadata, ConnectorMetadata, ContainerInfo, ReferenceInfo, SchemaDiff,
};
use aikoql_kernel::{ForgetMode, KnowledgeObject, PropertyMap};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Mutex;

pub(crate) const T_SOURCE: &str = "aikoql:source";
pub(crate) const T_TABLE: &str = "aikoql:source-table";
pub(crate) const T_RUN: &str = "aikoql:schema-run";

/// One snapshot writer at a time: a check-then-write over KOs must not race
/// with itself (single-node; discovery is a rare admin action).
static WRITER: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
pub(crate) struct SnapshotOutcome {
    pub run_koid: Option<String>,
    pub fingerprint: String,
    pub changed: bool,
    pub diff: SchemaDiff,
    pub previous_fingerprint: Option<String>,
}

fn subject(src: &SourceConfig) -> Subject {
    let s = Subject::with_roles("source-discovery", &["admin"]);
    match &src.tenant {
        Some(t) => s.in_tenant(t),
        None => s,
    }
}

fn text<'a>(ko: &'a KnowledgeObject, key: &str) -> Option<&'a str> {
    match ko.properties.get(key) {
        Some(Value::Text(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn props_of(j: &J) -> PropertyMap {
    parse_properties(&json!({ "properties": j })).unwrap_or_default()
}

fn json_of(ko: &KnowledgeObject, key: &str) -> J {
    ko.properties.get(key).map(value_to_json).unwrap_or(J::Null)
}

/// This source's live KOs of one type (same tenant, same source name).
fn owned(k: &Kernel, src: &SourceConfig, ty: &str) -> Result<Vec<KnowledgeObject>, String> {
    let key = if ty == T_SOURCE { "name" } else { "source" };
    Ok(k.scan_by_type(&subject(src), ty)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|ko| {
            ko.metadata.tenant.as_deref() == src.tenant.as_deref()
                && text(ko, key) == Some(src.name.as_str())
        })
        .collect())
}

fn write(
    k: &Kernel,
    src: &SourceConfig,
    ty: &str,
    existing: Option<&KnowledgeObject>,
    props: PropertyMap,
    note: &str,
) -> Result<String, String> {
    let r = k
        .remember(RememberRequest {
            context: subject(src).into(),
            koid: existing.map(|e| e.koid),
            expected_version: Some(existing.map_or(0, |e| e.version)),
            idempotency_key: None,
            metadata: Metadata {
                type_name: ty.into(),
                tenant: src.tenant.clone(),
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: "system".into(),
                acl: vec![],
                classification: None,
            }),
            extensions: ExtensionMap::new(),
            origin: Origin::System,
            note: Some(note.into()),
            referential_policy: ReferentialPolicy::default(),
        })
        .map_err(|e| e.to_string())?;
    Ok(r.koid.to_hex())
}

/// Hash of a container's schema (row counts are volatile and excluded).
fn container_hash(c: &ContainerInfo) -> String {
    let mut stable = c.clone();
    stable.row_count = None;
    let body = serde_json::to_string(&stable).unwrap_or_default();
    Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn record_snapshot(
    k: &Kernel,
    src: &SourceConfig,
    meta: &ConnectorMetadata,
) -> Result<SnapshotOutcome, String> {
    let _writer = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let fingerprint = meta.fingerprint();
    let source_ko = owned(k, src, T_SOURCE)?.into_iter().next();
    let stored_fp = source_ko
        .as_ref()
        .and_then(|ko| text(ko, "fingerprint").map(String::from));
    if stored_fp.as_deref() == Some(fingerprint.as_str()) {
        return Ok(SnapshotOutcome {
            run_koid: None,
            fingerprint,
            changed: false,
            diff: SchemaDiff::default(),
            previous_fingerprint: stored_fp,
        });
    }
    let previous = latest_snapshot(k, src)?;
    let diff = diff_metadata(&previous.clone().unwrap_or_default(), meta);

    let mut refs = meta.references.clone();
    refs.sort_by(|a, b| (&a.from_container, &a.name).cmp(&(&b.from_container, &b.name)));
    write(
        k,
        src,
        T_SOURCE,
        source_ko.as_ref(),
        props_of(&json!({
            "name": src.name,
            "kind": meta.connector_type,
            "label": meta.label,
            "database": src.database,
            "fingerprint": fingerprint,
            "containers": meta.containers.len(),
            "truncated": meta.truncated,
            "version": meta.version,
            "references": serde_json::to_value(&refs).map_err(|e| e.to_string())?,
        })),
        "source schema recorded",
    )?;

    let mut existing: BTreeMap<String, KnowledgeObject> = owned(k, src, T_TABLE)?
        .into_iter()
        .filter_map(|ko| text(&ko, "name").map(|n| (n.to_string(), ko.clone())))
        .collect();
    for c in &meta.containers {
        let hash = container_hash(c);
        let old = existing.remove(&c.name);
        if old.as_ref().and_then(|ko| text(ko, "hash")) == Some(hash.as_str()) {
            // schema unchanged; refresh nothing (row counts are volatile)
            continue;
        }
        write(
            k,
            src,
            T_TABLE,
            old.as_ref(),
            props_of(&json!({
                "source": src.name,
                "name": c.name,
                "hash": hash,
                "container": serde_json::to_value(c).map_err(|e| e.to_string())?,
            })),
            "source table recorded",
        )?;
    }
    for (_, gone) in existing {
        k.forget(
            subject(src),
            &gone.koid,
            ForgetMode::Tombstone,
            Some(gone.version),
            Some("container no longer in the source".into()),
        )
        .map_err(|e| e.to_string())?;
    }

    let run_koid = write(
        k,
        src,
        T_RUN,
        None,
        props_of(&json!({
            "source": src.name,
            "fingerprint": fingerprint,
            "previous": stored_fp,
            "containers": meta.containers.len(),
            "truncated": meta.truncated,
            "diff": serde_json::to_value(&diff).map_err(|e| e.to_string())?,
        })),
        "schema changed",
    )?;
    Ok(SnapshotOutcome {
        run_koid: Some(run_koid),
        fingerprint,
        changed: true,
        diff,
        previous_fingerprint: stored_fp,
    })
}

/// `{fingerprint, containers, truncated}` of the last recorded schema, or null.
pub(crate) fn last_summary(k: &Kernel, src: &SourceConfig) -> Result<J, String> {
    Ok(match owned(k, src, T_SOURCE)?.into_iter().next() {
        None => J::Null,
        Some(ko) => {
            let mut names: Vec<String> = owned(k, src, T_TABLE)?
                .iter()
                .filter_map(|t| text(t, "name").map(String::from))
                .collect();
            names.sort();
            names.truncate(500);
            json!({
                "fingerprint": text(&ko, "fingerprint"),
                "containers": json_of(&ko, "containers"),
                "truncated": json_of(&ko, "truncated"),
                "container_names": names,
            })
        }
    })
}

/// The latest recorded schema of the source, rebuilt from its KOs.
pub(crate) fn latest_snapshot(
    k: &Kernel,
    src: &SourceConfig,
) -> Result<Option<ConnectorMetadata>, String> {
    let Some(source_ko) = owned(k, src, T_SOURCE)?.into_iter().next() else {
        return Ok(None);
    };
    let mut containers: Vec<ContainerInfo> = owned(k, src, T_TABLE)?
        .iter()
        .filter_map(|ko| serde_json::from_value(json_of(ko, "container")).ok())
        .collect();
    containers.sort_by(|a, b| a.name.cmp(&b.name));
    let references: Vec<ReferenceInfo> =
        serde_json::from_value(json_of(&source_ko, "references")).unwrap_or_default();
    Ok(Some(ConnectorMetadata {
        connector_type: text(&source_ko, "kind").unwrap_or_default().to_string(),
        label: text(&source_ko, "label").unwrap_or_default().to_string(),
        containers,
        references,
        version: text(&source_ko, "version").map(String::from),
        truncated: matches!(
            source_ko.properties.get("truncated"),
            Some(Value::Bool(true))
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::SourceKind;
    use aikoql_ingestion::{ContainerInfo, FieldInfo, FieldProfile, Provenance, ReferenceInfo};
    use aikoql_kernel::{ManualClock, MemoryEngine};
    use std::sync::Arc;

    fn kernel() -> Arc<Kernel> {
        Arc::new(
            Kernel::open(
                Arc::new(MemoryEngine::new()),
                Arc::new(ManualClock::new(10_000)),
                0xBEEF,
            )
            .unwrap(),
        )
    }

    fn src(name: &str, tenant: Option<&str>) -> SourceConfig {
        SourceConfig {
            name: name.into(),
            kind: SourceKind::Sqlite,
            url_env: "PATH".into(),
            database: None,
            tenant: tenant.map(String::from),
            sample_limit: 100,
            max_containers: 500,
            timeout_secs: 5,
            import_max_rows: 100_000,
        }
    }

    fn field(name: &str, ty: &str) -> FieldInfo {
        FieldInfo {
            name: name.into(),
            data_type: ty.into(),
            ..Default::default()
        }
    }

    fn table(name: &str, cols: &[&str]) -> ContainerInfo {
        ContainerInfo {
            name: name.into(),
            fields: cols.iter().map(|c| field(c, "text")).collect(),
            row_count: Some(7),
            ..Default::default()
        }
    }

    fn meta(tables: &[(&str, &[&str])]) -> ConnectorMetadata {
        ConnectorMetadata {
            connector_type: "sqlite".into(),
            label: "crm".into(),
            containers: tables.iter().map(|(n, c)| table(n, c)).collect(),
            ..Default::default()
        }
    }

    fn admin(tenant: Option<&str>) -> crate::Subject {
        let s = crate::Subject::with_roles("test", &["admin"]);
        match tenant {
            Some(t) => s.in_tenant(t),
            None => s,
        }
    }

    fn live(k: &Kernel, ty: &str, tenant: Option<&str>) -> Vec<aikoql_kernel::KnowledgeObject> {
        k.scan_by_type(&admin(tenant), ty)
            .unwrap()
            .into_iter()
            .filter(|ko| ko.metadata.tenant.as_deref() == tenant)
            .collect()
    }

    fn head(k: &Kernel) -> u64 {
        k.journal_head().unwrap().0
    }

    #[test]
    fn first_snapshot_writes_a_source_its_tables_and_one_run() {
        let k = kernel();
        let s = src("crm", None);
        let m = meta(&[("customers", &["id", "email"]), ("orders", &["id"])]);
        let o = record_snapshot(&k, &s, &m).unwrap();
        assert!(o.changed && o.run_koid.is_some() && o.previous_fingerprint.is_none());
        assert_eq!(o.fingerprint, m.fingerprint());
        assert_eq!(o.diff.added_containers, ["customers", "orders"]);
        assert_eq!(live(&k, T_SOURCE, None).len(), 1);
        assert_eq!(live(&k, T_TABLE, None).len(), 2);
        assert_eq!(live(&k, T_RUN, None).len(), 1);
    }

    #[test]
    fn an_identical_snapshot_writes_nothing() {
        let k = kernel();
        let s = src("crm", None);
        let m = meta(&[("customers", &["id"])]);
        record_snapshot(&k, &s, &m).unwrap();
        let before = head(&k);
        // row counts are volatile and not part of the schema
        let mut again = m.clone();
        again.containers[0].row_count = Some(99_999);
        let o = record_snapshot(&k, &s, &again).unwrap();
        assert!(!o.changed && o.run_koid.is_none() && o.diff.is_empty());
        assert_eq!(head(&k), before, "the journal must not move");
    }

    #[test]
    fn a_new_column_bumps_exactly_that_table_and_records_the_diff() {
        let k = kernel();
        let s = src("crm", None);
        record_snapshot(
            &k,
            &s,
            &meta(&[("customers", &["id"]), ("orders", &["id"])]),
        )
        .unwrap();
        let o = record_snapshot(
            &k,
            &s,
            &meta(&[("customers", &["id", "phone"]), ("orders", &["id"])]),
        )
        .unwrap();
        assert!(o.changed && o.previous_fingerprint.is_some());
        assert_eq!(o.diff.changed.len(), 1);
        assert_eq!(o.diff.changed[0].name, "customers");
        assert_eq!(o.diff.changed[0].added_fields, ["phone"]);
        let tables = live(&k, T_TABLE, None);
        let version_of = |n: &str| {
            tables
                .iter()
                .find(|t| t.properties.get("name") == Some(&aikoql_kernel::Value::Text(n.into())))
                .unwrap()
                .version
        };
        assert_eq!((version_of("customers"), version_of("orders")), (2, 1));
        assert_eq!(live(&k, T_RUN, None).len(), 2);
        assert_eq!(
            live(&k, T_SOURCE, None).len(),
            1,
            "the source KO is updated in place"
        );
    }

    #[test]
    fn a_removed_table_is_tombstoned_and_leaves_the_live_set() {
        let k = kernel();
        let s = src("crm", None);
        record_snapshot(&k, &s, &meta(&[("a", &["x"]), ("b", &["x"])])).unwrap();
        let o = record_snapshot(&k, &s, &meta(&[("a", &["x"])])).unwrap();
        assert_eq!(o.diff.removed_containers, ["b"]);
        let names: Vec<String> = live(&k, T_TABLE, None)
            .iter()
            .filter_map(|t| match t.properties.get("name") {
                Some(aikoql_kernel::Value::Text(n)) => Some(n.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["a"]);
        assert!(
            k.scan_heads()
                .unwrap()
                .iter()
                .any(|(_, _, _, st)| *st == crate::LifecycleState::Deleted),
            "history is kept as a tombstone, not erased"
        );
        assert_eq!(
            latest_snapshot(&k, &s).unwrap().unwrap().containers.len(),
            1
        );
    }

    #[test]
    fn latest_snapshot_round_trips_the_schema_and_its_fingerprint() {
        let k = kernel();
        let s = src("crm", None);
        let mut m = meta(&[("customers", &["id", "age"]), ("orders", &["id"])]);
        m.containers[0].fields[1].provenance = Provenance::Observed;
        m.containers[0].fields[1].profile = Some(FieldProfile {
            types: vec!["Int32".into(), "Null".into()],
            presence_ratio: 0.91,
            null_ratio: 0.04,
            samples: 1000,
        });
        m.containers[0].unique_groups = vec![vec!["id".into()]];
        m.references = vec![ReferenceInfo {
            from_container: "orders".into(),
            from_fields: vec!["id".into()],
            to_container: "customers".into(),
            to_fields: vec!["id".into()],
            name: Some("fk".into()),
        }];
        assert!(latest_snapshot(&k, &s).unwrap().is_none());
        record_snapshot(&k, &s, &m).unwrap();
        let back = latest_snapshot(&k, &s).unwrap().unwrap();
        assert_eq!(back.fingerprint(), m.fingerprint());
        assert_eq!(back.references.len(), 1);
        assert_eq!(
            back.containers
                .iter()
                .find(|c| c.name == "customers")
                .unwrap()
                .fields[1]
                .profile,
            m.containers[0].fields[1].profile
        );
        assert_eq!(
            (back.connector_type.as_str(), back.label.as_str()),
            ("sqlite", "crm")
        );
    }

    #[test]
    fn nothing_stored_contains_the_connection_string_and_hostile_names_round_trip() {
        let k = kernel();
        let s = src("crm", None);
        let hostile = [
            "a\u{0}b",
            "line\nbreak",
            "q\"uote'",
            "💥\u{202e}",
            &"L".repeat(2_000),
            "../x",
            "`;--",
        ];
        let tables: Vec<(&str, &[&str])> = hostile.iter().map(|n| (*n, &["c\u{0}1"][..])).collect();
        record_snapshot(&k, &s, &meta(&tables)).unwrap();
        let back = latest_snapshot(&k, &s).unwrap().unwrap();
        let mut names: Vec<&str> = back.containers.iter().map(|c| c.name.as_str()).collect();
        let mut want = hostile.to_vec();
        names.sort();
        want.sort();
        assert_eq!(names, want, "names must round-trip byte for byte");
        let conn = s.connection().unwrap(); // PATH: stands in for any DSN
        for ty in [T_SOURCE, T_TABLE, T_RUN] {
            for ko in live(&k, ty, None) {
                let dump = format!("{:?}", ko.properties);
                assert!(!dump.contains(&conn), "{ty} stores the connection string");
            }
        }
    }

    #[test]
    fn tenants_are_isolated() {
        let k = kernel();
        record_snapshot(&k, &src("crm", Some("acme")), &meta(&[("a", &["x"])])).unwrap();
        record_snapshot(
            &k,
            &src("crm", Some("globex")),
            &meta(&[("a", &["x"]), ("b", &["x"])]),
        )
        .unwrap();
        record_snapshot(&k, &src("crm", None), &meta(&[("g", &["x"])])).unwrap();
        assert_eq!(live(&k, T_TABLE, Some("acme")).len(), 1);
        assert_eq!(live(&k, T_TABLE, Some("globex")).len(), 2);
        assert_eq!(live(&k, T_TABLE, None).len(), 1);
        assert_eq!(
            latest_snapshot(&k, &src("crm", Some("acme")))
                .unwrap()
                .unwrap()
                .containers
                .len(),
            1
        );
        assert_eq!(
            latest_snapshot(&k, &src("crm", Some("globex")))
                .unwrap()
                .unwrap()
                .containers
                .len(),
            2
        );
        assert_eq!(
            latest_snapshot(&k, &src("crm", None))
                .unwrap()
                .unwrap()
                .containers[0]
                .name,
            "g"
        );
        assert!(
            latest_snapshot(&k, &src("other", Some("acme")))
                .unwrap()
                .is_none(),
            "a different source name has no history"
        );
    }

    #[test]
    fn concurrent_snapshots_do_not_duplicate() {
        let k = kernel();
        let m = meta(&[("a", &["x"]), ("b", &["x"]), ("c", &["x"])]);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (k, m) = (k.clone(), m.clone());
                std::thread::spawn(move || record_snapshot(&k, &src("crm", None), &m).unwrap())
            })
            .collect();
        let outcomes: Vec<SnapshotOutcome> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            outcomes.iter().filter(|o| o.changed).count(),
            1,
            "exactly one writer records the change"
        );
        assert_eq!(live(&k, T_SOURCE, None).len(), 1);
        assert_eq!(live(&k, T_TABLE, None).len(), 3);
        assert_eq!(live(&k, T_RUN, None).len(), 1);
    }

    #[test]
    fn a_2000_table_snapshot_completes_in_bounded_time() {
        let k = kernel();
        let tables: Vec<(String, Vec<&str>)> = (0..2_000)
            .map(|i| (format!("t{i}"), vec!["id", "v"]))
            .collect();
        let m = ConnectorMetadata {
            connector_type: "sqlite".into(),
            label: "crm".into(),
            containers: tables.iter().map(|(n, c)| table(n, c)).collect(),
            ..Default::default()
        };
        let t = std::time::Instant::now();
        record_snapshot(&k, &src("crm", None), &m).unwrap();
        assert!(
            t.elapsed() < std::time::Duration::from_secs(60),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(live(&k, T_TABLE, None).len(), 2_000);
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    #[test]
    fn snapshot_sequence_fuzz_keeps_the_store_equal_to_the_latest_schema() {
        let iters = std::env::var("AIKOQL_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_000)
            / 20;
        for seed in 1..=iters.max(20) {
            let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let k = kernel();
            let s = src("crm", None);
            let mut current = meta(&[]);
            let mut runs = 0usize;
            let mut last_fp = None::<String>;
            for step in 0..12 {
                match r.below(5) {
                    0 => current
                        .containers
                        .push(table(&format!("t{}", r.below(8)), &["id"])),
                    1 if !current.containers.is_empty() => {
                        let i = r.below(current.containers.len());
                        current.containers.remove(i);
                    }
                    2 if !current.containers.is_empty() => {
                        let i = r.below(current.containers.len());
                        current.containers[i]
                            .fields
                            .push(field(&format!("c{step}"), "int"));
                    }
                    3 if !current.containers.is_empty() => {
                        let i = r.below(current.containers.len());
                        if let Some(f) = current.containers[i].fields.first_mut() {
                            f.data_type.push('x');
                        }
                    }
                    _ => {}
                }
                // duplicate container names are not a valid schema
                let mut seen = std::collections::BTreeSet::new();
                current.containers.retain(|c| seen.insert(c.name.clone()));
                let o = record_snapshot(&k, &s, &current)
                    .unwrap_or_else(|e| panic!("seed {seed} step {step}: {e}"));
                if last_fp.as_deref() != Some(current.fingerprint().as_str()) {
                    assert!(
                        o.changed,
                        "seed {seed} step {step}: a changed schema must be recorded"
                    );
                    runs += 1;
                } else {
                    assert!(
                        !o.changed,
                        "seed {seed} step {step}: an unchanged schema must be a no-op"
                    );
                }
                last_fp = Some(current.fingerprint());
                let tables = live(&k, T_TABLE, None);
                assert_eq!(
                    tables.len(),
                    current.containers.len(),
                    "seed {seed} step {step}: table KOs == containers"
                );
                assert_eq!(live(&k, T_SOURCE, None).len(), 1, "seed {seed}");
                assert_eq!(
                    live(&k, T_RUN, None).len(),
                    runs,
                    "seed {seed}: one run per change"
                );
                let back = latest_snapshot(&k, &s).unwrap().unwrap();
                assert_eq!(
                    back.fingerprint(),
                    current.fingerprint(),
                    "seed {seed} step {step}: stored == current"
                );
            }
        }
    }
}
