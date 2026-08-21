//! DoD 19 (HLD §52): golden fixture generator.
//!
//! Builds the ten checked-in PDFs in `tests/fixtures/multimodal/` with
//! lopdf and prints the extracted/compiled stage summary so expectations
//! (the recall lists in `multimodal_golden.rs`) can be kept honest.
//!
//! Run:
//!   cargo test -p aikoql-ingestion --test generate_multimodal_fixtures -- --ignored --nocapture
//!
//! The PDFs are checked in; regeneration is only needed when the fixture
//! content itself changes. Golden JSONs are written by `multimodal_golden`
//! with AIKOQL_UPDATE_GOLDENS=1, not here.

use std::collections::BTreeMap;
use std::path::Path;

const FIXTURE_DIR: &str = "tests/fixtures/multimodal";

/// Same synthetic JPEG payload as the lib.rs extraction tests — DCTDecode
/// passthrough, content-addressed. Two variants give two distinct hashes.
const JPEG_A: &[u8] = b"\xff\xd8\xff\xe0fakejpeg-a\xff\xd9";
const JPEG_B: &[u8] = b"\xff\xd8\xff\xe0fakejpeg-b\xff\xd9";

/// Minimal multi-page PDF builder: one text line per Tj run; empty lines
/// emit an empty Tj (lopdf renders them as real blank lines, which
/// `classify_blocks` uses as block boundaries). Images are DCTDecode
/// XObjects shared across pages via a per-page resource dict.
struct PdfBuilder {
    doc: lopdf::Document,
    font_id: lopdf::ObjectId,
    pages: Vec<lopdf::ObjectId>,
}

impl PdfBuilder {
    fn new() -> Self {
        let mut doc = lopdf::Document::with_version("1.4");
        let font_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Font".to_vec())),
            ("Subtype", lopdf::Object::Name(b"Type1".to_vec())),
            ("BaseFont", lopdf::Object::Name(b"Helvetica".to_vec())),
            ("Encoding", lopdf::Object::Name(b"WinAnsiEncoding".to_vec())),
        ]);
        let font_id = doc.add_object(lopdf::Object::Dictionary(font_dict));
        PdfBuilder {
            doc,
            font_id,
            pages: Vec::new(),
        }
    }

    /// Add a page: `lines` are written top-down (one Tj per line, empty
    /// line = blank), `images` are named DCTDecode JPEG XObjects.
    fn page(&mut self, lines: &[&str], images: &[(&str, &'static [u8])]) {
        let mut content = Vec::new();
        let mut y: i64 = 740;
        for line in lines {
            content.extend(format!("BT /F1 12 Tf 72 {} Td ({}) Tj ET\n", y, line).into_bytes());
            y -= 14;
        }

        let xobjs: Vec<(&str, lopdf::Object)> = images
            .iter()
            .map(|(name, bytes)| {
                let dict = lopdf::Dictionary::from_iter([
                    ("Type", lopdf::Object::Name(b"XObject".to_vec())),
                    ("Subtype", lopdf::Object::Name(b"Image".to_vec())),
                    ("Width", lopdf::Object::Integer(8)),
                    ("Height", lopdf::Object::Integer(8)),
                    ("ColorSpace", lopdf::Object::Name(b"DeviceRGB".to_vec())),
                    ("BitsPerComponent", lopdf::Object::Integer(8)),
                    ("Filter", lopdf::Object::Name(b"DCTDecode".to_vec())),
                ]);
                let id = self
                    .doc
                    .add_object(lopdf::Object::Stream(lopdf::Stream::new(
                        dict,
                        bytes.to_vec(),
                    )));
                (*name, lopdf::Object::Reference(id))
            })
            .collect();
        let xobj_dict = lopdf::Dictionary::from_iter(xobjs);
        let fonts = lopdf::Dictionary::from_iter([("F1", lopdf::Object::Reference(self.font_id))]);
        let resources = lopdf::Dictionary::from_iter([
            ("Font", lopdf::Object::Dictionary(fonts)),
            ("XObject", lopdf::Object::Dictionary(xobj_dict)),
        ]);
        let content_id = self
            .doc
            .add_object(lopdf::Object::Stream(lopdf::Stream::new(
                lopdf::Dictionary::new(),
                content,
            )));
        let page_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Page".to_vec())),
            (
                "MediaBox",
                lopdf::Object::Array(vec![
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(0),
                    lopdf::Object::Integer(612),
                    lopdf::Object::Integer(792),
                ]),
            ),
            ("Resources", lopdf::Object::Dictionary(resources)),
            ("Contents", lopdf::Object::Reference(content_id)),
        ]);
        let page_id = self.doc.add_object(lopdf::Object::Dictionary(page_dict));
        self.pages.push(page_id);
    }

    fn save(&mut self, name: &str) {
        let kids: Vec<lopdf::Object> = self
            .pages
            .iter()
            .map(|id| lopdf::Object::Reference(*id))
            .collect();
        let pages_dict = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Pages".to_vec())),
            ("Kids", lopdf::Object::Array(kids)),
            ("Count", lopdf::Object::Integer(self.pages.len() as i64)),
        ]);
        let pages_id = self.doc.add_object(lopdf::Object::Dictionary(pages_dict));
        let catalog = lopdf::Dictionary::from_iter([
            ("Type", lopdf::Object::Name(b"Catalog".to_vec())),
            ("Pages", lopdf::Object::Reference(pages_id)),
        ]);
        let catalog_id = self.doc.add_object(lopdf::Object::Dictionary(catalog));
        self.doc
            .trailer
            .set("Root", lopdf::Object::Reference(catalog_id));
        let path = Path::new(FIXTURE_DIR).join(name);
        self.doc.save(&path).unwrap();
    }
}

/// All ten HLD §52 fixtures, in the HLD's order.
#[test]
#[ignore]
fn generate_multimodal_fixtures() {
    std::fs::create_dir_all(FIXTURE_DIR).unwrap();

    // 1. plain-text.pdf — text-only document, heading + paragraph structure.
    let mut b = PdfBuilder::new();
    b.page(
        &[
            "1. Overview",
            "",
            "Acme Corporation publishes quarterly reports.",
            "",
            "Prepared by Globex Industries.",
        ],
        &[],
    );
    b.page(
        &[
            "1. Financials",
            "",
            "Revenue reached $10M for Q3 2025.",
            "",
            "Cash position remains healthy.",
        ],
        &[],
    );
    b.save("plain-text.pdf");

    // 2. scanned.pdf — image-only page (no text runs): the classic scanned
    // signature. Native text is absent; the image asset is extracted. The
    // OCR fill path is unit-tested separately (tesseract-dependent), so the
    // golden asserts the deterministic degraded path.
    let mut b = PdfBuilder::new();
    b.page(&["Scan of original invoice 2024", ""], &[("Im1", JPEG_A)]);
    b.save("scanned.pdf");

    // 3. tables.pdf — two simple pipe tables on separate pages.
    let mut b = PdfBuilder::new();
    b.page(
        &[
            "| Employee Name | Age |",
            "| Alice Smith | 30 |",
            "| Bob Johnson | 45 |",
        ],
        &[],
    );
    b.page(
        &[
            "| Region | Revenue (USD) |",
            "| North America | 1200 |",
            "| South America | 800 |",
        ],
        &[],
    );
    b.save("tables.pdf");

    // 4. complex-table.pdf — caption + multi-column table with units and
    // percents; second page with a different column set.
    let mut b = PdfBuilder::new();
    b.page(
        &[
            "Table 2: Quarterly margins",
            "",
            "| Quarter | Units Sold | Margin (%) |",
            "| Q1 2025 | 4000 | 12.5 |",
            "| Q2 2025 | 5100 | 13.0 |",
        ],
        &[],
    );
    b.page(
        &[
            "| Product Line | Warranty (months) |",
            "| Industrial Sensors | 36 |",
            "| Home Automation | 24 |",
        ],
        &[],
    );
    b.save("complex-table.pdf");

    // 5. charts.pdf — figure marker + caption + adjacent table: the chart
    // specialist pass fills axes/series from the table (HLD §33, no VLM).
    let mut b = PdfBuilder::new();
    b.page(
        &[
            "Figure 1: Revenue bar chart by quarter",
            "Revenue in USD millions",
            "",
            "| Fiscal Quarter | Total Revenue |",
            "| Q1 | 1200 |",
            "| Q2 | 1500 |",
        ],
        &[],
    );
    b.save("charts.pdf");

    // 6. architecture-diagram.pdf — figure marker with an arrow-chain
    // caption: diagram nodes/edges come from the caption text.
    let mut b = PdfBuilder::new();
    b.page(
        &[
            "Figure 2: Architecture diagram",
            "Client -> Gateway -> Database",
            "",
            "Gateway -> Cache",
        ],
        &[],
    );
    b.save("architecture-diagram.pdf");

    // 7. mixed-report.pdf — headings, paragraphs, a flow figure, a table
    // and an indented code block on one page.
    let mut b = PdfBuilder::new();
    b.page(
        &[
            "1. Billing Pipeline",
            "",
            "Acme Corporation processes payments nightly.",
            "",
            "Figure 4: Flow diagram of billing",
            "Payment -> Ledger",
            "",
            "| Step | Owner |",
            "| Validate | Billing Team |",
            "| Commit | Ledger Team |",
            "",
            "    let total = 100;",
            "    assert!(total > 0);",
        ],
        &[],
    );
    b.save("mixed-report.pdf");

    // 8. formulas.pdf — formula content has no structural marker in PDF
    // text; the golden asserts verbatim preservation (nothing lost) until
    // a visual formula detector lands behind the analyzer seam.
    let mut b = PdfBuilder::new();
    b.page(
        &[
            "Equation 1: E = mc^2",
            "",
            "Energy equals mass times light speed squared.",
            "",
            "Equation 2: F = B * R",
        ],
        &[],
    );
    b.save("formulas.pdf");

    // 9. images.pdf — figure caption + two embedded image assets.
    let mut b = PdfBuilder::new();
    b.page(
        &["Figure 3: Company logo", ""],
        &[("Im1", JPEG_A), ("Im2", JPEG_B)],
    );
    b.save("images.pdf");

    // 10. annual-report.pdf — three pages: title page, financials with
    // dates (temporal), outlook with a table.
    let mut b = PdfBuilder::new();
    b.page(
        &[
            "Annual Report 2025",
            "",
            "Prepared for the Board of Directors in January 2025.",
            "",
            "Acme Corporation overview follows.",
        ],
        &[],
    );
    b.page(
        &[
            "1. Financials",
            "",
            "Globex Industries revenue reached $10M.",
            "",
            "Approved on 2025-02-15.",
        ],
        &[],
    );
    b.page(
        &[
            "1. Outlook",
            "",
            "| Metric | Value |",
            "| Growth | 8 percent |",
            "",
            "Gamma Partners expect continued growth.",
        ],
        &[],
    );
    b.save("annual-report.pdf");

    // Summary pass — print extracted entities and stage counts per fixture
    // so hand-annotated expectations in multimodal_golden.rs stay honest.
    eprintln!("FIXTURE_SUMMARY");
    for name in [
        "plain-text.pdf",
        "scanned.pdf",
        "tables.pdf",
        "complex-table.pdf",
        "charts.pdf",
        "architecture-diagram.pdf",
        "mixed-report.pdf",
        "formulas.pdf",
        "images.pdf",
        "annual-report.pdf",
    ] {
        let path = Path::new(FIXTURE_DIR).join(name);
        let dm =
            aikoql_ingestion::extract_document(&path.to_string_lossy(), "application/pdf", None)
                .expect("extract");
        let result = aikoql_ingestion::compile_document_mock(&dm, &[]);
        let mut modality_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for f in &result.fragments {
            let key = match f.modality {
                aikoql_ingestion::FragmentModality::Text => "Text",
                aikoql_ingestion::FragmentModality::Table => "Table",
                aikoql_ingestion::FragmentModality::Image => "Image",
                aikoql_ingestion::FragmentModality::Chart => "Chart",
                aikoql_ingestion::FragmentModality::Diagram => "Diagram",
                aikoql_ingestion::FragmentModality::Formula => "Formula",
                aikoql_ingestion::FragmentModality::Code => "Code",
                aikoql_ingestion::FragmentModality::Mixed => "Mixed",
            };
            *modality_counts.entry(key).or_default() += 1;
        }
        let entities: Vec<&str> = result.ir.entities.iter().map(|e| e.name.as_str()).collect();
        eprintln!("{}: pages={} chars={} images={} frags={:?} entities={:?} facts={} rels={} temporal={} chunks={}",
            name, dm.page_count, dm.total_chars,
            dm.pages.iter().map(|p| p.images.len()).sum::<usize>(),
            modality_counts, entities, result.ir.facts.len(), result.ir.relations.len(),
            result.ir.temporal.len(), result.embedded_chunks.len());
    }
}
