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
            images: vec![],
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
fn acceptance_markdown_images_fail_soft_and_legacy_text_deserializes() {
    // PR-B: standalone images become content-addressed Image nodes (asserted
    // at the AST level in markdown.rs unit tests, which use the real fs).
    // Here: the public compile path must not hard-fail on present OR missing
    // image files, entities keep flowing, document_id propagates, and legacy
    // JSON with a plain-string `text` key still deserializes as Some.
    let dir = std::env::temp_dir().join("aikoql-multimodal-acceptance-assets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("logo.png"), b"\x89PNG fake bytes").unwrap();
    let md = dir.join("doc.md");
    std::fs::write(
        &md,
        "# Asset Doc\n\nIntro paragraph.\n\n![Logo](logo.png)\n\n![Missing](gone.png)\n",
    )
    .unwrap();

    let ir =
        aikoql_ingestion::compile_markdown_file(&md.to_string_lossy(), Some("doc.md".into()), None)
            .expect("markdown with present and missing images compiles");

    assert!(
        ir.entities.iter().any(|e| e.name == "Asset Doc"),
        "entities still flow through the asset-bearing document"
    );
    assert_eq!(
        ir.document_id.as_deref(),
        Some("doc.md"),
        "document_id propagates through the markdown compile"
    );

    // HLD §7 back-compat: pre-migration JSON has `text` as a plain string.
    let node: aikoql_ingestion::AstNode =
        serde_json::from_str(r#"{"block_type":"Paragraph","text":"hello"}"#)
            .expect("legacy plain-string text key deserializes");
    assert_eq!(node.text.as_deref(), Some("hello"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn acceptance_visual_golden_fixture_flows_to_typed_knowledge() {
    // PR-F golden fixture (DoD rows 3-6, 8, 11, 12, 18): mermaid fence →
    // diagram entities/relations with model versions, math fence → formula
    // fact, image + chart caption → chart fact citing its persisted asset.
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/multimodal-golden.md");
    let assets = std::env::temp_dir().join("aikoql-multimodal-golden-assets");
    std::fs::create_dir_all(&assets).unwrap();

    let ir = aikoql_ingestion::compile_markdown_file(
        &fixture.to_string_lossy(),
        Some("multimodal-golden.md".into()),
        Some(&assets.to_string_lossy()),
    )
    .expect("golden fixture compiles");

    // Diagram: nodes → entities, edges → relations, model versions persisted.
    for name in ["Client", "Gateway", "Payment Service", "Ledger"] {
        let entity = ir
            .entities
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("diagram node entity '{}'", name));
        assert_eq!(entity.evidence.model.as_deref(), Some("mock-diagram-v1"));
        assert!(
            matches!(
                &entity.evidence.source,
                Some(EvidenceSource::DiagramNode { .. })
            ),
            "'{}' carries DiagramNode evidence",
            name
        );
    }
    let edge = ir
        .relations
        .iter()
        .find(|r| r.subject == "client" && r.object == "gateway")
        .expect("diagram edge relation");
    assert_eq!(edge.predicate, "related_to");
    assert_eq!(edge.evidence.model.as_deref(), Some("mock-diagram-v1"));

    // Formula fact with model.
    assert!(ir.facts.iter().any(|f| {
        f.statement.contains("F = B * R") && f.evidence.model.as_deref() == Some("mock-formula-v1")
    }));

    // Chart fact cites its title + model; the backing asset was persisted.
    let chart = ir
        .facts
        .iter()
        .find(|f| f.statement.starts_with("Chart:"))
        .expect("chart fact");
    assert!(chart.statement.contains("Fee structure by plan"));
    assert_eq!(chart.evidence.model.as_deref(), Some("mock-chart-v1"));
    let chart_bytes = std::fs::read(fixture.parent().unwrap().join("golden-chart.png")).unwrap();
    let chart_hash = aikoql_ingestion::content_hash(&chart_bytes);
    assert!(
        assets.join(format!("{}.bin", chart_hash)).exists(),
        "chart asset persisted content-addressed"
    );

    // Standalone image fact with model + persisted asset.
    let image = ir
        .facts
        .iter()
        .find(|f| f.evidence.model.as_deref() == Some("mock-image-v1"))
        .expect("image fact");
    assert!(image.statement.contains("Logo"));
    let logo_bytes = std::fs::read(fixture.parent().unwrap().join("golden-logo.png")).unwrap();
    let logo_hash = aikoql_ingestion::content_hash(&logo_bytes);
    assert!(
        assets.join(format!("{}.bin", logo_hash)).exists(),
        "logo asset persisted content-addressed"
    );

    std::fs::remove_dir_all(&assets).ok();
}

#[test]
fn acceptance_end_to_end_text_file_ingestion() {
    // Full journey from a real file on disk through extraction to fragments.
    let dir = std::env::temp_dir().join("aikoql-multimodal-acceptance");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("spec.txt");
    std::fs::write(&path, invoice_doc().pages[0].text.clone()).unwrap();

    let extracted = extract_document(&path.to_string_lossy(), "text/plain", None).expect("extract");
    let result = compile_document_mock(&extracted, &[]);

    assert!(result
        .fragments
        .iter()
        .any(|f| f.modality == FragmentModality::Table));
    assert!(result.secret_findings.is_empty(), "no secrets in fixture");

    std::fs::remove_dir_all(&dir).ok();
}
