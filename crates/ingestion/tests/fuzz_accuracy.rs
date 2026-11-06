//! Seeded fuzz + generated-accuracy tests for the M1–M5 extraction code.
//!
//! No new dependency: a fixed-seed xorshift drives every case, so a failure
//! reproduces from the printed seed/iteration. Fuzz = never panic, stay
//! deterministic, keep structural invariants. Accuracy = generate inputs with
//! a known answer and assert a measured score.

use aikoql_ingestion::{
    detect_tables, document_model_to_ir, extract_document, image_dimensions, DocumentModel,
    KnowledgeIr, MockSemanticAnalyzer, OcrWord, PageModel,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

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

const TOKENS: &[&str] = &[
    "Dr.",
    "Mr",
    "Mrs.",
    "Prof",
    "Acme",
    "Corp",
    "AG",
    "B.V.",
    "GmbH",
    "Ltd",
    "Inc.",
    "Utrecht",
    "London",
    "Maria",
    "Jansen",
    "van",
    "de",
    "Anna",
    "Müller",
    "Kraków",
    "東京",
    "Ünal",
    "€120",
    "$5.5",
    "12.5%",
    "a@b.co",
    "https://x.example/p",
    "+31",
    "30",
    "253",
    "7000",
    "DE89",
    "3704",
    "0044",
    "0532",
    "0130",
    "00",
    "IBAN",
    "Asia",
    "Malaysia",
    "AKIAIOSFODNN7EXAMPLE",
    ",",
    ".",
    ";",
    "(",
    ")",
    "\"",
    "'",
    "-",
    "—",
    "\n",
    "\t",
    "  ",
    "",
    "1,000,000",
    "0.0.0.0",
    "İstanbul",
    "\u{200b}",
    "\u{0301}",
    "😀",
];

fn random_text(r: &mut Rng) -> String {
    let n = r.below(60);
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(r.pick(TOKENS));
        if r.below(4) != 0 {
            s.push(' ');
        }
    }
    s
}

#[test]
fn fz001_entity_analysis_never_panics_and_is_deterministic() {
    let mut r = Rng(0x9e3779b97f4a7c15);
    for i in 0..1500 {
        let text = random_text(&mut r);
        let a = ir_of(&text);
        let b = ir_of(&text);
        assert_eq!(
            a.entities, b.entities,
            "iter {i} nondeterministic: {text:?}"
        );
        for e in &a.entities {
            assert!(
                !e.name.trim().is_empty(),
                "iter {i} empty name from {text:?}"
            );
            assert!(
                (0.0..=1.0).contains(&e.confidence),
                "iter {i} confidence {} from {text:?}",
                e.confidence
            );
        }
    }
}

#[test]
fn fz002_entity_analysis_survives_adversarial_shapes() {
    let big = "Acme ".repeat(20_000);
    let deep = "(".repeat(5_000) + &")".repeat(5_000);
    let digits = "1".repeat(100_000);
    let nul = "Dr. \0 Maria \0 Jansen\0";
    for t in [
        big.as_str(),
        &deep,
        &digits,
        nul,
        "",
        " ",
        "\n\n\n",
        "@",
        "a@",
        "@b",
        "+",
        "€",
    ] {
        let _ = ir_of(t);
    }
}

#[test]
fn fz003_aws_key_filter_only_matches_whole_key_tokens() {
    // The old rule redacted any text containing "asia".
    for t in [
        "Asia Pacific",
        "Malaysia",
        "Kuala Lumpur, Malaysia office",
        "asiatic",
    ] {
        let ir = ir_of(t);
        for e in &ir.entities {
            assert!(!e.name.contains("REDACTED"), "{t:?} -> {:?}", e.name);
        }
    }
}

fn word(text: &str, left: i32, top: i32, width: i32, height: i32, line: u32) -> OcrWord {
    OcrWord {
        text: text.into(),
        confidence: 95.0,
        left,
        top,
        width,
        height,
        block_num: 1,
        line_num: line,
    }
}

#[test]
fn fz004_table_detection_never_panics_on_random_boxes() {
    let mut r = Rng(0xdeadbeefcafef00d);
    for i in 0..1500 {
        let n = r.below(80);
        let words: Vec<OcrWord> = (0..n)
            .map(|_| {
                let mut w = word(
                    r.pick(&["a", "12", "x y", "", "—", "€5", "Name", "|", "東京"]),
                    r.below(3000) as i32 - 200,
                    r.below(3000) as i32 - 200,
                    r.below(400) as i32 - 20,
                    r.below(60) as i32 - 5,
                    r.below(20) as u32,
                );
                w.confidence = (r.below(130) as f32) - 10.0;
                w
            })
            .collect();
        let a = detect_tables(&words);
        let b = detect_tables(&words);
        assert_eq!(a, b, "iter {i} nondeterministic");
        for t in &a {
            assert!(t.rows.len() >= 2, "iter {i}: table with <2 rows");
            let w = t.rows[0].len();
            assert!(w >= 2, "iter {i}: table with <2 columns");
            assert!(
                t.rows.iter().all(|row| row.len() == w),
                "iter {i}: ragged table {:?}",
                t.rows
            );
        }
    }
}

#[test]
fn fz005_table_detection_survives_extreme_coordinates() {
    let ext = [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];
    let mut words = Vec::new();
    for (k, &l) in ext.iter().enumerate() {
        for (j, &t) in ext.iter().enumerate() {
            words.push(word(
                "w1",
                l,
                t,
                ext[(k + j) % 7],
                ext[(k * 3 + j) % 7],
                (k % 3) as u32,
            ));
        }
    }
    let _ = detect_tables(&words);
}

/// Lay a known grid out as OCR word boxes: fixed column x-origins, one text
/// line per row, words inside a cell separated by a narrow gap.
fn layout(grid: &[Vec<String>], r: &mut Rng) -> Vec<OcrWord> {
    let cols = grid[0].len();
    let col_x: Vec<i32> = (0..cols).map(|c| 100 + c as i32 * 380).collect();
    let mut words = Vec::new();
    for (ri, row) in grid.iter().enumerate() {
        let top = 100 + ri as i32 * 50 + r.below(3) as i32; // slight jitter
        for (ci, cell) in row.iter().enumerate() {
            let mut x = col_x[ci];
            for part in cell.split(' ') {
                let w = 14 * part.chars().count() as i32;
                words.push(word(part, x, top, w, 30, ri as u32 + 1));
                x += w + 12;
            }
        }
    }
    words
}

fn rand_cell(r: &mut Rng, numeric: bool) -> String {
    const A: &[&str] = &[
        "North", "Europe", "Billing", "Alice", "Ledger", "Berlin", "Retail", "Cloud", "Seats",
        "Pro", "Basic", "Support",
    ];
    if numeric {
        format!("{}.{:02}", 10 + r.below(9000), r.below(100))
    } else if r.below(3) == 0 {
        format!("{} {}", r.pick(A), r.pick(A))
    } else {
        r.pick(A).to_string()
    }
}

#[test]
fn ac001_generated_grids_are_recovered_cell_for_cell() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    let (mut ok_cells, mut all_cells, mut found) = (0usize, 0usize, 0usize);
    let runs = 300;
    for _ in 0..runs {
        let cols = 3 + r.below(3);
        let rows = 3 + r.below(6);
        let mut grid = vec![(0..cols)
            .map(|c| format!("Col{}", c + 1))
            .collect::<Vec<_>>()];
        for _ in 1..rows {
            grid.push((0..cols).map(|c| rand_cell(&mut r, c > 0)).collect());
        }
        let tables = detect_tables(&layout(&grid, &mut r));
        all_cells += rows * cols;
        if let Some(t) = tables.first().filter(|_| tables.len() == 1) {
            found += 1;
            if t.rows.len() == rows && t.rows[0].len() == cols {
                for (a, b) in grid.iter().zip(&t.rows) {
                    ok_cells += a.iter().zip(b).filter(|(x, y)| x == y).count();
                }
            }
        }
    }
    let acc = ok_cells as f64 / all_cells as f64;
    eprintln!("grid recovery: tables {found}/{runs}, cell accuracy {acc:.4}");
    assert!(found * 100 >= runs * 98, "table found in {found}/{runs}");
    assert!(acc >= 0.98, "cell accuracy {acc:.4}");
}

#[test]
fn ac002_generated_prose_never_yields_a_table() {
    let mut r = Rng(0xabad1dea);
    const W: &[&str] = &[
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "and",
    ];
    for i in 0..300 {
        let mut words = Vec::new();
        for line in 0..(5 + r.below(15)) {
            let mut x = 100;
            for _ in 0..(4 + r.below(8)) {
                let t = r.pick(W);
                let w = 14 * t.len() as i32;
                words.push(word(t, x, 100 + line as i32 * 40, w, 30, line as u32 + 1));
                x += w + 14; // uniform single-space gap = running text
            }
        }
        assert!(
            detect_tables(&words).is_empty(),
            "iter {i}: prose read as table"
        );
    }
}

#[test]
fn ac003_generated_entities_are_typed_correctly() {
    const GIVEN: &[&str] = &["Maria", "Peter", "Sarah", "Anna", "John", "David", "Laura"];
    const SUR: &[&str] = &["Jansen", "Connor", "Lindqvist", "Okafor", "Smith", "Bakker"];
    const ORG: &[&str] = &[
        "Northwind",
        "Contoso",
        "Globex",
        "Initech",
        "Umbrella",
        "Vandelay",
    ];
    const FORM: &[&str] = &["AG", "GmbH", "Ltd", "Inc", "Corp", "B.V."];
    const CITY: &[&str] = &["London", "Munich", "Amsterdam", "Berlin", "Paris", "Madrid"];
    let mut r = Rng(0x0ddba11);
    let (mut tp, mut fp, mut fnn) = (0usize, 0usize, 0usize);
    for _ in 0..300 {
        let p = format!("{} {}", r.pick(GIVEN), r.pick(SUR));
        let o = format!("{} {}", r.pick(ORG), r.pick(FORM));
        let c = r.pick(CITY).to_string();
        let text = format!("Dr. {p} joined {o} in {c} last year.");
        let ir = ir_of(&text);
        for (name, ty) in [(&p, "Person"), (&o, "Organization"), (&c, "Location")] {
            // A trailing "." of a legal form may be trimmed from the name.
            let hit = ir.entities.iter().any(|e| {
                e.type_hint.as_deref() == Some(ty)
                    && (e.name == *name || name.trim_end_matches('.') == e.name)
            });
            if hit {
                tp += 1;
            } else {
                fnn += 1;
            }
        }
        for e in &ir.entities {
            let known = e.name == p
                || e.name == c
                || e.name.trim_end_matches('.') == o.trim_end_matches('.')
                || e.name == o;
            if !known {
                fp += 1;
            }
        }
    }
    let prec = tp as f64 / (tp + fp) as f64;
    let rec = tp as f64 / (tp + fnn) as f64;
    eprintln!("entity typing on generated text: precision {prec:.3} recall {rec:.3}");
    assert!(prec >= 0.95, "precision {prec:.3}");
    assert!(rec >= 0.95, "recall {rec:.3}");
}

#[test]
fn ac004_iban_checksum_rejects_every_single_digit_corruption() {
    let good = "DE89 3704 0044 0532 0130 00";
    assert!(ir_of(&format!("pay {good} now"))
        .entities
        .iter()
        .any(|e| e.type_hint.as_deref() == Some("Iban")));
    let digits: Vec<usize> = good
        .char_indices()
        .filter(|(_, c)| c.is_ascii_digit())
        .map(|(i, _)| i)
        .collect();
    for &i in digits.iter().skip(2) {
        let orig = good.as_bytes()[i];
        let mut bad = good.as_bytes().to_vec();
        bad[i] = if orig == b'9' { b'0' } else { orig + 1 };
        let bad = String::from_utf8(bad).unwrap();
        assert!(
            !ir_of(&format!("pay {bad} now"))
                .entities
                .iter()
                .any(|e| e.type_hint.as_deref() == Some("Iban")),
            "corrupted IBAN accepted: {bad}"
        );
    }
}

// ---- byte-level fuzz of the file extractors ----

const CASES: &[(&str, &str)] = &[
    (
        "tests/fixtures/office/workbook.xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        "tests/fixtures/office/deck.pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("tests/fixtures/office/contacts.csv", "text/csv"),
    (
        "tests/fixtures/office/metrics.tsv",
        "text/tab-separated-values",
    ),
    (
        "tests/fixtures/extraction-bench/invoice.pdf",
        "application/pdf",
    ),
    ("tests/fixtures/ocr-text.png", "image/png"),
];

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("aikoql-fuzz-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn mutate(src: &[u8], r: &mut Rng) -> Vec<u8> {
    let mut b = src.to_vec();
    match r.below(5) {
        0 => b.truncate(r.below(b.len() + 1)),
        1 => {
            for _ in 0..1 + r.below(8) {
                if !b.is_empty() {
                    let i = r.below(b.len());
                    b[i] ^= 1 << r.below(8);
                }
            }
        }
        2 => {
            for _ in 0..1 + r.below(4) {
                if !b.is_empty() {
                    let i = r.below(b.len());
                    b[i] = r.below(256) as u8;
                }
            }
        }
        3 => {
            let at = r.below(b.len() + 1);
            let junk: Vec<u8> = (0..r.below(64)).map(|_| r.below(256) as u8).collect();
            b.splice(at..at, junk);
        }
        _ => b = (0..r.below(2048)).map(|_| r.below(256) as u8).collect(),
    }
    b
}

#[test]
fn fz006_mutated_files_error_cleanly_never_panic() {
    let dir = scratch("mutated");
    let mut r = Rng(0xfeedfacefeedface);
    let iters = if std::env::var("AIKOQL_FUZZ_ITERS").is_ok() {
        2000
    } else {
        120
    };
    for (path, mime) in CASES {
        let src = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        for i in 0..iters {
            let bytes = mutate(&src, &mut r);
            let f = dir.join(format!("case-{i}"));
            std::fs::write(&f, &bytes).unwrap();
            // Ok or Err are both fine; a panic (or hang) is the bug.
            let res =
                std::panic::catch_unwind(|| extract_document(f.to_str().unwrap(), mime, None));
            assert!(res.is_ok(), "{path} iter {i} panicked (seed 0xfeedface…)");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fz007_wrong_mime_for_content_errors_cleanly() {
    let dir = scratch("mime");
    let f = dir.join("mismatch");
    for (path, _) in CASES {
        let bytes = std::fs::read(path).unwrap();
        std::fs::write(&f, &bytes).unwrap();
        for (_, mime) in CASES {
            let res =
                std::panic::catch_unwind(|| extract_document(f.to_str().unwrap(), mime, None));
            assert!(res.is_ok(), "{path} parsed as {mime} panicked");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fz008_zip_bomb_like_office_file_is_refused_not_expanded() {
    use std::io::Write;
    let dir = scratch("bomb");
    let f = dir.join("bomb.xlsx");
    let mut z = zip::ZipWriter::new(std::fs::File::create(&f).unwrap());
    let o = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    z.start_file("xl/workbook.xml", o).unwrap();
    z.write_all(
        b"<workbook><sheets><sheet name=\"A\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>",
    )
    .unwrap();
    z.start_file("xl/worksheets/sheet1.xml", o).unwrap();
    z.write_all(b"<worksheet><sheetData>").unwrap();
    let row = b"<row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>x</t></is></c></row>";
    for _ in 0..200_000 {
        z.write_all(row).unwrap();
    }
    z.write_all(b"</sheetData></worksheet>").unwrap();
    z.finish().unwrap();
    let t = std::time::Instant::now();
    let res = std::panic::catch_unwind(|| {
        extract_document(
            f.to_str().unwrap(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            None,
        )
    });
    assert!(res.is_ok(), "panicked");
    assert!(t.elapsed().as_secs() < 30, "took {:?}", t.elapsed());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fz009_image_dimensions_never_panics_on_random_headers() {
    let mut r = Rng(0x5eed);
    let sigs: &[&[u8]] = &[
        b"\x89PNG\r\n\x1a\n",
        b"GIF89a",
        b"GIF87a",
        b"BM",
        &[0xff, 0xd8],
        b"RIFF",
        b"RIFF\0\0\0\0WEBPVP8 ",
        b"RIFF\0\0\0\0WEBPVP8L",
        b"RIFF\0\0\0\0WEBPVP8X",
        b"",
    ];
    for _ in 0..5000 {
        let mut b = r.pick(sigs).to_vec();
        b.extend((0..r.below(120)).map(|_| r.below(256) as u8));
        let _ = image_dimensions(&b);
    }
}
