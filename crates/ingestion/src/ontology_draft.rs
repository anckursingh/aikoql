//! Turn discovered source schemas into a publishable ontology definition:
//! one class per container, typed properties, relationships from references,
//! and a `MappingEntry` per physical container so `MATCH <Class>` reaches every
//! mapped source. Same-named classes from different sources are never merged
//! automatically; only an explicit [`ClassMerge`] does that.

use crate::connector_bridge::{
    class_name_for_container, ConnectorMetadata, ContainerInfo, FieldInfo, Provenance,
};
use crate::ontology::{connector_type_to_value_type, to_snake_case};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub struct SourceInput<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub meta: &'a ConnectorMetadata,
}

/// Two `"<source>.<container>"` references a human decided are the same concept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassMerge {
    pub a: String,
    pub b: String,
    /// `(field of a, field of b)` pairs a human decided are the same property:
    /// both map to the canonical name of a's field (`email` / `email_address`
    /// -> `email`). Never inferred here; see `ontology_suggest` for proposals.
    pub properties: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassStatus {
    New,
    PossibleDuplicate { with: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftClass {
    pub name: String,
    /// `"<source>.<container>"` references feeding this class.
    pub sources: Vec<String>,
    pub status: ClassStatus,
}

#[derive(Clone, Debug)]
pub struct DefinitionDraft {
    /// `{namespace, version, classes, relationships, property_defs, mappings}`,
    /// exactly what the ontology save path accepts.
    pub definition: Value,
    pub classes: Vec<DraftClass>,
    pub warnings: Vec<String>,
}

/// One physical container of one source, on its way to a class.
struct Entry<'a> {
    source: &'a str,
    kind: &'a str,
    container: &'a ContainerInfo,
    key: String,
    base: String,
}

fn pascal(s: &str) -> String {
    class_name_for_container(&s.replace(['-', '.'], "_"))
}

pub(crate) fn canonical_property(name: &str) -> String {
    let c = to_snake_case(name);
    if c.is_empty() {
        name.to_string()
    } else {
        c
    }
}

/// Value type of one observed BSON type name, matching what the importer
/// stores (Decimal128 as exact text, a Timestamp as a {t,i} map).
fn observed_value_type(t: &str) -> &'static str {
    match t {
        "Decimal128" | "Timestamp" => "Text",
        // Neo4j's integer type name has no "int" in it
        "Long" => "Int",
        other => connector_type_to_value_type(other),
    }
}

/// aikoql value type of a field, and the observed types that disagreed.
///
/// With a profile, every observed non-null type counts: one type is that type,
/// Int + Float is Float, anything else is Text (the only type every observed
/// value fits) and is reported as mixed. No profile: the declared type.
pub(crate) fn field_value_type(f: &FieldInfo) -> (&'static str, Option<Vec<String>>) {
    // The observed types: the profile's when there is one, else the union
    // schema-only discovery keeps in the type string ("Int32|String").
    let from_type: Vec<String>;
    let all: &[String] = match &f.profile {
        Some(p) => &p.types,
        None => {
            from_type = f.data_type.split('|').map(str::to_string).collect();
            &from_type
        }
    };
    let observed: Vec<&String> = all
        .iter()
        .filter(|t| !t.eq_ignore_ascii_case("null"))
        .collect();
    if observed.is_empty() {
        return (connector_type_to_value_type(&f.data_type), None);
    }
    // a declared SQL type is read as SQL; a single observed (schemaless) type by its BSON/graph name
    if f.profile.is_none() && observed.len() == 1 && f.provenance != Provenance::Observed {
        return (connector_type_to_value_type(&f.data_type), None);
    }
    let kinds: BTreeSet<&'static str> = observed.iter().map(|t| observed_value_type(t)).collect();
    match kinds.iter().copied().collect::<Vec<_>>()[..] {
        [one] => (one, None),
        ["Float", "Int"] => ("Float", None),
        _ => ("Text", Some(observed.into_iter().cloned().collect())),
    }
}

impl DefinitionDraft {
    /// The kernel refuses an ontology with no class: a draft from sources with
    /// no containers has nothing to publish.
    pub fn publishable(&self) -> bool {
        !self.classes.is_empty()
    }
}

/// Fold field `fb` of `b` into the canonical property of field `fa` of `a`.
/// Refused (with a warning, nothing changes) when a field is missing or when
/// two fields of `b` would end up as one property: one column would overwrite
/// the other on every object.
fn align_property(
    a: &Entry,
    b: &Entry,
    fa: &str,
    fb: &str,
    aligned: &mut BTreeMap<(String, String), String>,
    warnings: &mut Vec<String>,
) {
    let skip = |why: &str, warnings: &mut Vec<String>| {
        warnings.push(format!(
            "alignment ignored: {}.{fa} / {}.{fb} - {why}",
            a.key, b.key
        ));
    };
    if !a.container.fields.iter().any(|f| f.name == fa) {
        return skip(&format!("no such field {fa} in {}", a.key), warnings);
    }
    if !b.container.fields.iter().any(|f| f.name == fb) {
        return skip(&format!("no such field {fb} in {}", b.key), warnings);
    }
    let canon = aligned
        .get(&(a.key.clone(), fa.to_string()))
        .cloned()
        .unwrap_or_else(|| canonical_property(fa));
    let effective = |f: &str| {
        aligned
            .get(&(b.key.clone(), f.to_string()))
            .cloned()
            .unwrap_or_else(|| canonical_property(f))
    };
    if b.container
        .fields
        .iter()
        .any(|f| f.name != fb && effective(&f.name) == canon)
    {
        return skip(
            &format!("another field of {} is already {canon}", b.key),
            warnings,
        );
    }
    aligned.insert((b.key.clone(), fb.to_string()), canon);
}

/// The key a source itself declares for a container's rows, in column order:
/// its primary key; failing that its first unique group; failing that, for a
/// replacing / collapsing ClickHouse table, its sorting key (that engine keeps
/// the latest row per sorting key, so the key IS the identity). A plain
/// MergeTree sorting key is not unique and is never claimed. Empty: none.
pub fn declared_identity(c: &ContainerInfo) -> Vec<String> {
    declared_identity_why(c).0
}

/// The declared key and the reason it counts as one ("" when there is none).
pub(crate) fn declared_identity_why(c: &ContainerInfo) -> (Vec<String>, &'static str) {
    // a ClickHouse container carries its engine: there the "primary key" is only the
    // sort prefix and no constraint enforces uniqueness, so only a replacing /
    // collapsing engine's sorting key is an identity
    let enforces_uniqueness = !c.properties.contains_key("engine");
    let pk: Vec<String> = c
        .fields
        .iter()
        .filter(|f| f.is_primary_key)
        .map(|f| f.name.clone())
        .collect();
    if enforces_uniqueness && !pk.is_empty() {
        return (pk, "primary key");
    }
    if let Some(g) = c.unique_groups.iter().find(|g| !g.is_empty()) {
        return (g.clone(), "unique constraint");
    }
    let engine = c.properties.get("engine").map_or("", String::as_str);
    if engine.contains("Replacing") || engine.contains("Collapsing") {
        let cols: Vec<String> = c
            .properties
            .get("sorting_key")
            .map(|k| {
                k.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        // an expression key (toDate(ts)) is not a column: no claim
        if !cols.is_empty() && cols.iter().all(|k| c.fields.iter().any(|f| &f.name == k)) {
            return (cols, "replacing engine sorting key");
        }
    }
    (Vec::new(), "")
}

pub fn definition_from_sources(sources: &[SourceInput], merges: &[ClassMerge]) -> DefinitionDraft {
    definition_from_sources_keyed(sources, merges, &BTreeMap::new())
}

/// Like [`definition_from_sources`], with identity keys the owner declared:
/// `"<source>.<container>"` -> physical columns. A declared key must name
/// existing columns (else it is refused whole, with a warning, and the source's
/// own key stays); it replaces the derived key.
pub fn definition_from_sources_keyed(
    sources: &[SourceInput],
    merges: &[ClassMerge],
    identities: &BTreeMap<String, Vec<String>>,
) -> DefinitionDraft {
    definition_from_sources_with(sources, merges, identities, &[])
}

/// A relationship the owner declares by column values: `from` and `to` are
/// `"<source>.<container>.<column>"`. Rows of `from`'s container get a
/// relationship `name` to the row of `to`'s container whose column equals theirs.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkSpec {
    pub name: String,
    pub from: String,
    pub to: String,
}

/// `"src.container.column"` -> (`"src.container"`, `"column"`).
fn split_ref(r: &str) -> Option<(&str, &str)> {
    let (container, column) = r.rsplit_once('.')?;
    (container.contains('.') && !column.is_empty()).then_some((container, column))
}

/// The full draft: classes, mappings, identity keys and declared links.
pub fn definition_from_sources_with(
    sources: &[SourceInput],
    merges: &[ClassMerge],
    identities: &BTreeMap<String, Vec<String>>,
    links: &[LinkSpec],
) -> DefinitionDraft {
    definition_from_sources_all(sources, merges, identities, links, &[])
}

/// "Rows of `from`'s container assert these canonical properties of the `class`
/// entity whose key is in `from`'s column" (`from` is `source.container.column`).
#[derive(Clone, Debug, PartialEq)]
pub struct AssertSpec {
    pub from: String,
    pub class: String,
    /// Physical column -> canonical property.
    pub properties: BTreeMap<String, String>,
    /// Source authority: the lowest rank wins a disagreement.
    pub rank: u32,
}

/// The full draft: classes, mappings, identity keys, declared links and assertions.
pub fn definition_from_sources_all(
    sources: &[SourceInput],
    merges: &[ClassMerge],
    identities: &BTreeMap<String, Vec<String>>,
    links: &[LinkSpec],
    asserts: &[AssertSpec],
) -> DefinitionDraft {
    let mut warnings: Vec<String> = Vec::new();
    let entries: Vec<Entry> = sources
        .iter()
        .flat_map(|s| {
            s.meta.containers.iter().map(move |c| Entry {
                source: s.name,
                kind: s.kind,
                container: c,
                key: format!("{}.{}", s.name, c.name),
                base: class_name_for_container(&c.name),
            })
        })
        .collect();

    // Group entries: explicit merges join groups, everything else is alone.
    let mut group_of: Vec<usize> = (0..entries.len()).collect();
    let find = |g: &mut Vec<usize>, mut i: usize| {
        while g[i] != i {
            g[i] = g[g[i]];
            i = g[i];
        }
        i
    };
    let mut leader: BTreeMap<usize, usize> = BTreeMap::new();
    // (entry key, physical field) -> the canonical property it was folded into
    let mut aligned: BTreeMap<(String, String), String> = BTreeMap::new();
    for m in merges {
        let idx = |k: &str| entries.iter().position(|e| e.key == k);
        match (idx(&m.a), idx(&m.b)) {
            (Some(a), Some(b)) => {
                let (ra, rb) = (find(&mut group_of, a), find(&mut group_of, b));
                let lead = leader.get(&ra).copied().unwrap_or(a);
                group_of[rb] = ra;
                leader.insert(ra, lead);
                for (fa, fb) in &m.properties {
                    align_property(
                        &entries[a],
                        &entries[b],
                        fa,
                        fb,
                        &mut aligned,
                        &mut warnings,
                    );
                }
            }
            _ => warnings.push(format!(
                "merge ignored: {} / {} not found among the discovered containers",
                m.a, m.b
            )),
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..entries.len() {
        let r = find(&mut group_of, i);
        groups.entry(r).or_default().push(i);
    }
    let groups: Vec<(usize, Vec<usize>)> = groups.into_iter().collect();

    // Class names: base name of the group leader; clashes are not merged.
    let base_of = |(root, members): &(usize, Vec<usize>)| {
        let lead = leader.get(root).copied().unwrap_or(members[0]);
        entries[lead].base.clone()
    };
    let mut by_base: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (gi, g) in groups.iter().enumerate() {
        by_base.entry(base_of(g)).or_default().push(gi);
    }
    let mut final_name: Vec<String> = vec![String::new(); groups.len()];
    let mut dup_with: Vec<Vec<String>> = vec![Vec::new(); groups.len()];
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for (base, gis) in &by_base {
        if gis.len() == 1 {
            continue;
        }
        let srcs: BTreeSet<&str> = gis
            .iter()
            .map(|&gi| entries[groups[gi].1[0]].source)
            .collect();
        for &gi in gis {
            let first = &entries[groups[gi].1[0]];
            let name = if srcs.len() > 1 {
                format!("{}{}", pascal(first.source), base)
            } else {
                base.clone()
            };
            final_name[gi] = name;
        }
        if srcs.len() > 1 {
            for &gi in gis {
                dup_with[gi] = gis
                    .iter()
                    .filter(|&&o| o != gi)
                    .map(|&o| entries[groups[o].1[0]].key.clone())
                    .collect();
            }
        }
    }
    for (gi, g) in groups.iter().enumerate() {
        if final_name[gi].is_empty() {
            final_name[gi] = base_of(g);
        }
    }
    // Make every name unique (same-source clashes such as `user` / `users`).
    for name in final_name.iter_mut() {
        let mut candidate = name.clone();
        let mut n = 2;
        while !taken.insert(candidate.clone()) {
            warnings.push(format!(
                "class name {name} already in use; renamed to {name}{n}"
            ));
            candidate = format!("{name}{n}");
            n += 1;
        }
        *name = candidate;
    }
    let class_of_entry = |i: usize| -> &String {
        let gi = groups.iter().position(|(_, m)| m.contains(&i)).unwrap_or(0);
        &final_name[gi]
    };

    // Properties (global by canonical name), per-class lists, mappings.
    struct Prop {
        types: BTreeSet<&'static str>,
        required_everywhere: bool,
    }
    let mut props: BTreeMap<String, Prop> = BTreeMap::new();
    let mut class_props: Vec<BTreeSet<String>> = vec![BTreeSet::new(); groups.len()];
    let mut mappings: Vec<Value> = Vec::new();
    let mut seen_physical: BTreeMap<(String, String), String> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        let gi = groups.iter().position(|(_, m)| m.contains(&i)).unwrap_or(0);
        let mut property_map = serde_json::Map::new();
        for f in &e.container.fields {
            let canon = aligned
                .get(&(e.key.clone(), f.name.clone()))
                .cloned()
                .unwrap_or_else(|| canonical_property(&f.name));
            let required = f.is_primary_key || !f.nullable;
            let p = props.entry(canon.clone()).or_insert(Prop {
                types: BTreeSet::new(),
                required_everywhere: true,
            });
            let (value_type, mixed) = field_value_type(f);
            if let Some(types) = mixed {
                warnings.push(format!(
                    "{}.{}.{}: mixed observed types ({}); widened to Text",
                    e.source,
                    e.container.name,
                    f.name,
                    types.join(", ")
                ));
            }
            p.types.insert(value_type);
            p.required_everywhere &= required;
            class_props[gi].insert(canon.clone());
            property_map.insert(f.name.clone(), json!(canon));
        }
        match seen_physical.get(&(e.kind.to_string(), e.container.name.clone())) {
            Some(other) => warnings.push(format!(
                "physical type {} ({}) is already mapped by source {other}; the mapping from {} was skipped",
                e.container.name, e.kind, e.source
            )),
            None => {
                seen_physical.insert(
                    (e.kind.to_string(), e.container.name.clone()),
                    e.source.to_string(),
                );
                let mut mapping = json!({
                    "source": e.kind,
                    "physical_type": e.container.name,
                    "class": class_of_entry(i),
                    "property_map": property_map,
                });
                let declared = identities.get(&e.key).filter(|cols| {
                    let missing = cols.iter().find(|c| !e.container.fields.iter().any(|f| &&f.name == c));
                    if let Some(c) = missing {
                        warnings.push(format!("identity ignored: {} has no column {c}", e.key));
                    }
                    missing.is_none() && !cols.is_empty()
                });
                let identity = declared.cloned().unwrap_or_else(|| declared_identity(e.container));
                if !identity.is_empty() {
                    mapping["identity"] = json!(identity);
                }
                mappings.push(mapping);
            }
        }
    }
    for key in identities.keys() {
        if !entries.iter().any(|e| &e.key == key) {
            warnings.push(format!(
                "identity ignored: {key} is not a discovered container"
            ));
        }
    }
    for s in sources {
        let observed = s
            .meta
            .containers
            .iter()
            .flat_map(|c| &c.fields)
            .filter(|f| f.provenance == Provenance::Observed)
            .count();
        if observed > 0 {
            warnings.push(format!(
                "source {}: {observed} field(s) are observed from a sample, not declared",
                s.name
            ));
        }
    }
    let property_defs: Vec<Value> = props
        .iter()
        .map(|(name, p)| {
            let ty = if p.types.len() == 1 {
                p.types.iter().next().copied().unwrap_or("Text")
            } else {
                warnings.push(format!(
                    "property {name} has conflicting types ({}); widened to Text",
                    p.types.iter().copied().collect::<Vec<_>>().join(", ")
                ));
                "Text"
            };
            json!({"name": name, "type": ty, "required": p.required_everywhere,
                   "nullable": !p.required_everywhere})
        })
        .collect();

    // Relationships from references; names stay unique.
    let mut rels: BTreeMap<String, Value> = BTreeMap::new();
    for s in sources {
        for r in &s.meta.references {
            let class_for = |container: &str| {
                entries
                    .iter()
                    .position(|e| e.source == s.name && e.container.name == container)
                    .map(|i| class_of_entry(i).clone())
            };
            let base_name = r
                .name
                .clone()
                .unwrap_or_else(|| format!("references_{}", r.to_container));
            if r.from_fields.len() > 1 {
                warnings.push(format!(
                    "composite foreign key {base_name} ({} columns) is one relationship",
                    r.from_fields.len()
                ));
            }
            let (domain, range) = (class_for(&r.from_container), class_for(&r.to_container));
            // the first free name for this (domain, range): the type's own name,
            // then `<name>_<source>`, then `<name>_<source>_2`, ...; the same pair
            // under any of them is already recorded
            let base = to_snake_case(&base_name);
            let mut n = 0;
            let name = loop {
                let candidate = match n {
                    0 => base.clone(),
                    1 => format!("{base}_{}", s.name),
                    k => format!("{base}_{}_{k}", s.name),
                };
                match rels.get(&candidate) {
                    None => break Some(candidate),
                    Some(e) if e["domain"] == json!(domain) && e["range"] == json!(range) => {
                        break None
                    }
                    Some(_) => n += 1,
                }
            };
            if let Some(name) = name {
                rels.insert(
                    name.clone(),
                    json!({"name": name, "domain": domain, "range": range}),
                );
            }
        }
    }

    // Declared links: validated whole, or ignored with the reason.
    for l in links {
        let ignore =
            |why: String, warnings: &mut Vec<String>| warnings.push(format!("link ignored: {why}"));
        if l.name.trim().is_empty() {
            ignore("a link needs a name".into(), &mut warnings);
            continue;
        }
        let (Some((from_key, from_col)), Some((to_key, to_col))) =
            (split_ref(&l.from), split_ref(&l.to))
        else {
            ignore(
                format!(
                    "{}: references must look like source.container.column",
                    l.name
                ),
                &mut warnings,
            );
            continue;
        };
        let find = |key: &str| entries.iter().position(|e| e.key == key);
        let (Some(fi), Some(ti)) = (find(from_key), find(to_key)) else {
            let missing = if find(from_key).is_none() {
                from_key
            } else {
                to_key
            };
            ignore(
                format!("{}: {missing} is not a discovered container", l.name),
                &mut warnings,
            );
            continue;
        };
        let has = |i: usize, col: &str| entries[i].container.fields.iter().any(|f| f.name == col);
        if let Some((key, col)) = [(from_key, from_col, fi), (to_key, to_col, ti)]
            .iter()
            .find(|(_, c, i)| !has(*i, c))
            .map(|(k, c, _)| (*k, *c))
        {
            ignore(
                format!("{}: {key} has no column {col}", l.name),
                &mut warnings,
            );
            continue;
        }
        let (from, to) = (&entries[fi], &entries[ti]);
        let Some(m) = mappings.iter_mut().find(|m| {
            m["source"] == from.kind && m["physical_type"] == from.container.name.as_str()
        }) else {
            ignore(format!("{}: {from_key} is not mapped (its physical type is already mapped by another source)", l.name), &mut warnings);
            continue;
        };
        if m["links"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x["name"] == l.name.as_str()))
        {
            ignore(
                format!("{}: already a link on {from_key}", l.name),
                &mut warnings,
            );
            continue;
        }
        let entry = json!({"name": l.name, "column": from_col, "target_source": to.kind,
                           "target_type": to.container.name, "target_column": to_col});
        match m["links"].as_array_mut() {
            Some(a) => a.push(entry),
            None => m["links"] = json!([entry]),
        }
        let (domain, range) = (class_of_entry(fi).clone(), class_of_entry(ti).clone());
        match rels.get(&l.name) {
            None => {
                rels.insert(l.name.clone(), json!({"name": l.name, "domain": domain, "range": range}));
            }
            Some(e) if e["domain"] == json!(domain) && e["range"] == json!(range) => {}
            Some(_) => warnings.push(format!(
                "link {}: a relationship of that name already connects other classes; the link is kept, the class relationship is not changed",
                l.name
            )),
        }
    }

    // Declared assertions: validated whole, or ignored with the reason.
    for a in asserts {
        let ignore = |why: String, warnings: &mut Vec<String>| {
            warnings.push(format!("assertion ignored: {why}"))
        };
        if a.class.trim().is_empty() {
            ignore("an assertion needs a class".into(), &mut warnings);
            continue;
        }
        let Some((key, key_col)) = split_ref(&a.from) else {
            ignore(
                format!(
                    "{}: reference must look like source.container.column",
                    a.class
                ),
                &mut warnings,
            );
            continue;
        };
        let Some(ei) = entries.iter().position(|e| e.key == key) else {
            ignore(
                format!("{}: {key} is not a discovered container", a.class),
                &mut warnings,
            );
            continue;
        };
        if a.properties.is_empty()
            || a.properties.len() > 64
            || a.properties
                .iter()
                .any(|(p, c)| p.is_empty() || c.trim().is_empty())
        {
            ignore(
                format!(
                    "{}: {key} asserts no properties (or an unnamed one)",
                    a.class
                ),
                &mut warnings,
            );
            continue;
        }
        let has = |col: &str| entries[ei].container.fields.iter().any(|f| f.name == col);
        if let Some(col) = std::iter::once(key_col)
            .chain(a.properties.keys().map(String::as_str))
            .find(|c| !has(c))
        {
            ignore(
                format!("{}: {key} has no column {col}", a.class),
                &mut warnings,
            );
            continue;
        }
        let e = &entries[ei];
        let Some(m) = mappings
            .iter_mut()
            .find(|m| m["source"] == e.kind && m["physical_type"] == e.container.name.as_str())
        else {
            ignore(format!("{}: {key} is not mapped", a.class), &mut warnings);
            continue;
        };
        if m["asserts"]
            .as_array()
            .is_some_and(|v| v.iter().any(|x| x["class"] == a.class.as_str()))
        {
            ignore(
                format!("{}: already asserted by {key}", a.class),
                &mut warnings,
            );
            continue;
        }
        let entry = json!({"class": a.class, "key_column": key_col, "properties": a.properties, "rank": a.rank});
        match m["asserts"].as_array_mut() {
            Some(v) => v.push(entry),
            None => m["asserts"] = json!([entry]),
        }
    }

    let classes_json: Vec<Value> = groups
        .iter()
        .enumerate()
        .map(|(gi, (_, members))| {
            let from: Vec<&str> = members.iter().map(|&i| entries[i].key.as_str()).collect();
            json!({
                "name": final_name[gi],
                "description": format!("Discovered from {}", from.join(", ")),
                "properties": class_props[gi].iter().collect::<Vec<_>>(),
            })
        })
        .collect();
    let classes: Vec<DraftClass> = groups
        .iter()
        .enumerate()
        .map(|(gi, (_, members))| DraftClass {
            name: final_name[gi].clone(),
            sources: members.iter().map(|&i| entries[i].key.clone()).collect(),
            status: if dup_with[gi].is_empty() {
                ClassStatus::New
            } else {
                ClassStatus::PossibleDuplicate {
                    with: dup_with[gi].clone(),
                }
            },
        })
        .collect();

    DefinitionDraft {
        definition: json!({
            "namespace": "sources",
            "version": "1.0",
            "classes": classes_json,
            "relationships": rels.into_values().collect::<Vec<_>>(),
            "property_defs": property_defs,
            "mappings": mappings,
        }),
        classes,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector_bridge::{discover_connector_schema, FieldProfile, Provenance};

    type Col = (&'static str, &'static str, bool, bool, bool);

    fn meta(
        kind: &str,
        label: &str,
        tables: &[(&str, &[Col])],
        fks: &[FkRow],
    ) -> ConnectorMetadata {
        discover_connector_schema(kind, label, tables, fks)
    }
    type FkRow<'a> = (
        &'a str,
        &'a [&'a str],
        &'a str,
        &'a [&'a str],
        Option<&'a str>,
    );

    const ID: Col = ("id", "integer", true, false, true);

    fn names(d: &DefinitionDraft) -> Vec<String> {
        let mut n: Vec<String> = d.classes.iter().map(|c| c.name.clone()).collect();
        n.sort();
        n
    }

    fn list<'a>(d: &'a DefinitionDraft, key: &str) -> &'a Vec<Value> {
        d.definition[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} missing: {}", d.definition))
    }

    #[test]
    fn three_tables_three_classes_with_mappings() {
        let m = meta(
            "postgres",
            "shop",
            &[
                ("customers", &[ID, ("email", "text", false, false, false)]),
                ("orders", &[ID]),
                ("products", &[ID]),
            ],
            &[],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "shop",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        assert_eq!(names(&d), ["Customer", "Order", "Product"]);
        let mappings = list(&d, "mappings");
        assert!(mappings.iter().any(|m| m["source"] == "postgres"
            && m["physical_type"] == "customers"
            && m["class"] == "Customer"));
        let customer = list(&d, "classes")
            .iter()
            .find(|c| c["name"] == "Customer")
            .unwrap();
        assert!(customer["properties"]
            .as_array()
            .unwrap()
            .contains(&json!("email")));
    }

    #[test]
    fn fk_becomes_relationship_with_domain_and_range() {
        let m = meta(
            "postgres",
            "shop",
            &[
                ("customers", &[ID]),
                (
                    "orders",
                    &[ID, ("customer_id", "integer", false, true, false)],
                ),
            ],
            &[(
                "orders",
                &["customer_id"],
                "customers",
                &["id"],
                Some("orders_customer_fk"),
            )],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "shop",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        let rel = list(&d, "relationships")
            .iter()
            .find(|r| r["name"] == "orders_customer_fk")
            .expect("relationship");
        assert_eq!(
            (rel["domain"].as_str(), rel["range"].as_str()),
            (Some("Order"), Some("Customer"))
        );
    }

    #[test]
    fn same_name_in_two_sources_is_possible_duplicate_not_merged() {
        let a = meta("postgres", "crm", &[("customers", &[ID])], &[]);
        let b = meta("sqlite", "shop", &[("customers", &[ID])], &[]);
        let d = definition_from_sources(
            &[
                SourceInput {
                    name: "crm",
                    kind: "postgres",
                    meta: &a,
                },
                SourceInput {
                    name: "shop",
                    kind: "sqlite",
                    meta: &b,
                },
            ],
            &[],
        );
        assert_eq!(names(&d), ["CrmCustomer", "ShopCustomer"]);
        assert!(d
            .classes
            .iter()
            .all(|c| matches!(c.status, ClassStatus::PossibleDuplicate { .. })));
    }

    #[test]
    fn explicit_merge_yields_one_class_two_mappings() {
        let a = meta(
            "postgres",
            "crm",
            &[("customers", &[ID, ("email", "text", false, false, false)])],
            &[],
        );
        let b = meta(
            "sqlite",
            "shop",
            &[(
                "clients",
                &[ID, ("email_address", "text", false, false, false)],
            )],
            &[],
        );
        let d = definition_from_sources(
            &[
                SourceInput {
                    name: "crm",
                    kind: "postgres",
                    meta: &a,
                },
                SourceInput {
                    name: "shop",
                    kind: "sqlite",
                    meta: &b,
                },
            ],
            &[ClassMerge {
                a: "crm.customers".into(),
                b: "shop.clients".into(),
                properties: Vec::new(),
            }],
        );
        assert_eq!(names(&d), ["Customer"]);
        let maps: Vec<&Value> = list(&d, "mappings")
            .iter()
            .filter(|m| m["class"] == "Customer")
            .collect();
        assert_eq!(maps.len(), 2, "{}", d.definition);
        let shop = maps
            .iter()
            .find(|m| m["physical_type"] == "clients")
            .unwrap();
        assert_eq!(shop["property_map"]["email_address"], "email_address");
        assert!(d.classes[0].sources.contains(&"shop.clients".to_string()));
    }

    fn crm_and_shop() -> (ConnectorMetadata, ConnectorMetadata) {
        (
            meta(
                "postgres",
                "crm",
                &[(
                    "customers",
                    &[
                        ID,
                        ("email", "text", false, false, false),
                        ("tier", "text", false, true, false),
                    ],
                )],
                &[],
            ),
            meta(
                "sqlite",
                "shop",
                &[(
                    "clients",
                    &[
                        ID,
                        ("email_address", "text", false, false, false),
                        ("tier", "integer", false, true, false),
                    ],
                )],
                &[],
            ),
        )
    }

    fn merged(properties: Vec<(&str, &str)>) -> DefinitionDraft {
        let (a, b) = crm_and_shop();
        definition_from_sources(
            &[
                SourceInput {
                    name: "crm",
                    kind: "postgres",
                    meta: &a,
                },
                SourceInput {
                    name: "shop",
                    kind: "sqlite",
                    meta: &b,
                },
            ],
            &[ClassMerge {
                a: "crm.customers".into(),
                b: "shop.clients".into(),
                properties: properties
                    .into_iter()
                    .map(|(x, y)| (x.to_string(), y.to_string()))
                    .collect(),
            }],
        )
    }

    #[test]
    fn aligned_properties_become_one_canonical_property_in_both_mappings() {
        let d = merged(vec![("email", "email_address")]);
        let class_props: Vec<&str> = list(&d, "classes")[0]["properties"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        assert!(class_props.contains(&"email"), "{class_props:?}");
        assert!(
            !class_props.contains(&"email_address"),
            "folded into email: {class_props:?}"
        );
        let maps = list(&d, "mappings");
        let of = |t: &str| maps.iter().find(|m| m["physical_type"] == t).unwrap();
        assert_eq!(of("customers")["property_map"]["email"], "email");
        assert_eq!(of("clients")["property_map"]["email_address"], "email");
        assert!(!list(&d, "property_defs")
            .iter()
            .any(|p| p["name"] == "email_address"));
        assert!(
            d.warnings.iter().all(|w| !w.contains("ignored")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn aligning_properties_of_different_types_widens_with_the_existing_warning() {
        let d = merged(vec![("tier", "tier")]);
        let tier = list(&d, "property_defs")
            .iter()
            .find(|p| p["name"] == "tier")
            .unwrap();
        assert_eq!(tier["type"], "Text");
    }

    #[test]
    fn an_alignment_naming_a_missing_field_is_ignored_with_a_warning() {
        let d = merged(vec![("email", "nope"), ("ghost", "email_address")]);
        assert_eq!(
            d.warnings
                .iter()
                .filter(|w| w.contains("alignment ignored"))
                .count(),
            2,
            "{:?}",
            d.warnings
        );
        // nothing was folded: both source properties survive
        let maps = list(&d, "mappings");
        let shop = maps
            .iter()
            .find(|m| m["physical_type"] == "clients")
            .unwrap();
        assert_eq!(shop["property_map"]["email_address"], "email_address");
    }

    #[test]
    fn two_fields_of_one_container_cannot_fold_into_one_property() {
        // folding both `email_address` and `tier` into `email` would make one source column overwrite another
        let d = merged(vec![("email", "email_address"), ("email", "tier")]);
        assert!(
            d.warnings.iter().any(|w| w.contains("alignment ignored")),
            "{:?}",
            d.warnings
        );
        let maps = list(&d, "mappings");
        let shop = maps
            .iter()
            .find(|m| m["physical_type"] == "clients")
            .unwrap();
        assert_eq!(shop["property_map"]["email_address"], "email");
        assert_eq!(
            shop["property_map"]["tier"], "tier",
            "the second fold was refused"
        );
    }

    #[test]
    fn unknown_merge_reference_warns_and_is_ignored() {
        let a = meta("postgres", "crm", &[("customers", &[ID])], &[]);
        let d = definition_from_sources(
            &[SourceInput {
                name: "crm",
                kind: "postgres",
                meta: &a,
            }],
            &[ClassMerge {
                a: "crm.customers".into(),
                b: "nope.nothing".into(),
                properties: Vec::new(),
            }],
        );
        assert_eq!(names(&d), ["Customer"]);
        assert!(
            d.warnings.iter().any(|w| w.contains("nope.nothing")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn conflicting_property_types_widen_to_text_with_warning() {
        let a = meta(
            "postgres",
            "a",
            &[("things", &[("code", "integer", false, false, false)])],
            &[],
        );
        let b = meta(
            "sqlite",
            "b",
            &[("items", &[("code", "text", false, false, false)])],
            &[],
        );
        let d = definition_from_sources(
            &[
                SourceInput {
                    name: "a",
                    kind: "postgres",
                    meta: &a,
                },
                SourceInput {
                    name: "b",
                    kind: "sqlite",
                    meta: &b,
                },
            ],
            &[],
        );
        let code = list(&d, "property_defs")
            .iter()
            .find(|p| p["name"] == "code")
            .unwrap();
        assert_eq!(code["type"], "Text");
        assert!(
            d.warnings.iter().any(|w| w.contains("code")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn odd_container_names_yield_unique_valid_class_names() {
        let m = meta(
            "postgres",
            "x",
            &[
                ("order", &[ID]),
                ("Order", &[ID]),
                ("user data", &[ID]),
                ("2024_sales", &[ID]),
                ("users", &[ID]),
                ("user", &[ID]),
            ],
            &[],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "x",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        let n = names(&d);
        let mut unique = n.clone();
        unique.dedup();
        assert_eq!(n.len(), 6);
        assert_eq!(n, unique, "class names must be unique: {n:?}");
        assert!(n.iter().all(|c| c.chars().next().unwrap().is_alphabetic()));
        assert!(n.contains(&"UserData".to_string()) && n.contains(&"T2024Sale".to_string()));
    }

    #[test]
    fn canonical_property_names_are_snake_case_and_mapped() {
        let m = meta(
            "mongodb",
            "crm",
            &[("people", &[("customerId", "bson", false, true, false)])],
            &[],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "crm",
                kind: "mongodb",
                meta: &m,
            }],
            &[],
        );
        assert!(list(&d, "property_defs")
            .iter()
            .any(|p| p["name"] == "customer_id"));
        assert_eq!(
            list(&d, "mappings")[0]["property_map"]["customerId"],
            "customer_id"
        );
    }

    #[test]
    fn observed_fields_warn_and_use_the_observed_type() {
        let mut m = meta(
            "mongodb",
            "crm",
            &[("people", &[("age", "bson", false, true, false)])],
            &[],
        );
        m.containers[0].fields[0].provenance = Provenance::Observed;
        m.containers[0].fields[0].profile = Some(FieldProfile {
            types: vec!["Int32".into(), "Null".into()],
            presence_ratio: 0.9,
            null_ratio: 0.1,
            samples: 100,
        });
        let d = definition_from_sources(
            &[SourceInput {
                name: "crm",
                kind: "mongodb",
                meta: &m,
            }],
            &[],
        );
        let age = list(&d, "property_defs")
            .iter()
            .find(|p| p["name"] == "age")
            .unwrap();
        assert_eq!(age["type"], "Int");
        assert!(
            d.warnings.iter().any(|w| w.contains("observed")),
            "{:?}",
            d.warnings
        );
    }

    fn observed_type_of(types: &[&str]) -> (String, Vec<String>) {
        let mut m = meta(
            "mongodb",
            "crm",
            &[("people", &[("v", "bson", false, true, false)])],
            &[],
        );
        m.containers[0].fields[0].provenance = Provenance::Observed;
        m.containers[0].fields[0].profile = Some(FieldProfile {
            types: types.iter().map(|t| t.to_string()).collect(),
            presence_ratio: 1.0,
            null_ratio: 0.0,
            samples: 10,
        });
        let d = definition_from_sources(
            &[SourceInput {
                name: "crm",
                kind: "mongodb",
                meta: &m,
            }],
            &[],
        );
        let ty = list(&d, "property_defs")
            .iter()
            .find(|p| p["name"] == "v")
            .unwrap()["type"]
            .as_str()
            .unwrap()
            .to_string();
        (ty, d.warnings)
    }

    #[test]
    fn mixed_observed_types_widen_to_text_with_a_warning() {
        let (ty, warnings) = observed_type_of(&["Int32", "Null", "String"]);
        assert_eq!(
            ty, "Text",
            "an Int-typed property would be wrong for the string rows"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("mixed") && w.contains("v")),
            "{warnings:?}"
        );
        let (ty, _) = observed_type_of(&["Boolean", "Int32"]);
        assert_eq!(ty, "Text");
    }

    #[test]
    fn int_and_double_observed_widen_to_float_without_a_mixed_warning() {
        let (ty, warnings) = observed_type_of(&["Int32", "Double", "Int64"]);
        assert_eq!(ty, "Float");
        assert!(
            !warnings.iter().any(|w| w.contains("mixed")),
            "{warnings:?}"
        );
        assert_eq!(observed_type_of(&["Int32", "Null"]).0, "Int");
        assert_eq!(
            observed_type_of(&["Null"]).0,
            "Text",
            "only nulls observed: no type claim"
        );
    }

    #[test]
    fn observed_types_follow_what_the_importer_stores() {
        // Decimal128 imports as exact text, a BSON Timestamp as a {t,i} map
        assert_eq!(observed_type_of(&["Decimal128"]).0, "Text");
        assert_eq!(observed_type_of(&["Timestamp"]).0, "Text");
        assert_eq!(observed_type_of(&["DateTime"]).0, "DateTime");
        assert_eq!(observed_type_of(&["ObjectId"]).0, "Text");
        assert_eq!(observed_type_of(&["Document"]).0, "Text");
    }

    #[test]
    fn a_union_declared_type_without_a_profile_widens_too() {
        // schema-only discovery keeps no profile: the union rides in the type string
        for (declared, want) in [
            ("Int32|String", "Text"),
            ("Int32|Double", "Float"),
            ("Long|Double", "Float"),
            ("String|Long|Boolean", "Text"),
            ("Int32", "Int"),
        ] {
            let m = meta(
                "mongodb",
                "crm",
                &[("people", &[("v", declared, false, true, false)])],
                &[],
            );
            let d = definition_from_sources(
                &[SourceInput {
                    name: "crm",
                    kind: "mongodb",
                    meta: &m,
                }],
                &[],
            );
            let ty = list(&d, "property_defs")
                .iter()
                .find(|p| p["name"] == "v")
                .unwrap()["type"]
                .clone();
            assert_eq!(ty, want, "{declared}");
        }
    }

    #[test]
    fn one_relationship_type_between_several_label_pairs_keeps_every_pair() {
        // a Neo4j type linking Person->Company, Employee->Customer and Person->Customer
        let mut m = meta(
            "neo4j",
            "g",
            &[
                ("Person", &[ID]),
                ("Employee", &[ID]),
                ("Company", &[ID]),
                ("Customer", &[ID]),
            ],
            &[],
        );
        let pair = |from: &str, to: &str| crate::connector_bridge::ReferenceInfo {
            from_container: from.into(),
            from_fields: Vec::new(),
            to_container: to.into(),
            to_fields: Vec::new(),
            name: Some("WORKS_AT".into()),
        };
        m.references = vec![
            pair("Person", "Company"),
            pair("Employee", "Customer"),
            pair("Person", "Customer"),
            pair("Person", "Company"), // the same pair again: one relationship
        ];
        let d = definition_from_sources(
            &[SourceInput {
                name: "g",
                kind: "neo4j",
                meta: &m,
            }],
            &[],
        );
        let mut got: Vec<(String, String)> = list(&d, "relationships")
            .iter()
            .map(|r| {
                (
                    r["domain"].as_str().unwrap().to_string(),
                    r["range"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("Employee".to_string(), "Customer".to_string()),
                ("Person".to_string(), "Company".to_string()),
                ("Person".to_string(), "Customer".to_string()),
            ],
            "{:?}",
            list(&d, "relationships")
        );
        let names: std::collections::BTreeSet<&str> = list(&d, "relationships")
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names.len(),
            3,
            "every relationship has its own name: {names:?}"
        );
    }

    // ---- identity keys (docs/ONTOLOGY-IN-PRACTICE.md, Phase A) ----

    fn identity_of(d: &DefinitionDraft, physical: &str) -> Vec<String> {
        list(d, "mappings")
            .iter()
            .find(|m| m["physical_type"] == physical)
            .and_then(|m| m["identity"].as_array())
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    }

    fn one(name: &str, kind: &str, m: &ConnectorMetadata) -> DefinitionDraft {
        definition_from_sources(
            &[SourceInput {
                name,
                kind,
                meta: m,
            }],
            &[],
        )
    }

    #[test]
    fn the_key_a_source_declares_becomes_the_mapping_identity() {
        // primary key (composite keeps column order), then a unique group, then a replacing engine's sorting key
        let mut m = meta(
            "postgres",
            "crm",
            &[
                (
                    "orders",
                    &[
                        ("tenant", "text", true, false, true),
                        ("id", "integer", true, false, true),
                        ("note", "text", false, true, false),
                    ],
                ),
                (
                    "people",
                    &[
                        ("email", "text", false, false, false),
                        ("name", "text", false, true, false),
                    ],
                ),
            ],
            &[],
        );
        m.containers[1].unique_groups = vec![vec!["email".into()]];
        let d = one("crm", "postgres", &m);
        assert_eq!(identity_of(&d, "orders"), ["tenant", "id"]);
        assert_eq!(
            identity_of(&d, "people"),
            ["email"],
            "a unique group is a key when there is no primary key"
        );

        let mut ch = meta(
            "clickhouse",
            "olap",
            &[
                (
                    "devices",
                    &[
                        ("visitor_id", "String", false, false, false),
                        ("risk", "Float32", false, false, false),
                    ],
                ),
                (
                    "visits",
                    &[
                        ("visitor_id", "String", false, false, false),
                        ("ts", "DateTime", false, false, false),
                    ],
                ),
            ],
            &[],
        );
        ch.containers[0]
            .properties
            .insert("engine".into(), "ReplacingMergeTree".into());
        ch.containers[0]
            .properties
            .insert("sorting_key".into(), "visitor_id".into());
        ch.containers[1]
            .properties
            .insert("engine".into(), "MergeTree".into());
        ch.containers[1]
            .properties
            .insert("sorting_key".into(), "visitor_id, ts".into());
        let d = one("olap", "clickhouse", &ch);
        assert_eq!(
            identity_of(&d, "devices"),
            ["visitor_id"],
            "a replacing engine's sorting key IS its identity"
        );
        assert!(
            identity_of(&d, "visits").is_empty(),
            "a plain MergeTree sorting key is not unique: no claim"
        );
    }

    // ---- links (docs/ONTOLOGY-IN-PRACTICE.md, Phase B) ----

    fn two_sources() -> (ConnectorMetadata, ConnectorMetadata) {
        (
            meta(
                "clickhouse",
                "olap",
                &[
                    (
                        "device_accounts",
                        &[
                            ("visitor_id", "String", false, false, false),
                            ("account_id", "String", false, false, false),
                        ],
                    ),
                    ("devices", &[("visitor_id", "String", false, false, false)]),
                ],
                &[],
            ),
            meta(
                "postgres",
                "crm",
                &[(
                    "accounts",
                    &[
                        ("id", "text", true, false, true),
                        ("email", "text", false, false, false),
                    ],
                )],
                &[],
            ),
        )
    }

    fn link(name: &str, from: &str, to: &str) -> LinkSpec {
        LinkSpec {
            name: name.into(),
            from: from.into(),
            to: to.into(),
        }
    }

    fn with_links(links: &[LinkSpec]) -> DefinitionDraft {
        let (a, b) = two_sources();
        definition_from_sources_with(
            &[
                SourceInput {
                    name: "olap",
                    kind: "clickhouse",
                    meta: &a,
                },
                SourceInput {
                    name: "crm",
                    kind: "postgres",
                    meta: &b,
                },
            ],
            &[],
            &BTreeMap::new(),
            links,
        )
    }

    #[test]
    fn a_link_becomes_a_mapping_link_and_a_class_relationship_even_across_sources() {
        let d = with_links(&[
            link(
                "for_account",
                "olap.device_accounts.account_id",
                "crm.accounts.id",
            ),
            link(
                "for_device",
                "olap.device_accounts.visitor_id",
                "olap.devices.visitor_id",
            ),
        ]);
        let m = list(&d, "mappings")
            .iter()
            .find(|m| m["physical_type"] == "device_accounts")
            .unwrap();
        let links = m["links"]
            .as_array()
            .expect("links on the from-container's mapping");
        let acct = links.iter().find(|l| l["name"] == "for_account").unwrap();
        assert_eq!(
            (
                acct["column"].as_str(),
                acct["target_source"].as_str(),
                acct["target_type"].as_str(),
                acct["target_column"].as_str()
            ),
            (
                Some("account_id"),
                Some("postgres"),
                Some("accounts"),
                Some("id")
            ),
            "the target is named by its source KIND and physical type"
        );
        let dev = links.iter().find(|l| l["name"] == "for_device").unwrap();
        assert_eq!(dev["target_source"], "clickhouse");
        // the class model says the same thing
        let rels = list(&d, "relationships");
        let r = rels.iter().find(|r| r["name"] == "for_account").unwrap();
        assert_eq!(
            (r["domain"].as_str(), r["range"].as_str()),
            (Some("DeviceAccount"), Some("Account")),
            "{r}"
        );
        assert!(
            d.warnings.iter().all(|w| !w.contains("link ignored")),
            "{:?}",
            d.warnings
        );
        // a container with no link writes none
        let plain = list(&d, "mappings")
            .iter()
            .find(|m| m["physical_type"] == "accounts")
            .unwrap();
        assert!(plain.get("links").is_none());
    }

    // ---- assertions (Phase C) ----

    fn assertion(from: &str, class: &str, props: &[(&str, &str)], rank: u32) -> AssertSpec {
        AssertSpec {
            from: from.into(),
            class: class.into(),
            properties: props
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            rank,
        }
    }

    fn with_asserts(asserts: &[AssertSpec]) -> DefinitionDraft {
        let (a, b) = two_sources();
        definition_from_sources_all(
            &[
                SourceInput {
                    name: "olap",
                    kind: "clickhouse",
                    meta: &a,
                },
                SourceInput {
                    name: "crm",
                    kind: "postgres",
                    meta: &b,
                },
            ],
            &[],
            &BTreeMap::new(),
            &[],
            asserts,
        )
    }

    #[test]
    fn an_assertion_lands_on_the_mapping_of_the_container_it_names() {
        let d = with_asserts(&[
            assertion("crm.accounts.id", "Account", &[("email", "email")], 10),
            assertion(
                "olap.device_accounts.account_id",
                "Account",
                &[("visitor_id", "last_device")],
                20,
            ),
        ]);
        let m = list(&d, "mappings")
            .iter()
            .find(|m| m["physical_type"] == "accounts")
            .unwrap();
        let a = &m["asserts"][0];
        assert_eq!(
            (
                a["class"].as_str(),
                a["key_column"].as_str(),
                a["rank"].as_u64()
            ),
            (Some("Account"), Some("id"), Some(10)),
            "{a}"
        );
        assert_eq!(a["properties"]["email"], "email");
        let m = list(&d, "mappings")
            .iter()
            .find(|m| m["physical_type"] == "device_accounts")
            .unwrap();
        assert_eq!(m["asserts"][0]["rank"], 20);
        assert!(
            d.warnings.iter().all(|w| !w.contains("assertion ignored")),
            "{:?}",
            d.warnings
        );
        // untouched containers write nothing
        let m = list(&d, "mappings")
            .iter()
            .find(|m| m["physical_type"] == "devices")
            .unwrap();
        assert!(m.get("asserts").is_none());
    }

    #[test]
    fn an_assertion_that_cannot_be_honoured_is_ignored_with_a_reason() {
        for (a, why) in [
            (
                assertion("crm.accounts.nope", "Account", &[("email", "email")], 1),
                "no column nope",
            ),
            (
                assertion("crm.accounts.id", "Account", &[("nope", "email")], 1),
                "no column nope",
            ),
            (
                assertion("crm.ghost.id", "Account", &[("email", "email")], 1),
                "not a discovered container",
            ),
            (
                assertion("no-dots", "Account", &[("email", "email")], 1),
                "source.container.column",
            ),
            (
                assertion("crm.accounts.id", "", &[("email", "email")], 1),
                "needs a class",
            ),
            (
                assertion("crm.accounts.id", "Account", &[], 1),
                "asserts no properties",
            ),
            (
                assertion("crm.accounts.id", "Account", &[("email", "")], 1),
                "asserts no properties",
            ),
        ] {
            let d = with_asserts(std::slice::from_ref(&a));
            assert!(
                d.warnings
                    .iter()
                    .any(|w| w.contains("assertion ignored") && w.contains(why)),
                "{a:?}: {:?}",
                d.warnings
            );
            assert!(
                list(&d, "mappings")
                    .iter()
                    .all(|m| m.get("asserts").is_none()),
                "{a:?}"
            );
        }
        // one class twice on one container: the second is refused
        let d = with_asserts(&[
            assertion("crm.accounts.id", "Account", &[("email", "email")], 1),
            assertion("crm.accounts.id", "Account", &[("email", "mail")], 2),
        ]);
        let m = list(&d, "mappings")
            .iter()
            .find(|m| m["physical_type"] == "accounts")
            .unwrap();
        assert_eq!(m["asserts"].as_array().unwrap().len(), 1);
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("assertion ignored") && w.contains("already")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn a_link_that_cannot_be_honoured_is_ignored_with_a_reason() {
        for (l, why) in [
            (
                link("x", "olap.device_accounts.nope", "crm.accounts.id"),
                "no column nope",
            ),
            (
                link("x", "olap.device_accounts.account_id", "crm.accounts.nope"),
                "no column nope",
            ),
            (
                link("x", "olap.ghost.account_id", "crm.accounts.id"),
                "not a discovered container",
            ),
            (
                link("x", "crm.accounts.id", "elsewhere.t.id"),
                "not a discovered container",
            ),
            (
                link("x", "no-dots", "crm.accounts.id"),
                "source.container.column",
            ),
            (
                link("", "olap.device_accounts.account_id", "crm.accounts.id"),
                "needs a name",
            ),
        ] {
            let d = with_links(std::slice::from_ref(&l));
            assert!(
                d.warnings
                    .iter()
                    .any(|w| w.contains("link ignored") && w.contains(why)),
                "{l:?}: {:?}",
                d.warnings
            );
            assert!(
                list(&d, "mappings")
                    .iter()
                    .all(|m| m.get("links").is_none()),
                "{l:?}"
            );
        }
        // two links with one name on one container: the second is refused, the first stands
        let d = with_links(&[
            link(
                "for_x",
                "olap.device_accounts.account_id",
                "crm.accounts.id",
            ),
            link(
                "for_x",
                "olap.device_accounts.visitor_id",
                "olap.devices.visitor_id",
            ),
        ]);
        let m = list(&d, "mappings")
            .iter()
            .find(|m| m["physical_type"] == "device_accounts")
            .unwrap();
        assert_eq!(m["links"].as_array().unwrap().len(), 1);
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("link ignored") && w.contains("already")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn a_clickhouse_primary_key_is_a_sort_prefix_not_an_identity() {
        // ClickHouse marks the sorting-key columns "primary key", but nothing enforces
        // uniqueness: claiming it would collapse rows or refuse the import
        let mut ch = meta(
            "clickhouse",
            "olap",
            &[
                (
                    "visits",
                    &[
                        ("visitor_id", "String", true, false, false),
                        ("ts", "DateTime", true, false, false),
                        ("ip", "String", false, false, false),
                    ],
                ),
                (
                    "devices",
                    &[
                        ("visitor_id", "String", true, false, false),
                        ("risk", "Float32", false, false, false),
                    ],
                ),
            ],
            &[],
        );
        ch.containers[0]
            .properties
            .insert("engine".into(), "MergeTree".into());
        ch.containers[0]
            .properties
            .insert("sorting_key".into(), "visitor_id, ts".into());
        ch.containers[1]
            .properties
            .insert("engine".into(), "ReplacingMergeTree".into());
        ch.containers[1]
            .properties
            .insert("sorting_key".into(), "visitor_id".into());
        let d = one("olap", "clickhouse", &ch);
        assert!(
            identity_of(&d, "visits").is_empty(),
            "a MergeTree 'primary key' is not unique"
        );
        assert_eq!(
            identity_of(&d, "devices"),
            ["visitor_id"],
            "a replacing table is keyed by its sorting key"
        );
    }

    #[test]
    fn a_declared_identity_wins_is_validated_and_an_empty_one_writes_nothing() {
        let m = meta(
            "clickhouse",
            "olap",
            &[(
                "device_activity",
                &[
                    ("visitor_id", "String", false, false, false),
                    ("visit_count", "UInt64", false, false, false),
                ],
            )],
            &[],
        );
        // nothing declared and nothing derivable: no identity key at all
        let none = one("olap", "clickhouse", &m);
        assert!(identity_of(&none, "device_activity").is_empty());
        assert!(
            list(&none, "mappings")[0].get("identity").is_none(),
            "no empty list is written"
        );
        // the owner declares it
        let ids: std::collections::BTreeMap<String, Vec<String>> = [(
            "olap.device_activity".to_string(),
            vec!["visitor_id".to_string()],
        )]
        .into();
        let d = definition_from_sources_keyed(
            &[SourceInput {
                name: "olap",
                kind: "clickhouse",
                meta: &m,
            }],
            &[],
            &ids,
        );
        assert_eq!(identity_of(&d, "device_activity"), ["visitor_id"]);
        // a column that does not exist is ignored with a warning, never silently half-applied
        let bad: std::collections::BTreeMap<String, Vec<String>> = [(
            "olap.device_activity".to_string(),
            vec!["visitor_id".to_string(), "nope".to_string()],
        )]
        .into();
        let d = definition_from_sources_keyed(
            &[SourceInput {
                name: "olap",
                kind: "clickhouse",
                meta: &m,
            }],
            &[],
            &bad,
        );
        assert!(
            identity_of(&d, "device_activity").is_empty(),
            "a key with a missing column is refused whole"
        );
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("identity ignored") && w.contains("nope")),
            "{:?}",
            d.warnings
        );
        // an unknown container is a warning too
        let unk: std::collections::BTreeMap<String, Vec<String>> =
            [("olap.ghost".to_string(), vec!["x".to_string()])].into();
        assert!(definition_from_sources_keyed(
            &[SourceInput {
                name: "olap",
                kind: "clickhouse",
                meta: &m
            }],
            &[],
            &unk
        )
        .warnings
        .iter()
        .any(|w| w.contains("ghost")));
    }

    #[test]
    fn composite_foreign_key_warns() {
        let m = meta(
            "postgres",
            "x",
            &[("a", &[ID]), ("b", &[ID])],
            &[("b", &["x", "y"], "a", &["p", "q"], Some("b_a_fk"))],
        );
        let d = definition_from_sources(
            &[SourceInput {
                name: "x",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        assert!(
            d.warnings.iter().any(|w| w.contains("b_a_fk")),
            "{:?}",
            d.warnings
        );
        assert!(list(&d, "relationships")
            .iter()
            .any(|r| r["name"] == "b_a_fk"));
    }

    #[test]
    fn same_physical_type_in_two_sources_of_one_kind_warns() {
        let a = meta("postgres", "a", &[("customers", &[ID])], &[]);
        let b = meta("postgres", "b", &[("customers", &[ID])], &[]);
        let d = definition_from_sources(
            &[
                SourceInput {
                    name: "a",
                    kind: "postgres",
                    meta: &a,
                },
                SourceInput {
                    name: "b",
                    kind: "postgres",
                    meta: &b,
                },
            ],
            &[],
        );
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("customers") && w.contains("already")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn empty_sources_is_an_empty_draft_not_an_error() {
        let d = definition_from_sources(&[], &[]);
        assert!(d.classes.is_empty() && d.warnings.is_empty());
        assert!(!d.publishable(), "an empty draft has nothing to publish");
        assert!(list(&d, "classes").is_empty() && list(&d, "mappings").is_empty());
    }
}
