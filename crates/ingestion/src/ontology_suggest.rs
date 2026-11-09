//! Proposals, never decisions: which containers of different sources look like
//! the same concept, and which of their fields look like the same property.
//!
//! Deterministic evidence only (names, a small synonym table, key roles,
//! value types). A suggestion carries its confidence and the evidence behind
//! it; a human applies it as an explicit [`crate::ClassMerge`] or ignores it.

use crate::connector_bridge::{class_name_for_container, ContainerInfo, FieldInfo};
use crate::ontology_draft::{
    canonical_property, declared_identity_why, field_value_type, SourceInput,
};
use std::collections::{BTreeMap, BTreeSet};

/// Two fields that look like one property.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyMatch {
    pub a_field: String,
    pub b_field: String,
    pub confidence: f32,
    pub evidence: Vec<String>,
}

/// Two `"<source>.<container>"` references that look like one concept.
#[derive(Clone, Debug, PartialEq)]
pub struct MergeSuggestion {
    pub a: String,
    pub b: String,
    pub confidence: f32,
    pub evidence: Vec<String>,
    /// Only pairs whose canonical names differ: the ones an alignment would change.
    pub properties: Vec<PropertyMatch>,
}

/// Words that mean the same entity in common schemas. Deliberately short: a
/// wrong synonym is a false suggestion, a missing one only a missed hint.
const SYNONYMS: [(&str, &str); 4] = [
    ("customer", "client"),
    ("employee", "staff"),
    ("vendor", "supplier"),
    ("person", "individual"),
];

/// Tokens that say a field is a key or reference, not what it identifies.
const GENERIC: [&str; 8] = ["id", "key", "no", "num", "number", "ref", "fk", "pk"];

/// A field name shared by this many containers is too common to be evidence.
const TOO_COMMON: usize = 200;
const MAX_SUGGESTIONS: usize = 200;
const THRESHOLD: f32 = 0.5;

struct Item<'a> {
    key: String,
    source: &'a str,
    container: &'a ContainerInfo,
    class_tokens: Vec<String>,
}

fn tokens(canonical: &str) -> Vec<String> {
    canonical
        .split(['_', '.'])
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Class tokens with synonyms folded onto their first spelling.
fn class_tokens(container: &str) -> Vec<String> {
    tokens(&canonical_property(&class_name_for_container(container)))
        .into_iter()
        .map(|t| {
            SYNONYMS
                .iter()
                .find(|(_, alt)| *alt == t)
                .map_or(t.clone(), |(main, _)| main.to_string())
        })
        .collect()
}

fn value_type_factor(a: &FieldInfo, b: &FieldInfo) -> (f32, Option<String>) {
    let (ta, tb) = (field_value_type(a).0, field_value_type(b).0);
    if ta == tb {
        (1.0, None)
    } else if matches!((ta, tb), ("Int", "Float") | ("Float", "Int")) {
        (0.9, None)
    } else {
        (0.5, Some(format!("value types differ ({ta} vs {tb})")))
    }
}

/// How alike two fields are, with the reason; `None` when not at all.
fn field_match(a: &FieldInfo, b: &FieldInfo) -> Option<(f32, Vec<String>)> {
    let (ca, cb) = (canonical_property(&a.name), canonical_property(&b.name));
    let (base, why) = if a.is_primary_key && b.is_primary_key {
        (1.0, "both are the primary key".to_string())
    } else if ca == cb {
        (1.0, format!("same name {ca}"))
    } else {
        let (ta, tb) = (tokens(&ca), tokens(&cb));
        let (small, large) = if ta.len() <= tb.len() {
            (&ta, &tb)
        } else {
            (&tb, &ta)
        };
        let contained = !small.is_empty()
            && small.iter().all(|t| large.contains(t))
            && small.iter().any(|t| !GENERIC.contains(&t.as_str()));
        if !contained
            || a.is_primary_key != b.is_primary_key && (a.is_primary_key || b.is_primary_key)
        {
            return None;
        }
        (
            0.8,
            format!("{} is contained in {}", small.join("_"), large.join("_")),
        )
    };
    let (factor, note) = value_type_factor(a, b);
    let mut evidence = vec![why];
    evidence.extend(note);
    Some((base * factor, evidence))
}

/// Greedy one-to-one field matching, best pairs first; the order of ties is
/// the order of the fields, so the result is deterministic.
fn match_fields(
    a: &ContainerInfo,
    b: &ContainerInfo,
) -> Vec<(usize, usize, f32, Vec<String>, bool)> {
    let mut cands = Vec::new();
    for (i, fa) in a.fields.iter().enumerate() {
        for (j, fb) in b.fields.iter().enumerate() {
            if let Some((score, ev)) = field_match(fa, fb) {
                if score >= THRESHOLD {
                    cands.push((i, j, score, ev, fa.is_primary_key && fb.is_primary_key));
                }
            }
        }
    }
    cands.sort_by(|x, y| y.2.total_cmp(&x.2).then(x.0.cmp(&y.0)).then(x.1.cmp(&y.1)));
    let (mut used_a, mut used_b) = (vec![false; a.fields.len()], vec![false; b.fields.len()]);
    cands
        .into_iter()
        .filter(|(i, j, ..)| {
            let free = !used_a[*i] && !used_b[*j];
            used_a[*i] |= free;
            used_b[*j] |= free;
            free
        })
        .collect()
}

fn score_pair(a: &Item, b: &Item) -> Option<MergeSuggestion> {
    let mut evidence = Vec::new();
    let name = if a.class_tokens == b.class_tokens {
        let same = class_name_for_container(&a.container.name)
            == class_name_for_container(&b.container.name);
        evidence.push(if same {
            format!(
                "class names match: {}",
                class_name_for_container(&a.container.name)
            )
        } else {
            format!(
                "synonym names: {} ~ {}",
                a.container.name.to_lowercase(),
                b.container.name.to_lowercase()
            )
        });
        if same {
            1.0
        } else {
            0.8
        }
    } else {
        0.0
    };
    let pairs = match_fields(a.container, b.container);
    // a shared key alone proves nothing: every table has one
    let informative: Vec<_> = pairs.iter().filter(|p| !p.4).collect();
    let denom = a
        .container
        .fields
        .len()
        .min(b.container.fields.len())
        .max(1) as f32;
    let overlap = informative.iter().map(|p| p.2).sum::<f32>() / denom;
    let confidence = if name > 0.0 {
        0.5 * name + 0.5 * overlap
    } else if informative.len() >= 2 && overlap >= 0.6 {
        0.8 * overlap
    } else {
        return None;
    };
    if confidence < THRESHOLD {
        return None;
    }
    if !informative.is_empty() {
        evidence.push(format!(
            "{} of {} fields align: {}",
            informative.len(),
            denom as usize,
            informative
                .iter()
                .map(|p| format!(
                    "{}~{}",
                    a.container.fields[p.0].name, b.container.fields[p.1].name
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let properties = pairs
        .iter()
        .filter(|p| {
            canonical_property(&a.container.fields[p.0].name)
                != canonical_property(&b.container.fields[p.1].name)
        })
        .map(|p| PropertyMatch {
            a_field: a.container.fields[p.0].name.clone(),
            b_field: b.container.fields[p.1].name.clone(),
            confidence: p.2,
            evidence: p.3.clone(),
        })
        .collect();
    Some(MergeSuggestion {
        a: a.key.clone(),
        b: b.key.clone(),
        confidence,
        evidence,
        properties,
    })
}

/// A proposed identity key for the rows of one container.
#[derive(Clone, Debug, PartialEq)]
pub struct IdentitySuggestion {
    /// `"<source>.<container>"`.
    pub container: String,
    pub columns: Vec<String>,
    pub confidence: f32,
    pub evidence: Vec<String>,
    /// The source itself declares this key (primary key, unique constraint, a
    /// replacing engine's sorting key): a fact, not a guess.
    pub declared: bool,
}

/// Keys for every container: what each source declares, and for the rest the
/// best guess with its evidence. A column that merely matches another
/// container's key (`visitor_id` in a per-device aggregate) is a key only when
/// the container is *about* that thing; in a table of its own events it is a
/// reference, and no key is guessed from it.
pub fn suggest_identities(sources: &[SourceInput]) -> Vec<IdentitySuggestion> {
    // (class name, single key column, container) for every declared single-column key
    let mut keys: Vec<(String, String, String)> = Vec::new();
    for s in sources {
        for c in &s.meta.containers {
            let (cols, _) = declared_identity_why(c);
            if let [one] = &cols[..] {
                keys.push((
                    class_name_for_container(&c.name),
                    one.clone(),
                    c.name.clone(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    for s in sources {
        for c in &s.meta.containers {
            let container = format!("{}.{}", s.name, c.name);
            let (cols, why) = declared_identity_why(c);
            if !cols.is_empty() {
                out.push(IdentitySuggestion {
                    container,
                    columns: cols,
                    confidence: 1.0,
                    evidence: vec![format!("declared by the source: {why}")],
                    declared: true,
                });
                continue;
            }
            let has = |n: &str| c.fields.iter().any(|f| f.name == n);
            let tokens = class_tokens(&c.name);
            let last = tokens.last().cloned().unwrap_or_default();
            let own_id = format!("{last}_id");
            let guess = if !last.is_empty() && has(&own_id) {
                Some((
                    vec![own_id.clone()],
                    0.8,
                    vec![format!("{own_id} is named for this container")],
                ))
            } else if has("id") {
                Some((
                    vec!["id".to_string()],
                    0.6,
                    vec!["a column named id".to_string()],
                ))
            } else {
                keys.iter()
                    .find(|(class, col, owner)| {
                        owner != &c.name
                            && has(col)
                            && class_tokens(class).len() < tokens.len()
                            && tokens.starts_with(&class_tokens(class))
                    })
                    .map(|(class, col, owner)| {
                        (
                            vec![col.clone()],
                            0.7,
                            vec![format!("{col} is the key of {class} ({owner}); this container is per-{class}")],
                        )
                    })
            };
            match guess {
                Some((columns, confidence, evidence)) => out.push(IdentitySuggestion {
                    container,
                    columns,
                    confidence,
                    evidence,
                    declared: false,
                }),
                None => out.push(IdentitySuggestion {
                    container,
                    columns: Vec::new(),
                    confidence: 0.0,
                    evidence: vec![
                        "no column looks like a key; declare one if rows must update in place"
                            .to_string(),
                    ],
                    declared: false,
                }),
            }
        }
    }
    out
}

/// Containers of different sources that look like one concept, best first.
///
/// shortcut: candidate pairs come from equal/synonym class names or from two or
/// more shared exact field names (names shared by 200+ containers are skipped),
/// not from comparing every pair - bounded for thousands of containers; a pair
/// that is similar only through contained field names needs a name signal too.
pub fn suggest_merges(sources: &[SourceInput]) -> Vec<MergeSuggestion> {
    let items: Vec<Item> = sources
        .iter()
        .flat_map(|s| {
            s.meta.containers.iter().map(move |c| Item {
                key: format!("{}.{}", s.name, c.name),
                source: s.name,
                container: c,
                class_tokens: class_tokens(&c.name),
            })
        })
        .collect();
    let mut by_class: BTreeMap<&[String], Vec<usize>> = BTreeMap::new();
    let mut by_field: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        by_class.entry(&it.class_tokens).or_default().push(i);
        for f in &it.container.fields {
            if !f.is_primary_key {
                by_field
                    .entry(canonical_property(&f.name))
                    .or_default()
                    .push(i);
            }
        }
    }
    let mut candidates: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut add_group = |group: &[usize]| {
        for (x, &i) in group.iter().enumerate() {
            for &j in &group[x + 1..] {
                if items[i].source != items[j].source {
                    candidates.insert((i.min(j), i.max(j)));
                }
            }
        }
    };
    by_class
        .values()
        .filter(|g| g.len() <= TOO_COMMON)
        .for_each(|g| add_group(g));
    let mut shared: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for g in by_field.values().filter(|g| g.len() <= TOO_COMMON) {
        let mut g = g.clone();
        g.dedup();
        for (x, &i) in g.iter().enumerate() {
            for &j in &g[x + 1..] {
                if items[i].source != items[j].source {
                    *shared.entry((i.min(j), i.max(j))).or_default() += 1;
                }
            }
        }
    }
    candidates.extend(shared.into_iter().filter(|(_, n)| *n >= 2).map(|(p, _)| p));

    let mut out: Vec<MergeSuggestion> = candidates
        .into_iter()
        .filter_map(|(i, j)| score_pair(&items[i], &items[j]))
        .collect();
    out.sort_by(|x, y| {
        y.confidence
            .total_cmp(&x.confidence)
            .then(x.a.cmp(&y.a))
            .then(x.b.cmp(&y.b))
    });
    out.truncate(MAX_SUGGESTIONS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector_bridge::{discover_connector_schema, ConnectorMetadata};

    type Col = (&'static str, &'static str, bool, bool, bool);
    const ID: Col = ("id", "integer", true, false, true);

    fn meta(kind: &str, label: &str, tables: &[(&str, &[Col])]) -> ConnectorMetadata {
        discover_connector_schema(kind, label, tables, &[])
    }

    fn suggest(a: &ConnectorMetadata, b: &ConnectorMetadata) -> Vec<MergeSuggestion> {
        suggest_merges(&[
            SourceInput {
                name: "crm",
                kind: "postgres",
                meta: a,
            },
            SourceInput {
                name: "shop",
                kind: "mongodb",
                meta: b,
            },
        ])
    }

    fn crm() -> ConnectorMetadata {
        meta(
            "postgres",
            "crm",
            &[
                (
                    "customers",
                    &[
                        ID,
                        ("email", "text", false, false, true),
                        ("tier", "text", false, true, false),
                        ("created_at", "timestamptz", false, true, false),
                    ],
                ),
                (
                    "orders",
                    &[
                        ID,
                        ("customer_id", "integer", false, false, false),
                        ("total", "numeric", false, false, false),
                    ],
                ),
            ],
        )
    }

    // ---- identity-key suggestions ----

    fn device_intel() -> ConnectorMetadata {
        let mut m = meta(
            "clickhouse",
            "olap",
            &[
                (
                    "devices",
                    &[
                        ("visitor_id", "String", false, false, false),
                        ("risk_score", "Float32", false, false, false),
                    ],
                ),
                (
                    "device_activity",
                    &[
                        ("visitor_id", "String", false, false, false),
                        ("visit_count", "UInt64", false, false, false),
                    ],
                ),
                (
                    "risk_decisions",
                    &[
                        ("decision_id", "String", false, false, false),
                        ("visitor_id", "String", false, false, false),
                        ("account_id", "String", false, false, false),
                    ],
                ),
                (
                    "visits",
                    &[
                        ("request_id", "String", false, false, false),
                        ("visitor_id", "String", false, false, false),
                        ("ts", "DateTime", false, false, false),
                    ],
                ),
            ],
        );
        m.containers[0]
            .properties
            .insert("engine".into(), "ReplacingMergeTree".into());
        m.containers[0]
            .properties
            .insert("sorting_key".into(), "visitor_id".into());
        m
    }

    fn id_sug(m: &ConnectorMetadata) -> std::collections::BTreeMap<String, IdentitySuggestion> {
        suggest_identities(&[SourceInput {
            name: "olap",
            kind: "clickhouse",
            meta: m,
        }])
        .into_iter()
        .map(|s| (s.container.clone(), s))
        .collect()
    }

    #[test]
    fn identity_suggestions_separate_a_key_from_a_reference() {
        let s = id_sug(&device_intel());
        // what the source declares is reported as declared, at full confidence
        let d = &s["olap.devices"];
        assert!(
            d.declared && d.columns == ["visitor_id"] && d.confidence == 1.0,
            "{d:?}"
        );
        assert!(
            d.evidence.iter().any(|e| e.contains("replacing")),
            "{:?}",
            d.evidence
        );
        // a per-device aggregate is keyed by the device's key, and says why
        let a = &s["olap.device_activity"];
        assert!(!a.declared && a.columns == ["visitor_id"], "{a:?}");
        assert!(
            a.evidence.iter().any(|e| e.contains("Device")),
            "{:?}",
            a.evidence
        );
        // visitor_id is only a REFERENCE in risk_decisions: its own <thing>_id is the key
        let r = &s["olap.risk_decisions"];
        assert_eq!(r.columns, ["decision_id"], "{r:?}");
        // a table with a foreign-key-like column and no key of its own gets no guess
        let v = &s["olap.visits"];
        assert!(v.columns == ["request_id"] || v.columns.is_empty(), "{v:?}");
        assert!(v.confidence < 1.0 && !v.declared);
        assert!(
            !v.columns.contains(&"visitor_id".to_string()),
            "visitor_id is not unique in visits"
        );
    }

    #[test]
    fn identity_suggestions_are_deterministic_and_only_for_discovered_containers() {
        let m = device_intel();
        assert_eq!(
            suggest_identities(&[SourceInput {
                name: "olap",
                kind: "clickhouse",
                meta: &m
            }]),
            suggest_identities(&[SourceInput {
                name: "olap",
                kind: "clickhouse",
                meta: &m
            }])
        );
        let empty = meta("clickhouse", "olap", &[("t", &[])]);
        assert!(
            id_sug(&empty)["olap.t"].columns.is_empty(),
            "no columns: nothing to suggest"
        );
    }

    #[test]
    fn synonym_names_with_aligned_fields_are_suggested_with_evidence() {
        let shop = meta(
            "mongodb",
            "shop",
            &[(
                "clients",
                &[
                    ("_id", "ObjectId", true, false, true),
                    ("email_address", "String", false, false, false),
                    ("age", "Int32", false, true, false),
                ],
            )],
        );
        let s = suggest(&crm(), &shop);
        assert_eq!(s.len(), 1, "{s:?}");
        let m = &s[0];
        assert_eq!(
            (m.a.as_str(), m.b.as_str()),
            ("crm.customers", "shop.clients")
        );
        assert!(
            m.confidence >= 0.5 && m.confidence < 1.0,
            "{}",
            m.confidence
        );
        assert!(
            m.evidence
                .iter()
                .any(|e| e.contains("customer") && e.contains("client")),
            "{:?}",
            m.evidence
        );
        // the email pair needs an alignment; id/_id already share a canonical name
        assert_eq!(m.properties.len(), 1, "{:?}", m.properties);
        assert_eq!(
            (
                m.properties[0].a_field.as_str(),
                m.properties[0].b_field.as_str()
            ),
            ("email", "email_address")
        );
        assert!(!m.properties[0].evidence.is_empty());
    }

    #[test]
    fn identical_class_names_with_identical_fields_are_a_confident_suggestion() {
        let other = meta(
            "mongodb",
            "shop",
            &[(
                "customers",
                &[
                    ID,
                    ("email", "text", false, false, true),
                    ("tier", "text", false, true, false),
                ],
            )],
        );
        let s = suggest(&crm(), &other);
        assert_eq!(s.len(), 1);
        assert!(s[0].confidence >= 0.8, "{}", s[0].confidence);
        assert!(
            s[0].properties.is_empty(),
            "nothing to align: names already agree"
        );
    }

    #[test]
    fn unrelated_containers_are_not_suggested() {
        let shop = meta(
            "mongodb",
            "shop",
            &[
                (
                    "products",
                    &[
                        ID,
                        ("sku", "text", false, false, true),
                        ("price", "numeric", false, false, false),
                    ],
                ),
                ("sessions", &[ID, ("token", "text", false, false, false)]),
            ],
        );
        assert!(suggest(&crm(), &shop).is_empty());
    }

    #[test]
    fn a_shared_primary_key_name_alone_is_not_evidence() {
        // every table has an `id`: orders/clients must not be suggested for that
        let shop = meta(
            "mongodb",
            "shop",
            &[(
                "clients",
                &[
                    ("_id", "ObjectId", true, false, true),
                    ("note", "String", false, true, false),
                ],
            )],
        );
        let only_orders = meta(
            "postgres",
            "crm",
            &[("orders", &[ID, ("total", "numeric", false, false, false)])],
        );
        assert!(suggest(&only_orders, &shop).is_empty());
    }

    #[test]
    fn containers_of_one_source_are_never_paired() {
        let m = meta(
            "postgres",
            "crm",
            &[
                ("customers", &[ID, ("email", "text", false, false, true)]),
                (
                    "clients",
                    &[ID, ("email_address", "text", false, false, true)],
                ),
            ],
        );
        let s = suggest_merges(&[SourceInput {
            name: "crm",
            kind: "postgres",
            meta: &m,
        }]);
        assert!(s.is_empty(), "{s:?}");
    }

    #[test]
    fn id_is_not_matched_to_a_foreign_key_that_merely_contains_it() {
        // customer_id (a foreign key in orders) is not the identity of clients
        let a = meta(
            "postgres",
            "crm",
            &[(
                "orders",
                &[ID, ("customer_id", "integer", false, false, false)],
            )],
        );
        let b = meta(
            "mongodb",
            "shop",
            &[(
                "clients",
                &[
                    ("_id", "ObjectId", true, false, true),
                    ("customer_ref", "String", false, true, false),
                ],
            )],
        );
        let s = suggest(&a, &b);
        assert!(
            s.iter().all(|m| m
                .properties
                .iter()
                .all(|p| p.a_field != "customer_id" || p.b_field != "_id")),
            "{s:?}"
        );
    }

    #[test]
    fn suggestions_are_deterministic_ordered_and_bounded() {
        let a = crm();
        let b = meta(
            "mongodb",
            "shop",
            &[
                ("customers", &[ID, ("email", "text", false, false, true)]),
                (
                    "clients",
                    &[
                        ID,
                        ("email_address", "text", false, false, true),
                        ("tier", "text", false, true, false),
                    ],
                ),
            ],
        );
        let first = suggest(&a, &b);
        for _ in 0..5 {
            assert_eq!(suggest(&a, &b), first);
        }
        assert!(first.windows(2).all(|w| w[0].confidence >= w[1].confidence));
        assert!(first.len() >= 2);
    }

    #[test]
    fn mismatched_value_types_lower_the_property_confidence() {
        let a = meta(
            "postgres",
            "crm",
            &[("customers", &[ID, ("age", "integer", false, true, false)])],
        );
        let same = meta(
            "mongodb",
            "shop",
            &[("customers", &[ID, ("age", "integer", false, true, false)])],
        );
        let diff = meta(
            "mongodb",
            "shop",
            &[("customers", &[ID, ("age", "text", false, true, false)])],
        );
        let (s1, s2) = (suggest(&a, &same), suggest(&a, &diff));
        assert!(
            s1[0].confidence > s2[0].confidence,
            "{} vs {}",
            s1[0].confidence,
            s2[0].confidence
        );
    }
}
