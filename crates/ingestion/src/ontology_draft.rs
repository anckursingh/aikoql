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

fn canonical_property(name: &str) -> String {
    let c = to_snake_case(name);
    if c.is_empty() {
        name.to_string()
    } else {
        c
    }
}

/// aikoql value type of a field: the observed non-null type when there is a
/// profile, else the declared type.
fn field_value_type(f: &FieldInfo) -> &'static str {
    let observed = f
        .profile
        .as_ref()
        .and_then(|p| p.types.iter().find(|t| !t.eq_ignore_ascii_case("null")));
    connector_type_to_value_type(observed.unwrap_or(&f.data_type))
}

impl DefinitionDraft {
    /// The kernel refuses an ontology with no class: a draft from sources with
    /// no containers has nothing to publish.
    pub fn publishable(&self) -> bool {
        !self.classes.is_empty()
    }
}

pub fn definition_from_sources(sources: &[SourceInput], merges: &[ClassMerge]) -> DefinitionDraft {
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
    for m in merges {
        let idx = |k: &str| entries.iter().position(|e| e.key == k);
        match (idx(&m.a), idx(&m.b)) {
            (Some(a), Some(b)) => {
                let (ra, rb) = (find(&mut group_of, a), find(&mut group_of, b));
                let lead = leader.get(&ra).copied().unwrap_or(a);
                group_of[rb] = ra;
                leader.insert(ra, lead);
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
            let canon = canonical_property(&f.name);
            let required = f.is_primary_key || !f.nullable;
            let p = props.entry(canon.clone()).or_insert(Prop {
                types: BTreeSet::new(),
                required_everywhere: true,
            });
            p.types.insert(field_value_type(f));
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
                mappings.push(json!({
                    "source": e.kind,
                    "physical_type": e.container.name,
                    "class": class_of_entry(i),
                    "property_map": property_map,
                }));
            }
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
            let mut name = to_snake_case(&base_name);
            if let Some(existing) = rels.get(&name) {
                if existing["domain"] != json!(domain) || existing["range"] != json!(range) {
                    name = format!("{name}_{}", s.name);
                } else {
                    continue;
                }
            }
            rels.insert(
                name.clone(),
                json!({"name": name, "domain": domain, "range": range}),
            );
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
