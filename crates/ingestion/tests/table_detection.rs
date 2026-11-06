//! M3: deterministic table detection for PDFs (text-layer word boxes) and
//! scanned images (OCR word boxes). Detected tables enter the canonical AST
//! as Table nodes with a `TablePayload`; the words stay in the page text.

use aikoql_ingestion::{
    document_model_to_ast, extract_document, extract_image_with, tool_available, AstNode,
    AstPayload, DocumentModel, OcrPageResult, OcrProvider, OcrWord, PageModel, TablePayload,
};

fn grid(t: &TablePayload) -> Vec<Vec<String>> {
    let mut rows = vec![t.headers.iter().map(|h| h.text.clone()).collect::<Vec<_>>()];
    for r in &t.rows {
        rows.push(
            t.cells
                .iter()
                .filter(|c| c.row_id == r.id)
                .map(|c| c.text.clone())
                .collect(),
        );
    }
    rows
}

fn tables(nodes: &[AstNode], out: &mut Vec<TablePayload>) {
    for n in nodes {
        if let Some(AstPayload::Table(t)) = &n.payload {
            out.push(t.clone());
        }
        tables(&n.children, out);
    }
}

fn page_tables(doc: &DocumentModel, page: usize) -> Vec<Vec<Vec<String>>> {
    let ast = document_model_to_ast(doc);
    let mut out = Vec::new();
    tables(&ast.pages[page - 1].children, &mut out);
    out.iter().map(grid).collect()
}

fn rows(r: &[&[&str]]) -> Vec<Vec<String>> {
    r.iter()
        .map(|row| row.iter().map(|c| c.to_string()).collect())
        .collect()
}

/// Lay `lines` out as OCR words: each line is a list of (x, text) cells;
/// a cell's words are 14px wide per char, 24px tall, 10px apart.
fn words(lines: &[(i32, &[(i32, &str)])]) -> Vec<OcrWord> {
    let mut out = Vec::new();
    for (li, (top, cells)) in lines.iter().enumerate() {
        for (x, text) in cells.iter() {
            let mut left = *x;
            for w in text.split_whitespace() {
                let width = 14 * w.chars().count() as i32;
                out.push(OcrWord {
                    text: w.into(),
                    confidence: 95.0,
                    left,
                    top: *top,
                    width,
                    height: 24,
                    block_num: 1,
                    line_num: li as u32,
                });
                left += width + 10;
            }
        }
    }
    out
}

struct FakeWords(Vec<OcrWord>);

impl OcrProvider for FakeWords {
    fn name(&self) -> &str {
        "fake-words"
    }
    fn available(&self) -> bool {
        true
    }
    fn recognize(&self, _: &str, _: &str, _: &str) -> Result<OcrPageResult, String> {
        let text = {
            let mut lines: Vec<(i32, Vec<&str>)> = Vec::new();
            for w in &self.0 {
                match lines.last_mut() {
                    Some((t, ws)) if *t == w.top => ws.push(&w.text),
                    _ => lines.push((w.top, vec![&w.text])),
                }
            }
            lines
                .iter()
                .map(|(_, ws)| ws.join(" "))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(OcrPageResult {
            text,
            confidence: 95.0,
            word_confidences: vec![95.0; self.0.len()],
            word_count: self.0.len(),
            words: self.0.clone(),
            block_bboxes: vec![],
        })
    }
}

// 1x1 PNG — the fake provider never decodes it.
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn image_file(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("aikoql-tbl-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("scan.png");
    std::fs::write(&p, PNG_1X1).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn ocr_words_aligned_in_columns_become_a_table() {
    let lines: &[(i32, &[(i32, &str)])] = &[
        (40, &[(80, "Quarterly sales by region")]),
        (180, &[(80, "Region"), (520, "Q1"), (820, "Q2")]),
        (
            250,
            &[(80, "North America"), (520, "1,200"), (820, "1,350")],
        ),
        (320, &[(80, "Europe"), (520, "980"), (820, "1,020")]),
        (390, &[(80, "Asia Pacific"), (520, "760")]),
    ];
    let doc = extract_image_with(
        &FakeWords(words(lines)),
        &image_file("grid"),
        "image/png",
        None,
    )
    .unwrap();
    let found = page_tables(&doc, 1);
    assert_eq!(
        found,
        vec![rows(&[
            &["Region", "Q1", "Q2"],
            &["North America", "1,200", "1,350"],
            &["Europe", "980", "1,020"],
            &["Asia Pacific", "760", ""],
        ])],
        "page text:\n{}",
        doc.pages[0].text
    );
    // The flat text keeps every word.
    for w in ["Quarterly", "America", "1,350", "Pacific"] {
        assert!(doc.pages[0].text.contains(w), "{w} lost");
    }
}

#[test]
fn two_columns_of_prose_are_not_a_table() {
    let left = [
        "Northwind Trading was founded in",
        "1998 as a small importer of tea and",
        "coffee. Today the company supplies",
        "independent shops across the region",
        "from its warehouse near the port.",
    ];
    let right = [
        "Our support team answers questions",
        "about deliveries, returns and the",
        "invoices on weekdays. Customers who",
        "order every month receive a modest",
        "discount on shipping and handling.",
    ];
    let cells: Vec<[(i32, &str); 2]> = left
        .iter()
        .zip(right.iter())
        .map(|(l, r)| [(80, *l), (900, *r)])
        .collect();
    let lines: Vec<(i32, &[(i32, &str)])> = cells
        .iter()
        .enumerate()
        .map(|(i, c)| (100 + 40 * i as i32, &c[..]))
        .collect();
    let doc = extract_image_with(
        &FakeWords(words(&lines)),
        &image_file("prose"),
        "image/png",
        None,
    )
    .unwrap();
    assert!(page_tables(&doc, 1).is_empty(), "{}", doc.pages[0].text);
}

#[test]
fn pipe_rows_keep_empty_cells_in_their_column() {
    let text = "| Item | Qty | Price |\n| Widget |  | 3.50 |\n| Gadget | 2 | 4.00 |";
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
    assert_eq!(
        page_tables(&doc, 1),
        vec![rows(&[
            &["Item", "Qty", "Price"],
            &["Widget", "", "3.50"],
            &["Gadget", "2", "4.00"],
        ])]
    );
}

const BENCH: &str = "tests/fixtures/extraction-bench";

#[test]
fn invoice_pdf_ruled_table_is_detected_from_the_text_layer() {
    if !tool_available("pdftotext") {
        eprintln!("Skipping: pdftotext (poppler) not on PATH");
        return;
    }
    let doc = extract_document(&format!("{BENCH}/invoice.pdf"), "application/pdf", None).unwrap();
    assert_eq!(
        page_tables(&doc, 1),
        vec![rows(&[
            &["Description", "Qty", "Unit price", "Amount"],
            &["Consulting services", "10", "EUR 150.00", "EUR 1,500.00"],
            &["Software licence", "2", "EUR 499.00", "EUR 998.00"],
            &["Travel expenses", "1", "EUR 320.50", "EUR 320.50"],
            &["Support plan", "12", "EUR 45.00", "EUR 540.00"],
        ])],
        "{}",
        doc.pages[0].text
    );
    assert!(page_tables(&doc, 2).is_empty(), "{}", doc.pages[1].text);
    assert!(doc.pages[0].text.contains("Northwind Trading B.V."));
    assert!(doc.pages[0].text.contains("NL91 ABNA 0417 1643 00"));
}

#[test]
fn real_scanned_table_is_detected_from_tesseract_words() {
    if !tool_available("tesseract") {
        eprintln!("Skipping: tesseract not on PATH");
        return;
    }
    let doc = extract_document(&format!("{BENCH}/scanned-table.png"), "image/png", None).unwrap();
    let found = page_tables(&doc, 1);
    assert_eq!(found.len(), 1, "{}", doc.pages[0].text);
    assert_eq!(found[0][0], vec!["Region", "Q1", "Q2", "Q3"]);
    assert_eq!(
        found[0][1],
        vec!["North America", "1,200", "1,350", "1,410"]
    );
}
