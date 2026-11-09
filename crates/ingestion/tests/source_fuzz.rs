//! Seeded fuzz / metamorphic tests for source-schema discovery (class naming,
//! fingerprint, diff, ontology draft). Reproduce a failure with its printed
//! seed; raise the volume with AIKOQL_FUZZ_ITERS (default 2 000).

use aikoql_ingestion::{
    class_name_for_container, definition_from_sources, diff_metadata, suggest_merges, ClassMerge,
    ConnectorMetadata, ContainerInfo, FieldInfo, FieldProfile, Provenance, ReferenceInfo,
    SourceInput,
};

fn to_value(j: &serde_json::Value) -> aikoql_kernel::Value {
    use aikoql_kernel::Value;
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(Value::Int)
            .unwrap_or_else(|| Value::Float(n.as_f64().unwrap_or(0.0))),
        serde_json::Value::String(s) => Value::Text(s.clone()),
        serde_json::Value::Array(a) => Value::List(a.iter().map(to_value).collect()),
        serde_json::Value::Object(o) => {
            Value::Map(o.iter().map(|(k, v)| (k.clone(), to_value(v))).collect())
        }
    }
}

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
    fn pick_pair<'a>(&mut self, xs: &[(&'a str, &'a str)]) -> (&'a str, &'a str) {
        xs[self.below(xs.len())]
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, self.below(i + 1));
        }
    }
}

fn iters() -> u64 {
    std::env::var("AIKOQL_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000)
}

const POOL: &[&str] = &[
    "",
    " ",
    "\0",
    "\n",
    "\"",
    "'",
    "`",
    ";--",
    "'; DROP TABLE x; --",
    "`) DETACH DELETE (m //",
    "}{",
    "../x",
    "a.b",
    "a-b",
    "user data",
    "order",
    "select",
    "Order",
    "ORDER",
    "users",
    "user",
    "Customers",
    "customer_id",
    "customerId",
    "2024_sales",
    "ünï",
    "Ünïcode",
    "💥",
    "a\u{200b}b",
    "\u{202e}rtl",
    "é",
    "e\u{0301}",
    "ΑΒΓ",
    "Привет",
    "сrm",
    "crm",
];

/// Plant one related family across two sources: the same concept under a
/// synonym / variant name with variant field spellings and types, so the
/// suggestion fuzz exercises real positives, not only hostile noise.
fn plant_related(r: &mut Rng, a: &mut ConnectorMetadata, b: &mut ConnectorMetadata) {
    let (na, nb) = r.pick_pair(&[
        ("customers", "clients"),
        ("customers", "customers"),
        ("employees", "staff"),
        ("orders", "orders"),
    ]);
    let spellings: [(&str, &str, &str); 4] = [
        ("email", "email_address", "text"),
        ("created_at", "createdAt", "timestamp"),
        ("tier", "tier", "text"),
        ("age", "age", "integer"),
    ];
    let mk = |r: &mut Rng, container: &str, second: bool| {
        let mut fields = vec![FieldInfo {
            name: if second { "_id".into() } else { "id".into() },
            data_type: "integer".into(),
            is_primary_key: true,
            ..Default::default()
        }];
        for (x, y, ty) in spellings {
            if r.below(4) != 0 {
                let ty = if r.below(5) == 0 { "text" } else { ty };
                fields.push(FieldInfo {
                    name: if second { y.into() } else { x.into() },
                    data_type: ty.into(),
                    nullable: r.below(2) == 0,
                    ..Default::default()
                });
            }
        }
        ContainerInfo {
            name: container.to_string(),
            fields,
            ..Default::default()
        }
    };
    a.containers.retain(|c| c.name != na);
    b.containers.retain(|c| c.name != nb);
    let (ca, cb) = (mk(r, na, false), mk(r, nb, true));
    a.containers.push(ca);
    b.containers.push(cb);
}

fn hostile_name(r: &mut Rng) -> String {
    let mut s = String::new();
    for _ in 0..1 + r.below(3) {
        s.push_str(r.pick(POOL));
    }
    if r.below(200) == 0 {
        s.push_str(&"x".repeat(10_000));
    }
    s
}

fn random_meta(r: &mut Rng, kind: &str) -> ConnectorMetadata {
    let n = r.below(6);
    let mut containers: Vec<ContainerInfo> = Vec::new();
    for _ in 0..n {
        let name = hostile_name(r);
        if containers.iter().any(|c| c.name == name) {
            continue;
        }
        let mut fields: Vec<FieldInfo> = Vec::new();
        for _ in 0..r.below(5) {
            let fname = hostile_name(r);
            if fields.iter().any(|f| f.name == fname) {
                continue;
            }
            let observed = r.below(4) == 0;
            fields.push(FieldInfo {
                name: fname,
                data_type: r
                    .pick(&[
                        "integer",
                        "text",
                        "bigint",
                        "timestamp",
                        "bson",
                        "",
                        "jsonb",
                    ])
                    .into(),
                is_primary_key: r.below(5) == 0,
                nullable: r.below(2) == 0,
                is_unique: r.below(6) == 0,
                provenance: if observed {
                    Provenance::Observed
                } else {
                    Provenance::Declared
                },
                profile: observed.then(|| FieldProfile {
                    types: vec![r.pick(&["Int32", "String", "Null", "Double"]).into()],
                    presence_ratio: (r.below(100) as f64) / 100.0,
                    null_ratio: 0.0,
                    samples: r.below(1000) as u64,
                }),
            });
        }
        containers.push(ContainerInfo {
            name,
            fields,
            row_count: Some(r.next() % 1000),
            ..Default::default()
        });
    }
    let mut references = Vec::new();
    if !containers.is_empty() {
        for _ in 0..r.below(4) {
            let a = containers[r.below(containers.len())].name.clone();
            // sometimes dangling on purpose
            let b = if r.below(4) == 0 {
                hostile_name(r)
            } else {
                containers[r.below(containers.len())].name.clone()
            };
            references.push(ReferenceInfo {
                from_container: a,
                from_fields: (0..1 + r.below(2)).map(|_| hostile_name(r)).collect(),
                to_container: b,
                to_fields: vec![hostile_name(r)],
                name: (r.below(3) > 0).then(|| hostile_name(r)),
            });
        }
    }
    ConnectorMetadata {
        connector_type: kind.into(),
        label: hostile_name(r),
        containers,
        references,
        ..Default::default()
    }
}

#[test]
fn class_name_fuzz_never_panics_and_is_a_valid_identifier() {
    for seed in 1..=iters() * 10 {
        let mut r = Rng::new(seed);
        let input = hostile_name(&mut r);
        let out = class_name_for_container(&input);
        assert!(!out.is_empty(), "seed {seed}: empty for {input:?}");
        assert!(
            out.chars().next().is_some_and(char::is_alphabetic),
            "seed {seed}: {out:?} for {input:?}"
        );
        assert!(
            out.chars().all(char::is_alphanumeric),
            "seed {seed}: non-identifier char in {out:?} for {input:?}"
        );
        assert!(
            // the only growth: the "Thing" fallback and the one-letter digit prefix
            out.chars().count() <= input.chars().count().max(5) + 1,
            "seed {seed}: grew {input:?}"
        );
        assert_eq!(
            out,
            class_name_for_container(&input),
            "seed {seed}: not deterministic"
        );
    }
}

#[test]
fn fingerprint_permutation_invariance_fuzz() {
    for seed in 1..=iters() {
        let mut r = Rng::new(seed);
        let a = random_meta(&mut r, "postgres");
        let mut b = a.clone();
        r.shuffle(&mut b.containers);
        for c in &mut b.containers {
            r.shuffle(&mut c.fields);
            r.shuffle(&mut c.unique_groups);
            c.row_count = Some(r.next());
        }
        r.shuffle(&mut b.references);
        assert_eq!(a.fingerprint(), b.fingerprint(), "seed {seed}");
        let rt: ConnectorMetadata =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(
            a.fingerprint(),
            rt.fingerprint(),
            "seed {seed}: JSON round trip"
        );
    }
}

#[test]
fn fingerprint_single_mutation_changes_it_fuzz() {
    for seed in 1..=iters() {
        let mut r = Rng::new(seed);
        let a = random_meta(&mut r, "postgres");
        let mut b = a.clone();
        let Some(ci) = (!b.containers.is_empty()).then(|| r.below(b.containers.len())) else {
            continue;
        };
        match r.below(5) {
            0 => b.containers[ci].fields.push(FieldInfo {
                name: "__added__".into(),
                data_type: "text".into(),
                ..Default::default()
            }),
            1 if !b.containers[ci].fields.is_empty() => {
                let fi = r.below(b.containers[ci].fields.len());
                b.containers[ci].fields.remove(fi);
            }
            2 if !b.containers[ci].fields.is_empty() => {
                let fi = r.below(b.containers[ci].fields.len());
                b.containers[ci].fields[fi].data_type.push_str("_x");
            }
            3 if !b.containers[ci].fields.is_empty() => {
                let fi = r.below(b.containers[ci].fields.len());
                b.containers[ci].fields[fi].nullable ^= true;
            }
            _ => b.containers[ci].name.push_str("_renamed"),
        }
        assert_ne!(
            a.fingerprint(),
            b.fingerprint(),
            "seed {seed}: mutation not detected"
        );
        assert!(
            !diff_metadata(&a, &b).is_empty()
                || b.containers[ci].fields.len() == a.containers[ci].fields.len(),
            "seed {seed}: structural change must show in the diff unless it was a nullability flip"
        );
    }
}

#[test]
fn diff_symmetry_and_identity_fuzz() {
    for seed in 1..=iters() {
        let mut r = Rng::new(seed);
        let a = random_meta(&mut r, "postgres");
        let b = random_meta(&mut r, "postgres");
        assert!(diff_metadata(&a, &a).is_empty(), "seed {seed}");
        let (ab, ba) = (diff_metadata(&a, &b), diff_metadata(&b, &a));
        assert_eq!(ab.added_containers, ba.removed_containers, "seed {seed}");
        assert_eq!(ab.removed_containers, ba.added_containers, "seed {seed}");
    }
}

#[test]
fn draft_fuzz_always_valid() {
    for seed in 1..=iters() {
        let mut r = Rng::new(seed);
        let metas: Vec<(String, &str, ConnectorMetadata)> = (0..1 + r.below(3))
            .map(|i| {
                let kind = r.pick(&["postgres", "mongodb", "sqlite", "neo4j"]);
                (format!("src{i}"), kind, random_meta(&mut r, kind))
            })
            .collect();
        let inputs: Vec<SourceInput> = metas
            .iter()
            .map(|(n, k, m)| SourceInput {
                name: n,
                kind: k,
                meta: m,
            })
            .collect();
        let mut keys: Vec<String> = metas
            .iter()
            .flat_map(|(n, _, m)| m.containers.iter().map(move |c| format!("{n}.{}", c.name)))
            .collect();
        r.shuffle(&mut keys);
        let merges: Vec<ClassMerge> = (0..r.below(3))
            .map(|_| ClassMerge {
                a: keys
                    .get(r.below(keys.len().max(1)))
                    .cloned()
                    .unwrap_or_else(|| "nope.a".into()),
                b: if r.below(5) == 0 {
                    "nope.b".into()
                } else {
                    keys.get(r.below(keys.len().max(1)))
                        .cloned()
                        .unwrap_or_else(|| "nope.c".into())
                },
                // alignments name real, missing and colliding fields alike
                properties: (0..r.below(3))
                    .map(|_| {
                        let pick = |r: &mut Rng| {
                            ["id", "name", "email", "tier", "zz"][r.below(5)].to_string()
                        };
                        (pick(&mut r), pick(&mut r))
                    })
                    .collect(),
            })
            .collect();
        let d = definition_from_sources(&inputs, &merges);

        let names: Vec<&str> = d.classes.iter().map(|c| c.name.as_str()).collect();
        let mut uniq = names.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(
            names.len(),
            uniq.len(),
            "seed {seed}: duplicate class names {names:?}"
        );
        assert!(
            names
                .iter()
                .all(|n| n.chars().next().is_some_and(char::is_alphabetic)),
            "seed {seed}: {names:?}"
        );
        let class_ok =
            |v: &serde_json::Value| v.is_null() || v.as_str().is_some_and(|c| names.contains(&c));
        for m in d.definition["mappings"].as_array().expect("mappings") {
            assert!(
                class_ok(&m["class"]) && !m["class"].is_null(),
                "seed {seed}: mapping to unknown class {m}"
            );
        }
        for rel in d.definition["relationships"]
            .as_array()
            .expect("relationships")
        {
            assert!(
                class_ok(&rel["domain"]) && class_ok(&rel["range"]),
                "seed {seed}: dangling endpoint {rel}"
            );
        }
        // publishable: the kernel decodes and validates exactly this JSON
        let props = match to_value(&d.definition) {
            aikoql_kernel::Value::Map(m) => m,
            other => panic!("seed {seed}: definition is not an object: {other:?}"),
        };
        let decoded = aikoql_kernel::OntologyDef::from_property_map(&props);
        assert_eq!(
            d.publishable(),
            decoded.is_ok(),
            "seed {seed}: publishable() must agree with the kernel: {:?}\n{}",
            decoded.err(),
            d.definition
        );
        let containers: usize = metas.iter().map(|(_, _, m)| m.containers.len()).sum();
        assert!(
            d.classes.len() <= containers,
            "seed {seed}: more classes than containers"
        );
        // every container is reachable from exactly one class
        let covered: usize = d.classes.iter().map(|c| c.sources.len()).sum();
        assert_eq!(covered, containers, "seed {seed}: container coverage");
    }
}

#[test]
fn a_5000_container_schema_fingerprints_and_drafts_in_bounded_time() {
    let containers: Vec<ContainerInfo> = (0..5_000)
        .map(|i| ContainerInfo {
            name: format!("table_{i}"),
            fields: (0..8)
                .map(|j| FieldInfo {
                    name: format!("c{j}"),
                    data_type: "text".into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
        .collect();
    let m = ConnectorMetadata {
        connector_type: "postgres".into(),
        label: "big".into(),
        containers,
        ..Default::default()
    };
    let t = std::time::Instant::now();
    let fp = m.fingerprint();
    let d = definition_from_sources(
        &[SourceInput {
            name: "big",
            kind: "postgres",
            meta: &m,
        }],
        &[],
    );
    assert_eq!(fp.len(), 64);
    assert_eq!(d.classes.len(), 5_000);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(10),
        "took {:?}",
        t.elapsed()
    );
}

/// Suggestions are deterministic, bounded, well-formed, and applying every one
/// of them still yields a valid draft (unique class names, no panic).
#[test]
fn suggestion_fuzz_is_deterministic_well_formed_and_applicable() {
    let mut with_suggestions = 0;
    for seed in 1..=iters() {
        let mut r = Rng::new(seed);
        let mut metas: Vec<(String, &str, ConnectorMetadata)> = (0..2 + r.below(3))
            .map(|i| {
                let kind = r.pick(&["postgres", "mongodb", "sqlite", "neo4j"]);
                (format!("src{i}"), kind, random_meta(&mut r, kind))
            })
            .collect();
        if r.below(3) != 0 {
            let (left, right) = metas.split_at_mut(1);
            plant_related(&mut r, &mut left[0].2, &mut right[0].2);
        }
        let inputs: Vec<SourceInput> = metas
            .iter()
            .map(|(n, k, m)| SourceInput {
                name: n,
                kind: k,
                meta: m,
            })
            .collect();
        let first = suggest_merges(&inputs);
        with_suggestions += usize::from(!first.is_empty());
        assert_eq!(
            first,
            suggest_merges(&inputs),
            "seed {seed}: not deterministic"
        );
        assert!(first.len() <= 200, "seed {seed}");
        let source_of = |key: &str| key.split('.').next().unwrap_or("").to_string();
        let keys: Vec<String> = metas
            .iter()
            .flat_map(|(n, _, m)| m.containers.iter().map(move |c| format!("{n}.{}", c.name)))
            .collect();
        for w in first.windows(2) {
            assert!(w[0].confidence >= w[1].confidence, "seed {seed}: unordered");
        }
        for m in &first {
            assert!(
                (0.5..=1.0).contains(&m.confidence),
                "seed {seed}: {}",
                m.confidence
            );
            assert!(
                keys.contains(&m.a) && keys.contains(&m.b),
                "seed {seed}: unknown {m:?}"
            );
            assert_ne!(
                source_of(&m.a),
                source_of(&m.b),
                "seed {seed}: same-source pair {m:?}"
            );
            assert!(
                !m.evidence.is_empty(),
                "seed {seed}: a suggestion without evidence"
            );
            for p in &m.properties {
                assert!(
                    (0.0..=1.0).contains(&p.confidence) && !p.evidence.is_empty(),
                    "seed {seed}"
                );
            }
        }
        let merges: Vec<ClassMerge> = first
            .iter()
            .map(|m| ClassMerge {
                a: m.a.clone(),
                b: m.b.clone(),
                properties: m
                    .properties
                    .iter()
                    .map(|p| (p.a_field.clone(), p.b_field.clone()))
                    .collect(),
            })
            .collect();
        let d = definition_from_sources(&inputs, &merges);
        let names: Vec<&str> = d.classes.iter().map(|c| c.name.as_str()).collect();
        let mut uniq = names.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(
            names.len(),
            uniq.len(),
            "seed {seed}: duplicate class names after applying suggestions"
        );
    }
    eprintln!("seeds with suggestions: {with_suggestions}/{}", iters());
    assert!(
        with_suggestions > 0,
        "the generator never produced a suggestion: the fuzz is vacuous"
    );
}

/// Thousands of containers, including the pathological shape where every one
/// shares a name and every field: suggestions stay bounded in time and count.
#[test]
fn suggestions_are_bounded_on_huge_and_pathological_schemas() {
    let field = |n: &str, pk: bool| FieldInfo {
        name: n.to_string(),
        data_type: "text".into(),
        is_primary_key: pk,
        ..Default::default()
    };
    let big = |prefix: &str, same_name: bool| ConnectorMetadata {
        connector_type: "postgres".into(),
        label: prefix.into(),
        containers: (0..5_000)
            .map(|i| ContainerInfo {
                name: if same_name {
                    "data".into()
                } else {
                    format!("{prefix}{i}")
                },
                fields: (0..8).map(|j| field(&format!("c{j}"), j == 0)).collect(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    for same_name in [false, true] {
        let (a, b) = (big("a", same_name), big("b", same_name));
        let started = std::time::Instant::now();
        let s = suggest_merges(&[
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
        ]);
        let took = started.elapsed();
        assert!(s.len() <= 200, "{}", s.len());
        assert!(
            took < std::time::Duration::from_secs(20),
            "same_name={same_name}: {took:?} for 10k containers"
        );
    }
}
