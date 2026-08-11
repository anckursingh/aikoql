//! Ontology — semantic model above the physical schema (MRFC-0041).
//!
//! An Ontology defines classes (with single inheritance), typed properties,
//! named relationships (domain→range, cardinality), and connector-source
//! mappings that link physical table/collection/label names to ontology classes.
//!
//! Ontologies are stored as regular Knowledge Objects with
//! `metadata.type_name == "Ontology"`.  The definition is encoded as a nested
//! `Value::Map` in the KO's `properties` block — hand-rolled to keep the
//! kernel free of serde_json.
//!
//! This module is std-only and free of I/O (same policy as `kom`).

use crate::knowledge::kom::*;
use std::collections::BTreeMap;

/// Type name used for ontology Knowledge Objects.
pub const ONTOLOGY_TYPE: &str = "Ontology";

// ---------------------------------------------------------------------------
// Ontology definition types
// ---------------------------------------------------------------------------

/// Cardinality of an ontology relationship.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cardinality {
    OneToOne,
    OneToMany,
    ManyToMany,
}

impl Cardinality {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "1:1" => Some(Cardinality::OneToOne),
            "1:N" => Some(Cardinality::OneToMany),
            "N:M" => Some(Cardinality::ManyToMany),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Cardinality::OneToOne => "1:1",
            Cardinality::OneToMany => "1:N",
            Cardinality::ManyToMany => "N:M",
        }
    }
}

/// A class in the ontology hierarchy.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassDef {
    pub name: String,
    /// Parent class for single inheritance (e.g. `Employee` → parent = `Person`).
    pub parent: Option<String>,
    pub description: Option<String>,
}

/// A named relationship between two ontology classes.
#[derive(Clone, Debug, PartialEq)]
pub struct RelDef {
    pub name: String,
    /// Source class (domain).
    pub domain: Option<String>,
    /// Target class (range).
    pub range: Option<String>,
    pub cardinality: Option<Cardinality>,
    /// Maximum outbound relationships allowed from source (MRFC-0060 Phase C3).
    /// `None` = unlimited. Enforced alongside cardinality at write time.
    pub max_count: Option<u32>,
}

/// A typed property definition within an ontology class.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyDef {
    pub name: String,
    /// aikoql value type: "Text", "Int", "Float", "Bool", "DateTime", "Json".
    pub value_type: String,
    pub required: bool,
    /// When `true`, a Null value is accepted for this property.
    /// When `false`, any write must provide a non-Null value of `value_type`.
    pub nullable: bool,
}

/// Maps a physical data source element to an ontology class with property
/// renaming rules.
#[derive(Clone, Debug, PartialEq)]
pub struct MappingEntry {
    /// Source identifier: `"postgres"`, `"neo4j"`, `"mongodb"`, `"sqlite"`.
    pub source: String,
    /// Physical type name in the source: PG table, Neo4j label, Mongo collection.
    pub physical_type: String,
    /// Target ontology class.
    pub class: String,
    /// Physical property name → canonical ontology property name.
    /// Keys not listed here are passed through unchanged.
    pub property_map: BTreeMap<String, String>,
}

/// A complete ontology definition — the typed in-memory representation of an
/// Ontology Knowledge Object.
#[derive(Clone, Debug, PartialEq)]
pub struct OntologyDef {
    pub namespace: String,
    pub version: String,
    pub classes: BTreeMap<String, ClassDef>,
    pub relationships: BTreeMap<String, RelDef>,
    pub property_defs: BTreeMap<String, PropertyDef>,
    pub mappings: Vec<MappingEntry>,
}

// ---------------------------------------------------------------------------
// Value::Map encoding — hand-rolled, no serde_json
// ---------------------------------------------------------------------------

fn map_get<'a>(m: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a Value> {
    m.get(key)
}

fn map_get_str<'a>(m: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    match m.get(key)? {
        Value::Text(s) => Some(s.as_str()),
        _ => None,
    }
}

fn map_get_bool(m: &BTreeMap<String, Value>, key: &str) -> Option<bool> {
    match m.get(key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

impl OntologyDef {
    /// Encode this ontology definition into a `PropertyMap` suitable for
    /// storage in a Knowledge Object's `properties` block.
    pub fn to_property_map(&self) -> PropertyMap {
        let mut m = PropertyMap::new();

        m.insert("namespace".into(), Value::Text(self.namespace.clone()));
        m.insert("version".into(), Value::Text(self.version.clone()));

        // Classes
        let mut classes_list = Vec::new();
        for (name, cd) in &self.classes {
            let mut cm = BTreeMap::new();
            cm.insert("name".into(), Value::Text(name.clone()));
            if let Some(ref parent) = cd.parent {
                cm.insert("parent".into(), Value::Text(parent.clone()));
            }
            if let Some(ref desc) = cd.description {
                cm.insert("description".into(), Value::Text(desc.clone()));
            }
            classes_list.push(Value::Map(cm));
        }
        m.insert("classes".into(), Value::List(classes_list));

        // Relationships
        let mut rels_list = Vec::new();
        for (name, rd) in &self.relationships {
            let mut rm = BTreeMap::new();
            rm.insert("name".into(), Value::Text(name.clone()));
            if let Some(ref domain) = rd.domain {
                rm.insert("domain".into(), Value::Text(domain.clone()));
            }
            if let Some(ref range) = rd.range {
                rm.insert("range".into(), Value::Text(range.clone()));
            }
            if let Some(ref card) = rd.cardinality {
                rm.insert("cardinality".into(), Value::Text(card.as_str().into()));
            }
            if let Some(max) = rd.max_count {
                rm.insert("max_count".into(), Value::Int(max as i64));
            }
            rels_list.push(Value::Map(rm));
        }
        m.insert("relationships".into(), Value::List(rels_list));

        // Property defs
        let mut props_list = Vec::new();
        for (name, pd) in &self.property_defs {
            let mut pm = BTreeMap::new();
            pm.insert("name".into(), Value::Text(name.clone()));
            pm.insert("type".into(), Value::Text(pd.value_type.clone()));
            pm.insert("required".into(), Value::Bool(pd.required));
            props_list.push(Value::Map(pm));
        }
        m.insert("property_defs".into(), Value::List(props_list));

        // Mappings
        let mut map_list = Vec::new();
        for me in &self.mappings {
            let mut mm = BTreeMap::new();
            mm.insert("source".into(), Value::Text(me.source.clone()));
            mm.insert(
                "physical_type".into(),
                Value::Text(me.physical_type.clone()),
            );
            mm.insert("class".into(), Value::Text(me.class.clone()));
            let mut pm_map = BTreeMap::new();
            for (k, v) in &me.property_map {
                pm_map.insert(k.clone(), Value::Text(v.clone()));
            }
            mm.insert("property_map".into(), Value::Map(pm_map));
            map_list.push(Value::Map(mm));
        }
        m.insert("mappings".into(), Value::List(map_list));

        m
    }

    /// Decode an OntologyDef from a Knowledge Object's properties block.
    pub fn from_property_map(props: &PropertyMap) -> KResult<OntologyDef> {
        let namespace = map_get_str(props, "namespace")
            .unwrap_or("default")
            .to_string();
        let version = map_get_str(props, "version").unwrap_or("1.0").to_string();

        let mut classes: BTreeMap<String, ClassDef> = BTreeMap::new();
        if let Some(Value::List(cl)) = map_get(props, "classes") {
            for item in cl {
                if let Value::Map(cm) = item {
                    let name = map_get_str(cm, "name")
                        .ok_or_else(|| KError::InvalidObject("ontology class missing name".into()))?
                        .to_string();
                    let parent = map_get_str(cm, "parent").map(String::from);
                    let description = map_get_str(cm, "description").map(String::from);
                    classes.insert(
                        name.clone(),
                        ClassDef {
                            name,
                            parent,
                            description,
                        },
                    );
                }
            }
        }

        let mut relationships: BTreeMap<String, RelDef> = BTreeMap::new();
        if let Some(Value::List(rl)) = map_get(props, "relationships") {
            for item in rl {
                if let Value::Map(rm) = item {
                    let name = map_get_str(rm, "name")
                        .ok_or_else(|| {
                            KError::InvalidObject("ontology relationship missing name".into())
                        })?
                        .to_string();
                    let domain = map_get_str(rm, "domain").map(String::from);
                    let range = map_get_str(rm, "range").map(String::from);
                    let cardinality =
                        map_get_str(rm, "cardinality").and_then(Cardinality::from_str);
                    let max_count = match rm.get("max_count") {
                        Some(Value::Int(n)) if *n > 0 => Some(*n as u32),
                        _ => None,
                    };
                    relationships.insert(
                        name.clone(),
                        RelDef {
                            name,
                            domain,
                            range,
                            cardinality,
                            max_count,
                        },
                    );
                }
            }
        }

        let mut property_defs: BTreeMap<String, PropertyDef> = BTreeMap::new();
        if let Some(Value::List(pl)) = map_get(props, "property_defs") {
            for item in pl {
                if let Value::Map(pm) = item {
                    let name = map_get_str(pm, "name")
                        .ok_or_else(|| {
                            KError::InvalidObject("ontology property_def missing name".into())
                        })?
                        .to_string();
                    let value_type = map_get_str(pm, "type").unwrap_or("Text").to_string();
                    let required = map_get_bool(pm, "required").unwrap_or(false);
                    property_defs.insert(
                        name.clone(),
                        PropertyDef {
                            name,
                            value_type,
                            required,
                            nullable: false,
                        },
                    );
                }
            }
        }

        let mut mappings: Vec<MappingEntry> = Vec::new();
        if let Some(Value::List(ml)) = map_get(props, "mappings") {
            for item in ml {
                if let Value::Map(mm) = item {
                    let source = map_get_str(mm, "source").unwrap_or("unknown").to_string();
                    let physical_type = map_get_str(mm, "physical_type")
                        .unwrap_or("unknown")
                        .to_string();
                    let class = map_get_str(mm, "class").unwrap_or("unknown").to_string();
                    let mut property_map = BTreeMap::new();
                    if let Some(Value::Map(pm)) = map_get(mm, "property_map") {
                        for (k, v) in pm {
                            if let Value::Text(tv) = v {
                                property_map.insert(k.clone(), tv.clone());
                            }
                        }
                    }
                    mappings.push(MappingEntry {
                        source,
                        physical_type,
                        class,
                        property_map,
                    });
                }
            }
        }

        let def = OntologyDef {
            namespace,
            version,
            classes,
            relationships,
            property_defs,
            mappings,
        };
        def.validate()?;
        Ok(def)
    }

    /// Decode from a full Knowledge Object. Validates type_name == "Ontology".
    pub fn from_ko(ko: &KnowledgeObject) -> KResult<OntologyDef> {
        if ko.metadata.type_name != ONTOLOGY_TYPE {
            return Err(KError::InvalidObject(format!(
                "expected type_name '{}', got '{}'",
                ONTOLOGY_TYPE, ko.metadata.type_name
            )));
        }
        Self::from_property_map(&ko.properties)
    }

    /// Validate the ontology definition for internal consistency.
    pub fn validate(&self) -> KResult<()> {
        if self.namespace.trim().is_empty() {
            return Err(KError::InvalidObject(
                "ontology namespace must be non-empty".into(),
            ));
        }
        if self.classes.is_empty() {
            return Err(KError::InvalidObject(
                "ontology must define at least one class".into(),
            ));
        }

        // Every class referenced as a parent must exist.
        for (name, cd) in &self.classes {
            if let Some(ref parent) = cd.parent {
                if !self.classes.contains_key(parent) {
                    return Err(KError::InvalidObject(format!(
                        "ontology class '{}' has unknown parent '{}'",
                        name, parent
                    )));
                }
            }
        }

        // Every relationship's domain and range must reference existing classes.
        for (name, rd) in &self.relationships {
            if let Some(ref domain) = rd.domain {
                if !self.classes.contains_key(domain) {
                    return Err(KError::InvalidObject(format!(
                        "ontology relationship '{}' has unknown domain '{}'",
                        name, domain
                    )));
                }
            }
            if let Some(ref range) = rd.range {
                if !self.classes.contains_key(range) {
                    return Err(KError::InvalidObject(format!(
                        "ontology relationship '{}' has unknown range '{}'",
                        name, range
                    )));
                }
            }
        }

        // Every mapping must reference an existing class.
        for me in &self.mappings {
            if !self.classes.contains_key(&me.class) {
                return Err(KError::InvalidObject(format!(
                    "ontology mapping for '{}' references unknown class '{}'",
                    me.physical_type, me.class
                )));
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// OntologyRegistry — in-memory, mirrors SchemaRegistry
// ---------------------------------------------------------------------------

/// Fast-lookup registry for ontology definitions.
#[derive(Clone, Debug)]
pub struct OntologyRegistry {
    def: OntologyDef,
    /// (source, physical_type) → index into def.mappings
    by_physical: std::collections::HashMap<(String, String), usize>,
}

impl OntologyRegistry {
    /// Create an empty registry with no classes or mappings. Useful as a
    /// default when ontology is not configured.
    pub fn empty() -> Self {
        OntologyRegistry {
            def: OntologyDef {
                namespace: "empty".into(),
                version: "0".into(),
                classes: BTreeMap::new(),
                relationships: BTreeMap::new(),
                property_defs: BTreeMap::new(),
                mappings: Vec::new(),
            },
            by_physical: std::collections::HashMap::new(),
        }
    }

    pub fn new(def: OntologyDef) -> Result<Self, String> {
        def.validate().map_err(|e| e.to_string())?;
        let mut by_physical = std::collections::HashMap::new();
        for (i, me) in def.mappings.iter().enumerate() {
            by_physical.insert((me.source.clone(), me.physical_type.clone()), i);
        }
        Ok(OntologyRegistry { def, by_physical })
    }

    /// Resolve a class by name.
    pub fn resolve_class(&self, name: &str) -> Option<&ClassDef> {
        self.def.classes.get(name)
    }

    /// Check if `a` is a (transitive) subclass of `b`.
    pub fn is_subclass_of(&self, a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        let mut current = a;
        // Guard against cycles (shouldn't happen after validate, but be safe).
        let mut seen = std::collections::HashSet::new();
        seen.insert(current);
        while let Some(cd) = self.def.classes.get(current) {
            if let Some(ref parent) = cd.parent {
                if parent == b {
                    return true;
                }
                if !seen.insert(parent.as_str()) {
                    return false; // cycle detected
                }
                current = parent;
            } else {
                break;
            }
        }
        false
    }

    /// Get all property definitions.
    /// ponytail: MVP returns all property_defs globally. Per-class property
    /// assignment and type-level scoping land when Schema gains property_types.
    pub fn class_properties(&self, _class_name: &str) -> Vec<&PropertyDef> {
        self.def.property_defs.values().collect()
    }

    /// Resolve a relationship by domain class and relationship name.
    /// Checks the exact domain class and its superclasses.
    pub fn resolve_relationship(&self, domain: &str, rel_name: &str) -> Option<&RelDef> {
        self.def.relationships.get(rel_name).and_then(|rd| {
            match &rd.domain {
                Some(d) if d == domain => Some(rd),
                Some(d) if self.is_subclass_of(domain, d) => Some(rd),
                Some(_) => None,
                None => Some(rd), // unconstrained domain matches anything
            }
        })
    }

    /// Map an external source+type to its ontology MappingEntry.
    pub fn map_external(&self, source: &str, physical_type: &str) -> Option<&MappingEntry> {
        let idx = self
            .by_physical
            .get(&(source.to_string(), physical_type.to_string()))?;
        self.def.mappings.get(*idx)
    }

    /// Find the ontology class for a physical type name, regardless of source.
    pub fn class_for_physical(&self, physical_type: &str) -> Option<&str> {
        self.def
            .mappings
            .iter()
            .find(|me| me.physical_type == physical_type)
            .map(|me| me.class.as_str())
    }

    /// Get all (source, physical_type) pairs that map to the given class,
    /// its superclasses (inherited), or its subclasses.
    pub fn physical_types_for_class(&self, class: &str) -> Vec<(String, String)> {
        self.def
            .mappings
            .iter()
            .filter(|me| {
                me.class == class
                    || self.is_subclass_of(class, &me.class)  // query class IS_A mapping class (inherited up)
                    || self.is_subclass_of(&me.class, class) // mapping class IS_A query class (subclasses)
            })
            .map(|me| (me.source.clone(), me.physical_type.clone()))
            .collect()
    }

    /// Access the underlying ontology definition.
    pub fn definition(&self) -> &OntologyDef {
        &self.def
    }

    /// Register (replace) from a KO. The KO must have type_name == "Ontology".
    pub fn register_from_ko(&mut self, ko: &KnowledgeObject) -> KResult<&OntologyDef> {
        let new_def = OntologyDef::from_ko(ko)?;
        *self = OntologyRegistry::new(new_def)
            .map_err(|e| KError::InvalidObject(format!("ontology reload failed: {}", e)))?;
        Ok(&self.def)
    }
}

/// Discover an ontology from a set of Knowledge Objects.
///
/// Groups KOs by type_name, infers property names and value types, detects
/// relationships from RelationshipRefs, and generates an OntologyDef with
/// mappings for every non-system type.
///
/// Source is detected from KO tags (`source:postgres`, `source:neo4j`, etc.).
/// If no source tag is found, falls back to `"native"`.
///
/// Skip built-in types: "Ontology", "aikoql:*".
/// Idempotent: running multiple times (e.g. after adding more connectors)
/// produces a cumulative ontology including all types from all sources.
pub fn discover_ontology(kos: &[KnowledgeObject]) -> OntologyDef {
    use std::collections::{BTreeMap, BTreeSet};

    let mut type_props: BTreeMap<String, (String, BTreeMap<String, String>)> = BTreeMap::new(); // type → (source, prop → value_type)
    let mut rel_pairs: BTreeSet<(String, String)> = BTreeSet::new(); // (source_type, rel_type)

    for ko in kos {
        if ko.metadata.type_name == ONTOLOGY_TYPE || ko.metadata.type_name.starts_with("aikoql:")
        {
            continue;
        }
        // Detect source from tags: "source:postgres", "source:neo4j", "source:mongodb", "source:sqlite"
        let source = ko
            .metadata
            .tags
            .iter()
            .find(|t| t.starts_with("source:"))
            .map(|t| t.trim_start_matches("source:").to_string())
            .unwrap_or_else(|| "native".to_string());

        let entry = &mut type_props
            .entry(ko.metadata.type_name.clone())
            .or_insert_with(|| (source, BTreeMap::new()))
            .1;
        for (k, v) in &ko.properties {
            let vt = match v {
                Value::Null => "Text",
                Value::Bool(_) => "Bool",
                Value::Int(_) => "Int",
                Value::Float(_) => "Float",
                Value::Text(_) => "Text",
                Value::Bytes(_) => "Text",
                Value::List(_) => "Json",
                Value::Map(_) => "Json",
            };
            entry.entry(k.clone()).or_insert(vt.to_string());
        }
        for rel in &ko.relationships {
            rel_pairs.insert((ko.metadata.type_name.clone(), rel.rel_type.clone()));
        }
    }

    let mut classes: BTreeMap<String, ClassDef> = BTreeMap::new();
    let mut property_defs: BTreeMap<String, PropertyDef> = BTreeMap::new();
    let mut mappings: Vec<MappingEntry> = Vec::new();

    for (type_name, (source, props)) in &type_props {
        classes.insert(
            type_name.clone(),
            ClassDef {
                name: type_name.clone(),
                parent: None,
                description: Some(format!(
                    "Auto-discovered from {} (source: {})",
                    type_name, source
                )),
            },
        );
        for (prop_name, vt) in props {
            property_defs
                .entry(prop_name.clone())
                .or_insert_with(|| PropertyDef {
                    name: prop_name.clone(),
                    value_type: vt.clone(),
                    required: false,
                    nullable: false,
                });
        }
        mappings.push(MappingEntry {
            source: source.clone(),
            physical_type: type_name.clone(),
            class: type_name.clone(),
            property_map: BTreeMap::new(),
        });
    }

    let mut relationships: BTreeMap<String, RelDef> = BTreeMap::new();
    for (src_type, rel_name) in &rel_pairs {
        relationships
            .entry(rel_name.clone())
            .or_insert_with(|| RelDef {
                name: rel_name.clone(),
                domain: Some(src_type.clone()),
                range: None,
                cardinality: Some(Cardinality::OneToMany),
                max_count: None,
            });
    }

    OntologyDef {
        namespace: "discovered".into(),
        version: "1.0".into(),
        classes,
        relationships,
        property_defs,
        mappings,
    }
}

/// Rename physical properties to canonical ontology names per MappingEntry.
/// Tags the KO with `class:<class>`.
/// Pure function — no I/O, no side effects. Idempotent (rename only).
pub fn conform(ko: &mut KnowledgeObject, registry: &OntologyRegistry) {
    let type_name = &ko.metadata.type_name;
    // ponytail: scan all mappings to find matching physical_type. O(n).
    // Build a reverse index if called per-KO in hot loops.
    for me in &registry.def.mappings {
        if me.physical_type == *type_name {
            // Tag with the ontology class.
            let tag = format!("class:{}", me.class);
            if !ko.metadata.tags.contains(&tag) {
                ko.metadata.tags.push(tag);
            }
            // Rename properties per the mapping.
            let mut renamed = PropertyMap::new();
            for (k, v) in std::mem::take(&mut ko.properties) {
                let new_key = me.property_map.get(&k).cloned().unwrap_or(k);
                renamed.insert(new_key, v);
            }
            ko.properties = renamed;
            break; // one mapping per physical_type; first wins
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ontology() -> OntologyDef {
        let mut classes = BTreeMap::new();
        classes.insert(
            "Person".into(),
            ClassDef {
                name: "Person".into(),
                parent: None,
                description: Some("A human being".into()),
            },
        );
        classes.insert(
            "Employee".into(),
            ClassDef {
                name: "Employee".into(),
                parent: Some("Person".into()),
                description: Some("A person employed by a company".into()),
            },
        );
        classes.insert(
            "Manager".into(),
            ClassDef {
                name: "Manager".into(),
                parent: Some("Employee".into()),
                description: Some("An employee who manages others".into()),
            },
        );
        classes.insert(
            "Department".into(),
            ClassDef {
                name: "Department".into(),
                parent: None,
                description: Some("An organizational unit".into()),
            },
        );

        let mut relationships = BTreeMap::new();
        relationships.insert(
            "belongsTo".into(),
            RelDef {
                name: "belongsTo".into(),
                domain: Some("Employee".into()),
                range: Some("Department".into()),
                cardinality: Some(Cardinality::OneToMany),
                max_count: None,
            },
        );

        let mut property_defs = BTreeMap::new();
        property_defs.insert(
            "id".into(),
            PropertyDef {
                name: "id".into(),
                value_type: "Text".into(),
                required: true,
                nullable: false,
            },
        );
        property_defs.insert(
            "dept".into(),
            PropertyDef {
                name: "dept".into(),
                value_type: "Text".into(),
                required: false,
                nullable: false,
            },
        );

        let mut property_map = BTreeMap::new();
        property_map.insert("employee_id".into(), "id".into());
        property_map.insert("department_id".into(), "dept".into());

        let mappings = vec![
            MappingEntry {
                source: "postgres".into(),
                physical_type: "employees".into(),
                class: "Employee".into(),
                property_map: property_map.clone(),
            },
            MappingEntry {
                source: "mongodb".into(),
                physical_type: "employee".into(),
                class: "Employee".into(),
                property_map: property_map.clone(),
            },
        ];

        OntologyDef {
            namespace: "enterprise".into(),
            version: "1.0".into(),
            classes,
            relationships,
            property_defs,
            mappings,
        }
    }

    #[test]
    fn round_trip_through_property_map() {
        let def = sample_ontology();
        let props = def.to_property_map();
        let def2 = OntologyDef::from_property_map(&props).unwrap();
        assert_eq!(def, def2);
    }

    #[test]
    fn round_trip_through_ko() {
        let def = sample_ontology();
        let props = def.to_property_map();
        let ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: ONTOLOGY_TYPE.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "system".into(),
                acl: vec![],
                classification: None,
            },
        );
        let mut ko = ko;
        ko.properties = props.clone();
        let def2 = OntologyDef::from_ko(&ko).unwrap();
        assert_eq!(def, def2);
    }

    #[test]
    fn max_count_round_trips_through_property_map() {
        // Prove max_count survives serialization round-trip.
        let mut classes = std::collections::BTreeMap::new();
        classes.insert(
            "A".into(),
            ClassDef {
                name: "A".into(),
                parent: None,
                description: None,
            },
        );
        let mut relationships = std::collections::BTreeMap::new();
        relationships.insert(
            "r".into(),
            RelDef {
                name: "r".into(),
                domain: None,
                range: None,
                cardinality: Some(Cardinality::ManyToMany),
                max_count: Some(5),
            },
        );
        let def = OntologyDef {
            namespace: "test".into(),
            version: "1".into(),
            classes,
            relationships,
            property_defs: std::collections::BTreeMap::new(),
            mappings: vec![],
        };
        let props = def.to_property_map();
        let def2 = OntologyDef::from_property_map(&props).unwrap();
        assert_eq!(def, def2);
    }

    #[test]
    fn rejects_wrong_type_name() {
        let def = sample_ontology();
        let props = def.to_property_map();
        let ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "NotOntology".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "system".into(),
                acl: vec![],
                classification: None,
            },
        );
        let mut ko = ko;
        ko.properties = props;
        assert!(OntologyDef::from_ko(&ko).is_err());
    }

    #[test]
    fn validate_rejects_unknown_parent() {
        let mut def = sample_ontology();
        def.classes.get_mut("Manager").unwrap().parent = Some("NonExistent".into());
        assert!(def.validate().is_err());
    }

    #[test]
    fn validate_rejects_unknown_relationship_domain() {
        let mut def = sample_ontology();
        def.relationships.get_mut("belongsTo").unwrap().domain = Some("NonExistent".into());
        assert!(def.validate().is_err());
    }

    #[test]
    fn validate_rejects_mapping_unknown_class() {
        let mut def = sample_ontology();
        def.mappings[0].class = "NonExistent".into();
        assert!(def.validate().is_err());
    }

    #[test]
    fn round_trip_through_binary_codec() {
        // Prove the ontology survives the binary codec — critical for persistence.
        let def = sample_ontology();
        let ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: ONTOLOGY_TYPE.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "system".into(),
                acl: vec![],
                classification: None,
            },
        );
        let mut ko = ko;
        ko.properties = def.to_property_map();

        let buf = crate::knowledge::codec::encode_ko(&ko);
        let decoded = crate::knowledge::codec::decode_ko(&buf).unwrap();

        assert_eq!(decoded.metadata.type_name, ONTOLOGY_TYPE);
        let def2 = OntologyDef::from_ko(&decoded).unwrap();
        assert_eq!(def, def2);
    }

    // ------------------------------------------------------------------
    // OntologyRegistry tests
    // ------------------------------------------------------------------

    fn sample_registry() -> OntologyRegistry {
        OntologyRegistry::new(sample_ontology()).unwrap()
    }

    #[test]
    fn registry_resolve_class() {
        let r = sample_registry();
        assert!(r.resolve_class("Person").is_some());
        assert!(r.resolve_class("NonExistent").is_none());
    }

    #[test]
    fn registry_inheritance_chain() {
        let r = sample_registry();
        // Manager → Employee → Person
        assert!(r.is_subclass_of("Manager", "Employee"));
        assert!(r.is_subclass_of("Manager", "Person"));
        assert!(r.is_subclass_of("Employee", "Person"));
        assert!(!r.is_subclass_of("Person", "Manager"));
        assert!(!r.is_subclass_of("Department", "Person"));
    }

    #[test]
    fn registry_resolve_relationship() {
        let r = sample_registry();
        // belongsTo: domain=Employee, range=Department
        let rel = r.resolve_relationship("Employee", "belongsTo");
        assert!(rel.is_some());
        assert_eq!(rel.unwrap().range.as_deref(), Some("Department"));

        // Manager is subclass of Employee → should also resolve
        let rel = r.resolve_relationship("Manager", "belongsTo");
        assert!(rel.is_some());

        // Department is NOT a subclass of Employee
        assert!(r.resolve_relationship("Department", "belongsTo").is_none());
    }

    #[test]
    fn registry_map_external() {
        let r = sample_registry();
        let m = r.map_external("postgres", "employees");
        assert!(m.is_some());
        assert_eq!(m.unwrap().class, "Employee");

        let m = r.map_external("mongodb", "employee");
        assert!(m.is_some());
        assert_eq!(m.unwrap().class, "Employee");

        assert!(r.map_external("postgres", "nonexistent").is_none());
    }

    #[test]
    fn registry_physical_types_for_class() {
        let r = sample_registry();
        // Employee has 2 mappings (postgres, mongodb)
        let pts = r.physical_types_for_class("Employee");
        assert_eq!(pts.len(), 2);

        // Person has 0 direct mappings, but Employee extends Person → 2 inherited
        let pts = r.physical_types_for_class("Person");
        assert_eq!(pts.len(), 2);
    }

    #[test]
    fn conform_renames_properties() {
        let r = sample_registry();
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "employees".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "system".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("employee_id".into(), Value::Text("E123".into()));
        ko.properties
            .insert("department_id".into(), Value::Text("D10".into()));
        ko.properties
            .insert("name".into(), Value::Text("Alice".into()));

        conform(&mut ko, &r);

        // employee_id → id, department_id → dept, name unchanged
        assert_eq!(
            ko.properties.get("id").and_then(|v| match v {
                Value::Text(s) => Some(s.as_str()),
                _ => None,
            }),
            Some("E123")
        );
        assert_eq!(
            ko.properties.get("dept").and_then(|v| match v {
                Value::Text(s) => Some(s.as_str()),
                _ => None,
            }),
            Some("D10")
        );
        assert_eq!(
            ko.properties.get("name").and_then(|v| match v {
                Value::Text(s) => Some(s.as_str()),
                _ => None,
            }),
            Some("Alice")
        );
        // Original keys are gone
        assert!(!ko.properties.contains_key("employee_id"));
        assert!(!ko.properties.contains_key("department_id"));
        // Class tag added
        assert!(ko.metadata.tags.contains(&"class:Employee".to_string()));
    }

    #[test]
    fn conform_untouched_for_unmapped_type() {
        let r = sample_registry();
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "some_unknown_table".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "system".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("foo".into(), Value::Text("bar".into()));
        let orig = ko.properties.clone();

        conform(&mut ko, &r);
        assert_eq!(ko.properties, orig); // untouched
    }

    #[test]
    fn conform_idempotent() {
        let r = sample_registry();
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "employees".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "system".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("employee_id".into(), Value::Text("E123".into()));

        conform(&mut ko, &r);
        let after_first = ko.properties.clone();
        let tags_after_first = ko.metadata.tags.clone();

        conform(&mut ko, &r);
        assert_eq!(ko.properties, after_first); // properties unchanged
        assert_eq!(ko.metadata.tags, tags_after_first); // tag not duplicated
    }

    // ------------------------------------------------------------------
    // Edge case tests — comprehensive QA
    // ------------------------------------------------------------------

    #[test]
    fn edge_empty_ontology_rejected_by_validate() {
        // Empty classes should be rejected by validate().
        let def = OntologyDef {
            namespace: "empty".into(),
            version: "0".into(),
            classes: BTreeMap::new(),
            relationships: BTreeMap::new(),
            property_defs: BTreeMap::new(),
            mappings: vec![],
        };
        assert!(def.validate().is_err());
        let props = def.to_property_map();
        assert!(OntologyDef::from_property_map(&props).is_err());
    }

    #[test]
    fn edge_ontology_defaults_missing_fields() {
        // from_property_map should use defaults for missing optional fields.
        let mut classes: BTreeMap<String, ClassDef> = BTreeMap::new();
        classes.insert(
            "A".into(),
            ClassDef {
                name: "A".into(),
                parent: None,
                description: None,
            },
        );
        let mut props = PropertyMap::new();
        props.insert("namespace".into(), Value::Text("test".into()));
        props.insert("version".into(), Value::Text("1".into()));
        props.insert(
            "classes".into(),
            Value::List(vec![Value::Map({
                let mut m = BTreeMap::new();
                m.insert("name".into(), Value::Text("A".into()));
                m
            })]),
        );
        // No relationships, property_defs, mappings — should default to empty.
        let def = OntologyDef::from_property_map(&props).unwrap();
        assert_eq!(def.classes.len(), 1);
        assert_eq!(def.relationships.len(), 0);
        assert_eq!(def.property_defs.len(), 0);
        assert_eq!(def.mappings.len(), 0);
    }

    #[test]
    fn edge_validate_rejects_empty_namespace() {
        let def = OntologyDef {
            namespace: "  ".into(),
            version: "1".into(),
            classes: {
                let mut m = BTreeMap::new();
                m.insert(
                    "A".into(),
                    ClassDef {
                        name: "A".into(),
                        parent: None,
                        description: None,
                    },
                );
                m
            },
            relationships: BTreeMap::new(),
            property_defs: BTreeMap::new(),
            mappings: vec![],
        };
        assert!(def.validate().is_err());
    }

    #[test]
    fn edge_validate_rejects_empty_classes() {
        let def = OntologyDef {
            namespace: "test".into(),
            version: "1".into(),
            classes: BTreeMap::new(),
            relationships: BTreeMap::new(),
            property_defs: BTreeMap::new(),
            mappings: vec![],
        };
        assert!(def.validate().is_err());
    }

    #[test]
    fn edge_validate_rejects_unknown_relationship_range() {
        let mut def = sample_ontology();
        def.relationships.get_mut("belongsTo").unwrap().range = Some("NonExistent".into());
        assert!(def.validate().is_err());
    }

    #[test]
    fn edge_validate_allows_wildcard_domain_and_range() {
        // Relationships with no domain/range should be allowed (wildcard).
        let mut classes = BTreeMap::new();
        classes.insert(
            "Thing".into(),
            ClassDef {
                name: "Thing".into(),
                parent: None,
                description: None,
            },
        );
        let mut rels = BTreeMap::new();
        rels.insert(
            "relatedTo".into(),
            RelDef {
                name: "relatedTo".into(),
                domain: None,
                range: None,
                cardinality: None,
                max_count: None,
            },
        );
        let def = OntologyDef {
            namespace: "test".into(),
            version: "1".into(),
            classes,
            relationships: rels,
            property_defs: BTreeMap::new(),
            mappings: vec![],
        };
        assert!(def.validate().is_ok());
    }

    #[test]
    fn edge_cardinality_all_variants() {
        assert_eq!(Cardinality::from_str("1:1"), Some(Cardinality::OneToOne));
        assert_eq!(Cardinality::from_str("1:N"), Some(Cardinality::OneToMany));
        assert_eq!(Cardinality::from_str("N:M"), Some(Cardinality::ManyToMany));
        assert_eq!(Cardinality::from_str("invalid"), None);
        assert_eq!(Cardinality::OneToOne.as_str(), "1:1");
        assert_eq!(Cardinality::OneToMany.as_str(), "1:N");
        assert_eq!(Cardinality::ManyToMany.as_str(), "N:M");
    }

    #[test]
    fn edge_class_for_physical_multiple_sources() {
        let r = sample_registry();
        // "employees" maps to Employee via postgres source.
        assert_eq!(r.class_for_physical("employees"), Some("Employee"));
        assert_eq!(r.class_for_physical("employee"), Some("Employee"));
        assert_eq!(r.class_for_physical("nonexistent"), None);
    }

    #[test]
    fn edge_is_subclass_of_self() {
        let r = sample_registry();
        assert!(r.is_subclass_of("Person", "Person"));
        assert!(r.is_subclass_of("Employee", "Employee"));
    }

    #[test]
    fn edge_deep_inheritance_multiple_levels() {
        let r = sample_registry();
        // Manager → Employee → Person (3 levels)
        // Person: Employee is subclass → 2 mappings inherited
        let pts = r.physical_types_for_class("Person");
        assert_eq!(pts.len(), 2);
        // Manager IS_A Employee → inherits Employee's 2 mappings
        let pts = r.physical_types_for_class("Manager");
        assert_eq!(pts.len(), 2);
        // Employee: exact match + subclasses (Manager has no additional mappings)
        let pts = r.physical_types_for_class("Employee");
        assert_eq!(pts.len(), 2);
    }

    #[test]
    fn edge_class_with_no_mappings() {
        let r = sample_registry();
        // Department has no mappings in the fixture.
        let pts = r.physical_types_for_class("Department");
        assert_eq!(pts.len(), 0);
        // But Department should still resolve.
        assert!(r.resolve_class("Department").is_some());
    }

    #[test]
    fn edge_registry_empty_always_returns_none() {
        let r = OntologyRegistry::empty();
        assert!(r.resolve_class("Anything").is_none());
        assert!(r.class_for_physical("anything").is_none());
        assert!(r.map_external("any", "thing").is_none());
        assert_eq!(r.physical_types_for_class("Anything").len(), 0);
        assert!(!r.is_subclass_of("A", "B"));
    }

    #[test]
    fn edge_discover_ontology_empty_input() {
        let def = discover_ontology(&[]);
        assert_eq!(def.classes.len(), 0);
        assert_eq!(def.mappings.len(), 0);
        assert_eq!(def.relationships.len(), 0);
    }

    #[test]
    fn edge_discover_ontology_skips_builtin_types() {
        let mut kos = vec![];
        // Ontology type should be skipped.
        kos.push(KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: ONTOLOGY_TYPE.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        ));
        // aikoql:* types should be skipped.
        kos.push(KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "aikoql:role".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        ));
        // Regular type should be included.
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "Widget".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("color".into(), Value::Text("red".into()));
        kos.push(ko);

        let def = discover_ontology(&kos);
        assert_eq!(def.classes.len(), 1);
        assert!(def.classes.contains_key("Widget"));
    }

    #[test]
    fn edge_discover_ontology_detects_source_from_tags() {
        let mut kos = vec![];
        let mut ko1 = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "t1".into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["source:postgres".into()],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko1.properties.insert("a".into(), Value::Text("x".into()));
        kos.push(ko1);

        let mut ko2 = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "t2".into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["source:mongodb".into()],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko2.properties.insert("b".into(), Value::Int(42));
        kos.push(ko2);

        // No source tag — should fall back to "native".
        let mut ko3 = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "t3".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko3.properties.insert("c".into(), Value::Float(3.14));
        kos.push(ko3);

        let def = discover_ontology(&kos);
        assert_eq!(def.mappings.len(), 3);
        let pg = def
            .mappings
            .iter()
            .find(|m| m.physical_type == "t1")
            .unwrap();
        assert_eq!(pg.source, "postgres");
        let mongo = def
            .mappings
            .iter()
            .find(|m| m.physical_type == "t2")
            .unwrap();
        assert_eq!(mongo.source, "mongodb");
        let native = def
            .mappings
            .iter()
            .find(|m| m.physical_type == "t3")
            .unwrap();
        assert_eq!(native.source, "native");
    }

    #[test]
    fn edge_discover_ontology_infers_property_types() {
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "Test".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties.insert("bool_prop".into(), Value::Bool(true));
        ko.properties.insert("int_prop".into(), Value::Int(42));
        ko.properties
            .insert("float_prop".into(), Value::Float(3.14));
        ko.properties
            .insert("text_prop".into(), Value::Text("hello".into()));
        ko.properties.insert("null_prop".into(), Value::Null);
        ko.properties
            .insert("list_prop".into(), Value::List(vec![]));
        ko.properties
            .insert("map_prop".into(), Value::Map(BTreeMap::new()));

        let def = discover_ontology(&[ko]);
        let pd = &def.property_defs;
        assert_eq!(pd.get("bool_prop").unwrap().value_type, "Bool");
        assert_eq!(pd.get("int_prop").unwrap().value_type, "Int");
        assert_eq!(pd.get("float_prop").unwrap().value_type, "Float");
        assert_eq!(pd.get("text_prop").unwrap().value_type, "Text");
        assert_eq!(pd.get("null_prop").unwrap().value_type, "Text");
        assert_eq!(pd.get("list_prop").unwrap().value_type, "Json");
        assert_eq!(pd.get("map_prop").unwrap().value_type, "Json");
    }

    #[test]
    fn edge_discover_ontology_detects_relationships() {
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "Employee".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.relationships.push(RelationshipRef {
            rel_type: "worksFor".into(),
            target: KOID::ZERO,
            direction: crate::knowledge::kom::Direction::Outbound,
        });
        ko.relationships.push(RelationshipRef {
            rel_type: "manages".into(),
            target: KOID::ZERO,
            direction: crate::knowledge::kom::Direction::Outbound,
        });
        ko.properties
            .insert("name".into(), Value::Text("Alice".into()));

        let def = discover_ontology(&[ko]);
        assert!(def.relationships.contains_key("worksFor"));
        assert!(def.relationships.contains_key("manages"));
        let works_for = def.relationships.get("worksFor").unwrap();
        assert_eq!(works_for.domain.as_deref(), Some("Employee"));
    }

    #[test]
    fn edge_discover_ontology_cumulative_across_sources() {
        // Simulate PG import then Neo4j import — should be cumulative.
        let mut ko_pg = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "pg_emp".into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["source:postgres".into()],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko_pg
            .properties
            .insert("emp_id".into(), Value::Text("E1".into()));

        let mut ko_neo = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "neo_emp".into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["source:neo4j".into()],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko_neo
            .properties
            .insert("employeeId".into(), Value::Text("N1".into()));
        ko_neo.relationships.push(RelationshipRef {
            rel_type: "WORKS_FOR".into(),
            target: KOID::ZERO,
            direction: crate::knowledge::kom::Direction::Outbound,
        });

        // First discovery (PG only)
        let def1 = discover_ontology(&[ko_pg.clone()]);
        assert_eq!(def1.classes.len(), 1);
        assert_eq!(def1.mappings[0].source, "postgres");

        // Second discovery (PG + Neo4j)
        let def2 = discover_ontology(&[ko_pg, ko_neo]);
        assert_eq!(def2.classes.len(), 2);
        assert_eq!(def2.relationships.len(), 1);
        let sources: Vec<&str> = def2.mappings.iter().map(|m| m.source.as_str()).collect();
        assert!(sources.contains(&"postgres"));
        assert!(sources.contains(&"neo4j"));
    }

    #[test]
    fn edge_conform_preserves_unmapped_properties() {
        let r = sample_registry();
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "employees".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("employee_id".into(), Value::Text("E1".into()));
        ko.properties
            .insert("extra_field".into(), Value::Text("keep_me".into())); // no mapping
        ko.properties
            .insert("department_id".into(), Value::Text("D1".into()));

        conform(&mut ko, &r);

        // employee_id → id, department_id → dept, extra_field → extra_field (preserved)
        assert_eq!(ko.properties.get("id").and_then(text_val), Some("E1"));
        assert_eq!(ko.properties.get("dept").and_then(text_val), Some("D1"));
        assert_eq!(
            ko.properties.get("extra_field").and_then(text_val),
            Some("keep_me")
        );
    }

    #[test]
    fn edge_round_trip_all_cardinality_values() {
        let def = sample_ontology();
        let props = def.to_property_map();
        let def2 = OntologyDef::from_property_map(&props).unwrap();
        let rel = def2.relationships.get("belongsTo").unwrap();
        assert_eq!(rel.cardinality, Some(Cardinality::OneToMany));
    }

    #[test]
    fn edge_round_trip_optional_description_fields() {
        // ClassDef with and without description.
        let mut classes = BTreeMap::new();
        classes.insert(
            "A".into(),
            ClassDef {
                name: "A".into(),
                parent: None,
                description: Some("has desc".into()),
            },
        );
        classes.insert(
            "B".into(),
            ClassDef {
                name: "B".into(),
                parent: Some("A".into()),
                description: None,
            },
        );

        let def = OntologyDef {
            namespace: "t".into(),
            version: "1".into(),
            classes,
            relationships: BTreeMap::new(),
            property_defs: BTreeMap::new(),
            mappings: vec![],
        };
        let props = def.to_property_map();
        let def2 = OntologyDef::from_property_map(&props).unwrap();
        assert_eq!(
            def2.classes.get("A").unwrap().description.as_deref(),
            Some("has desc")
        );
        assert_eq!(def2.classes.get("B").unwrap().description, None);
    }

    #[test]
    fn edge_property_def_without_type_defaults_to_text() {
        let def = sample_ontology();
        let mut props = def.to_property_map();
        // Manually remove the "type" field from one property_def.
        if let Value::List(ref mut pl) = props.get_mut("property_defs").unwrap() {
            if let Value::Map(ref mut pm) = pl[0] {
                pm.remove("type");
            }
        }
        let def2 = OntologyDef::from_property_map(&props).unwrap();
        // Should default to "Text"
        let pd = def2.property_defs.values().next().unwrap();
        assert_eq!(pd.value_type, "Text");
    }

    #[test]
    fn edge_conform_empty_property_map_no_rename() {
        let r = OntologyRegistry::new(OntologyDef {
            namespace: "t".into(),
            version: "1".into(),
            classes: {
                let mut m = BTreeMap::new();
                m.insert(
                    "E".into(),
                    ClassDef {
                        name: "E".into(),
                        parent: None,
                        description: None,
                    },
                );
                m
            },
            relationships: BTreeMap::new(),
            property_defs: BTreeMap::new(),
            mappings: vec![MappingEntry {
                source: "pg".into(),
                physical_type: "emp".into(),
                class: "E".into(),
                property_map: BTreeMap::new(),
            }],
        })
        .unwrap();

        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "emp".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "s".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("foo".into(), Value::Text("bar".into()));
        let orig = ko.properties.clone();

        conform(&mut ko, &r);
        // No rename — empty property_map. But class tag should still be added.
        assert_eq!(ko.properties, orig);
        assert!(ko.metadata.tags.contains(&"class:E".to_string()));
    }

    fn text_val(v: &Value) -> Option<&str> {
        match v {
            Value::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }
}
