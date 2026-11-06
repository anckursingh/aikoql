//! Over-limit documents are processed batch by batch: a batch is at most
//! `max_pages` pages, the per-batch limits apply per batch, and the merged
//! batch results equal the single-pass extraction of the same document —
//! same pages, same real page numbers, same text.

use aikoql_ingestion::{
    document_page_count, extract_document, extract_document_pages, is_limit_error, merge_batches,
    plan_batches, DocumentModel, ProcessingPolicy,
};

const MM: &str = "tests/fixtures/multimodal";
const OFFICE: &str = "tests/fixtures/office";
const BENCH: &str = "tests/fixtures/extraction-bench";
const PDF: &str = "application/pdf";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

fn covers(plan: &[(u32, u32)], pages: usize, max: usize) {
    let mut next = 1u64;
    for &(a, b) in plan {
        assert_eq!(a as u64, next, "contiguous: {plan:?}");
        assert!(a <= b && (b - a + 1) as usize <= max, "size: {plan:?}");
        next = b as u64 + 1;
    }
    assert_eq!(next - 1, pages as u64, "covers every page");
}

#[test]
fn the_batch_plan_covers_every_page_once_in_order() {
    assert_eq!(plan_batches(1, 1), vec![(1, 1)]);
    assert_eq!(
        plan_batches(2000, 2000),
        vec![(1, 2000)],
        "exactly max_pages"
    );
    assert_eq!(
        plan_batches(2001, 2000),
        vec![(1, 2000), (2001, 2001)],
        "one page over"
    );
    assert_eq!(plan_batches(5, 2), vec![(1, 2), (3, 4), (5, 5)]);
    assert_eq!(plan_batches(4, 2), vec![(1, 2), (3, 4)]);
    assert!(plan_batches(0, 10).is_empty(), "no pages, no batches");
    // max_pages 0 is refused by validate(); the plan still never loops
    assert_eq!(plan_batches(2, 0), vec![(1, 1), (2, 2)]);
    let huge = plan_batches(10_000_000, 2000);
    assert_eq!(huge.len(), 5000);
    covers(&huge, 10_000_000, 2000);
    for (pages, max) in [(7, 3), (100, 7), (99, 99), (100, 99), (1, 2000)] {
        let p = plan_batches(pages, max);
        assert_eq!(p.len(), pages.div_ceil(max));
        covers(&p, pages, max);
    }
}

#[test]
fn page_count_is_read_without_limits() {
    assert_eq!(
        document_page_count(&format!("{MM}/annual-report.pdf"), PDF),
        Ok(3)
    );
    assert_eq!(
        document_page_count(&format!("{OFFICE}/workbook.xlsx"), XLSX),
        Ok(2)
    );
    assert_eq!(
        document_page_count(&format!("{OFFICE}/deck.pptx"), PPTX),
        Ok(4)
    );
}

/// A text PDF of `pages` pages: page `i` says who it is, plus filler words.
fn generated_pdf(path: &std::path::Path, pages: usize, seed: u64) {
    let mut rng = seed;
    let mut word = || {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        [
            "alpha", "Acme", "revenue", "beta", "Globex", "2025", "gamma",
        ][(rng >> 33) as usize % 7]
    };
    let mut doc = lopdf::Document::with_version("1.4");
    let font = doc.add_object(lopdf::Dictionary::from_iter([
        ("Type", lopdf::Object::Name(b"Font".to_vec())),
        ("Subtype", lopdf::Object::Name(b"Type1".to_vec())),
        ("BaseFont", lopdf::Object::Name(b"Helvetica".to_vec())),
    ]));
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for i in 1..=pages {
        let line = format!("Page {i} of the report {} {} {}", word(), word(), word());
        let content = format!("BT /F1 12 Tf 72 740 Td ({line}) Tj ET\n");
        let c = doc.add_object(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            content.into_bytes(),
        ));
        let res = lopdf::Dictionary::from_iter([(
            "Font",
            lopdf::Object::Dictionary(lopdf::Dictionary::from_iter([(
                "F1",
                lopdf::Object::Reference(font),
            )])),
        )]);
        let page = doc.add_object(lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Page".to_vec())),
            ("Parent", lopdf::Object::Reference(pages_id)),
            (
                "MediaBox",
                lopdf::Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()]),
            ),
            ("Resources", lopdf::Object::Dictionary(res)),
            ("Contents", lopdf::Object::Reference(c)),
        ]));
        kids.push(lopdf::Object::Reference(page));
    }
    doc.objects.insert(
        pages_id,
        lopdf::Object::Dictionary(lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Pages".to_vec())),
            ("Count", lopdf::Object::Integer(pages as i64)),
            ("Kids", lopdf::Object::Array(kids)),
        ])),
    );
    let catalog = doc.add_object(lopdf::Dictionary::from_iter([
        ("Type", lopdf::Object::Name(b"Catalog".to_vec())),
        ("Pages", lopdf::Object::Reference(pages_id)),
    ]));
    doc.trailer.set("Root", lopdf::Object::Reference(catalog));
    doc.save(path).unwrap();
}

/// What a page means for compile: number, text, provenance, image hashes.
fn shape(d: &DocumentModel) -> Vec<(u32, String, String, Vec<String>)> {
    d.pages
        .iter()
        .map(|p| {
            (
                p.page_number,
                p.text.clone(),
                p.source.clone(),
                p.images
                    .iter()
                    .map(|i| i.asset.content_hash.clone())
                    .collect(),
            )
        })
        .collect()
}

fn assert_batched_equals_single_pass(path: &str, mime: &str) {
    let single = extract_document(path, mime, None).unwrap();
    let pages = document_page_count(path, mime).unwrap();
    assert_eq!(pages, single.pages.len(), "{path}: page count");
    for max in 1..=pages {
        let policy = ProcessingPolicy {
            max_pages: max,
            ..Default::default()
        };
        let batches: Vec<DocumentModel> = plan_batches(pages, max)
            .into_iter()
            .map(|(a, b)| {
                let d = extract_document_pages(path, mime, None, &policy, a, b)
                    .unwrap_or_else(|e| panic!("{path} {a}-{b}: {e}"));
                assert!(d.pages.len() <= max, "{path}: a batch is at most max_pages");
                d
            })
            .collect();
        let merged = merge_batches(batches);
        assert_eq!(shape(&merged), shape(&single), "{path} max_pages={max}");
        assert_eq!(merged.total_chars, single.total_chars, "{path}");
        assert_eq!(merged.page_count, single.page_count, "{path}");
        assert_eq!(
            merged.ocr_stats.as_ref().map(|s| s.status()),
            single.ocr_stats.as_ref().map(|s| s.status()),
            "{path}"
        );
    }
}

#[test]
fn batched_extraction_of_fixtures_equals_the_single_pass() {
    for (path, mime) in [
        (format!("{MM}/annual-report.pdf"), PDF),
        (format!("{MM}/tables.pdf"), PDF),
        (format!("{BENCH}/invoice.pdf"), PDF),
        (format!("{OFFICE}/workbook.xlsx"), XLSX),
        (format!("{OFFICE}/deck.pptx"), PPTX),
    ] {
        assert_batched_equals_single_pass(&path, mime);
    }
}

#[test]
fn batched_extraction_of_generated_pdfs_equals_the_single_pass() {
    let dir = std::env::temp_dir().join(format!("aikoql-batch-gen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // seeded: page counts 1..=9 with varied filler, every max_pages
    for seed in 0..6u64 {
        let pages = 1 + (seed as usize * 5) % 9;
        let path = dir.join(format!("gen-{seed}.pdf"));
        generated_pdf(&path, pages, seed);
        let p = path.to_string_lossy();
        assert_batched_equals_single_pass(&p, PDF);
        // real page numbers survive batching
        let last = extract_document_pages(
            &p,
            PDF,
            None,
            &ProcessingPolicy {
                max_pages: 1,
                ..Default::default()
            },
            pages as u32,
            pages as u32,
        )
        .unwrap();
        assert_eq!(last.pages[0].page_number, pages as u32);
        assert!(last.pages[0].text.starts_with(&format!("Page {pages} of")));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_batch_is_held_to_max_pages_and_the_per_batch_limits() {
    let deck = format!("{OFFICE}/deck.pptx");
    let two = ProcessingPolicy {
        max_pages: 2,
        ..Default::default()
    };
    // a range wider than max_pages is itself over the limit
    let e = extract_document_pages(&deck, PPTX, None, &two, 1, 3).unwrap_err();
    assert!(is_limit_error(&e) && e.contains("max_pages"), "{e}");
    // image limits are per batch: a batch with no image passes, one with an
    // image fails — the other batches are unaffected
    let no_images = ProcessingPolicy {
        max_pages: 1,
        max_image_count: 0,
        ..Default::default()
    };
    let single = extract_document(&deck, PPTX, None).unwrap();
    let mut outcomes = Vec::new();
    for p in &single.pages {
        let n = p.page_number;
        let r = extract_document_pages(&deck, PPTX, None, &no_images, n, n);
        let raster = p.images.iter().any(|i| {
            i.asset.mime_type.starts_with("image/") && i.asset.mime_type != "image/svg+xml"
        });
        match r {
            Ok(d) => assert!(!raster && d.pages[0].page_number == n, "slide {n}"),
            Err(e) => assert!(raster && e.contains("max_image_count"), "slide {n}: {e}"),
        }
        outcomes.push(raster);
    }
    assert!(
        outcomes.contains(&true) && outcomes.contains(&false),
        "the deck has slides with and without images: {outcomes:?}"
    );
    // out-of-range pages are an error, not an empty success
    assert!(extract_document_pages(&deck, PPTX, None, &two, 5, 5).is_err());
}
