//! M5: HLD §55 ProcessingPolicy with the HLD §54 asset limits. Every limit
//! hit is a classified error ("limit exceeded: <limit> …") — never a panic,
//! never a silent truncation.

use aikoql_ingestion::{
    extract_document_with_policy, extract_image_with_policy, is_limit_error, OcrPageResult,
    OcrProvider, ProcessingPolicy,
};

const BENCH: &str = "tests/fixtures/extraction-bench";
const OFFICE: &str = "tests/fixtures/office";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

fn limited(f: impl FnOnce(&mut ProcessingPolicy)) -> ProcessingPolicy {
    let mut p = ProcessingPolicy::default();
    f(&mut p);
    p
}

fn assert_limit(r: Result<aikoql_ingestion::DocumentModel, String>, limit: &str) {
    let e = r
        .err()
        .unwrap_or_else(|| panic!("{limit}: expected a limit error"));
    assert!(is_limit_error(&e) && e.contains(limit), "{limit}: {e}");
}

/// Fails the test if OCR is attempted.
struct NoOcr;
impl OcrProvider for NoOcr {
    fn name(&self) -> &str {
        "no-ocr"
    }
    fn available(&self) -> bool {
        true
    }
    fn recognize(&self, _: &str, _: &str, _: &str) -> Result<OcrPageResult, String> {
        panic!("OCR must not run")
    }
}

struct SlowOcr;
impl OcrProvider for SlowOcr {
    fn name(&self) -> &str {
        "slow"
    }
    fn available(&self) -> bool {
        true
    }
    fn recognize(&self, _: &str, _: &str, _: &str) -> Result<OcrPageResult, String> {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        Ok(OcrPageResult {
            text: "late".into(),
            confidence: 90.0,
            word_confidences: vec![90.0],
            word_count: 1,
            words: vec![],
            block_bboxes: vec![],
        })
    }
}

#[test]
fn defaults_are_safe_and_permit_normal_documents() {
    let p = ProcessingPolicy::default();
    assert!(p.enable_ocr && p.max_pages > 0 && p.max_image_count > 0);
    assert!(p.max_image_dimension >= 10_000 && p.max_ocr_seconds > 0);
    assert!(
        !p.enable_transformer_boundaries,
        "no model in the default path"
    );
    extract_document_with_policy(&format!("{OFFICE}/deck.pptx"), PPTX, None, &p).unwrap();
    extract_document_with_policy(&format!("{BENCH}/invoice.pdf"), "application/pdf", None, &p)
        .unwrap();
}

#[test]
fn max_pages_covers_pdf_pages_sheets_and_slides() {
    let p = limited(|p| p.max_pages = 1);
    assert_limit(
        extract_document_with_policy(&format!("{BENCH}/invoice.pdf"), "application/pdf", None, &p),
        "max_pages",
    );
    assert_limit(
        extract_document_with_policy(&format!("{OFFICE}/workbook.xlsx"), XLSX, None, &p),
        "max_pages",
    );
    assert_limit(
        extract_document_with_policy(&format!("{OFFICE}/deck.pptx"), PPTX, None, &p),
        "max_pages",
    );
}

#[test]
fn max_image_count_and_max_visual_assets() {
    let path = format!("{OFFICE}/deck.pptx");
    assert_limit(
        extract_document_with_policy(&path, PPTX, None, &limited(|p| p.max_image_count = 0)),
        "max_image_count",
    );
    assert_limit(
        extract_document_with_policy(&path, PPTX, None, &limited(|p| p.max_visual_assets = 0)),
        "max_visual_assets",
    );
}

#[test]
fn max_image_dimension_is_read_from_headers_before_ocr() {
    let p = limited(|p| p.max_image_dimension = 1000);
    // scanned-table.png is 1500 px wide: refused before any OCR runs.
    assert_limit(
        extract_image_with_policy(
            &NoOcr,
            &format!("{BENCH}/scanned-table.png"),
            "image/png",
            None,
            &p,
        ),
        "max_image_dimension",
    );
    // Embedded PDF images: /Width and /Height of the image XObject (8x8).
    assert_limit(
        extract_document_with_policy(
            "tests/fixtures/multimodal/images.pdf",
            "application/pdf",
            None,
            &limited(|p| p.max_image_dimension = 4),
        ),
        "max_image_dimension",
    );
}

#[test]
fn max_ocr_seconds_is_a_document_deadline() {
    let p = limited(|p| p.max_ocr_seconds = 1);
    assert_limit(
        extract_image_with_policy(
            &SlowOcr,
            &format!("{BENCH}/scanned-letter.png"),
            "image/png",
            None,
            &p,
        ),
        "max_ocr_seconds",
    );
}

#[test]
fn disabled_ocr_never_runs_and_says_so() {
    let p = limited(|p| p.enable_ocr = false);
    let doc = extract_image_with_policy(
        &NoOcr,
        &format!("{BENCH}/scanned-letter.png"),
        "image/png",
        None,
        &p,
    )
    .unwrap();
    assert!(doc.pages[0].text.is_empty());
    assert_eq!(doc.pages[0].images.len(), 1, "the image is still kept");
    assert_eq!(
        doc.ocr_stats.as_ref().map(|s| s.status()),
        Some("ocr_disabled")
    );
}
