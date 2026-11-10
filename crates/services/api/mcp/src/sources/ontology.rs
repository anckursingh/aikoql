//! `POST /api/v1/sources/ontology`: preview and publish the ontology drafted
//! from recorded source schemas, merged into the tenant's manual ontology.

use super::{
    args_obj, audit, find_source, name_arg, store, tenant_arg, INVALID, NOTHING, NO_SCHEMA, STALE,
};
use crate::helpers::value_to_json;
use crate::{json, Kernel, J};
use aikoql_ingestion::{
    definition_from_sources_with, suggest_identities, suggest_links, suggest_merges, ClassMerge,
    ClassStatus, LinkSpec, SourceInput,
};
use std::collections::BTreeMap;

const DISCOVERED: &str = "Discovered from";

/// What publishing would do to the tenant's manual ontology.
struct Merged {
    definition: J,
    adds: Vec<String>,
    refreshed: Vec<String>,
    untouched: Vec<String>,
    mappings: Vec<J>,
    warnings: Vec<String>,
}

/// Was this class written by discovery? The description alone is not proof, since a
/// human can type the same words: it must also name containers ("<source>.<container>")
/// that the ontology's mappings tie to this very class, which only discovery writes.
fn written_by_discovery(class: &J, mappings: &[J]) -> bool {
    let Some(keys) = class["description"]
        .as_str()
        .and_then(|d| d.strip_prefix(DISCOVERED))
        .map(str::trim)
        .filter(|k| !k.is_empty())
    else {
        return false;
    };
    let name = name_of(class);
    keys.split(", ").all(|key| {
        key.split_once('.').is_some_and(|(_, container)| {
            mappings
                .iter()
                .any(|m| m["physical_type"] == container && m["class"] == name.as_str())
        })
    })
}

fn arr(v: &J, key: &str) -> Vec<J> {
    v.get(key)
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default()
}

fn name_of(v: &J) -> String {
    v.get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string()
}

/// Merge the discovered draft into the existing manual ontology: what a human
/// wrote is never changed; discovered classes are added; classes that discovery
/// itself wrote earlier are refreshed (properties unioned); existing mappings win.
fn merge(existing: Option<&J>, draft: &J) -> Merged {
    let mut warnings = Vec::new();
    let base = existing.cloned().unwrap_or_else(|| {
        json!({
            "namespace": draft["namespace"], "version": draft["version"],
            "classes": [], "relationships": [], "property_defs": [], "mappings": [],
        })
    });
    let mut def = base.clone();
    let (mut classes, mut rels, mut pdefs, mut maps) = (
        arr(&base, "classes"),
        arr(&base, "relationships"),
        arr(&base, "property_defs"),
        arr(&base, "mappings"),
    );
    let (mut adds, mut refreshed, mut added_maps) = (Vec::new(), Vec::new(), Vec::new());

    for dc in arr(draft, "classes") {
        let name = name_of(&dc);
        match classes.iter().position(|c| name_of(c) == name) {
            None => {
                classes.push(dc);
                adds.push(name);
            }
            Some(i) => {
                let ours = written_by_discovery(&classes[i], &maps);
                if !ours {
                    warnings.push(format!(
                        "class {name} already exists and was not written by discovery; kept as is"
                    ));
                    continue;
                }
                let mut props: Vec<J> = arr(&classes[i], "properties");
                for p in arr(&dc, "properties") {
                    if !props.contains(&p) {
                        props.push(p);
                    }
                }
                let changed = props != arr(&classes[i], "properties")
                    || classes[i]["description"] != dc["description"];
                classes[i]["properties"] = J::Array(props);
                classes[i]["description"] = dc["description"].clone();
                if changed {
                    refreshed.push(name);
                }
            }
        }
    }
    for pd in arr(draft, "property_defs") {
        match pdefs.iter().find(|e| name_of(e) == name_of(&pd)) {
            None => pdefs.push(pd),
            Some(e) if e["type"] != pd["type"] => warnings.push(format!(
                "property {} is {} in the existing ontology but {} in discovery; kept the existing type",
                name_of(&pd), e["type"], pd["type"]
            )),
            Some(_) => {}
        }
    }
    for r in arr(draft, "relationships") {
        if !rels.iter().any(|e| name_of(e) == name_of(&r)) {
            rels.push(r);
        }
    }
    for m in arr(draft, "mappings") {
        let key = |x: &J| (x["source"].clone(), x["physical_type"].clone());
        match maps.iter().find(|e| key(e) == key(&m)) {
            None => {
                added_maps.push(m.clone());
                maps.push(m.clone());
            }
            Some(e) if e["class"] != m["class"] => warnings.push(format!(
                "{} {} is already mapped to {}; the discovered mapping to {} was skipped",
                m["source"].as_str().unwrap_or(""),
                m["physical_type"].as_str().unwrap_or(""),
                e["class"],
                m["class"]
            )),
            Some(_) => {}
        }
        // the identity key is not an alias a human may have tuned by hand: a key the
        // owner declares later fills a mapping that has none, and corrects one on a
        // class discovery itself wrote; a key a human wrote on their own class stays
        let want = m["identity"].clone();
        if want.as_array().is_some_and(|a| !a.is_empty()) {
            if let Some(i) = maps.iter().position(|e| {
                e["source"] == m["source"]
                    && e["physical_type"] == m["physical_type"]
                    && e["class"] == m["class"]
            }) {
                if maps[i]["identity"] != want {
                    let had = maps[i]["identity"]
                        .as_array()
                        .is_some_and(|a| !a.is_empty());
                    let ours = classes
                        .iter()
                        .find(|c| c["name"] == maps[i]["class"])
                        .is_some_and(|c| written_by_discovery(c, &maps));
                    if !had || ours {
                        maps[i]["identity"] = want;
                    } else {
                        warnings.push(format!(
                            "{} {} already has an identity key written by hand; kept as is",
                            m["source"].as_str().unwrap_or(""),
                            m["physical_type"].as_str().unwrap_or("")
                        ));
                    }
                }
            }
        }
        // links: a name not yet on the mapping is added; the same name pointing elsewhere
        // is corrected on a class discovery wrote and kept (with a warning) on a human's;
        // a draft that says nothing about links never removes any
        if let Some(wanted) = m["links"].as_array().filter(|a| !a.is_empty()) {
            if let Some(i) = maps.iter().position(|e| {
                e["source"] == m["source"]
                    && e["physical_type"] == m["physical_type"]
                    && e["class"] == m["class"]
            }) {
                let ours = classes
                    .iter()
                    .find(|c| c["name"] == maps[i]["class"])
                    .is_some_and(|c| written_by_discovery(c, &maps));
                let mut have = maps[i]["links"].as_array().cloned().unwrap_or_default();
                let mut changed = false;
                for l in wanted {
                    match have.iter().position(|e| e["name"] == l["name"]) {
                        None => {
                            have.push(l.clone());
                            changed = true;
                        }
                        Some(j) if have[j] == *l => {}
                        Some(j) if ours => {
                            have[j] = l.clone();
                            changed = true;
                        }
                        Some(_) => warnings.push(format!(
                            "{} {}: link {} already points elsewhere and was written by hand; kept as is",
                            m["source"].as_str().unwrap_or(""),
                            m["physical_type"].as_str().unwrap_or(""),
                            l["name"].as_str().unwrap_or("")
                        )),
                    }
                }
                if changed {
                    maps[i]["links"] = J::Array(have);
                }
            }
        }
    }
    let untouched = classes
        .iter()
        .map(name_of)
        .filter(|n| !adds.contains(n) && !refreshed.contains(n))
        .collect();
    def["classes"] = J::Array(classes);
    def["relationships"] = J::Array(rels);
    def["property_defs"] = J::Array(pdefs);
    def["mappings"] = J::Array(maps);
    Merged {
        definition: def,
        adds,
        refreshed,
        untouched,
        mappings: added_maps,
        warnings,
    }
}

const MERGE_SHAPE: &str = "merges must be a list of [\"source.container\", \"source.container\"] pairs or {a, b, properties: [[field of a, field of b], ...]} objects";

/// A merge is a pair of `source.container` references, or an object adding
/// the property alignments the reviewer accepted.
fn parse_merge(p: &J) -> Result<ClassMerge, String> {
    let bad = || format!("{INVALID}{MERGE_SHAPE}");
    let text = |v: &J| v.as_str().filter(|s| s.len() <= 512).map(str::to_string);
    if let Some([J::String(x), J::String(y)]) = p.as_array().map(Vec::as_slice) {
        return Ok(ClassMerge {
            a: x.clone(),
            b: y.clone(),
            properties: Vec::new(),
        });
    }
    let o = p.as_object().ok_or_else(bad)?;
    if o.keys()
        .any(|k| !matches!(k.as_str(), "a" | "b" | "properties"))
    {
        return Err(bad());
    }
    let (a, b) = (
        o.get("a").and_then(text).ok_or_else(bad)?,
        o.get("b").and_then(text).ok_or_else(bad)?,
    );
    let properties = match o.get("properties") {
        None | Some(J::Null) => Vec::new(),
        Some(J::Array(pairs)) if pairs.len() <= 200 => pairs
            .iter()
            .map(|pair| match pair.as_array().map(Vec::as_slice) {
                Some([x, y]) => text(x).zip(text(y)).ok_or_else(bad),
                _ => Err(bad()),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(bad()),
    };
    Ok(ClassMerge { a, b, properties })
}

pub(crate) fn tool_sources_ontology(k: &Kernel, args: &J, db_path: &str) -> Result<J, String> {
    let result = run(k, args);
    let detail = |what: &str| {
        format!(
            "{what} sources={}",
            args.get("sources")
                .map(|s| s.to_string())
                .unwrap_or_default()
        )
    };
    match &result {
        Ok(r) if r["preview"] == true => audit(db_path, args, "previewed", &detail("preview")),
        Ok(r) => audit(
            db_path,
            args,
            "published",
            &format!("{} saved={}", detail("apply"), r["saved"]),
        ),
        Err(e) => audit(
            db_path,
            args,
            "error",
            &format!(
                "{} reason={}",
                detail("ontology"),
                e.chars().take(50).collect::<String>()
            ),
        ),
    }
    result
}

fn run(k: &Kernel, args: &J) -> Result<J, String> {
    let o = args_obj(args)?;
    let tenant = tenant_arg(o)?;
    let names: Vec<String> = match o.get("sources") {
        Some(J::Array(a)) if !a.is_empty() && a.len() <= 50 => a
            .iter()
            .map(|n| name_arg(Some(n), "sources[]"))
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(format!(
                "{INVALID}sources must be a list of 1-50 source names"
            ))
        }
    };
    let preview = match o.get("preview") {
        None | Some(J::Null) => true,
        Some(J::Bool(b)) => *b,
        _ => return Err(format!("{INVALID}preview must be true or false")),
    };
    let merges: Vec<ClassMerge> = match o.get("merges") {
        None | Some(J::Null) => Vec::new(),
        Some(J::Array(a)) if a.len() <= 1_000 => {
            a.iter().map(parse_merge).collect::<Result<_, _>>()?
        }
        _ => return Err(format!("{INVALID}merges must be a list of merges")),
    };
    // identity keys the owner declares: {"source.container": ["column", ...]}
    let identities: BTreeMap<String, Vec<String>> = match o.get("identities") {
        None | Some(J::Null) => BTreeMap::new(),
        Some(J::Object(m)) if m.len() <= 1_000 => m
            .iter()
            .map(|(k, v)| match v.as_array() {
                Some(cols)
                    if k.len() <= 512
                        && (1..=32).contains(&cols.len())
                        && cols.iter().all(|c| c.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 256)) =>
                {
                    Ok((k.clone(), cols.iter().filter_map(|c| c.as_str().map(str::to_string)).collect()))
                }
                _ => Err(format!(
                    "{INVALID}identities must map \"source.container\" to a list of 1-32 column names"
                )),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(format!("{INVALID}identities must be an object")),
    };
    // links the owner declares: [{"name", "from": "source.container.column", "to": "..."}]
    let links: Vec<LinkSpec> = match o.get("relationships") {
        None | Some(J::Null) => Vec::new(),
        Some(J::Array(a)) if a.len() <= 200 => a
            .iter()
            .map(|r| {
                let bad = || format!("{INVALID}relationships must be objects {{name, from, to}} of source.container.column references");
                let o = r.as_object().ok_or_else(bad)?;
                if o.keys().any(|k| !matches!(k.as_str(), "name" | "from" | "to")) {
                    return Err(bad());
                }
                let text = |k: &str| o.get(k).and_then(J::as_str).filter(|s| s.len() <= 512).map(str::to_string);
                Ok(LinkSpec { name: text("name").ok_or_else(bad)?, from: text("from").ok_or_else(bad)?, to: text("to").ok_or_else(bad)? })
            })
            .collect::<Result<_, String>>()?,
        _ => return Err(format!("{INVALID}relationships must be a list of at most 200")),
    };
    let given: BTreeMap<String, String> = match o.get("fingerprints") {
        None | Some(J::Null) => BTreeMap::new(),
        Some(J::Object(m)) => m
            .iter()
            .map(|(k, v)| {
                v.as_str()
                    .map(|s| (k.clone(), s.to_string()))
                    .ok_or_else(|| {
                        format!("{INVALID}fingerprints must map source names to strings")
                    })
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(format!("{INVALID}fingerprints must be an object")),
    };
    let expected_version =
        match o.get("expected_version") {
            None | Some(J::Null) => None,
            Some(v) => Some(v.as_u64().ok_or_else(|| {
                format!("{INVALID}expected_version must be a non-negative integer")
            })?),
        };

    let mut unique = names.clone();
    unique.dedup();
    let mut snapshots = Vec::new();
    for n in &unique {
        let src = find_source(n, tenant.as_deref())?;
        let meta = store::latest_snapshot(k, &src)?
            .ok_or_else(|| format!("source {n} has {NO_SCHEMA} - discover it first"))?;
        snapshots.push((src, meta));
    }
    let inputs: Vec<SourceInput> = snapshots
        .iter()
        .map(|(s, m)| SourceInput {
            name: &s.name,
            kind: s.kind.as_str(),
            meta: m,
        })
        .collect();
    let draft = definition_from_sources_with(&inputs, &merges, &identities, &links);
    if !draft.publishable() {
        return Err(format!("{NOTHING}: no containers were discovered"));
    }
    let fingerprints: BTreeMap<String, String> = snapshots
        .iter()
        .map(|(s, m)| (s.name.clone(), m.fingerprint()))
        .collect();

    // whatever is active is the base, whatever its provenance: an auto-discovered
    // ontology is shadowed by the manual one this publish creates, so it must be carried over
    let existing = crate::http::active_ontology_for(k, tenant.as_deref());
    let base = existing.as_ref().map_or("none", |(_, src)| *src);
    let existing_json = existing.as_ref().map(|(ko, _)| {
        J::Object(
            ko.properties
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        )
    });
    let active_version = existing.as_ref().map(|(ko, _)| ko.version);
    let merged = merge(existing_json.as_ref(), &draft.definition);

    let mut warnings = draft.warnings.clone();
    warnings.extend(merged.warnings.clone());
    let classes: Vec<J> = draft
        .classes
        .iter()
        .map(|c| match &c.status {
            ClassStatus::New => json!({"name": c.name, "sources": c.sources, "status": "new"}),
            ClassStatus::PossibleDuplicate { with } => {
                json!({"name": c.name, "sources": c.sources, "status": "possible_duplicate", "with": with})
            }
        })
        .collect();
    // advice only: pairs the reviewer already merged are not offered again
    let suggestions: Vec<J> = suggest_merges(&inputs)
        .into_iter()
        .filter(|s| {
            !merges
                .iter()
                .any(|m| (m.a == s.a && m.b == s.b) || (m.a == s.b && m.b == s.a))
        })
        .map(|s| {
            json!({
                "a": s.a, "b": s.b, "confidence": s.confidence, "evidence": s.evidence,
                "properties": s.properties.iter().map(|p| json!({
                    "a_field": p.a_field, "b_field": p.b_field,
                    "confidence": p.confidence, "evidence": p.evidence,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    // keys: what each source declares, and the best guess (with evidence) for the rest
    let identity_suggestions: Vec<J> = suggest_identities(&inputs)
        .into_iter()
        .map(|s| {
            json!({"container": s.container, "columns": s.columns, "confidence": s.confidence,
                   "evidence": s.evidence, "declared": s.declared})
        })
        .collect();
    // links: proposed from the keys; one already declared is not offered again
    let link_suggestions: Vec<J> = suggest_links(&inputs, &identities)
        .into_iter()
        .filter(|s| !links.iter().any(|l| l.from == s.from && l.name == s.name))
        .map(|s| json!({"name": s.name, "from": s.from, "to": s.to, "confidence": s.confidence, "evidence": s.evidence}))
        .collect();
    let summary = json!({
        "links": link_suggestions,
        "identities": identity_suggestions,
        "classes": classes, "adds": merged.adds, "refreshed": merged.refreshed,
        "untouched": merged.untouched, "mappings": merged.mappings, "warnings": warnings,
        "fingerprints": fingerprints, "active_version": active_version, "base": base,
        "suggestions": suggestions,
    });
    if preview {
        let mut out = summary;
        out["preview"] = json!(true);
        return Ok(out);
    }

    // apply: the reviewer must have seen exactly this schema
    if fingerprints.iter().any(|(n, fp)| given.get(n) != Some(fp)) {
        if given.is_empty() {
            return Err(format!(
                "{INVALID}fingerprints from the preview are required to publish"
            ));
        }
        return Err(STALE.to_string());
    }
    if let Some(current) = active_version {
        match expected_version {
            None => {
                return Err(format!(
                    "expected_version required: the current version is {current}"
                ))
            }
            Some(v) if v != current => {
                return Err(format!(
                    "expected_version conflict: the current version is {current}"
                ))
            }
            Some(_) => {}
        }
    }
    let mut out = summary;
    out["preview"] = json!(false);
    if existing_json.as_ref() == Some(&merged.definition) {
        out["saved"] = json!(false);
        out["version"] = json!(active_version);
        return Ok(out);
    }
    let mut save = json!({"definition": merged.definition});
    if let Some(t) = &tenant {
        save["tenant"] = json!(t);
    }
    // the pin guards a manual ontology; an auto one has no manual KO to update, so a
    // racing publisher must be refused by the save itself rather than matched by number
    if let (Some(v), "manual") = (expected_version, base) {
        save["expected_version"] = json!(v);
    }
    for key in ["subject", "roles"] {
        if let Some(v) = args.get(key) {
            save[key] = v.clone();
        }
    }
    let saved = crate::tools::tool_save_ontology(k, &save)?;
    out["saved"] = json!(true);
    out["koid"] = saved["koid"].clone();
    out["version"] = saved["version"].clone();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use crate::sources::service::tool_sources_discover;
    use crate::sources::testkit::*;
    use crate::sources::{with_test_sources, STALE};

    fn two_sources<R>(crm: &std::path::Path, shop: &std::path::Path, f: impl FnOnce() -> R) -> R {
        with_test_sources(
            vec![sqlite_source("crm", None), sqlite_source("shop", None)],
            vec![("crm".into(), conn(crm)), ("shop".into(), conn(shop))],
            f,
        )
    }

    fn discover(k: &Kernel, name: &str) -> J {
        tool_sources_discover(k, &json!({"source": name}), "x").unwrap()
    }

    fn head(k: &Kernel) -> u64 {
        k.journal_head().unwrap().0
    }

    fn manual(k: &Kernel) -> Option<(aikoql_kernel::KnowledgeObject, aikoql_kernel::OntologyDef)> {
        let (ko, source) = crate::http::active_ontology_for(k, None)?;
        assert_eq!(source, "manual");
        let def = aikoql_kernel::OntologyDef::from_ko(&ko).unwrap();
        Some((ko, def))
    }

    fn apply_args(preview: &J, extra: J) -> J {
        let mut a =
            json!({"sources": ["crm"], "preview": false, "fingerprints": preview["fingerprints"]});
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        a
    }

    #[test]
    fn a_merge_is_a_pair_or_an_object_with_alignments_and_nothing_else() {
        let ok = parse_merge(&json!(["a.x", "b.y"])).unwrap();
        assert_eq!(
            (ok.a.as_str(), ok.b.as_str(), ok.properties.len()),
            ("a.x", "b.y", 0)
        );
        let m = parse_merge(
            &json!({"a": "a.x", "b": "b.y", "properties": [["email", "email_address"]]}),
        )
        .unwrap();
        assert_eq!(
            m.properties,
            [("email".to_string(), "email_address".to_string())]
        );
        assert!(parse_merge(&json!({"a": "a.x", "b": "b.y"}))
            .unwrap()
            .properties
            .is_empty());
        for bad in [
            json!(["only-one"]),
            json!(["a", "b", "c"]),
            json!([1, 2]),
            json!("a.x"),
            json!(null),
            json!({"a": "a.x"}),
            json!({"a": "a.x", "b": 7}),
            json!({"a": "a.x", "b": "b.y", "extra": 1}),
            json!({"a": "a.x", "b": "b.y", "properties": "email"}),
            json!({"a": "a.x", "b": "b.y", "properties": [["only"]]}),
            json!({"a": "a.x", "b": "b.y", "properties": [["x", 1]]}),
            json!({"a": "a.x", "b": "b.y", "properties": [["x", "y", "z"]]}),
            json!({"a": "x".repeat(513), "b": "b.y"}),
            json!({"a": "a.x", "b": "b.y", "properties": (0..201).map(|_| json!(["x", "y"])).collect::<Vec<_>>()}),
        ] {
            let e = parse_merge(&bad).unwrap_err();
            assert!(e.starts_with(INVALID), "{bad}: {e}");
        }
    }

    #[test]
    fn preview_offers_suggestions_and_stops_offering_an_applied_one() {
        let (k, crm, shop) = (kernel(), crm_db("sg"), shop_db("sg"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            discover(&k, "shop");
            let p = tool_sources_ontology(
                &k,
                &json!({"sources": ["crm", "shop"], "preview": true}),
                "x",
            )
            .unwrap();
            let all = p["suggestions"].as_array().unwrap();
            assert!(
                all.iter()
                    .all(|s| s["a"].as_str().unwrap().split('.').next()
                        != s["b"].as_str().unwrap().split('.').next()),
                "never a same-source pair: {all:?}"
            );
            let applied = tool_sources_ontology(
                &k,
                &json!({"sources": ["crm", "shop"], "preview": true, "merges": [["crm.customers", "shop.clients"]]}),
                "x",
            )
            .unwrap();
            assert!(applied["suggestions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| { !(s["a"] == "crm.customers" && s["b"] == "shop.clients") }));
        });
    }

    // ---- an identity key declared AFTER the first publish (found on the case study) ----

    fn onto(class_desc: &str, identity: Option<Vec<&str>>) -> J {
        let mut m =
            json!({"source": "clickhouse", "physical_type": "t", "class": "T", "property_map": {}});
        if let Some(cols) = identity {
            m["identity"] = json!(cols);
        }
        json!({"namespace": "sources", "version": "1.0", "relationships": [], "property_defs": [],
               "classes": [{"name": "T", "description": class_desc, "properties": []}],
               "mappings": [m]})
    }

    fn draft_with(identity: Option<Vec<&str>>) -> J {
        let mut d = onto("Discovered from s.t", identity);
        d["mappings"][0]["property_map"] = json!({});
        d
    }

    fn identity_of(m: &Merged) -> J {
        m.definition["mappings"][0]["identity"].clone()
    }

    // ---- links published after the first publish ----

    fn lk(name: &str, col: &str) -> J {
        json!({"name": name, "column": col, "target_source": "postgres", "target_type": "accounts", "target_column": "id"})
    }

    fn with_links(mut o: J, links: Vec<J>) -> J {
        o["mappings"][0]["links"] = J::Array(links);
        o
    }

    fn links_of(m: &Merged) -> Vec<J> {
        m.definition["mappings"][0]["links"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn links_declared_later_are_added_and_a_humans_are_kept() {
        // added to a mapping that had none, and alongside existing ones
        let plain = onto("Discovered from s.t", None);
        let m = merge(
            Some(&plain),
            &with_links(draft_with(None), vec![lk("for_account", "account_id")]),
        );
        assert_eq!(links_of(&m), vec![lk("for_account", "account_id")]);
        let have = with_links(
            onto("Discovered from s.t", None),
            vec![lk("for_account", "account_id")],
        );
        let m = merge(
            Some(&have),
            &with_links(draft_with(None), vec![lk("for_owner", "owner_id")]),
        );
        assert_eq!(
            links_of(&m).len(),
            2,
            "a new link name is added, the old one stays"
        );
        // a draft that says nothing about links never removes any
        assert_eq!(
            links_of(&merge(Some(&have), &draft_with(None))),
            vec![lk("for_account", "account_id")]
        );
        // the same link again is not a change
        let same = merge(
            Some(&have),
            &with_links(draft_with(None), vec![lk("for_account", "account_id")]),
        );
        assert_eq!(same.definition, have);
        // the same name pointing elsewhere: corrected on discovery's class, kept (with a warning) on a human's
        let ours = merge(
            Some(&have),
            &with_links(draft_with(None), vec![lk("for_account", "acct")]),
        );
        assert_eq!(links_of(&ours), vec![lk("for_account", "acct")]);
        let theirs_base = with_links(
            onto("our own T", None),
            vec![lk("for_account", "account_id")],
        );
        let theirs = merge(
            Some(&theirs_base),
            &with_links(draft_with(None), vec![lk("for_account", "acct")]),
        );
        assert_eq!(links_of(&theirs), vec![lk("for_account", "account_id")]);
        assert!(
            theirs
                .warnings
                .iter()
                .any(|w| w.contains("link") && w.contains("kept")),
            "{:?}",
            theirs.warnings
        );
    }

    #[test]
    fn a_key_declared_later_fills_a_mapping_that_had_none() {
        let existing = onto("Discovered from s.t", None);
        let m = merge(Some(&existing), &draft_with(Some(vec!["visitor_id"])));
        assert_eq!(identity_of(&m), json!(["visitor_id"]), "{}", m.definition);
        assert_ne!(
            m.definition, existing,
            "the publish must register as a change"
        );
    }

    #[test]
    fn a_key_may_be_corrected_on_a_class_discovery_wrote_but_never_over_a_humans() {
        // discovery's own class: the new declaration replaces the old
        let ours = onto("Discovered from s.t", Some(vec!["a"]));
        assert_eq!(
            identity_of(&merge(Some(&ours), &draft_with(Some(vec!["b"])))),
            json!(["b"])
        );
        // a hand-written class keeps its key, and says so
        let theirs = onto("our own T", Some(vec!["a"]));
        let m = merge(Some(&theirs), &draft_with(Some(vec!["b"])));
        assert_eq!(identity_of(&m), json!(["a"]));
        assert!(
            m.warnings
                .iter()
                .any(|w| w.contains("identity") && w.contains("kept")),
            "{:?}",
            m.warnings
        );
        // a draft with no key never erases one
        let kept = merge(Some(&ours), &draft_with(None));
        assert_eq!(identity_of(&kept), json!(["a"]));
        // and an unchanged key is not a change
        let same = merge(Some(&ours), &draft_with(Some(vec!["a"])));
        assert_eq!(same.definition, ours);
    }

    #[test]
    fn preview_writes_nothing_and_describes_the_draft() {
        let (k, crm, shop) = (kernel(), crm_db("p1"), shop_db("p1"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            discover(&k, "shop");
            let before = head(&k);
            let p = tool_sources_ontology(
                &k,
                &json!({"sources": ["crm", "shop"], "preview": true}),
                "x",
            )
            .unwrap();
            assert_eq!(head(&k), before, "a preview writes nothing");
            let classes: Vec<&str> = p["classes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["name"].as_str().unwrap())
                .collect();
            assert_eq!(classes, ["Customer", "Order", "Client"]);
            assert_eq!(p["classes"][0]["status"], "new");
            assert_eq!(p["adds"], json!(["Customer", "Order", "Client"]));
            assert_eq!(p["fingerprints"].as_object().unwrap().len(), 2);
            assert!(p["mappings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["physical_type"] == "customers" && m["class"] == "Customer"));
            assert!(p["active_version"].is_null());
        });
    }

    #[test]
    fn publishing_requires_the_fingerprint_the_reviewer_saw() {
        let (k, crm, shop) = (kernel(), crm_db("p2"), shop_db("p2"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            make(&crm, "ALTER TABLE customers ADD COLUMN phone TEXT;");
            discover(&k, "crm"); // the schema moved after the preview
            let e = tool_sources_ontology(&k, &apply_args(&p, json!({})), "x").unwrap_err();
            assert_eq!(e, STALE);
            assert!(manual(&k).is_none(), "nothing was published");
            let e = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": false}), "x")
                .unwrap_err();
            assert!(
                e.starts_with(crate::sources::INVALID),
                "apply without fingerprints: {e}"
            );
        });
    }

    #[test]
    fn publishing_creates_the_ontology_and_the_class_maps_to_the_physical_tables() {
        let (k, crm, shop) = (kernel(), crm_db("p3"), shop_db("p3"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            let r = tool_sources_ontology(&k, &apply_args(&p, json!({})), "x").unwrap();
            assert_eq!(r["saved"], true);
            let (_, def) = manual(&k).expect("a manual ontology exists");
            assert!(def.classes.contains_key("Customer") && def.classes.contains_key("Order"));
            assert!(def.mappings.iter().any(|m| m.class == "Customer"
                && m.physical_type == "customers"
                && m.source == "sqlite"));
            assert!(def
                .relationships
                .values()
                .any(|r| r.domain.as_deref() == Some("Order")
                    && r.range.as_deref() == Some("Customer")));
        });
    }

    #[test]
    fn publishing_never_touches_what_a_human_wrote() {
        let (k, crm, shop) = (kernel(), crm_db("p4"), shop_db("p4"));
        two_sources(&crm, &shop, || {
            crate::tools::tool_save_ontology(
                &k,
                &json!({"definition": {
                    "namespace": "acme", "version": "2.0",
                    "classes": [{"name": "Widget", "description": "hand written", "properties": ["sku"]},
                                {"name": "Customer", "description": "our own Customer", "properties": ["tier"]}],
                    "relationships": [{"name": "made_of", "domain": "Widget", "range": "Widget"}],
                    "property_defs": [{"name": "sku", "type": "Text", "required": true}, {"name": "tier", "type": "Int"}],
                    "mappings": [],
                }}),
            )
            .unwrap();
            let (ko, before) = manual(&k).unwrap();
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            assert_eq!(p["active_version"], ko.version);
            assert!(p["untouched"]
                .as_array()
                .unwrap()
                .contains(&json!("Widget")));
            assert!(
                p["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|w| w.as_str().unwrap().contains("Customer")),
                "{}",
                p["warnings"]
            );
            let e = tool_sources_ontology(&k, &apply_args(&p, json!({})), "x").unwrap_err();
            assert!(
                e.starts_with("expected_version required"),
                "a CAS version is mandatory over an existing ontology: {e}"
            );
            tool_sources_ontology(
                &k,
                &apply_args(&p, json!({"expected_version": ko.version})),
                "x",
            )
            .unwrap();
            let (_, after) = manual(&k).unwrap();
            assert_eq!(after.namespace, "acme");
            assert_eq!(
                after.classes["Widget"], before.classes["Widget"],
                "hand-written class untouched"
            );
            assert_eq!(
                after.classes["Customer"], before.classes["Customer"],
                "a same-named human class wins"
            );
            assert_eq!(after.property_defs["sku"], before.property_defs["sku"]);
            assert!(
                after.classes.contains_key("Order"),
                "discovered classes are added"
            );
            assert!(after.relationships.contains_key("made_of"));
        });
    }

    #[test]
    fn a_stale_expected_version_is_a_conflict() {
        let (k, crm, shop) = (kernel(), crm_db("p5"), shop_db("p5"));
        two_sources(&crm, &shop, || {
            crate::tools::tool_save_ontology(&k, &json!({"definition": {"namespace": "n", "version": "1", "classes": [{"name": "Widget"}]}})).unwrap();
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            let e =
                tool_sources_ontology(&k, &apply_args(&p, json!({"expected_version": 99})), "x")
                    .unwrap_err();
            assert!(
                e.contains("VERSION_CONFLICT") || e.starts_with("expected_version conflict"),
                "{e}"
            );
        });
    }

    #[test]
    fn applying_twice_is_idempotent_and_reapplying_refreshes_only_discovered_classes() {
        let (k, crm, shop) = (kernel(), crm_db("p6"), shop_db("p6"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            tool_sources_ontology(&k, &apply_args(&p, json!({})), "x").unwrap();
            let (ko, def1) = manual(&k).unwrap();
            let before = head(&k);
            let again = tool_sources_ontology(
                &k,
                &apply_args(&p, json!({"expected_version": ko.version})),
                "x",
            )
            .unwrap();
            assert_eq!(again["saved"], false, "identical content writes nothing");
            assert_eq!(head(&k), before);
            // schema drift: new column, nothing changes until a human re-approves
            make(&crm, "ALTER TABLE customers ADD COLUMN phone TEXT;");
            let d = discover(&k, "crm");
            assert_eq!(d["diff"]["changed"][0]["added_fields"], json!(["phone"]));
            let (ko2, _) = manual(&k).unwrap();
            assert_eq!(
                ko2.version, ko.version,
                "drift alone never rewrites the ontology"
            );
            let p2 = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            assert!(
                p2["refreshed"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("Customer")),
                "{}",
                p2["refreshed"]
            );
            tool_sources_ontology(
                &k,
                &apply_args(&p2, json!({"expected_version": ko2.version})),
                "x",
            )
            .unwrap();
            let (_, def2) = manual(&k).unwrap();
            assert!(!def1.classes["Customer"]
                .properties
                .contains(&"phone".to_string()));
            assert!(def2.classes["Customer"]
                .properties
                .contains(&"phone".to_string()));
            assert!(def2.property_defs.contains_key("phone"));
        });
    }

    #[test]
    fn an_explicit_merge_makes_one_class_with_two_mappings() {
        let (k, crm, shop) = (kernel(), crm_db("p7"), shop_db("p7"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            discover(&k, "shop");
            let merges = json!([["crm.customers", "shop.clients"]]);
            let p = tool_sources_ontology(
                &k,
                &json!({"sources": ["crm", "shop"], "preview": true, "merges": merges}),
                "x",
            )
            .unwrap();
            let mut a = apply_args(&p, json!({"sources": ["crm", "shop"], "merges": merges}));
            a["fingerprints"] = p["fingerprints"].clone();
            tool_sources_ontology(&k, &a, "x").unwrap();
            let (_, def) = manual(&k).unwrap();
            assert!(def.classes.contains_key("Customer") && !def.classes.contains_key("Client"));
            let maps: Vec<&str> = def
                .mappings
                .iter()
                .filter(|m| m.class == "Customer")
                .map(|m| m.physical_type.as_str())
                .collect();
            assert_eq!(maps.len(), 2, "{maps:?}");
            assert!(maps.contains(&"customers") && maps.contains(&"clients"));
        });
    }

    #[test]
    fn nothing_to_publish_is_a_clean_client_error() {
        let k = kernel();
        let undiscovered = tmp_db("noneyet");
        make(&undiscovered, "SELECT 1;");
        with_test_sources(
            vec![sqlite_source("crm", None)],
            vec![("crm".into(), conn(&undiscovered))],
            || {
                let e =
                    tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                        .unwrap_err();
                assert!(
                    e.contains(crate::sources::NO_SCHEMA),
                    "not discovered yet: {e}"
                );
                assert_eq!(crate::sources::status_for(&e), Some(400));
            },
        );
        let zero = tmp_db("zero");
        make(&zero, "SELECT 1;"); // a valid database with no tables
        with_test_sources(
            vec![sqlite_source("z", None)],
            vec![("z".into(), conn(&zero))],
            || {
                tool_sources_discover(&k, &json!({"source": "z"}), "x").unwrap();
                let e = tool_sources_ontology(&k, &json!({"sources": ["z"], "preview": true}), "x")
                    .unwrap_err();
                assert!(e.starts_with(crate::sources::NOTHING), "{e}");
                assert_eq!(crate::sources::status_for(&e), Some(400));
            },
        );
    }

    #[test]
    fn bad_arguments_are_client_errors() {
        let (k, crm, shop) = (kernel(), crm_db("p9"), shop_db("p9"));
        two_sources(&crm, &shop, || {
            discover(&k, "crm");
            for bad in [
                json!({}),
                json!({"sources": []}),
                json!({"sources": "crm"}),
                json!({"sources": [1]}),
                json!({"sources": ["crm"], "merges": "x"}),
                json!({"sources": ["crm"], "merges": [["only-one"]]}),
                json!({"sources": ["crm"], "merges": [[1, 2]]}),
                json!({"sources": ["crm"], "preview": "yes"}),
                json!({"sources": ["crm"], "fingerprints": [1]}),
                json!({"sources": (0..60).map(|i| format!("s{i}")).collect::<Vec<_>>()}),
            ] {
                let e = tool_sources_ontology(&k, &bad, "x").unwrap_err();
                assert_eq!(crate::sources::status_for(&e), Some(400), "{bad} -> {e}");
            }
            assert_eq!(
                tool_sources_ontology(&k, &json!({"sources": ["nope"], "preview": true}), "x")
                    .unwrap_err(),
                crate::sources::NOT_FOUND
            );
        });
    }

    #[test]
    fn publish_fuzz_random_merges_always_save_a_valid_ontology() {
        let iters = std::env::var("AIKOQL_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_000)
            / 20;
        for seed in 1..=iters.max(10) {
            let (k, crm, shop) = (
                kernel(),
                crm_db(&format!("f{seed}")),
                shop_db(&format!("f{seed}")),
            );
            let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut rnd = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            two_sources(&crm, &shop, || {
                discover(&k, "crm");
                discover(&k, "shop");
                let refs = ["crm.customers", "crm.orders", "shop.clients", "nope.x"];
                let merges: Vec<J> = (0..rnd() % 3)
                    .map(|_| json!([refs[(rnd() % 4) as usize], refs[(rnd() % 4) as usize]]))
                    .collect();
                let base = json!({"sources": ["crm", "shop"], "merges": merges});
                let mut pv = base.clone();
                pv["preview"] = json!(true);
                let p = tool_sources_ontology(&k, &pv, "x")
                    .unwrap_or_else(|e| panic!("seed {seed}: preview {e}"));
                let mut ap = base.clone();
                ap["preview"] = json!(false);
                ap["fingerprints"] = p["fingerprints"].clone();
                tool_sources_ontology(&k, &ap, "x")
                    .unwrap_or_else(|e| panic!("seed {seed}: apply {e}"));
                let (_, def) = manual(&k).unwrap_or_else(|| panic!("seed {seed}: nothing saved"));
                assert!(!def.classes.is_empty(), "seed {seed}");
                assert!(
                    def.mappings
                        .iter()
                        .all(|m| def.classes.contains_key(&m.class)),
                    "seed {seed}: mapping to a missing class"
                );
            });
        }
    }

    #[test]
    fn concurrent_first_publishes_cannot_both_succeed() {
        // Codex review finding: with no ontology at preview time there is no version to
        // pin, so two admins publishing at once could each create one and the later
        // would silently replace the earlier's classes.
        for round in 0..15 {
            let (k, crm, shop) = (
                kernel(),
                crm_db(&format!("race{round}")),
                shop_db(&format!("race{round}")),
            );
            two_sources(&crm, &shop, || {
                discover(&k, "crm");
                discover(&k, "shop");
                let p =
                    tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                        .unwrap();
                let apply = apply_args(&p, json!({}));
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
                let handles: Vec<_> = (0..6)
                    .map(|_| {
                        let (k, apply, barrier) = (k.clone(), apply.clone(), barrier.clone());
                        std::thread::spawn(move || {
                            barrier.wait();
                            tool_sources_ontology(&k, &apply, "x")
                        })
                    })
                    .collect();
                let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
                let ok = results.iter().filter(|r| r.is_ok()).count();
                assert_eq!(
                    ok, 1,
                    "round {round}: exactly one first publish may win: {results:?}"
                );
                for r in results.iter().filter_map(|r| r.as_ref().err()) {
                    assert!(
                        r.starts_with("expected_version required"),
                        "round {round}: losers must be told to re-preview: {r}"
                    );
                }
                let kos = k
                    .scan_by_type(
                        &crate::Subject::with_roles("t", &["admin"]),
                        crate::ONTOLOGY_TYPE,
                    )
                    .unwrap();
                assert_eq!(
                    kos.len(),
                    1,
                    "round {round}: one ontology object, not several"
                );
            });
        }
    }

    #[test]
    fn publishing_over_an_auto_discovered_ontology_keeps_its_classes() {
        // Codex review finding: only a manual ontology was a merge base, so a published
        // source ontology shadowed an active auto-discovered one and dropped its classes.
        let (k, crm, shop) = (kernel(), crm_db("auto"), shop_db("auto"));
        two_sources(&crm, &shop, || {
            seed_object(&k, "Legacy", "legacy-1");
            crate::tools::tool_discover_ontology(&k, &json!({})).expect("auto-discover");
            let (_, source) =
                crate::http::active_ontology_for(&k, None).expect("an active ontology");
            assert_eq!(
                source, "auto-discovered",
                "setup: the active ontology is the auto one"
            );
            discover(&k, "crm");
            let p = tool_sources_ontology(&k, &json!({"sources": ["crm"], "preview": true}), "x")
                .unwrap();
            assert!(
                p["untouched"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("Legacy")),
                "the auto class is part of the base: {p}"
            );
            assert_eq!(p["base"], "auto-discovered");
            let pin = json!({"expected_version": p["active_version"]});
            tool_sources_ontology(&k, &apply_args(&p, pin), "x").unwrap();
            let (_, def) = manual(&k).expect("published as the manual ontology");
            assert!(
                def.classes.contains_key("Legacy"),
                "Legacy survived the publish: {:?}",
                def.classes.keys().collect::<Vec<_>>()
            );
            assert!(def.classes.contains_key("Customer") && def.classes.contains_key("Order"));
        });
    }

    fn seed_object(k: &Kernel, type_name: &str, name: &str) {
        use crate::{
            ExtensionMap, Metadata, Origin, PropertyMap, ReferentialPolicy, RememberRequest, Value,
        };
        let mut props = PropertyMap::new();
        props.insert("name".into(), Value::Text(name.into()));
        k.remember(RememberRequest {
            context: crate::Subject::with_roles("seed", &["admin"]).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: type_name.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap();
    }

    fn merge_fixture(description: &str, with_mapping: bool) -> (J, J) {
        let class =
            |d: &str, props: J| json!({"name": "Customer", "description": d, "properties": props});
        let mut existing = json!({
            "namespace": "n", "version": "1",
            "classes": [class(description, json!(["note"]))],
            "relationships": [], "property_defs": [], "mappings": [],
        });
        if with_mapping {
            existing["mappings"] = json!([{"source": "sqlite", "physical_type": "customers", "class": "Customer", "property_map": {}}]);
        }
        let draft = json!({
            "namespace": "n", "version": "1",
            "classes": [class("Discovered from crm.customers", json!(["email"]))],
            "relationships": [], "property_defs": [],
            "mappings": [{"source": "sqlite", "physical_type": "customers", "class": "Customer", "property_map": {}}],
        });
        (existing, draft)
    }

    #[test]
    fn a_human_class_that_borrows_the_discovery_phrase_is_never_refreshed() {
        // the phrase alone must not hand a human's class to discovery
        for text in [
            "Discovered from the CRM team's spreadsheet",
            "Discovered from crm.customers",
            "Discovered from",
        ] {
            let (existing, draft) = merge_fixture(text, false);
            let m = merge(Some(&existing), &draft);
            assert!(
                m.refreshed.is_empty(),
                "{text:?}: refreshed {:?}",
                m.refreshed
            );
            let c = &m.definition["classes"][0];
            assert_eq!(c["description"], text, "description rewritten for {text:?}");
            assert_eq!(
                c["properties"],
                json!(["note"]),
                "properties changed for {text:?}"
            );
            assert!(
                m.warnings
                    .iter()
                    .any(|w| w.contains("not written by discovery")),
                "{text:?}: {:?}",
                m.warnings
            );
        }
    }

    #[test]
    fn a_class_discovery_wrote_still_refreshes() {
        // the description names the container and a mapping ties it to the class
        let (existing, draft) = merge_fixture("Discovered from crm.customers", true);
        let m = merge(Some(&existing), &draft);
        assert_eq!(m.refreshed, vec!["Customer".to_string()]);
        assert_eq!(
            m.definition["classes"][0]["properties"],
            json!(["note", "email"])
        );
    }
}
