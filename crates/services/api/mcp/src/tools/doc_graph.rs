//! Saving an ingested document into the knowledge base, whole and provably complete.
//!
//! Compile is a dry-run preview. Saving writes every page (the text itself), fragment, chunk,
//! entity, relation and fact of the document as objects tagged with the document, then reads
//! them back from the kernel and records a manifest (`planned` against `stored`, per kind) on the
//! document. `complete` is true only when every count matches; any gap is named, never hidden.
//! Ids are deterministic and tenant-scoped, so saving again updates in place and removal then
//! re-saving shows the objects again (see `derived_ids`).

use crate::derived_ids::{identity_koid, live_koid};
use crate::imports::{fresh_run_id, ImportSink};
use crate::{
    json, ExtensionMap, ForgetMode, Kernel, KnowledgeContext, Lifecycle, LifecycleState, Metadata,
    Origin, RelationshipRef, SecurityDescriptor, Subject, J,
};
use aikoql_ingestion::{CommitAction, CompilationResult, DocumentModel, FragmentContent};
use aikoql_kernel::{Direction, KnowledgeObject, Value, KOID};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub(crate) const T_PAGE: &str = "DocumentPage";
pub(crate) const T_CHUNK: &str = "DocumentChunk";
pub(crate) const T_FRAGMENT: &str = "DocumentFragment";
pub(crate) const T_FACT: &str = "DocumentFact";
pub(crate) const T_ENTITY: &str = "DocumentEntity";
pub(crate) const DOC_TYPES: [&str; 5] = [T_PAGE, T_FRAGMENT, T_CHUNK, T_ENTITY, T_FACT];

/// Edge types the structure itself uses; a document's own relations are everything else.
const RESERVED: [&str; 5] = [
    "part_of",
    "on_page",
    "about",
    "extracted_from",
    "mentioned_in",
];

/// One object to save, with the edges it carries.
struct Obj {
    ty: &'static str,
    koid: KOID,
    props: BTreeMap<String, Value>,
    edges: Vec<(String, KOID)>,
}

/// What saving the document means: every object, and the counts the result must match.
struct Plan {
    objects: Vec<Obj>,
    pages: usize,
    page_chars: usize,
    chunks: usize,
    fragments: usize,
    entities: usize,
    facts: usize,
    relations: usize,
    pages_without_chunks: usize,
    merged: J,
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn int(n: usize) -> Value {
    Value::Int(n as i64)
}

/// The tag every saved object carries, so a document's knowledge can be found and removed.
fn doc_tag(sha: &str) -> String {
    format!("doc:{}", &sha[..sha.len().min(16)])
}

/// `relates to` -> `relates_to`; an empty predicate is `related_to`; a structural name is kept apart.
fn predicate(p: &str) -> String {
    let mut out: String = p
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    out = out.trim_matches('_').to_string();
    if out.is_empty() {
        out = "related_to".into();
    }
    if RESERVED.contains(&out.as_str()) {
        out.push_str("_rel");
    }
    out
}

fn name_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Cut long evidence text for display fields; the full text lives in the page and chunk objects.
fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// What `ensure_entity` needs to name and describe an entity object.
struct EntityCtx<'a> {
    class_of: &'a HashMap<String, (Option<String>, Vec<(String, String)>)>,
    doc_koid: KOID,
    doc_hex: &'a str,
    id: &'a dyn Fn(&str, &str) -> KOID,
}

/// The entity object for `name`, created on first sight (`implicit` when only a relation, fact or
/// chunk mentioned it, so that no relation ever loses an endpoint).
fn ensure_entity(
    entities: &mut Vec<Obj>,
    idx: &mut HashMap<String, usize>,
    c: &EntityCtx,
    name: &str,
    implicit: bool,
    e: Option<&aikoql_ingestion::EntityCandidate>,
) -> usize {
    let key = name_key(name);
    if let Some(&i) = idx.get(&key) {
        return i;
    }
    let koid = (c.id)("entity", &key);
    let (class, attrs) = c.class_of.get(&key).cloned().unwrap_or((None, vec![]));
    let mut props = BTreeMap::new();
    props.insert("document".into(), text(c.doc_hex));
    props.insert("ordinal".into(), int(entities.len()));
    props.insert("name".into(), text(name.trim()));
    let kind = class
        .or_else(|| e.and_then(|e| e.type_hint.clone()))
        .unwrap_or_else(|| "Entity".into());
    props.insert("entity_type".into(), text(&kind));
    props.insert("implicit".into(), Value::Bool(implicit));
    if let Some(e) = e {
        props.insert("confidence".into(), Value::Float(f64::from(e.confidence)));
        props.insert("mention_count".into(), int(e.mentions.len()));
        props.insert(
            "mentions".into(),
            Value::List(
                e.mentions
                    .iter()
                    .take(3)
                    .map(|m| text(&cut(m, 300)))
                    .collect(),
            ),
        );
        if let Some(pg) = e.evidence.page {
            props.insert("page".into(), int(pg as usize));
        }
    }
    for (a, v) in attrs.iter().take(30) {
        props.insert(format!("attr.{a}"), text(&cut(v, 500)));
    }
    entities.push(Obj {
        ty: T_ENTITY,
        koid,
        props,
        edges: vec![("extracted_from".into(), c.doc_koid)],
    });
    idx.insert(key, entities.len() - 1);
    entities.len() - 1
}

fn plan_objects(
    k: &Kernel,
    doc: &DocumentModel,
    cr: &CompilationResult,
    doc_koid: KOID,
    sha: &str,
    tenant: Option<&str>,
) -> Plan {
    let scope = match tenant {
        Some(t) => format!("{t}\u{1f}{sha}"),
        None => sha.to_string(),
    };
    let id = |kind: &str, key: &str| {
        live_koid(k, |g| {
            let mut parts = vec![kind.to_string(), key.to_string()];
            if g > 0 {
                parts.push(format!("generation {g}"));
            }
            identity_koid("aikoql:doc-graph", &scope, &parts)
        })
    };
    let doc_hex = doc_koid.to_hex();
    let mut objects: Vec<Obj> = Vec::new();
    let base = |props: &mut BTreeMap<String, Value>, ordinal: usize| {
        props.insert("document".into(), text(&doc_hex));
        props.insert("ordinal".into(), int(ordinal));
    };

    // pages: the text itself, in full
    let mut page_ids: HashMap<u32, KOID> = HashMap::new();
    for (i, p) in doc.pages.iter().enumerate() {
        let koid = id("page", &format!("{i}:{}", p.page_number));
        page_ids.insert(p.page_number, koid);
        let mut props = BTreeMap::new();
        base(&mut props, i);
        props.insert("page".into(), int(p.page_number as usize));
        props.insert("text".into(), text(&p.text));
        props.insert("char_count".into(), int(p.text.chars().count()));
        props.insert("source".into(), text(&p.source));
        if let Some(c) = p.ocr_confidence {
            props.insert("ocr_confidence".into(), Value::Float(f64::from(c)));
        }
        objects.push(Obj {
            ty: T_PAGE,
            koid,
            props,
            edges: vec![("part_of".into(), doc_koid)],
        });
    }
    let page_chars: usize = doc.pages.iter().map(|p| p.text.chars().count()).sum();

    // entities: from the compiler's plan (class, attributes) and the IR (mentions, confidence)
    let mut class_of: HashMap<String, (Option<String>, Vec<(String, String)>)> = HashMap::new();
    for a in &cr.commit_plan.actions {
        if let CommitAction::CreateKO {
            entity_name,
            class_name,
            properties,
            ..
        } = a
        {
            if !entity_name.starts_with("rel:") {
                class_of.insert(
                    name_key(entity_name),
                    (class_name.clone(), properties.clone()),
                );
            }
        }
    }
    let ctx = EntityCtx {
        class_of: &class_of,
        doc_koid,
        doc_hex: &doc_hex,
        id: &id,
    };
    let mut entity_idx: HashMap<String, usize> = HashMap::new();
    let mut entities: Vec<Obj> = Vec::new();
    for e in &cr.ir.entities {
        ensure_entity(
            &mut entities,
            &mut entity_idx,
            &ctx,
            &e.name,
            false,
            Some(e),
        );
    }

    // chunks: the retrieval units, each tied to its pages and to the entities it mentions
    let mut mentioned: HashMap<usize, Vec<KOID>> = HashMap::new();
    let mut covered: BTreeSet<u32> = BTreeSet::new();
    for (i, ec) in cr.embedded_chunks.iter().enumerate() {
        let c = &ec.chunk;
        let koid = id("chunk", &format!("{i}:{}", c.chunk_id));
        let mut props = BTreeMap::new();
        base(&mut props, i);
        props.insert("chunk_id".into(), text(&c.chunk_id));
        props.insert("chunk_index".into(), int(c.position.chunk_index));
        props.insert("start_page".into(), int(c.position.start_page as usize));
        props.insert("end_page".into(), int(c.position.end_page as usize));
        props.insert("char_offset".into(), int(c.position.char_offset));
        props.insert("text".into(), text(&c.text));
        props.insert("char_count".into(), int(c.text.chars().count()));
        props.insert(
            "heading_path".into(),
            text(&c.structure.heading_path.join(" > ")),
        );
        props.insert("source_type".into(), text(&c.structure.source_type));
        if let Some(t) = &c.structure.section_title {
            props.insert("section_title".into(), text(t));
        }
        let mut edges = vec![("part_of".to_string(), doc_koid)];
        for pg in c.position.start_page..=c.position.end_page.max(c.position.start_page) {
            covered.insert(pg);
            if let Some(p) = page_ids.get(&pg) {
                edges.push(("on_page".into(), *p));
            }
        }
        for name in &c.entity_mentions {
            let ei = ensure_entity(&mut entities, &mut entity_idx, &ctx, name, true, None);
            mentioned.entry(ei).or_default().push(koid);
        }
        objects.push(Obj {
            ty: T_CHUNK,
            koid,
            props,
            edges,
        });
    }
    let pages_without_chunks = doc
        .pages
        .iter()
        .filter(|p| p.text.chars().any(|c| !c.is_whitespace()) && !covered.contains(&p.page_number))
        .count();

    // fragments: the compiler's canonical segmentation, tables and images included
    for (i, f) in cr.fragments.iter().enumerate() {
        let koid = id("fragment", &format!("{i}:{}", f.fragment_id));
        let mut props = BTreeMap::new();
        base(&mut props, i);
        props.insert("fragment_id".into(), text(&f.fragment_id));
        props.insert("modality".into(), text(&format!("{:?}", f.modality)));
        props.insert("confidence".into(), Value::Float(f64::from(f.confidence)));
        let page = f.context.page.or_else(|| f.source.as_ref().map(|s| s.page));
        if let Some(pg) = page {
            props.insert("page".into(), int(pg as usize));
        }
        props.insert(
            "heading_path".into(),
            text(&f.context.heading_path.join(" > ")),
        );
        match &f.content {
            FragmentContent::Text(t) | FragmentContent::Code(t) => {
                props.insert("text".into(), text(t));
            }
            other => {
                props.insert(
                    "content".into(),
                    text(&serde_json::to_string(other).unwrap_or_default()),
                );
            }
        }
        let mut edges = vec![("part_of".to_string(), doc_koid)];
        if let Some(p) = page.and_then(|pg| page_ids.get(&pg)) {
            edges.push(("on_page".into(), *p));
        }
        objects.push(Obj {
            ty: T_FRAGMENT,
            koid,
            props,
            edges,
        });
    }

    // relations: real edges between entity objects, deduplicated on (subject, predicate, object)
    let mut seen: HashSet<(usize, String, usize)> = HashSet::new();
    for r in &cr.ir.relations {
        let s_i = ensure_entity(&mut entities, &mut entity_idx, &ctx, &r.subject, true, None);
        let o_i = ensure_entity(&mut entities, &mut entity_idx, &ctx, &r.object, true, None);
        let pred = predicate(&r.predicate);
        if seen.insert((s_i, pred.clone(), o_i)) {
            let target = entities[o_i].koid;
            entities[s_i].edges.push((pred, target));
        }
    }
    let relations = seen.len();

    // facts: the statements, with the source text they were read from, linked to their entities
    let mut fact_objs: Vec<Obj> = Vec::new();
    for (i, f) in cr.ir.facts.iter().enumerate() {
        let koid = id("fact", &i.to_string());
        let mut props = BTreeMap::new();
        base(&mut props, i);
        props.insert("statement".into(), text(&f.statement));
        props.insert("confidence".into(), Value::Float(f64::from(f.confidence)));
        props.insert("entities".into(), text(&f.entities.join("; ")));
        if let Some(src) = &f.snippet {
            props.insert("source_text".into(), text(src));
        }
        if let Some(pg) = f.evidence.page {
            props.insert("page".into(), int(pg as usize));
        }
        let mut edges = vec![("part_of".to_string(), doc_koid)];
        for n in &f.entities {
            let ei = ensure_entity(&mut entities, &mut entity_idx, &ctx, n, true, None);
            edges.push(("about".into(), entities[ei].koid));
        }
        fact_objs.push(Obj {
            ty: T_FACT,
            koid,
            props,
            edges,
        });
    }
    for (ei, chunks) in mentioned {
        for c in chunks {
            entities[ei].edges.push(("mentioned_in".into(), c));
        }
    }
    let planned_entities = entities.len();
    let merged = json!({
        "entities": cr.ir.entities.len().saturating_sub(entities.iter().filter(|e| e.props.get("implicit") == Some(&Value::Bool(false))).count()),
        "relations": cr.ir.relations.len().saturating_sub(relations),
        "implicit_entities": entities.iter().filter(|e| e.props.get("implicit") == Some(&Value::Bool(true))).count(),
    });
    objects.extend(entities);
    objects.extend(fact_objs);
    Plan {
        pages: doc.pages.len(),
        page_chars,
        chunks: cr.embedded_chunks.len(),
        fragments: cr.fragments.len(),
        entities: planned_entities,
        facts: cr.ir.facts.len(),
        relations,
        pages_without_chunks,
        merged,
        objects,
    }
}

fn tenant_of(ko: &KnowledgeObject) -> Option<String> {
    ko.metadata.tenant.clone()
}

fn admin(tenant: Option<&str>) -> Subject {
    let s = Subject::with_roles("system", &["admin"]);
    match tenant {
        Some(t) => s.in_tenant(t),
        None => s,
    }
}

/// The saved objects of `ty` that belong to this document and tenant (live ones only).
fn saved(k: &Kernel, ty: &str, tag: &str, tenant: Option<&str>) -> Vec<KnowledgeObject> {
    k.scan_by_type(&admin(tenant), ty)
        .unwrap_or_default()
        .into_iter()
        .filter(|o| {
            o.metadata.tenant.as_deref() == tenant && o.metadata.tags.iter().any(|t| t == tag)
        })
        .collect()
}

fn sha_of(ko: &KnowledgeObject) -> Result<String, String> {
    match ko.properties.get("sha256") {
        Some(Value::Text(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err("document missing sha256 property".into()),
    }
}

/// Write the document's whole knowledge and return the verified manifest.
pub(crate) fn commit_knowledge(
    k: &Kernel,
    doc_ko: &KnowledgeObject,
    doc: &DocumentModel,
    cr: &CompilationResult,
    failed_batches: usize,
    progress: &dyn Fn(usize, usize),
) -> Result<J, String> {
    let sha = sha_of(doc_ko)?;
    let tenant = tenant_of(doc_ko);
    let tag = doc_tag(&sha);
    let plan = plan_objects(k, doc, cr, doc_ko.koid, &sha, tenant.as_deref());

    let mut subject = Subject::new("document-graph");
    if let Some(t) = &tenant {
        subject = subject.in_tenant(t);
    }
    let sink = ImportSink::new(k, fresh_run_id(), subject, vec![]);
    let total = plan.objects.len();
    let (mut written, mut present): (usize, BTreeMap<&str, HashSet<KOID>>) = (0, BTreeMap::new());
    for (n, o) in plan.objects.iter().enumerate() {
        let ko = KnowledgeObject {
            koid: o.koid,
            version: 0,
            commit_ts: 0,
            metadata: Metadata {
                type_name: o.ty.into(),
                tenant: tenant.clone(),
                schema_version: 1,
                tags: vec!["document-graph".into(), tag.clone()],
            },
            properties: o.props.clone(),
            semantic: None,
            relationships: o
                .edges
                .iter()
                .map(|(rel, to)| RelationshipRef {
                    rel_type: rel.clone(),
                    target: *to,
                    direction: Direction::Outbound,
                })
                .collect(),
            event_refs: vec![],
            security: SecurityDescriptor {
                owner: "document-graph".into(),
                acl: vec![],
                classification: None,
            },
            lifecycle: Lifecycle {
                state: LifecycleState::Draft,
                origin: Origin::Human,
            },
            extensions: ExtensionMap::new(),
        };
        present.entry(o.ty).or_default().insert(o.koid);
        written += usize::from(
            sink.put(ko, "document-graph", "saved from a document")
                .map_err(|e| e.to_string())?,
        );
        if n % 250 == 0 {
            progress(n, total);
        }
    }
    progress(total, total);
    // what an earlier save wrote and this one no longer contains
    let mine = |ko: &KnowledgeObject| {
        ko.metadata.tenant.as_deref() == tenant.as_deref()
            && ko.metadata.tags.iter().any(|t| *t == tag)
    };
    let mut pruned = 0;
    for ty in DOC_TYPES {
        let none = HashSet::new();
        pruned += sink
            .prune_missing_where(ty, present.get(ty).unwrap_or(&none), mine)
            .map_err(|e| e.to_string())?;
    }

    // read it all back: a count is only believed when the kernel gives the objects back
    let by_type: HashMap<&str, Vec<KnowledgeObject>> = DOC_TYPES
        .iter()
        .map(|ty| (*ty, saved(k, ty, &tag, tenant.as_deref())))
        .collect();
    let stored_page_chars: usize = by_type[T_PAGE]
        .iter()
        .map(|p| match p.properties.get("text") {
            Some(Value::Text(t)) => t.chars().count(),
            _ => 0,
        })
        .sum();
    let stored_relations: usize = by_type[T_ENTITY]
        .iter()
        .flat_map(|e| e.relationships.iter())
        .filter(|r| !RESERVED.contains(&r.rel_type.as_str()))
        .count();
    let kinds = [
        ("pages", plan.pages, by_type[T_PAGE].len()),
        ("fragments", plan.fragments, by_type[T_FRAGMENT].len()),
        ("chunks", plan.chunks, by_type[T_CHUNK].len()),
        ("entities", plan.entities, by_type[T_ENTITY].len()),
        ("relations", plan.relations, stored_relations),
        ("facts", plan.facts, by_type[T_FACT].len()),
    ];
    let mut gaps: Vec<String> = kinds
        .iter()
        .filter(|(_, p, s)| p != s)
        .map(|(n, p, s)| format!("{n}: {s} stored of {p} planned"))
        .collect();
    if stored_page_chars != plan.page_chars {
        gaps.push(format!(
            "page text: {stored_page_chars} characters stored of {}",
            plan.page_chars
        ));
    }
    if failed_batches > 0 {
        gaps.push(format!(
            "{failed_batches} batch(es) of the document failed: their pages are missing"
        ));
    }
    if plan.pages_without_chunks > 0 {
        gaps.push(format!(
            "{} page(s) with text are in no chunk",
            plan.pages_without_chunks
        ));
    }
    Ok(json!({
        "complete": gaps.is_empty(),
        "gaps": gaps,
        "kinds": kinds.iter().map(|(n, p, s)| (n.to_string(), json!({"planned": p, "stored": s}))).collect::<serde_json::Map<_, _>>(),
        "page_chars": {"planned": plan.page_chars, "stored": stored_page_chars},
        "pages_without_chunks": plan.pages_without_chunks,
        "failed_batches": failed_batches,
        "merged": plan.merged,
        "written": written,
        "pruned": pruned,
    }))
}

/// Tombstone everything saved for this document.
pub(crate) fn remove_knowledge(k: &Kernel, doc_ko: &KnowledgeObject) -> Result<J, String> {
    let tag = doc_tag(&sha_of(doc_ko)?);
    let tenant = tenant_of(doc_ko);
    let ctx = KnowledgeContext::from(admin(tenant.as_deref()));
    let mut removed = 0usize;
    for ty in DOC_TYPES {
        for ko in saved(k, ty, &tag, tenant.as_deref()) {
            k.forget(
                ctx.clone(),
                &ko.koid,
                ForgetMode::Tombstone,
                None,
                Some("removed from the knowledge base".into()),
            )
            .map_err(|e| e.to_string())?;
            removed += 1;
        }
    }
    Ok(json!({"removed": removed}))
}

/// A complete, paged listing of the document's saved objects of one kind.
pub(crate) fn list_objects(k: &Kernel, args: &J) -> Result<J, String> {
    let hex = args
        .get("koid")
        .and_then(J::as_str)
        .ok_or("missing: koid")?;
    let koid = KOID::from_hex(hex).map_err(|e| e.to_string())?;
    let subject = crate::session::subject_of(args);
    let doc_ko = k
        .get(KnowledgeContext::from(subject), &koid)
        .map_err(|e| e.to_string())?;
    let ty = match args.get("kind").and_then(J::as_str).unwrap_or("entity") {
        "page" => T_PAGE,
        "chunk" => T_CHUNK,
        "fragment" => T_FRAGMENT,
        "entity" => T_ENTITY,
        "fact" => T_FACT,
        other => {
            return Err(format!(
                "invalid request: kind must be page, chunk, fragment, entity or fact (got {other})"
            ))
        }
    };
    let offset = args.get("offset").and_then(J::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(J::as_u64)
        .unwrap_or(50)
        .clamp(1, 1000) as usize;
    let q = args
        .get("q")
        .and_then(J::as_str)
        .map(str::to_lowercase)
        .filter(|q| !q.is_empty());
    let tag = doc_tag(&sha_of(&doc_ko)?);
    let mut all = saved(k, ty, &tag, tenant_of(&doc_ko).as_deref());
    let get = |o: &KnowledgeObject, p: &str| match o.properties.get(p) {
        Some(Value::Text(t)) => t.clone(),
        Some(Value::Int(n)) => n.to_string(),
        _ => String::new(),
    };
    if let Some(q) = &q {
        all.retain(|o| {
            ["text", "name", "statement", "content"]
                .iter()
                .any(|p| get(o, p).to_lowercase().contains(q.as_str()))
        });
    }
    let ordinal = |o: &KnowledgeObject| match o.properties.get("ordinal") {
        Some(Value::Int(n)) => *n,
        _ => 0,
    };
    all.sort_by_key(|o| (ordinal(o), o.koid));
    let total = all.len();
    let rows: Vec<J> = all
        .iter()
        .skip(offset)
        .take(limit)
        .map(|o| {
            let label = ["name", "statement", "text", "content"]
                .iter()
                .map(|p| get(o, p))
                .find(|v| !v.is_empty())
                .unwrap_or_default();
            json!({
                "koid": o.koid.to_hex(), "ordinal": ordinal(o), "kind": ty,
                "label": cut(&label, 200), "page": get(o, "page"),
                "entity_type": get(o, "entity_type"), "links": o.relationships.len(),
            })
        })
        .collect();
    let next = (offset + rows.len() < total).then_some(offset + rows.len());
    Ok(
        json!({"koid": hex, "kind": ty, "total": total, "offset": offset, "rows": rows, "next_offset": next}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::testkit::{kernel, tmp_db};
    use crate::tools::ingestion::tool_document_ingest;
    use aikoql_ingestion::PageModel;
    use aikoql_kernel::Value;
    use std::sync::Arc;

    const PAGES: usize = 12;

    /// A document of 12 pages with people, places and a marker on the last page.
    fn document() -> DocumentModel {
        let pages: Vec<PageModel> = (1..=PAGES)
            .map(|n| {
                let mut text = format!(
                    "Page {n}. Alice Johnson works at Acme Corporation in Paris. \
                     Acme Corporation acquired Beta Labs in Berlin. Bob Smith leads Beta Labs. \
                     Section {n} describes the Acme Corporation roadmap in detail for the board."
                );
                if n == PAGES {
                    text.push_str(" ZEBRA-END marks the last sentence of the document.");
                }
                PageModel {
                    page_number: n as u32,
                    char_count: text.chars().count(),
                    text,
                    source: "native".into(),
                    ocr_confidence: None,
                    images: vec![],
                }
            })
            .collect();
        let total_chars = pages.iter().map(|p| p.char_count).sum();
        DocumentModel {
            page_count: PAGES as u32,
            pages,
            total_chars,
            ocr_stats: None,
        }
    }

    fn compiled(doc: &DocumentModel) -> CompilationResult {
        aikoql_ingestion::compile_document_mock_with_policy(doc, &[], None, &Default::default())
    }

    /// An ingested document KO (the commit needs one to hang the manifest on).
    fn ingest(k: &Kernel, tenant: Option<&str>, tag: &str) -> KnowledgeObject {
        use base64::Engine;
        let db = tmp_db(tag);
        let mut args = json!({
            "filename": "book.txt", "mime_type": "text/plain",
            "content_base64": base64::engine::general_purpose::STANDARD.encode("placeholder text for the document object"),
        });
        if let Some(t) = tenant {
            args["tenant"] = json!(t);
        }
        let r = tool_document_ingest(k, &args, db.to_str().unwrap()).unwrap();
        let koid = aikoql_kernel::KOID::from_hex(r["koid"].as_str().unwrap()).unwrap();
        k.get(
            crate::KnowledgeContext::from(crate::Subject::with_roles("t", &["admin"])),
            &koid,
        )
        .unwrap()
    }

    fn live(k: &Kernel, ty: &str, tenant: Option<&str>) -> Vec<KnowledgeObject> {
        let s = crate::Subject::with_roles("t", &["admin"]);
        let s = match tenant {
            Some(t) => s.in_tenant(t),
            None => s,
        };
        k.scan_by_type(&s, ty)
            .unwrap()
            .into_iter()
            .filter(|o| o.metadata.tenant.as_deref() == tenant)
            .collect()
    }

    fn save(
        k: &Arc<Kernel>,
        ko: &KnowledgeObject,
        doc: &DocumentModel,
        cr: &CompilationResult,
    ) -> J {
        commit_knowledge(k, ko, doc, cr, 0, &|_, _| {}).unwrap()
    }

    #[test]
    fn saving_stores_every_page_chunk_fragment_entity_and_fact_and_proves_it() {
        let (k, doc) = (kernel(), document());
        let cr = compiled(&doc);
        assert!(
            !cr.ir.entities.is_empty()
                && !cr.embedded_chunks.is_empty()
                && !cr.fragments.is_empty(),
            "the fixture must exercise every kind"
        );
        let ko = ingest(&k, None, "d1");
        let m = save(&k, &ko, &doc, &cr);
        assert_eq!(m["complete"], true, "{m}");
        assert_eq!(m["gaps"], json!([]), "{m}");
        for (kind, ty, planned) in [
            ("pages", T_PAGE, doc.pages.len()),
            ("chunks", T_CHUNK, cr.embedded_chunks.len()),
            ("fragments", T_FRAGMENT, cr.fragments.len()),
        ] {
            assert_eq!(
                m["kinds"][kind],
                json!({"planned": planned, "stored": planned}),
                "{kind}: {m}"
            );
            assert_eq!(
                live(&k, ty, None).len(),
                planned,
                "{kind} are really in the kernel"
            );
        }
        let stored_entities = live(&k, T_ENTITY, None).len();
        assert_eq!(m["kinds"]["entities"]["stored"], stored_entities);
        assert_eq!(m["kinds"]["entities"]["planned"], stored_entities, "{m}");
        assert_eq!(m["kinds"]["facts"]["planned"], cr.ir.facts.len());
        assert_eq!(live(&k, T_FACT, None).len(), cr.ir.facts.len());
        // the text itself is stored: every character of every page, the last page included
        assert_eq!(
            m["page_chars"],
            json!({"planned": doc.total_chars, "stored": doc.total_chars}),
            "{m}"
        );
        let last = live(&k, T_PAGE, None)
            .into_iter()
            .find(|p| p.properties.get("page") == Some(&Value::Int(PAGES as i64)))
            .unwrap();
        assert!(
            matches!(last.properties.get("text"), Some(Value::Text(t)) if t.contains("ZEBRA-END"))
        );
        // every object points back at the document
        assert!(live(&k, T_CHUNK, None).iter().all(|c| c
            .relationships
            .iter()
            .any(|r| r.rel_type == "part_of" && r.target == ko.koid)));
        assert_eq!(m["pages_without_chunks"], 0, "{m}");
        // relations are real edges between entity objects, and entities point back at their text
        assert!(
            m["kinds"]["relations"]["planned"].as_u64().unwrap() > 0,
            "the fixture must have relations: {m}"
        );
        let entities = live(&k, T_ENTITY, None);
        let own = |r: &aikoql_kernel::RelationshipRef| {
            ![
                "part_of",
                "on_page",
                "about",
                "extracted_from",
                "mentioned_in",
            ]
            .contains(&r.rel_type.as_str())
        };
        let ids: std::collections::HashSet<_> = entities.iter().map(|e| e.koid).collect();
        assert!(
            entities
                .iter()
                .flat_map(|e| &e.relationships)
                .filter(|r| own(r))
                .all(|r| ids.contains(&r.target)),
            "every relation ends at a stored entity"
        );
        assert!(
            entities
                .iter()
                .any(|e| e.relationships.iter().any(|r| r.rel_type == "mentioned_in")),
            "entities link to the chunks that mention them"
        );
        assert!(entities.iter().all(|e| e
            .relationships
            .iter()
            .any(|r| r.rel_type == "extracted_from" && r.target == ko.koid)));
    }

    #[test]
    fn saving_again_writes_nothing_and_removing_leaves_nothing_and_saving_after_removal_shows_it_again(
    ) {
        let (k, doc) = (kernel(), document());
        let cr = compiled(&doc);
        let ko = ingest(&k, None, "d2");
        let first = save(&k, &ko, &doc, &cr);
        assert!(first["written"].as_u64().unwrap() > 0);
        let again = save(&k, &ko, &doc, &cr);
        assert_eq!(
            again["written"], 0,
            "an identical save writes nothing: {again}"
        );
        assert_eq!(again["complete"], true);
        let removed = remove_knowledge(&k, &ko).unwrap();
        assert!(removed["removed"].as_u64().unwrap() > 0, "{removed}");
        for ty in DOC_TYPES {
            assert!(
                live(&k, ty, None).is_empty(),
                "{ty} must be gone after removal"
            );
        }
        let back = save(&k, &ko, &doc, &cr);
        assert_eq!(back["complete"], true, "{back}");
        assert_eq!(
            live(&k, T_PAGE, None).len(),
            PAGES,
            "removed then saved: visible again"
        );
    }

    #[test]
    fn two_tenants_with_the_same_document_stay_independent() {
        let (k, doc) = (kernel(), document());
        let cr = compiled(&doc);
        let (a, b) = (ingest(&k, Some("t1"), "d3a"), ingest(&k, Some("t2"), "d3b"));
        assert!(
            save(&k, &a, &doc, &cr)["complete"] == true
                && save(&k, &b, &doc, &cr)["complete"] == true
        );
        assert_eq!(live(&k, T_PAGE, Some("t1")).len(), PAGES);
        assert_eq!(live(&k, T_PAGE, Some("t2")).len(), PAGES);
        assert!(live(&k, T_PAGE, None).is_empty());
        remove_knowledge(&k, &a).unwrap();
        assert!(live(&k, T_PAGE, Some("t1")).is_empty());
        assert_eq!(
            live(&k, T_PAGE, Some("t2")).len(),
            PAGES,
            "removing one tenant's copy leaves the other's"
        );
    }

    #[test]
    fn missing_batches_make_the_manifest_incomplete_and_say_so() {
        let (k, doc) = (kernel(), document());
        let cr = compiled(&doc);
        let ko = ingest(&k, None, "d4");
        let m = commit_knowledge(&k, &ko, &doc, &cr, 2, &|_, _| {}).unwrap();
        assert_eq!(m["complete"], false, "{m}");
        assert!(
            m["gaps"].to_string().contains("2 batch"),
            "the gap is named: {m}"
        );
    }

    #[test]
    fn the_listing_pages_through_every_object_exactly_once() {
        let (k, doc) = (kernel(), document());
        let cr = compiled(&doc);
        let ko = ingest(&k, None, "d5");
        save(&k, &ko, &doc, &cr);
        let mut seen = std::collections::BTreeSet::new();
        let mut offset = 0u64;
        loop {
            let r = list_objects(
                &k,
                &json!({"koid": ko.koid.to_hex(), "kind": "chunk", "offset": offset, "limit": 5}),
            )
            .unwrap();
            assert_eq!(
                r["total"],
                cr.embedded_chunks.len(),
                "the total is exact: {r}"
            );
            for row in r["rows"].as_array().unwrap() {
                assert!(
                    seen.insert(row["koid"].as_str().unwrap().to_string()),
                    "no repeats"
                );
            }
            match r["next_offset"].as_u64() {
                Some(n) => offset = n,
                None => break,
            }
        }
        assert_eq!(
            seen.len(),
            cr.embedded_chunks.len(),
            "every chunk was reachable"
        );
        let q = list_objects(
            &k,
            &json!({"koid": ko.koid.to_hex(), "kind": "page", "q": "ZEBRA-END"}),
        )
        .unwrap();
        assert_eq!(q["total"], 1, "{q}");
    }
}
