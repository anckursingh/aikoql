//! Acceptance tests: multimodal canonical ingestion (PR-A + PR-C, HLD §57/§60).
//!
//! End-to-end: DocumentModel → DocumentAst → KnowledgeFragment[] → retrieval
//! projection (chunks). Validates the core HLD promises: modality preservation
//! (a table fragment is a table, not text soup), typed provenance, heading
//! context, and backward-compatible result serialization.

use aikoql_ingestion::{
    compile_document_mock, extract_document, CompilationResult, EvidenceSource, FactCandidate,
    FragmentContent, FragmentModality, KnowledgeFragment,
};

fn invoice_doc() -> aikoql_ingestion::DocumentModel {
    aikoql_ingestion::DocumentModel {
        page_count: 1,
        pages: vec![aikoql_ingestion::PageModel {
            page_number: 1,
            text: "1. Payment Terms\n\n\
                   Payment is due within 30 days of invoice date.\n\n\
                   | Item | Qty | Unit Price |\n\
                   | Widget | 10 | $2.50 |\n\
                   | Gadget | 5 | $12.00 |\n\n\
                   Total including tax: $145.00."
                .into(),
            char_count: 220,
            source: "native".into(),
            ocr_confidence: None,
        }],
        total_chars: 220,
        ocr_stats: None,
    }
}

#[test]
fn acceptance_compile_yields_modality_preserving_fragments() {
    let doc = invoice_doc();
    let result = compile_document_mock(&doc, &[]);

    // D4-fragments ran and produced fragments.
    assert!(
        !result.fragments.is_empty(),
        "compilation must emit knowledge fragments"
    );
    assert!(
        result
            .stats
            .phases
            .iter()
            .any(|p| p.phase == "D4-fragments"),
        "D4-fragments phase must be reported in stats"
    );

    // The table must remain a table (HLD §9) with typed cells.
    let table_frag = result
        .fragments
        .iter()
        .find(|f| f.modality == FragmentModality::Table)
        .expect("table fragment");
    match &table_frag.content {
        FragmentContent::Table(payload) => {
            assert_eq!(payload.headers.len(), 3, "three header columns");
            assert_eq!(payload.rows.len(), 2, "two data rows");
            let price = payload
                .cells
                .iter()
                .find(|c| c.text == "$2.50")
                .expect("currency cell");
            assert!(
                price.value.is_some(),
                "currency cell must carry a typed ScalarValue"
            );
        }
        other => panic!("expected table content, got {:?}", other),
    }

    // Text content outside the table survives as text fragments with context.
    let text_frags: Vec<&KnowledgeFragment> = result
        .fragments
        .iter()
        .filter(|f| f.modality == FragmentModality::Text)
        .collect();
    assert!(!text_frags.is_empty(), "paragraph fragments");
    assert!(
        text_frags.iter().any(|f| {
            f.context
                .heading_path
                .iter()
                .any(|h| h.contains("Payment Terms"))
        }),
        "paragraph under the heading must carry heading_path context"
    );
}

#[test]
fn acceptance_fragments_carry_typed_provenance() {
    let doc = invoice_doc();
    let result = compile_document_mock(&doc, &[]);

    for frag in &result.fragments {
        let span = frag
            .source
            .as_ref()
            .expect("every fragment carries a SourceSpan");
        assert_eq!(span.page, 1, "single-page doc → page 1");
        assert!(
            frag.evidence.iter().any(|e| e.extractor == "rule_boundary"),
            "evidence names the boundary extractor"
        );
    }

    // Neighbors link fragments in document order.
    assert_eq!(
        result.fragments[0].context.neighboring_fragments,
        vec![result.fragments[1].fragment_id.clone()]
    );
    assert!(result.fragments[0].fragment_id != result.fragments[1].fragment_id);
}

#[test]
fn acceptance_retrieval_projection_still_functions() {
    // Chunks are a projection of the same AST — both must come out of one
    // compile, and chunking must be unaffected by the new D4-fragments phase.
    let doc = invoice_doc();
    let result = compile_document_mock(&doc, &[]);

    assert!(
        !result.embedded_chunks.is_empty(),
        "retrieval projection must still produce chunks"
    );
    assert!(!result.ir.is_empty(), "semantic IR unaffected");
}

#[test]
fn acceptance_chunks_project_whole_fragments_never_split() {
    // PR-E invariant through the full pipeline: every chunk is composed of
    // whole fragments. Table content appears in exactly one chunk, whole.
    let doc = invoice_doc();
    let result = compile_document_mock(&doc, &[]);

    let table_chunks: Vec<&aikoql_ingestion::DocumentChunk> = result
        .embedded_chunks
        .iter()
        .map(|ec| &ec.chunk)
        .filter(|c| c.text.contains("Widget | 10"))
        .collect();
    assert_eq!(
        table_chunks.len(),
        1,
        "table rows live in exactly one chunk"
    );
    let chunk = table_chunks[0];
    assert!(
        chunk.text.contains("Gadget | 5") && chunk.text.contains("$12.00"),
        "the table chunk carries the whole table"
    );

    // Fragment and chunk sets must agree: chunk texts are built from the
    // fragments emitted by the same compile, so the union of chunk content
    // for the table equals the table fragment's rendered rows.
    let table_fragment = result
        .fragments
        .iter()
        .find(|f| f.modality == FragmentModality::Table)
        .expect("table fragment");
    match &table_fragment.content {
        FragmentContent::Table(payload) => {
            assert!(payload.cells.iter().any(|c| c.text == "$2.50"));
            assert!(chunk.text.contains("Item | Qty | Unit Price"));
        }
        other => panic!("expected table content, got {:?}", other),
    }
}

#[test]
fn acceptance_serde_backward_and_forward_compatible() {
    let doc = invoice_doc();
    let result = compile_document_mock(&doc, &[]);
    let json = serde_json::to_string(&result).expect("serialize result");

    // Full roundtrip including fragments.
    let back: CompilationResult = serde_json::from_str(&json).expect("deserialize result");
    assert_eq!(back.fragments.len(), result.fragments.len());
    assert_eq!(
        back.fragments[0].fragment_id,
        result.fragments[0].fragment_id
    );

    // Pre-multimodal JSON (no `fragments` key) must still deserialize.
    let legacy = json.replace("\"fragments\":", "\"legacy_unused\":");
    let back_legacy: CompilationResult =
        serde_json::from_str(&legacy).expect("legacy result deserializes");
    assert!(
        back_legacy.fragments.is_empty(),
        "missing fragments key defaults to empty"
    );
}

#[test]
fn acceptance_deterministic_fragment_ids() {
    let doc = invoice_doc();
    let first = compile_document_mock(&doc, &[]);
    let second = compile_document_mock(&doc, &[]);

    let ids1: Vec<&str> = first
        .fragments
        .iter()
        .map(|f| f.fragment_id.as_str())
        .collect();
    let ids2: Vec<&str> = second
        .fragments
        .iter()
        .map(|f| f.fragment_id.as_str())
        .collect();
    assert_eq!(ids1, ids2, "fragment ids deterministic across compiles");
    assert_eq!(first.fragments.len(), second.fragments.len());
}

#[test]
fn acceptance_semantic_ir_cites_typed_sources() {
    // PR-D: the semantic leg consumes the fragment stream — table cells
    // become facts cited at cell granularity, entities keep flowing.
    let doc = invoice_doc();
    let result = compile_document_mock(&doc, &[]);

    assert!(
        result.ir.entities.iter().any(|e| e.name == "Payment Terms"),
        "heading entities still extracted through the fragment leg"
    );

    let cell_facts: Vec<&FactCandidate> = result
        .ir
        .facts
        .iter()
        .filter(|f| matches!(&f.evidence.source, Some(EvidenceSource::TableCell { .. })))
        .collect();
    assert!(
        !cell_facts.is_empty(),
        "table facts carry TableCell evidence"
    );
    assert!(
        cell_facts
            .iter()
            .any(|f| f.statement == "Unit Price: $12.00"),
        "cell fact pairs header with value"
    );
}

#[test]
fn acceptance_end_to_end_text_file_ingestion() {
    // Full journey from a real file on disk through extraction to fragments.
    let dir = std::env::temp_dir().join("aikoql-multimodal-acceptance");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("spec.txt");
    std::fs::write(&path, invoice_doc().pages[0].text.clone()).unwrap();

    let extracted = extract_document(&path.to_string_lossy(), "text/plain").expect("extract");
    let result = compile_document_mock(&extracted, &[]);

    assert!(result
        .fragments
        .iter()
        .any(|f| f.modality == FragmentModality::Table));
    assert!(result.secret_findings.is_empty(), "no secrets in fixture");

    std::fs::remove_dir_all(&dir).ok();
}
