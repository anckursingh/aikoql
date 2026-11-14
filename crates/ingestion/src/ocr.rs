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
    /// OCR was needed but no OCR tool could run (e.g. tesseract not on PATH).
    /// Omitted when false, so existing payloads and goldens are unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ocr_unavailable: bool,
    /// OCR was needed but `ProcessingPolicy.enable_ocr` is off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ocr_disabled: bool,
    /// Why pages failed (the first few: "page N: <reason>"), so a failure can be diagnosed
    /// without server logs. Omitted when there were none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
}

impl OcrStats {
    /// Reasons kept per document; every failure is still counted.
    pub const MAX_FAILURES: usize = 5;

    /// Count a failed page and keep the reason (the first few only).
    pub fn note_failure(&mut self, page: u32, reason: &str) {
        self.pages_ocr_failed += 1;
        if self.failures.len() < Self::MAX_FAILURES {
            self.failures.push(format!("page {page}: {reason}"));
        }
    }

    /// Derive the document-level status from OCR statistics.
    pub fn status(&self) -> &'static str {
        if self.ocr_disabled {
            return "ocr_disabled";
        }
        if self.ocr_unavailable {
            return "ocr_unavailable";
        }
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
    /// Seconds a single tesseract run may take before it is killed.
    pub timeout_secs: u64,
}

impl Default for TesseractCli {
    fn default() -> Self {
        Self::new()
    }
}

impl TesseractCli {
    pub fn new() -> Self {
        TesseractCli {
            tesseract_path: None,
            pdftoppm_path: None,
            timeout_secs: 120,
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

        // tesseract's own words are the only clue to why it failed: keep them in a file in the
        // work dir (a file cannot fill up and stall the child the way an unread pipe can).
        let err_path = format!("{}/ocr_err.txt", work_dir);
        let err_file = std::fs::File::create(&err_path).map(std::process::Stdio::from);
        // Run tesseract: produce both plain text and TSV.
        let mut child = self
            .tesseract_cmd()
            .arg(image_path)
            .arg(&output_base)
            .arg("-l")
            .arg(language)
            .arg("txt")
            .arg("tsv")
            .stdout(std::process::Stdio::null())
            .stderr(err_file.unwrap_or_else(|_| std::process::Stdio::null()))
            .spawn()
            .map_err(|e| format!("tesseract not found: {}", e))?;
        // A hung tesseract must not hold an ingest request forever.
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(self.timeout_secs);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    child.kill().ok();
                    child.wait().ok();
                    return Err(format!("tesseract timed out after {}s", self.timeout_secs));
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
                Err(e) => return Err(format!("tesseract wait: {}", e)),
            }
        };

        if !status.success() {
            let said = std::fs::read_to_string(&err_path).unwrap_or_default();
            let said: String = said
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(300)
                .collect();
            return Err(if said.is_empty() {
                format!("tesseract exited with {status}")
            } else {
                format!("tesseract exited with {status}: {said}")
            });
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
        .into_values()
        .map(|block_words| {
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
    // stdin null: a binary that ignores --version (Windows `cmd`) would
    // otherwise sit on an inherited, never-closing stdin forever.
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
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
    // -singlefile: the image is `<prefix>.png`. Without it pdftoppm pads the page number to the
    // width of the page count ("-0001" for 1446 pages), so any PDF of 10+ pages had its image
    // looked up under a name that does not exist and could never be OCR'd.
    let expected_png = format!("{}.png", output_prefix);

    let status = cli
        .pdftoppm_cmd()
        .arg("-f")
        .arg(page_num.to_string())
        .arg("-l")
        .arg(page_num.to_string())
        .arg("-png")
        .arg("-singlefile")
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

    if std::path::Path::new(&expected_png).exists() {
        return Ok(expected_png);
    }
    // A pdftoppm without -singlefile pads the page number to the width of the page count;
    // take the one image it wrote for this page rather than guessing the padding.
    let stem = format!("page-{page_num}-");
    let found: Vec<std::path::PathBuf> = std::fs::read_dir(output_dir)
        .map_err(|e| format!("pdftoppm output dir: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&stem) && n.ends_with(".png"))
        })
        .collect();
    match found.as_slice() {
        [one] => Ok(one.to_string_lossy().into_owned()),
        [] => Err(format!("pdftoppm wrote no image for page {page_num}")),
        _ => Err(format!("pdftoppm wrote several images for page {page_num}")),
    }
}

// ---------------------------------------------------------------------------
// Mixed PDF pipeline (uses OcrProvider trait)
// ---------------------------------------------------------------------------

/// Process a PDF that may have scanned pages: pages with too little native
/// text are rasterized (pdftoppm) and OCR'd, under a `ProcessingPolicy`:
/// OCR off → pages needing OCR
/// keep their native text and the stats say `ocr_disabled`; one
/// `max_ocr_seconds` deadline covers every page (a limit error, not a
/// partial result).
pub(crate) fn ocr_pdf_pages_policy(
    pdf_path: &str,
    native_pages: &[PageModel],
    work_dir: &str,
    policy: &crate::ProcessingPolicy,
) -> Result<(DocumentModel, OcrStats), String> {
    if !policy.enable_ocr {
        let stats = OcrStats {
            ocr_disabled: native_pages
                .iter()
                .any(|p| page_needs_ocr(&p.text, OCR_THRESHOLD)),
            ..Default::default()
        };
        let pages = native_pages.to_vec();
        let total_chars = pages.iter().map(|p| p.char_count).sum();
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
    let provider = TesseractCli {
        timeout_secs: policy.max_ocr_seconds.max(1),
        ..TesseractCli::new()
    };
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(policy.max_ocr_seconds);
    ocr_pdf_pages_inner(
        &provider,
        pdf_path,
        native_pages,
        work_dir,
        Some((deadline, policy.max_ocr_seconds)),
    )
}

fn ocr_deadline_error(secs: u64) -> String {
    crate::policy::exceeded(
        "max_ocr_seconds",
        format!("OCR took longer than {secs}s for this document"),
    )
}

/// The page loop; `deadline` = (instant, budget seconds) when bounded.
fn ocr_pdf_pages_inner(
    provider: &dyn OcrProvider,
    pdf_path: &str,
    native_pages: &[PageModel],
    work_dir: &str,
    deadline: Option<(std::time::Instant, u64)>,
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
            if let Some((d, secs)) = deadline {
                if std::time::Instant::now() >= d {
                    return Err(ocr_deadline_error(secs));
                }
            }
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
                    stats.note_failure(page.page_number, &e);
                    merged.push(PageModel {
                        page_number: page.page_number,
                        text: page.text.clone(),
                        char_count: page.char_count,
                        source: "native".into(),
                        ocr_confidence: None,
                        images: page.images.clone(),
                    });
                    continue;
                }
            };

            // OCR the rasterized page.
            let recognized = provider.recognize(&png_path, "eng", work_dir);
            if let Some((d, secs)) = deadline {
                let timed_out = recognized
                    .as_ref()
                    .is_err_and(|e| e.starts_with("tesseract timed out"));
                if timed_out || std::time::Instant::now() > d {
                    std::fs::remove_file(&png_path).ok();
                    return Err(ocr_deadline_error(secs));
                }
            }
            match recognized {
                Ok(result) => {
                    let trimmed = crate::table_detect::text_with_tables(&result.words)
                        .unwrap_or_else(|| result.text.trim().to_string());
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
                            images: page.images.clone(),
                        });
                    } else {
                        // OCR produced nothing — keep native page.
                        merged.push(PageModel {
                            page_number: page.page_number,
                            text: page.text.clone(),
                            char_count: page.char_count,
                            source: "native".into(),
                            ocr_confidence: None,
                            images: page.images.clone(),
                        });
                    }
                }
                Err(e) => {
                    eprintln!(
                        "OCR failed for page {}: {} — keeping native text",
                        page.page_number, e
                    );
                    stats.note_failure(page.page_number, &e);
                    merged.push(PageModel {
                        page_number: page.page_number,
                        text: page.text.clone(),
                        char_count: page.char_count,
                        source: "native".into(),
                        ocr_confidence: None,
                        images: page.images.clone(),
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
                images: page.images.clone(),
            });
        }
    }

    let total_chars: usize = merged.iter().map(|p| p.char_count).sum();

    eprintln!(
        "aikoql-ocr: {} pages total, {} OCR attempted, {} succeeded, {} failed, avg conf {:.1}",
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

    #[cfg(unix)]
    #[test]
    fn a_failing_tesseract_says_why_in_the_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ocr-why-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("tesseract");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'Error opening data file /x/eng.traineddata' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let t = TesseractCli {
            tesseract_path: Some(fake.to_str().unwrap().into()),
            ..TesseractCli::new()
        };
        let e = t
            .recognize("img.png", "eng", dir.to_str().unwrap())
            .unwrap_err();
        assert!(e.contains("Error opening data file"), "{e}");
        assert!(e.contains("exit"), "the exit status is named too: {e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_page_image_is_found_however_pdftoppm_pads_its_name() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ocr-pad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // like the real tool: the page number is padded to the width of the page count
        // ("-0001" for a 1446-page file) unless -singlefile is given
        let fake = dir.join("pdftoppm");
        std::fs::write(
            &fake,
            "#!/bin/sh\nsingle=0; for a in \"$@\"; do [ \"$a\" = -singlefile ] && single=1; last=$a; done\n\
             if [ $single = 1 ]; then : > \"$last.png\"; else : > \"$last-0001.png\"; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cli = TesseractCli {
            pdftoppm_path: Some(fake.to_str().unwrap().into()),
            ..TesseractCli::new()
        };
        let png = rasterize_with(&cli, "doc.pdf", 1, dir.to_str().unwrap()).unwrap();
        assert!(
            std::path::Path::new(&png).exists(),
            "the image {png} must exist"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failure_reasons_are_kept_capped_and_absent_when_there_are_none() {
        let mut stats = OcrStats::default();
        assert!(
            !serde_json::to_string(&stats).unwrap().contains("failures"),
            "old payloads are unchanged"
        );
        for page in 1..=9 {
            stats.note_failure(
                page,
                "tesseract exited with exit status: 1: cannot read input file",
            );
        }
        assert_eq!(stats.pages_ocr_failed, 9, "every failure is counted");
        assert_eq!(
            stats.failures.len(),
            OcrStats::MAX_FAILURES,
            "but only a few reasons are kept"
        );
        assert!(
            stats.failures[0].starts_with("page 1: tesseract exited"),
            "{:?}",
            stats.failures
        );
        let back: OcrStats = serde_json::from_str(&serde_json::to_string(&stats).unwrap()).unwrap();
        assert_eq!(back.failures, stats.failures);
    }

    #[cfg(unix)]
    #[test]
    fn a_pdftoppm_that_ignores_singlefile_is_still_found() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ocr-nosingle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("pdftoppm");
        std::fs::write(
            &fake,
            "#!/bin/sh\nfor a in \"$@\"; do last=$a; done\n: > \"$last-0007.png\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cli = TesseractCli {
            pdftoppm_path: Some(fake.to_str().unwrap().into()),
            ..TesseractCli::new()
        };
        let png = rasterize_with(&cli, "doc.pdf", 7, dir.to_str().unwrap()).unwrap();
        assert!(
            png.ends_with("page-7-0007.png") && std::path::Path::new(&png).exists(),
            "{png}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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
            ..Default::default()
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
            ..Default::default()
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
