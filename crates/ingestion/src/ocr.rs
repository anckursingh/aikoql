//! D2: OCR via Tesseract CLI subprocess.
//!
//! Zero new crate deps — `std::process::Command` for tesseract + pdftoppm.
//! Both must be on PATH (documented prerequisites, like rustup).
//!
//! # Architecture
//! - `OcrProvider` trait — abstraction for OCR backends.
//! - `TesseractCli` — default implementation via CLI subprocess.
//! - TSV confidence parsing — per-word confidence → per-page average.
//! - Granular status: `ocr_complete` (all OCR'd OK), `ocr_partial` (some failures).

use crate::{DocumentModel, PageModel};
use std::process::Command;

/// Minimum characters on a page before we consider native text "sufficient."
const OCR_THRESHOLD: usize = 10;

// ---------------------------------------------------------------------------
// OcrProvider trait
// ---------------------------------------------------------------------------

/// A single recognized word with position and confidence.
#[derive(Clone, Debug)]
pub struct OcrWord {
    pub text: String,
    pub confidence: f32,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub block_num: u32,
    pub line_num: u32,
}

/// Bounding box for a block of text, aggregated from word-level bboxes.
#[derive(Clone, Debug, Default)]
pub struct BlockBbox {
    pub page: u32,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    /// Average word height in this block (proxy for font size).
    pub avg_word_height: f32,
    /// Average word confidence in this block.
    pub avg_confidence: f32,
}

/// Result of OCR on a single page image.
#[derive(Clone, Debug)]
pub struct OcrPageResult {
    /// Recognized text.
    pub text: String,
    /// Average word-level confidence (0.0–100.0), or -1.0 if unavailable.
    pub confidence: f32,
    /// Per-word confidences from TSV parsing.
    pub word_confidences: Vec<f32>,
    /// Number of words recognized.
    pub word_count: usize,
    /// Per-word details with bounding boxes.
    pub words: Vec<OcrWord>,
    /// Block-level bounding boxes aggregated from word data, keyed by block_num.
    pub block_bboxes: Vec<BlockBbox>,
}

/// Statistics for OCR processing across a document.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct OcrStats {
    /// How many pages were sent to OCR.
    pub pages_ocr_attempted: u32,
    /// How many pages OCR succeeded on.
    pub pages_ocr_succeeded: u32,
    /// How many pages OCR failed on.
    pub pages_ocr_failed: u32,
    /// Average confidence across all OCR'd pages (0.0–100.0).
    pub average_confidence: f32,
}

impl OcrStats {
    /// Derive the document-level status from OCR statistics.
    pub fn status(&self) -> &'static str {
        if self.pages_ocr_attempted == 0 {
            return "extracted";
        }
        if self.pages_ocr_failed > 0 {
            return "ocr_partial";
        }
        "ocr_complete"
    }
}

/// Abstraction for OCR backends.
pub trait OcrProvider: Send + Sync {
    /// Human-readable name (e.g. "tesseract-cli").
    fn name(&self) -> &str;

    /// Check whether this provider is available (tool on PATH, etc.).
    fn available(&self) -> bool;

    /// OCR a single page image. Returns recognized text with confidence.
    fn recognize(
        &self,
        image_path: &str,
        language: &str,
        work_dir: &str,
    ) -> Result<OcrPageResult, String>;
}

// ---------------------------------------------------------------------------
// Tesseract CLI implementation
// ---------------------------------------------------------------------------

/// Tesseract OCR via CLI subprocess.
///
/// If `tesseract_path` is `None`, looks for `tesseract` on PATH.
/// Also needs `pdftoppm` on PATH for PDF rasterization (separate concern).
pub struct TesseractCli {
    pub tesseract_path: Option<String>,
    pub pdftoppm_path: Option<String>,
}

impl TesseractCli {
    pub fn new() -> Self {
        TesseractCli {
            tesseract_path: None,
            pdftoppm_path: None,
        }
    }

    fn tesseract_cmd(&self) -> Command {
        match &self.tesseract_path {
            Some(p) => Command::new(p),
            None => Command::new("tesseract"),
        }
    }

    fn pdftoppm_cmd(&self) -> Command {
        match &self.pdftoppm_path {
            Some(p) => Command::new(p),
            None => Command::new("pdftoppm"),
        }
    }
}

impl OcrProvider for TesseractCli {
    fn name(&self) -> &str {
        "tesseract-cli"
    }

    fn available(&self) -> bool {
        tool_available_cmd(&mut self.tesseract_cmd())
            && tool_available_cmd(&mut self.pdftoppm_cmd())
    }

    fn recognize(
        &self,
        image_path: &str,
        language: &str,
        work_dir: &str,
    ) -> Result<OcrPageResult, String> {
        let output_base = format!("{}/ocr_out", work_dir);
        let txt_path = format!("{}.txt", output_base);
        let tsv_path = format!("{}.tsv", output_base);

        // Run tesseract: produce both plain text and TSV.
        let status = self
            .tesseract_cmd()
            .arg(image_path)
            .arg(&output_base)
            .arg("-l")
            .arg(language)
            .arg("txt")
            .arg("tsv")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .map_err(|e| format!("tesseract not found: {}", e))?;

        if !status.success() {
            return Err("tesseract exited with error".into());
        }

        let text =
            std::fs::read_to_string(&txt_path).map_err(|e| format!("read ocr output: {}", e))?;

        // Parse TSV for confidence scores and bounding boxes.
        let (word_confidences, avg_confidence, words, block_bboxes) =
            match std::fs::read_to_string(&tsv_path) {
                Ok(tsv) => parse_tesseract_tsv(&tsv),
                Err(_) => (vec![], -1.0, vec![], vec![]),
            };

        // Clean up.
        std::fs::remove_file(&txt_path).ok();
        std::fs::remove_file(&tsv_path).ok();

        let word_count = word_confidences.len();
        Ok(OcrPageResult {
            text: text.trim().to_string(),
            confidence: avg_confidence,
            word_confidences,
            words,
            block_bboxes,
            word_count,
        })
    }
}

// ---------------------------------------------------------------------------
// TSV confidence parsing
// ---------------------------------------------------------------------------

/// Parse Tesseract TSV output, extracting per-word confidence scores.
///
/// TSV format: level, page_num, block_num, par_num, line_num, word_num,
/// left, top, width, height, conf, text
///
/// Word-level rows have level=5. Returns (confidences, average).
fn parse_tesseract_tsv(tsv: &str) -> (Vec<f32>, f32, Vec<OcrWord>, Vec<BlockBbox>) {
    let mut confidences: Vec<f32> = Vec::new();
    let mut words: Vec<OcrWord> = Vec::new();

    for line in tsv.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 12 {
            continue;
        }
        // level=5 means word-level
        if fields[0] != "5" {
            continue;
        }
        let conf = fields[10].parse::<f32>().unwrap_or(-1.0);
        if conf >= 0.0 {
            confidences.push(conf);
        }
        let left = fields[6].parse::<i32>().unwrap_or(0);
        let top = fields[7].parse::<i32>().unwrap_or(0);
        let width = fields[8].parse::<i32>().unwrap_or(0);
        let height = fields[9].parse::<i32>().unwrap_or(0);
        let block_num = fields[2].parse::<u32>().unwrap_or(0);
        let line_num = fields[4].parse::<u32>().unwrap_or(0);

        words.push(OcrWord {
            text: fields[11].to_string(),
            confidence: conf,
            left,
            top,
            width,
            height,
            block_num,
            line_num,
        });
    }

    let avg = if confidences.is_empty() {
        -1.0
    } else {
        let sum: f32 = confidences.iter().sum();
        (sum / confidences.len() as f32 * 10.0).round() / 10.0
    };

    // Aggregate word bboxes into block-level bboxes.
    let block_bboxes = aggregate_block_bboxes(&words);

    (confidences, avg, words, block_bboxes)
}

/// Group word-level bboxes by block_num to produce block-level bounding boxes.
fn aggregate_block_bboxes(words: &[OcrWord]) -> Vec<BlockBbox> {
    // Group words by block_num.
    let mut groups: std::collections::BTreeMap<u32, Vec<&OcrWord>> =
        std::collections::BTreeMap::new();
    for w in words {
        groups.entry(w.block_num).or_default().push(w);
    }

    groups
        .into_iter()
        .map(|(_block_num, block_words)| {
            if block_words.is_empty() {
                return BlockBbox::default();
            }
            let min_left = block_words.iter().map(|w| w.left).min().unwrap_or(0);
            let min_top = block_words.iter().map(|w| w.top).min().unwrap_or(0);
            let max_right = block_words
                .iter()
                .map(|w| w.left + w.width)
                .max()
                .unwrap_or(0);
            let max_bottom = block_words
                .iter()
                .map(|w| w.top + w.height)
                .max()
                .unwrap_or(0);
            let avg_h: f32 =
                block_words.iter().map(|w| w.height as f32).sum::<f32>() / block_words.len() as f32;
            let avg_c: f32 = block_words
                .iter()
                .map(|w| w.confidence)
                .filter(|c| *c >= 0.0)
                .sum::<f32>()
                / block_words
                    .iter()
                    .filter(|w| w.confidence >= 0.0)
                    .count()
                    .max(1) as f32;

            BlockBbox {
                page: 0, // filled in by caller
                left: min_left,
                top: min_top,
                width: max_right - min_left,
                height: max_bottom - min_top,
                avg_word_height: (avg_h * 10.0).round() / 10.0,
                avg_confidence: (avg_c * 10.0).round() / 10.0,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Public API (backward-compatible)
// ---------------------------------------------------------------------------

/// Check whether a page's native text is too sparse — needs OCR.
pub fn page_needs_ocr(text: &str, threshold: usize) -> bool {
    text.trim().len() < threshold
}

/// Check if a CLI tool is available on PATH (generic, uses Command::new).
pub fn tool_available(name: &str) -> bool {
    tool_available_cmd(&mut Command::new(name))
}

fn tool_available_cmd(cmd: &mut Command) -> bool {
    cmd.arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Run Tesseract OCR on a single page image (simple, no confidence).
///
/// For confidence-aware OCR, use `OcrProvider::recognize()` instead.
#[allow(dead_code)]
pub fn ocr_page_image(image_path: &str, language: &str, work_dir: &str) -> Result<String, String> {
    let provider = TesseractCli::new();
    provider
        .recognize(image_path, language, work_dir)
        .map(|r| r.text)
}

/// Rasterize a single PDF page to PNG using pdftoppm.
/// Returns path to the generated PNG.
#[allow(dead_code)]
pub fn rasterize_pdf_page(
    pdf_path: &str,
    page_num: u32,
    output_dir: &str,
) -> Result<String, String> {
    let cli = TesseractCli::new();
    rasterize_with(&cli, pdf_path, page_num, output_dir)
}

fn rasterize_with(
    cli: &TesseractCli,
    pdf_path: &str,
    page_num: u32,
    output_dir: &str,
) -> Result<String, String> {
    let output_prefix = format!("{}/page-{}", output_dir, page_num);
    let expected_png = format!("{}-{}.png", output_prefix, page_num);

    let status = cli
        .pdftoppm_cmd()
        .arg("-f")
        .arg(page_num.to_string())
        .arg("-l")
        .arg(page_num.to_string())
        .arg("-png")
        .arg("-r")
        .arg("300")
        .arg(pdf_path)
        .arg(output_prefix)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| format!("pdftoppm not found: {}", e))?;

    if !status.success() {
        return Err("pdftoppm exited with error".into());
    }

    Ok(expected_png)
}

// ---------------------------------------------------------------------------
// Mixed PDF pipeline (uses OcrProvider trait)
// ---------------------------------------------------------------------------

/// Process a PDF that may have scanned pages.
///
/// 1. Uses existing native text extraction (via `native_pages` from pdf_extract).
/// 2. For pages with insufficient native text, rasterizes + OCRs via the provider.
/// 3. Returns a merged page list with source tags and confidence scores.
/// 4. Returns OCR statistics for status derivation.
pub fn ocr_pdf_pages(
    pdf_path: &str,
    native_pages: &[PageModel],
    work_dir: &str,
) -> Result<(DocumentModel, OcrStats), String> {
    let provider = TesseractCli::new();
    ocr_pdf_pages_with(&provider, pdf_path, native_pages, work_dir)
}

/// Same as `ocr_pdf_pages` but with a custom OCR provider.
pub fn ocr_pdf_pages_with(
    provider: &dyn OcrProvider,
    pdf_path: &str,
    native_pages: &[PageModel],
    work_dir: &str,
) -> Result<(DocumentModel, OcrStats), String> {
    let mut stats = OcrStats::default();

    if !provider.available() {
        // OCR tools not available — return native pages as-is.
        let mut pages = native_pages.to_vec();
        for p in &mut pages {
            p.source = "native".into();
        }
        let total_chars: usize = pages.iter().map(|p| p.char_count).sum();
        return Ok((
            DocumentModel {
                page_count: pages.len() as u32,
                pages,
                total_chars,
                ocr_stats: None,
            },
            stats,
        ));
    }

    let mut merged = Vec::with_capacity(native_pages.len());

    for page in native_pages {
        if page_needs_ocr(&page.text, OCR_THRESHOLD) {
            stats.pages_ocr_attempted += 1;

            // Rasterize this page.
            let png_path = match rasterize_with(
                // ponytail: downcast to concrete type for rasterization (same CLI).
                // The OcrProvider trait doesn't include rasterization — that's a PDF concern.
                &TesseractCli::new(),
                pdf_path,
                page.page_number,
                work_dir,
            ) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!(
                        "rasterize failed for page {}: {} — keeping native text",
                        page.page_number, e
                    );
                    stats.pages_ocr_failed += 1;
                    merged.push(PageModel {
                        page_number: page.page_number,
                        text: page.text.clone(),
                        char_count: page.char_count,
                        source: "native".into(),
                        ocr_confidence: None,
                    });
                    continue;
                }
            };

            // OCR the rasterized page.
            match provider.recognize(&png_path, "eng", work_dir) {
                Ok(result) => {
                    let trimmed = result.text.trim().to_string();
                    let char_count = trimmed.len();
                    if char_count > 0 {
                        stats.pages_ocr_succeeded += 1;
                        let avg_conf = result.confidence; // already avg from TSV
                                                          // Track running average.
                        if stats.pages_ocr_succeeded == 1 {
                            stats.average_confidence = avg_conf;
                        } else if avg_conf >= 0.0 {
                            let n = stats.pages_ocr_succeeded as f32;
                            stats.average_confidence =
                                (stats.average_confidence * (n - 1.0) + avg_conf) / n;
                        }
                        merged.push(PageModel {
                            page_number: page.page_number,
                            text: trimmed,
                            char_count,
                            source: "ocr".into(),
                            ocr_confidence: Some(avg_conf),
                        });
                    } else {
                        // OCR produced nothing — keep native page.
                        merged.push(PageModel {
                            page_number: page.page_number,
                            text: page.text.clone(),
                            char_count: page.char_count,
                            source: "native".into(),
                            ocr_confidence: None,
                        });
                    }
                }
                Err(e) => {
                    eprintln!(
                        "OCR failed for page {}: {} — keeping native text",
                        page.page_number, e
                    );
                    stats.pages_ocr_failed += 1;
                    merged.push(PageModel {
                        page_number: page.page_number,
                        text: page.text.clone(),
                        char_count: page.char_count,
                        source: "native".into(),
                        ocr_confidence: None,
                    });
                }
            }

            // Clean up the PNG.
            let png = format!(
                "{}/page-{}-{}.png",
                work_dir, page.page_number, page.page_number
            );
            std::fs::remove_file(&png).ok();
        } else {
            // Native text is sufficient.
            merged.push(PageModel {
                page_number: page.page_number,
                text: page.text.clone(),
                char_count: page.char_count,
                source: "native".into(),
                ocr_confidence: None,
            });
        }
    }

    let total_chars: usize = merged.iter().map(|p| p.char_count).sum();

    eprintln!(
        "mnemosyne-ocr: {} pages total, {} OCR attempted, {} succeeded, {} failed, avg conf {:.1}",
        merged.len(),
        stats.pages_ocr_attempted,
        stats.pages_ocr_succeeded,
        stats.pages_ocr_failed,
        stats.average_confidence
    );

    Ok((
        DocumentModel {
            page_count: merged.len() as u32,
            pages: merged,
            total_chars,
            ocr_stats: None, // caller sets this from returned stats
        },
        stats,
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Threshold tests --

    #[test]
    fn ocr_threshold_empty_page() {
        assert!(page_needs_ocr("", OCR_THRESHOLD));
        assert!(page_needs_ocr("   ", OCR_THRESHOLD));
    }

    #[test]
    fn ocr_threshold_short_text() {
        assert!(page_needs_ocr("Hi", OCR_THRESHOLD));
    }

    #[test]
    fn ocr_threshold_sufficient_text() {
        assert!(!page_needs_ocr(
            "This is a full paragraph with sufficient native text.",
            OCR_THRESHOLD
        ));
    }

    #[test]
    fn ocr_threshold_custom() {
        assert!(page_needs_ocr("abc", 5));
        assert!(!page_needs_ocr("abcdef", 5));
    }

    // -- Tool availability tests --

    #[test]
    fn tool_available_true_for_existing_tool() {
        let found = tool_available("cmd") || tool_available("sh");
        assert!(found, "neither cmd nor sh found — odd environment");
    }

    #[test]
    fn tool_available_false_for_nonexistent() {
        assert!(!tool_available("this-tool-does-not-exist-xyzzy"));
    }

    // -- TSV parsing tests --

    #[test]
    fn parse_tsv_extracts_word_confidences() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    5\t1\t1\t1\t1\t1\t100\t200\t50\t20\t95.5\tHello\n\
                    5\t1\t1\t1\t1\t2\t160\t200\t60\t20\t87.3\tWorld\n\
                    5\t1\t1\t1\t2\t1\t100\t230\t40\t20\t72.1\tTest\n";
        let (confs, avg, _words, _bboxes) = parse_tesseract_tsv(tsv);
        assert_eq!(confs, vec![95.5, 87.3, 72.1]);
        assert!((avg - 85.0).abs() < 0.1, "expected avg ~85.0, got {}", avg);
    }

    #[test]
    fn parse_tsv_skips_non_word_rows() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    1\t1\t0\t0\t0\t0\t0\t0\t100\t100\t-1\t\n\
                    2\t1\t1\t0\t0\t0\t10\t10\t80\t10\t-1\t\n\
                    3\t1\t1\t1\t0\t0\t10\t10\t80\t10\t-1\t\n\
                    4\t1\t1\t1\t1\t0\t10\t10\t80\t10\t-1\t\n\
                    5\t1\t1\t1\t1\t1\t10\t10\t50\t10\t99.0\tWord\n";
        let (confs, avg, _words, _bboxes) = parse_tesseract_tsv(tsv);
        assert_eq!(confs.len(), 1);
        assert_eq!(confs[0], 99.0);
        assert!((avg - 99.0).abs() < 0.1);
    }

    #[test]
    fn parse_tsv_skips_negative_confidence() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    5\t1\t1\t1\t1\t1\t10\t10\t50\t10\t-1\tNoText\n\
                    5\t1\t1\t1\t1\t2\t70\t10\t50\t10\t85.0\tGood\n";
        let (confs, avg, _words, _bboxes) = parse_tesseract_tsv(tsv);
        assert_eq!(confs, vec![85.0]);
        assert!((avg - 85.0).abs() < 0.1);
    }

    #[test]
    fn parse_tsv_empty_returns_minus_one() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
        let (confs, avg, _words, _bboxes) = parse_tesseract_tsv(tsv);
        assert!(confs.is_empty());
        assert!((avg - (-1.0)).abs() < 0.01);
    }

    #[test]
    fn parse_tsv_extracts_word_bboxes() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    5\t1\t3\t1\t1\t1\t100\t200\t50\t25\t90.0\tHello\n\
                    5\t1\t3\t1\t1\t2\t160\t200\t60\t25\t85.0\tWorld\n";
        let (_confs, _avg, words, block_bboxes) = parse_tesseract_tsv(tsv);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello");
        assert_eq!(words[0].left, 100);
        assert_eq!(words[0].top, 200);
        assert_eq!(words[0].width, 50);
        assert_eq!(words[0].height, 25);
        assert_eq!(words[0].block_num, 3);
        assert_eq!(words[0].line_num, 1);

        // Block-level aggregation
        assert_eq!(block_bboxes.len(), 1);
        assert_eq!(block_bboxes[0].left, 100); // min left
        assert_eq!(block_bboxes[0].top, 200); // min top
        assert_eq!(block_bboxes[0].width, 120); // (160+60) - 100 = 120
        assert_eq!(block_bboxes[0].height, 25); // (200+25) - 200 = 25
        assert!((block_bboxes[0].avg_word_height - 25.0).abs() < 0.1);
    }

    #[test]
    fn parse_tsv_aggregates_multiple_blocks() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                    5\t1\t1\t1\t1\t1\t10\t10\t50\t20\t90.0\tBlock1\n\
                    5\t1\t1\t1\t1\t2\t70\t10\t50\t20\t90.0\tWord\n\
                    5\t1\t2\t1\t1\t1\t10\t80\t50\t30\t85.0\tBlock2\n\
                    5\t1\t2\t1\t1\t2\t70\t80\t50\t30\t85.0\tWord\n";
        let (_confs, _avg, words, block_bboxes) = parse_tesseract_tsv(tsv);
        assert_eq!(words.len(), 4);
        assert_eq!(block_bboxes.len(), 2);

        // Block 1: words at top=10, height=20
        let b1 = &block_bboxes[0];
        assert_eq!(b1.left, 10);
        assert_eq!(b1.top, 10);
        assert_eq!(b1.height, 20);
        assert!((b1.avg_word_height - 20.0).abs() < 0.1);

        // Block 2: words at top=80, height=30
        let b2 = &block_bboxes[1];
        assert_eq!(b2.top, 80);
        assert_eq!(b2.height, 30);
        assert!((b2.avg_word_height - 30.0).abs() < 0.1);
    }

    // -- OcrStats tests --

    #[test]
    fn ocr_stats_status_no_ocr() {
        let stats = OcrStats::default();
        assert_eq!(stats.status(), "extracted");
    }

    #[test]
    fn ocr_stats_status_all_succeeded() {
        let stats = OcrStats {
            pages_ocr_attempted: 3,
            pages_ocr_succeeded: 3,
            pages_ocr_failed: 0,
            average_confidence: 92.5,
        };
        assert_eq!(stats.status(), "ocr_complete");
    }

    #[test]
    fn ocr_stats_status_partial() {
        let stats = OcrStats {
            pages_ocr_attempted: 3,
            pages_ocr_succeeded: 2,
            pages_ocr_failed: 1,
            average_confidence: 88.0,
        };
        assert_eq!(stats.status(), "ocr_partial");
    }

    // -- Legacy wrapper tests --

    #[test]
    fn legacy_wrappers_delegate_to_provider() {
        // Both wrappers should be callable without panicking on availability check.
        // They will fail if the tool is not on PATH — that's fine.
        let _ = rasterize_pdf_page("nonexistent.pdf", 1, "/tmp");
        let _ = ocr_page_image("nonexistent.png", "eng", "/tmp");
    }

    // -- OcrProvider trait tests --

    #[test]
    fn tesseract_cli_has_name() {
        let provider = TesseractCli::new();
        assert_eq!(provider.name(), "tesseract-cli");
    }

    #[test]
    fn tesseract_cli_available_checks_path() {
        let provider = TesseractCli::new();
        // available() returns true only if both tesseract and pdftoppm are on PATH.
        // We don't assert true/false — just that it doesn't panic.
        let _ = provider.available();
    }
}
