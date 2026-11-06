//! A standalone image is a supported document type: the image is stored as a
//! content-addressed asset and OCR (when a provider can read it) supplies the
//! text. A missing or failing OCR tool degrades to an image-only page — it
//! never hard-fails the document.

use aikoql_ingestion::{
    compile_document_mock_with_assets, extract_document, extract_image_with, OcrPageResult,
    OcrProvider,
};

// 1x1 PNG
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

struct Fake(Result<&'static str, &'static str>);

impl OcrProvider for Fake {
    fn name(&self) -> &str {
        "fake"
    }
    fn available(&self) -> bool {
        true
    }
    fn recognize(&self, _: &str, _: &str, _: &str) -> Result<OcrPageResult, String> {
        match self.0 {
            Ok(t) => Ok(OcrPageResult {
                text: t.into(),
                confidence: 91.5,
                word_confidences: vec![91.5],
                word_count: 1,
                words: vec![],
                block_bboxes: vec![],
            }),
            Err(e) => Err(e.into()),
        }
    }
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("aikoql-img-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn im001_a_png_extracts_to_one_page_carrying_its_asset() {
    let d = scratch("001");
    let img = d.join("shot.png");
    std::fs::write(&img, PNG).unwrap();
    let assets = d.join("assets");
    let doc = extract_document(
        img.to_str().unwrap(),
        "image/png",
        Some(assets.to_str().unwrap()),
    )
    .expect("image/png must extract");
    assert_eq!(doc.page_count, 1);
    let image = &doc.pages[0].images[0];
    assert_eq!(image.asset.mime_type, "image/png");
    assert!(assets
        .join(format!("{}.bin", image.asset.content_hash))
        .exists());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn im002_ocr_text_becomes_the_page_text() {
    let d = scratch("002");
    let img = d.join("scan.png");
    std::fs::write(&img, PNG).unwrap();
    let doc = extract_image_with(
        &Fake(Ok("Invoice total 42 EUR")),
        img.to_str().unwrap(),
        "image/png",
        None,
    )
    .unwrap();
    assert_eq!(doc.pages[0].text, "Invoice total 42 EUR");
    assert_eq!(doc.pages[0].source, "ocr");
    assert_eq!(doc.pages[0].ocr_confidence, Some(91.5));
    assert_eq!(doc.ocr_stats.as_ref().unwrap().status(), "ocr_complete");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn im003_a_failing_ocr_still_yields_the_image() {
    let d = scratch("003");
    let img = d.join("scan.png");
    std::fs::write(&img, PNG).unwrap();
    let doc = extract_image_with(
        &Fake(Err("tesseract exited with error")),
        img.to_str().unwrap(),
        "image/png",
        None,
    )
    .expect("an OCR failure must not fail the document");
    assert_eq!(doc.pages[0].text, "");
    assert_eq!(doc.pages[0].images.len(), 1);
    assert_eq!(doc.ocr_stats.as_ref().unwrap().status(), "ocr_partial");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn im004_an_image_document_compiles() {
    let d = scratch("004");
    let img = d.join("shot.png");
    std::fs::write(&img, PNG).unwrap();
    let assets = d.join("assets");
    let doc = extract_image_with(
        &Fake(Ok("Quarterly revenue grew in Utrecht")),
        img.to_str().unwrap(),
        "image/png",
        Some(assets.to_str().unwrap()),
    )
    .unwrap();
    let cr = compile_document_mock_with_assets(&doc, &[], Some(assets.to_str().unwrap()));
    assert!(serde_json::to_value(&cr).is_ok());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn im005_other_types_stay_unsupported() {
    let err = extract_document("/nonexistent", "application/zip", None).unwrap_err();
    assert!(err.contains("unsupported mime type"), "{err}");
}
