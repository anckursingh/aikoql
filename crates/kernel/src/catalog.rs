//! P5-M7 (ND-09) — the database catalog: the database's own metadata,
//! stored as ordinary journaled KOs in the SAME engine (no second store).
//!
//! Every catalog row is a KnowledgeObject of the reserved type
//! `aikoql:catalog`, in the reserved tenant `aikoql:catalog`, owned by the
//! system principal `aikoql:system`. The default owner-only ACL is the
//! invisibility barrier: no normal subject can read or write catalog rows.
//! The kernel `list_types()` filters the reserved type out, so catalog rows
//! never appear as user types.
//!
//! Row shape: `kind` (Text — "type", "index", …, "version"),
//! `name` (Text, unique per kind), plus kind-specific properties
//! (a type row carries `properties: List(Text)`).
//!
//! One version row (kind "version", name "catalog", `version: Int`) drives
//! the migration decision at open ([`ensure`]): absent → bootstrap at the
//! current version; below → deterministic vN→vN+1 dispatch then stamp;
//! above or corrupt → the open fails closed. The first real migration step
//! ships when a v2 schema change lands — the dispatch loop is the seam
//! (P5-M7 honest-ledger row).

use crate::index::property::PropertyIndex;
use crate::index::unified::Index;
use crate::knowledge::kom::{KError, KResult, KnowledgeObject, PropertyMap, Value, KOID};
use crate::transaction::kernel::{ForgetMode, Kernel, KnowledgeContext, RememberRequest, Subject};
use crate::Metadata;
use std::sync::Arc;

pub const CATALOG_TYPE: &str = "aikoql:catalog";
pub const CATALOG_TENANT: &str = "aikoql:catalog";
pub const SYSTEM_PRINCIPAL: &str = "aikoql:system";
pub const SUPPORTED_VERSION: i64 = 1;

/// Catalog rows are the database's own metadata — every consumer that
/// separates user data from system data uses this one predicate.
pub fn is_catalog_type(type_name: &str) -> bool {
    type_name.starts_with(CATALOG_TYPE)
}

pub fn ensure(k: &Kernel) -> KResult<()> {
    match k.version_rows()?.into_iter().max() {
        // A pre-catalog database: bootstrap at the current version.
        None => k.write_catalog_row(version_props(SUPPORTED_VERSION))?,
        Some(v) if v < SUPPORTED_VERSION => migrate_and_stamp(k, v)?,
        Some(v) if v > SUPPORTED_VERSION => {
            return Err(KError::Store(format!(
                "catalog version {v} unsupported: this build supports up to {SUPPORTED_VERSION}"
            )))
        }
        Some(_) => {} // current — nothing to do
    }
    Ok(())
}

/// Deterministic vN→vN+1 dispatch, then stamp the version row in place.
/// The loop is the seam; the first real step ships when a v2 schema change
/// lands. Stamping keeps ensure convergent: a DB stamped below the current
/// version upgrades in one open, never re-runs on the next.
fn migrate_and_stamp(k: &Kernel, from: i64) -> KResult<()> {
    let mut v = from;
    while v < SUPPORTED_VERSION {
        // vN → vN+1 steps land here (the dispatch seam); v0 → v1 was only
        // the version row itself, so there is nothing to run yet.
        v += 1;
    }
    k.stamp_version(SUPPORTED_VERSION)
}

fn version_props(version: i64) -> PropertyMap {
    let mut props = PropertyMap::new();
    props.insert("kind".into(), Value::Text("version".into()));
    props.insert("name".into(), Value::Text("catalog".into()));
    props.insert("version".into(), Value::Int(version));
    props
}

fn catalog_metadata() -> Metadata {
    Metadata {
        type_name: CATALOG_TYPE.into(),
        tenant: Some(CATALOG_TENANT.into()),
        schema_version: 1,
        tags: vec![],
    }
}

fn system_ctx() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new(SYSTEM_PRINCIPAL).in_tenant(CATALOG_TENANT))
}

impl Kernel {
    // ---- generic entry API --------------------------------------------------

    /// Create a catalog entry. Fails closed on a duplicate (kind, name).
    pub fn catalog_create_entry(
        &self,
        kind: &str,
        name: &str,
        properties: PropertyMap,
    ) -> KResult<()> {
        if self.catalog_entry(kind, name)?.is_some() {
            return Err(KError::InvalidObject(format!(
                "catalog entry '{kind}' '{name}' already exists"
            )));
        }
        let mut props = PropertyMap::new();
        props.insert("kind".into(), Value::Text(kind.into()));
        props.insert("name".into(), Value::Text(name.into()));
        props.extend(properties);
        self.write_catalog_row(props)
    }

    /// Read a catalog entry's payload properties (`kind`/`name` are the
    /// addressing keys — not returned).
    pub fn catalog_entry(&self, kind: &str, name: &str) -> KResult<Option<PropertyMap>> {
        Ok(self.catalog_entry_object(kind, name)?.map(|ko| {
            let mut props = ko.properties;
            props.remove("kind");
            props.remove("name");
            props
        }))
    }

    /// Tombstone a catalog entry. Fails closed on an unknown (kind, name).
    pub fn catalog_drop_entry(&self, kind: &str, name: &str) -> KResult<()> {
        let Some(ko) = self.catalog_entry_object(kind, name)? else {
            return Err(KError::InvalidObject(format!(
                "catalog entry '{kind}' '{name}' not found"
            )));
        };
        self.forget(
            Subject::new(SYSTEM_PRINCIPAL),
            &ko.koid,
            ForgetMode::Tombstone,
            None,
            None,
        )
        .map(|_| ())
    }

    // ---- index sugar (P5-M8) ----------------------------------------------------

    /// Register a property/composite index in the catalog AND the live
    /// registry. Fails closed on a duplicate name (idx2-001).
    pub fn catalog_create_index(
        &self,
        name: &str,
        type_name: &str,
        properties: &[&str],
    ) -> KResult<()> {
        let mut props = PropertyMap::new();
        props.insert("type_name".into(), Value::Text(type_name.into()));
        props.insert(
            "properties".into(),
            Value::List(
                properties
                    .iter()
                    .map(|p| Value::Text(p.to_string()))
                    .collect(),
            ),
        );
        self.catalog_create_entry("index", name, props)?;
        // justified: RwLock poison is unrecoverable
        self.property_indexes
            .write()
            .unwrap()
            .push(Arc::new(PropertyIndex::new(name, type_name, properties)));
        // P5-M17b: the synchronous make-good — a just-declared index is
        // seeded from the committed heads before analyze() prices it
        // (idx2-008's manual catch-up is the declaration's own duty now).
        self.rebuild_index(name)
    }

    /// Drop a catalog index: the row is tombstoned and the live registry
    /// entry removed (idx2-002).
    pub fn catalog_drop_index(&self, name: &str) -> KResult<()> {
        self.catalog_drop_entry("index", name)?;
        // P5-M22 (idx4-004) test hook: park between the durable drop and the
        // registry removal — the window a concurrent CBO could still see the
        // index. (The RED ships the hook; the feat gates the window.)
        if std::env::var_os("INDEX_DROP_PARK").is_some() {
            std::env::set_var("INDEX_DROP_PARK_AT", "1");
            let mut waited = 0u64;
            while std::env::var_os("INDEX_DROP_PARK").is_some() && waited < 30_000 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                waited += 10;
            }
        }
        // justified: RwLock poison is unrecoverable
        self.property_indexes
            .write()
            .unwrap()
            .retain(|i| i.name() != name);
        Ok(())
    }

    /// Every catalog-registered index declaration (idx2-001). Corrupt payloads
    /// fail closed.
    pub fn catalog_list_indexes(&self) -> KResult<Vec<IndexDecl>> {
        let mut out = Vec::new();
        for ko in self.scan_catalog_rows()? {
            if ko.properties.get("kind") != Some(&Value::Text("index".into())) {
                continue;
            }
            let Some(Value::Text(name)) = ko.properties.get("name") else {
                return Err(KError::Store(
                    "catalog index row corrupt: missing name".into(),
                ));
            };
            out.push(parse_index_decl(name, &ko.properties)?);
        }
        Ok(out)
    }

    /// The live property-index registry. Contents are never persisted — they
    /// replay through the maintainer (idx2-007).
    pub fn property_indexes(&self) -> KResult<Vec<Arc<dyn Index>>> {
        // justified: RwLock poison is unrecoverable
        Ok(self.property_indexes.read().unwrap().clone())
    }

    /// Rebuild a registered property index from the committed heads —
    /// the synchronous make-good behind a declaration (P5-M17b). A
    /// lagging maintainer may still transiently re-apply older versions
    /// after the rebuild; `wait_caught_up` before `analyze` settles that
    /// (the M17b contract), and the verify gate covers any residual window.
    /// Fails closed on an unknown name.
    pub fn rebuild_index(&self, name: &str) -> KResult<()> {
        let reg = self.property_indexes()?;
        let idx = reg
            .iter()
            .find(|i| i.name() == name)
            .ok_or_else(|| KError::InvalidObject(format!("index '{name}' not found")))?;
        idx.rebuild(self)
    }

    /// Equality scan on a registered index by name (idx2-composite). Unknown
    /// names and wrong key arities fail closed.
    pub fn scan_index(&self, name: &str, key: &[Value]) -> KResult<Vec<KOID>> {
        // justified: RwLock poison is unrecoverable
        let reg = self.property_indexes.read().unwrap();
        let idx = reg
            .iter()
            .find(|i| i.name() == name)
            .ok_or_else(|| KError::InvalidObject(format!("index '{name}' not found")))?;
        idx.scan_eq(key)
    }

    /// P5-M17b — an index's freshness stamp (the last committed event seq
    /// it has fully applied). The CBO's verify gate short-circuits when
    /// stamp == journal head (idx2-010). Unknown names fail closed.
    pub fn index_applied_seq(&self, name: &str) -> KResult<u64> {
        let reg = self.property_indexes()?;
        let idx = reg
            .iter()
            .find(|i| i.name() == name)
            .ok_or_else(|| KError::InvalidObject(format!("index '{name}' not found")))?;
        Ok(idx.applied_seq())
    }

    // ---- type sugar -----------------------------------------------------------

    pub fn catalog_create_type(&self, name: &str, properties: &[&str]) -> KResult<()> {
        let mut props = PropertyMap::new();
        props.insert(
            "properties".into(),
            Value::List(
                properties
                    .iter()
                    .map(|p| Value::Text(p.to_string()))
                    .collect(),
            ),
        );
        self.catalog_create_entry("type", name, props)
    }

    pub fn catalog_drop_type(&self, name: &str) -> KResult<()> {
        self.catalog_drop_entry("type", name)
    }

    /// The declared property list of a type, in declaration order.
    pub fn catalog_get_type(&self, name: &str) -> KResult<Option<Vec<String>>> {
        let Some(props) = self.catalog_entry("type", name)? else {
            return Ok(None);
        };
        match props.get("properties") {
            Some(Value::List(items)) => {
                let mut out = Vec::new();
                for item in items {
                    match item {
                        Value::Text(t) => out.push(t.clone()),
                        other => {
                            return Err(KError::Store(format!(
                                "catalog type '{name}' corrupt: property {other:?}"
                            )))
                        }
                    }
                }
                Ok(Some(out))
            }
            other => Err(KError::Store(format!(
                "catalog type '{name}' corrupt: properties = {other:?}"
            ))),
        }
    }

    /// Extend a type's declared property list (no-op on a duplicate).
    pub fn catalog_add_property(&self, type_name: &str, property: &str) -> KResult<()> {
        let Some(ko) = self.catalog_entry_object("type", type_name)? else {
            return Err(KError::InvalidObject(format!(
                "catalog type '{type_name}' not found"
            )));
        };
        let mut props = ko.properties.clone();
        match props.get_mut("properties") {
            Some(Value::List(items)) => {
                if !items.iter().any(|v| v == &Value::Text(property.into())) {
                    items.push(Value::Text(property.into()));
                }
            }
            other => {
                return Err(KError::Store(format!(
                    "catalog type '{type_name}' corrupt: properties = {other:?}"
                )))
            }
        }
        self.update_catalog_row(&ko, props)
    }

    /// All registered type names, sorted.
    pub fn catalog_list_types(&self) -> KResult<Vec<String>> {
        let mut out = Vec::new();
        for ko in self.scan_catalog_rows()? {
            if ko.properties.get("kind") != Some(&Value::Text("type".into())) {
                continue;
            }
            if let Some(Value::Text(name)) = ko.properties.get("name") {
                out.push(name.clone());
            }
        }
        out.sort();
        Ok(out)
    }

    // ---- the version row -------------------------------------------------------

    /// The catalog version — the single migration state of the database.
    pub fn catalog_version(&self) -> KResult<i64> {
        self.version_rows()?
            .into_iter()
            .max()
            .ok_or_else(|| KError::Store("catalog: no version row".into()))
    }

    /// Every version row's value (corrupt = fail closed; empty = pre-catalog
    /// database). Shared by [`catalog_version`] and [`ensure`] so the open
    /// decision and the reported version can never diverge.
    fn version_rows(&self) -> KResult<Vec<i64>> {
        let mut versions = Vec::new();
        for ko in self.scan_catalog_rows()? {
            if ko.properties.get("kind") != Some(&Value::Text("version".into())) {
                continue;
            }
            match ko.properties.get("version") {
                Some(Value::Int(v)) => versions.push(*v),
                other => {
                    return Err(KError::Store(format!(
                        "catalog version row corrupt: version must be an Int, got {other:?}"
                    )))
                }
            }
        }
        Ok(versions)
    }

    // ---- internals ----------------------------------------------------------------

    fn catalog_entry_object(&self, kind: &str, name: &str) -> KResult<Option<KnowledgeObject>> {
        for ko in self.scan_catalog_rows()? {
            if ko.properties.get("kind") == Some(&Value::Text(kind.into()))
                && ko.properties.get("name") == Some(&Value::Text(name.into()))
            {
                return Ok(Some(ko));
            }
        }
        Ok(None)
    }

    fn write_catalog_row(&self, properties: PropertyMap) -> KResult<()> {
        let mut req = RememberRequest::create(system_ctx(), catalog_metadata());
        req.properties = properties;
        self.remember(req)?;
        Ok(())
    }

    fn update_catalog_row(&self, ko: &KnowledgeObject, properties: PropertyMap) -> KResult<()> {
        let mut req = RememberRequest::create(system_ctx(), catalog_metadata());
        req.koid = Some(ko.koid);
        req.expected_version = Some(ko.version);
        req.properties = properties;
        self.remember(req)?;
        Ok(())
    }

    /// Rewrite the version row in place (kind/name/version preserved by the
    /// caller's property set) — the migration stamp.
    fn stamp_version(&self, version: i64) -> KResult<()> {
        let Some(ko) = self.catalog_entry_object("version", "catalog")? else {
            return Err(KError::Store(
                "catalog: version row vanished during migration".into(),
            ));
        };
        self.update_catalog_row(&ko, version_props(version))
    }
}

// ---------------------------------------------------------------------------
// Index declarations (P5-M8) — parsing and the open-time registry load
// ---------------------------------------------------------------------------

/// A catalog-registered index declaration (kind "index").
#[derive(Debug, Clone)]
pub struct IndexDecl {
    pub name: String,
    pub type_name: String,
    pub properties: Vec<String>,
}

/// Parse an index row's payload — corrupt rows fail closed (idx2-010).
fn parse_index_decl(name: &str, props: &PropertyMap) -> KResult<IndexDecl> {
    let Some(Value::Text(type_name)) = props.get("type_name") else {
        return Err(KError::Store(format!(
            "catalog index '{name}' corrupt: missing type_name"
        )));
    };
    let Some(Value::List(items)) = props.get("properties") else {
        return Err(KError::Store(format!(
            "catalog index '{name}' corrupt: properties must be a list"
        )));
    };
    let mut properties = Vec::new();
    for item in items {
        match item {
            Value::Text(t) => properties.push(t.clone()),
            other => {
                return Err(KError::Store(format!(
                    "catalog index '{name}' corrupt: property {other:?}"
                )))
            }
        }
    }
    Ok(IndexDecl {
        name: name.into(),
        type_name: type_name.clone(),
        properties,
    })
}

/// Populate the live registry from the catalog at open. Corrupt index rows
/// fail the open closed (idx2-010); contents start empty and replay through
/// the maintainer (idx2-007).
pub fn load_property_indexes(k: &Kernel) -> KResult<()> {
    let mut indexes: Vec<Arc<dyn Index>> = Vec::new();
    for decl in k.catalog_list_indexes()? {
        let props: Vec<&str> = decl.properties.iter().map(|p| p.as_str()).collect();
        indexes.push(Arc::new(PropertyIndex::new(
            &decl.name,
            &decl.type_name,
            &props,
        )));
    }
    // justified: RwLock poison is unrecoverable
    *k.property_indexes.write().unwrap() = indexes;
    Ok(())
}
