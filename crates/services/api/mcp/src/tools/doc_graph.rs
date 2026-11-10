//! Saving an ingested document into the knowledge base, whole and provably complete.
//!
//! Compile is a dry-run preview. Saving writes every page (the text itself), fragment, chunk,
//! entity, relation and fact of the document as objects tagged with the document, then reads
//! them back from the kernel and records a manifest (`planned` against `stored`, per kind) on the
//! document. `complete` is true only when every count matches; any gap is named, never hidden.
//! Ids are deterministic and tenant-scoped, so saving again updates in place and removal then
//! re-saving shows the objects again (see `derived_ids`).

use crate::{json, Kernel, J};
use aikoql_ingestion::{CompilationResult, DocumentModel};
use aikoql_kernel::KnowledgeObject;

pub(crate) const T_PAGE: &str = "DocumentPage";
pub(crate) const T_CHUNK: &str = "DocumentChunk";
pub(crate) const T_FRAGMENT: &str = "DocumentFragment";
pub(crate) const T_FACT: &str = "DocumentFact";
pub(crate) const T_ENTITY: &str = "DocumentEntity";
pub(crate) const DOC_TYPES: [&str; 5] = [T_PAGE, T_FRAGMENT, T_CHUNK, T_ENTITY, T_FACT];

/// Write the document's whole knowledge and return the verified manifest.
pub(crate) fn commit_knowledge(
    _k: &Kernel,
    _doc_ko: &KnowledgeObject,
    _doc: &DocumentModel,
    _cr: &CompilationResult,
    _failed_batches: usize,
    _progress: &dyn Fn(usize, usize),
) -> Result<J, String> {
    Err("unimplemented".into())
}

/// Tombstone everything saved for this document.
pub(crate) fn remove_knowledge(_k: &Kernel, _doc_ko: &KnowledgeObject) -> Result<J, String> {
    Err("unimplemented".into())
}

/// A complete, paged listing of the document's saved objects of one kind.
pub(crate) fn list_objects(_k: &Kernel, _args: &J) -> Result<J, String> {
    Err(json!("unimplemented").to_string())
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
        DocumentModel { page_count: PAGES as u32, pages, total_chars, ocr_stats: None }
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
        k.get(crate::KnowledgeContext::from(crate::Subject::with_roles("t", &["admin"])), &koid).unwrap()
    }

    fn live(k: &Kernel, ty: &str, tenant: Option<&str>) -> Vec<KnowledgeObject> {
        let s = crate::Subject::with_roles("t", &["admin"]);
        let s = match tenant { Some(t) => s.in_tenant(t), None => s };
        k.scan_by_type(&s, ty).unwrap().into_iter().filter(|o| o.metadata.tenant.as_deref() == tenant).collect()
    }

    fn save(k: &Arc<Kernel>, ko: &KnowledgeObject, doc: &DocumentModel, cr: &CompilationResult) -> J {
        commit_knowledge(k, ko, doc, cr, 0, &|_, _| {}).unwrap()
    }

    #[test]
    fn saving_stores_every_page_chunk_fragment_entity_and_fact_and_proves_it() {
        let (k, doc) = (kernel(), document());
        let cr = compiled(&doc);
        assert!(!cr.ir.entities.is_empty() && !cr.embedded_chunks.is_empty() && !cr.fragments.is_empty(), "the fixture must exercise every kind");
        let ko = ingest(&k, None, "d1");
        let m = save(&k, &ko, &doc, &cr);
        assert_eq!(m["complete"], true, "{m}");
        assert_eq!(m["gaps"], json!([]), "{m}");
        for (kind, ty, planned) in [
            ("pages", T_PAGE, doc.pages.len()),
            ("chunks", T_CHUNK, cr.embedded_chunks.len()),
            ("fragments", T_FRAGMENT, cr.fragments.len()),
        ] {
            assert_eq!(m["kinds"][kind], json!({"planned": planned, "stored": planned}), "{kind}: {m}");
            assert_eq!(live(&k, ty, None).len(), planned, "{kind} are really in the kernel");
        }
        let stored_entities = live(&k, T_ENTITY, None).len();
        assert_eq!(m["kinds"]["entities"]["stored"], stored_entities);
        assert_eq!(m["kinds"]["entities"]["planned"], stored_entities, "{m}");
        assert_eq!(m["kinds"]["facts"]["planned"], cr.ir.facts.len());
        assert_eq!(live(&k, T_FACT, None).len(), cr.ir.facts.len());
        // the text itself is stored: every character of every page, the last page included
        assert_eq!(m["page_chars"], json!({"planned": doc.total_chars, "stored": doc.total_chars}), "{m}");
        let last = live(&k, T_PAGE, None).into_iter().find(|p| p.properties.get("page") == Some(&Value::Int(PAGES as i64))).unwrap();
        assert!(matches!(last.properties.get("text"), Some(Value::Text(t)) if t.contains("ZEBRA-END")));
        // every object points back at the document
        assert!(live(&k, T_CHUNK, None).iter().all(|c| c.relationships.iter().any(|r| r.rel_type == "part_of" && r.target == ko.koid)));
        assert_eq!(m["pages_without_chunks"], 0, "{m}");
    }

    #[test]
    fn saving_again_writes_nothing_and_removing_leaves_nothing_and_saving_after_removal_shows_it_again() {
        let (k, doc) = (kernel(), document());
        let cr = compiled(&doc);
        let ko = ingest(&k, None, "d2");
        let first = save(&k, &ko, &doc, &cr);
        assert!(first["written"].as_u64().unwrap() > 0);
        let again = save(&k, &ko, &doc, &cr);
        assert_eq!(again["written"], 0, "an identical save writes nothing: {again}");
        assert_eq!(again["complete"], true);
        let removed = remove_knowledge(&k, &ko).unwrap();
        assert!(removed["removed"].as_u64().unwrap() > 0, "{removed}");
        for ty in DOC_TYPES {
            assert!(live(&k, ty, None).is_empty(), "{ty} must be gone after removal");
        }
        let back = save(&k, &ko, &doc, &cr);
        assert_eq!(back["complete"], true, "{back}");
        assert_eq!(live(&k, T_PAGE, None).len(), PAGES, "removed then saved: visible again");
    }

    #[test]
    fn two_tenants_with_the_same_document_stay_independent() {
        let (k, doc) = (kernel(), document());
        let cr = compiled(&doc);
        let (a, b) = (ingest(&k, Some("t1"), "d3a"), ingest(&k, Some("t2"), "d3b"));
        assert!(save(&k, &a, &doc, &cr)["complete"] == true && save(&k, &b, &doc, &cr)["complete"] == true);
        assert_eq!(live(&k, T_PAGE, Some("t1")).len(), PAGES);
        assert_eq!(live(&k, T_PAGE, Some("t2")).len(), PAGES);
        assert!(live(&k, T_PAGE, None).is_empty());
        remove_knowledge(&k, &a).unwrap();
        assert!(live(&k, T_PAGE, Some("t1")).is_empty());
        assert_eq!(live(&k, T_PAGE, Some("t2")).len(), PAGES, "removing one tenant's copy leaves the other's");
    }

    #[test]
    fn missing_batches_make_the_manifest_incomplete_and_say_so() {
        let (k, doc) = (kernel(), document());
        let cr = compiled(&doc);
        let ko = ingest(&k, None, "d4");
        let m = commit_knowledge(&k, &ko, &doc, &cr, 2, &|_, _| {}).unwrap();
        assert_eq!(m["complete"], false, "{m}");
        assert!(m["gaps"].to_string().contains("2 batch"), "the gap is named: {m}");
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
            let r = list_objects(&k, &json!({"koid": ko.koid.to_hex(), "kind": "chunk", "offset": offset, "limit": 5})).unwrap();
            assert_eq!(r["total"], cr.embedded_chunks.len(), "the total is exact: {r}");
            for row in r["rows"].as_array().unwrap() {
                assert!(seen.insert(row["koid"].as_str().unwrap().to_string()), "no repeats");
            }
            match r["next_offset"].as_u64() {
                Some(n) => offset = n,
                None => break,
            }
        }
        assert_eq!(seen.len(), cr.embedded_chunks.len(), "every chunk was reachable");
        let q = list_objects(&k, &json!({"koid": ko.koid.to_hex(), "kind": "page", "q": "ZEBRA-END"})).unwrap();
        assert_eq!(q["total"], 1, "{q}");
    }
}
