//! M2: rule-based typed entity extraction in the default analyzer.
//!
//! Deterministic recognisers (no model): typed values (money, email, URL,
//! phone, IBAN, percentage) and organisation/person/location typing from
//! cues (legal-form suffixes, honorifics, a built-in gazetteer). Every
//! candidate carries provenance: extractor, model `rules-v1`, confidence and
//! a source span.

use aikoql_ingestion::{
    document_model_to_ir, DocumentModel, EntityCandidate, EvidenceSource, KnowledgeIr,
    MockSemanticAnalyzer, PageModel,
};

fn ir_of(text: &str) -> KnowledgeIr {
    let doc = DocumentModel {
        page_count: 1,
        pages: vec![PageModel {
            page_number: 1,
            text: text.into(),
            char_count: text.len(),
            source: "native".into(),
            ocr_confidence: None,
            images: vec![],
        }],
        total_chars: text.len(),
        ocr_stats: None,
    };
    document_model_to_ir(&doc, &MockSemanticAnalyzer::new())
}

fn find<'a>(ir: &'a KnowledgeIr, name: &str) -> &'a EntityCandidate {
    ir.entities
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| {
            panic!(
                "no entity {name:?}; got {:?}",
                ir.entities
                    .iter()
                    .map(|e| (&e.name, &e.type_hint))
                    .collect::<Vec<_>>()
            )
        })
}

fn ty<'a>(ir: &'a KnowledgeIr, name: &str) -> Option<&'a str> {
    find(ir, name).type_hint.as_deref()
}

#[test]
fn utrecht_international_center_is_an_organisation_not_a_person() {
    let ir = ir_of("The Utrecht International Center welcomes new residents every week.");
    assert_eq!(
        ty(&ir, "Utrecht International Center"),
        Some("Organization")
    );
    assert!(
        !ir.entities
            .iter()
            .any(|e| e.type_hint.as_deref() == Some("Person")),
        "no person in this sentence: {:?}",
        ir.entities
    );
}

#[test]
fn legal_form_suffixes_honorifics_and_gazetteer_type_names() {
    let ir = ir_of(
        "Dr. Maria Jansen left Siemens AG in Munich, Germany, for Northwind Trading B.V. \
         Mr. Peter van Dijk met Sarah Connor in London.",
    );
    assert_eq!(ty(&ir, "Siemens AG"), Some("Organization"));
    assert_eq!(ty(&ir, "Northwind Trading B.V"), Some("Organization"));
    assert_eq!(ty(&ir, "Maria Jansen"), Some("Person"));
    assert_eq!(ty(&ir, "Peter van Dijk"), Some("Person"));
    assert_eq!(ty(&ir, "Sarah Connor"), Some("Person"));
    assert_eq!(ty(&ir, "Munich"), Some("Location"));
    assert_eq!(ty(&ir, "Germany"), Some("Location"));
    assert_eq!(ty(&ir, "London"), Some("Location"));
    // A capitalised run never crosses a comma or swallows an honorific.
    for junk in ["Munich Germany", "Dr Maria", "Mr Peter"] {
        assert!(
            !ir.entities.iter().any(|e| e.name == junk),
            "junk window {junk:?} extracted"
        );
    }
}

#[test]
fn typed_values_are_recognised_with_rules_provenance() {
    let text = "Pay €120,000 or $2.5 million (12.5% interest) to IBAN DE89 3704 0044 0532 0130 00. \
                Mail john.okafor@uic.example, call +31 30 253 7000, see https://www.uic.example/partners.";
    let ir = ir_of(text);
    for (name, kind) in [
        ("€120,000", "Money"),
        ("$2.5 million", "Money"),
        ("12.5%", "Percentage"),
        ("DE89 3704 0044 0532 0130 00", "Iban"),
        ("john.okafor@uic.example", "Email"),
        ("+31 30 253 7000", "Phone"),
        ("https://www.uic.example/partners", "Url"),
    ] {
        let e = find(&ir, name);
        assert_eq!(e.type_hint.as_deref(), Some(kind), "{name}");
        assert_eq!(e.evidence.model.as_deref(), Some("rules-v1"), "{name}");
        assert!(e.confidence > 0.0 && e.confidence <= 1.0);
        match &e.evidence.source {
            Some(EvidenceSource::TextSpan {
                start_offset,
                end_offset,
            }) => {
                assert!(start_offset < end_offset, "{name}: empty span");
            }
            other => panic!("{name}: expected a TextSpan source, got {other:?}"),
        }
    }
    // A bad IBAN checksum is not an IBAN.
    let ir = ir_of("Transfer to DE00 3704 0044 0532 0130 00 today.");
    assert!(!ir
        .entities
        .iter()
        .any(|e| e.type_hint.as_deref() == Some("Iban")));
}

#[test]
fn value_entities_do_not_spawn_cooccurrence_relations() {
    let ir = ir_of("Acme Corporation paid €1,500.00 and €998.00 to Globex Industries.");
    assert!(ir
        .relations
        .iter()
        .all(|r| !r.subject.contains('€') && !r.object.contains('€')));
    assert!(ir
        .relations
        .iter()
        .any(|r| r.subject == "Acme Corporation" && r.object == "Globex Industries"));
}

#[test]
fn day_month_year_and_numeric_dates_are_temporal() {
    let ir = ir_of("Signed on 12 May 2025 and reviewed on 15/09/2025.");
    let texts: Vec<&str> = ir.temporal.iter().map(|t| t.text.as_str()).collect();
    assert!(texts.contains(&"12 May 2025"), "{texts:?}");
    assert!(texts.contains(&"15/09/2025"), "{texts:?}");
    assert!(!texts.contains(&"May 2025"), "partial date kept: {texts:?}");
    let d = ir
        .temporal
        .iter()
        .find(|t| t.text == "12 May 2025")
        .unwrap();
    assert_eq!(d.start_time.as_deref(), Some("2025-05-12T00:00:00Z"));
    let d = ir.temporal.iter().find(|t| t.text == "15/09/2025").unwrap();
    assert_eq!(d.start_time.as_deref(), Some("2025-09-15T00:00:00Z"));
}
