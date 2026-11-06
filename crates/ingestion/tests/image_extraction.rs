//! A standalone image is a supported document type: the image is stored as a
//! content-addressed asset and OCR (when a provider can read it) supplies the
//! text. A missing or failing OCR tool degrades to an image-only page — it
//! never hard-fails the document.

use aikoql_ingestion::{
    compile_document_mock_with_assets, extract_document, extract_image_with, tool_available,
    OcrPageResult, OcrProvider, TesseractCli,
};
use std::sync::Mutex;

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

#[test]
fn im006_no_ocr_tool_is_reported_as_ocr_unavailable_not_extracted() {
    let d = scratch("006");
    let img = d.join("scan.png");
    std::fs::write(&img, PNG).unwrap();
    let missing = TesseractCli {
        tesseract_path: Some("/nonexistent/tesseract".into()),
        pdftoppm_path: None,
    };
    let doc = extract_image_with(&missing, img.to_str().unwrap(), "image/png", None).unwrap();
    assert_eq!(doc.pages[0].text, "");
    assert_eq!(doc.pages[0].images.len(), 1, "the image is still kept");
    let status = doc.ocr_stats.as_ref().map(|s| s.status());
    assert_eq!(
        status,
        Some("ocr_unavailable"),
        "an image with no OCR tool must say so, not claim `extracted`"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn im007_ocr_that_reads_no_text_is_not_counted_as_a_success() {
    let d = scratch("007");
    let img = d.join("blank.png");
    std::fs::write(&img, PNG).unwrap();
    let doc =
        extract_image_with(&Fake(Ok("   ")), img.to_str().unwrap(), "image/png", None).unwrap();
    let stats = doc.ocr_stats.as_ref().unwrap();
    // parity with the PDF path: attempted, neither succeeded nor failed
    assert_eq!(stats.pages_ocr_attempted, 1);
    assert_eq!(stats.pages_ocr_succeeded, 0);
    assert_eq!(doc.pages[0].text, "");
    assert_eq!(doc.pages[0].source, "native");
    assert_eq!(doc.pages[0].ocr_confidence, None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn im008_only_formats_the_ocr_engine_reads_are_accepted() {
    let d = scratch("008");
    let img = d.join("x.img");
    std::fs::write(&img, PNG).unwrap();
    let p = img.to_str().unwrap();
    for ok in [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/bmp",
        "image/tiff",
    ] {
        assert!(extract_document(p, ok, None).is_ok(), "{ok} must extract");
    }
    for bad in ["image/heic", "image/avif", "image/x-icon", "image/svg+xml"] {
        let err = extract_document(p, bad, None).unwrap_err();
        assert!(err.contains("unsupported mime type"), "{bad}: {err}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Records each OCR work dir and whether it existed during the call.
struct Recorder(Mutex<Vec<String>>);

impl OcrProvider for Recorder {
    fn name(&self) -> &str {
        "recorder"
    }
    fn available(&self) -> bool {
        true
    }
    fn recognize(&self, _: &str, _: &str, work_dir: &str) -> Result<OcrPageResult, String> {
        assert!(std::path::Path::new(work_dir).is_dir());
        self.0.lock().unwrap().push(work_dir.to_string());
        Err("recorded".into())
    }
}

#[test]
fn im009_each_ocr_call_gets_its_own_work_dir_and_it_is_removed() {
    let d = scratch("009");
    let img = d.join("same.png");
    std::fs::write(&img, PNG).unwrap();
    let rec = Recorder(Mutex::new(vec![]));
    for _ in 0..2 {
        extract_image_with(&rec, img.to_str().unwrap(), "image/png", None).unwrap();
    }
    let dirs = rec.0.into_inner().unwrap();
    // the same image ingested and compiled at once must not share (and
    // delete) one work dir
    assert_ne!(dirs[0], dirs[1]);
    for w in &dirs {
        assert!(
            !std::path::Path::new(w).exists(),
            "work dir left behind: {w}"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn im010_an_empty_image_is_bad_input() {
    let d = scratch("010");
    let img = d.join("empty.png");
    std::fs::write(&img, b"").unwrap();
    let err = extract_document(img.to_str().unwrap(), "image/png", None).unwrap_err();
    // "bad" is what the REST layer maps to 400
    assert!(err.starts_with("bad image"), "{err}");
    let _ = std::fs::remove_dir_all(&d);
}

/// Real Tesseract on committed PNG/JPEG fixtures with known text. Needs only
/// `tesseract` — a standalone image is never rasterized, so `pdftoppm` is not
/// required. Skips (passes with a note) when tesseract is not on PATH.
#[test]
fn im011_real_tesseract_reads_png_and_jpeg_and_the_text_reaches_compile() {
    if !tool_available("tesseract") {
        eprintln!("Skipping im011: tesseract not on PATH (brew install tesseract)");
        return;
    }
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (file, mime) in [
        ("ocr-text.png", "image/png"),
        ("ocr-text.jpg", "image/jpeg"),
    ] {
        let d = scratch(&format!("011-{file}"));
        let assets = d.join("assets");
        let path = fixtures.join(file);
        let doc =
            extract_document(path.to_str().unwrap(), mime, Some(assets.to_str().unwrap())).unwrap();
        let page = &doc.pages[0];
        eprintln!(
            "im011 {file}: {:?} confidence {:?}",
            page.text, page.ocr_confidence
        );
        for word in ["Invoice", "Acme", "Utrecht", "42", "EUR"] {
            assert!(
                page.text.contains(word),
                "{file}: {word} not in {:?}",
                page.text
            );
        }
        assert_eq!(page.source, "ocr");
        assert!(page.ocr_confidence.unwrap() > 50.0);
        assert_eq!(doc.ocr_stats.as_ref().unwrap().status(), "ocr_complete");
        assert_eq!(doc.total_chars, page.text.len());
        let cr = compile_document_mock_with_assets(&doc, &[], Some(assets.to_str().unwrap()));
        let chunks = serde_json::to_string(&cr.embedded_chunks).unwrap();
        assert!(
            chunks.contains("Utrecht"),
            "OCR text must reach the compiled chunks"
        );

        // A standalone image is never rasterized: with pdftoppm unrunnable
        // (so `available()` is false) the image is still OCR'd.
        let no_pdftoppm = TesseractCli {
            tesseract_path: None,
            pdftoppm_path: Some("/nonexistent/pdftoppm".into()),
        };
        assert!(!no_pdftoppm.available());
        let doc = extract_image_with(&no_pdftoppm, path.to_str().unwrap(), mime, None).unwrap();
        assert!(
            doc.pages[0].text.contains("Utrecht"),
            "{file}: {:?}",
            doc.pages[0].text
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// The scanned-PDF path on an image-only PDF fixture (no text layer): pages
/// are rasterized by pdftoppm and read by Tesseract. Skips without both.
#[test]
fn im012_real_tesseract_reads_a_scanned_pdf() {
    if !TesseractCli::new().available() {
        eprintln!("Skipping im012: tesseract and pdftoppm not both on PATH");
        return;
    }
    let pdf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ocr-scan.pdf");
    let d = scratch("012");
    let assets = d.join("assets");
    let doc = extract_document(
        pdf.to_str().unwrap(),
        "application/pdf",
        Some(assets.to_str().unwrap()),
    )
    .unwrap();
    let page = &doc.pages[0];
    eprintln!(
        "im012 ocr-scan.pdf: {:?} confidence {:?}",
        page.text, page.ocr_confidence
    );
    // case-insensitive: Tesseract may read a leading capital as lowercase
    let text = page.text.to_lowercase();
    for word in ["scanned", "receipt", "rotterdam", "total", "17", "eur"] {
        assert!(text.contains(word), "{word} not in {:?}", page.text);
    }
    assert_eq!(page.source, "ocr");
    assert_eq!(doc.ocr_stats.as_ref().unwrap().status(), "ocr_complete");
    let _ = std::fs::remove_dir_all(&d);
}
