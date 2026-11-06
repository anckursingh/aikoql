//! Deterministic table detection from word boxes (HLD §9/§29) — no model.
//!
//! One detector serves both sources of word geometry:
//! - native PDFs: the text layer's word boxes from poppler's
//!   `pdftotext -tsv` (a soft dependency, like tesseract: absent → no table
//!   detection, the flat text is kept);
//! - scans: Tesseract's TSV word boxes (`OcrPageResult.words`).
//!
//! Method: words → geometric lines → cell segments (a horizontal gap wider
//! than the line's median word height splits cells) → runs of ≥ 3
//! consecutive multi-segment lines → column bands from the x-projection of
//! the segments → a grid. Guards reject prose: cells must be short (median
//! ≤ 4 words) and the grid mostly filled.
//!
//! A detected table is written back into the page text as pipe rows (the
//! DOCX/HTML dialect `classify_blocks` turns into a Table node with a
//! `TablePayload`), at the position of its first word; every other word
//! keeps the tool's reading order. Known misses: skewed scans (rows drift
//! across lines), tables whose header cell spans columns (bands merge),
//! rows with a single filled cell (they end the table), long-text cells.

use crate::ocr::OcrWord;
use std::collections::HashSet;

/// A detected table: header row first, empty strings for empty cells.
#[derive(Clone, Debug, PartialEq)]
pub struct DetectedTable {
    pub rows: Vec<Vec<String>>,
    /// Indices into the input word slice that the table consumed.
    words: Vec<usize>,
}

/// Mean word confidence below which a detected OCR grid is discarded.
const MIN_MEAN_CONFIDENCE: f32 = 80.0;

fn has_alnum(s: &str) -> bool {
    s.chars().any(char::is_alphanumeric)
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Geometric lines: word indices sorted left to right.
fn geometric_lines(words: &[OcrWord], keep: &[usize]) -> Vec<Vec<usize>> {
    let mut idx: Vec<usize> = keep.to_vec();
    idx.sort_by_key(|&i| (words[i].top, words[i].left));
    let mut lines: Vec<(f32, f32, Vec<usize>)> = Vec::new(); // (top, bottom, words)
    for i in idx {
        let w = &words[i];
        let center = w.top as f32 + w.height as f32 / 2.0;
        match lines.last_mut() {
            Some((top, bottom, ws)) if center >= *top && center <= *bottom => {
                ws.push(i);
                *top = top.min(w.top as f32);
                *bottom = bottom.max((w.top + w.height) as f32);
            }
            _ => lines.push((w.top as f32, (w.top + w.height) as f32, vec![i])),
        }
    }
    lines
        .into_iter()
        .map(|(_, _, mut ws)| {
            ws.sort_by_key(|&i| words[i].left);
            ws
        })
        .collect()
}

/// Split a line into cell segments at gaps wider than the median height.
fn segments(words: &[OcrWord], line: &[usize]) -> Vec<Vec<usize>> {
    let h = median(line.iter().map(|&i| words[i].height as f32).collect());
    let mut out: Vec<Vec<usize>> = Vec::new();
    for &i in line {
        match out.last_mut() {
            Some(seg)
                if (words[i].left
                    - (words[*seg.last().unwrap()].left + words[*seg.last().unwrap()].width))
                    as f32
                    <= h =>
            {
                seg.push(i)
            }
            _ => out.push(vec![i]),
        }
    }
    out
}

fn is_numeric_cell(s: &str) -> bool {
    let t: String = s
        .chars()
        .filter(|c| !matches!(c, ',' | '.' | '%' | '$' | '€' | '£' | '-' | '+'))
        .collect();
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
}

/// Tables in a page's words. Words without any alphanumeric character
/// (ruling-line debris in OCR output) are ignored.
pub fn detect_tables(words: &[OcrWord]) -> Vec<DetectedTable> {
    let keep: Vec<usize> = (0..words.len())
        .filter(|&i| has_alnum(&words[i].text) && words[i].height > 0)
        .collect();
    let lines = geometric_lines(words, &keep);
    let segs: Vec<Vec<Vec<usize>>> = lines.iter().map(|l| segments(words, l)).collect();
    let line_h: Vec<f32> = lines
        .iter()
        .map(|l| median(l.iter().map(|&i| words[i].height as f32).collect()))
        .collect();
    let top = |l: usize| lines[l].iter().map(|&i| words[i].top).min().unwrap_or(0) as f32;
    let bottom = |l: usize| {
        lines[l]
            .iter()
            .map(|&i| words[i].top + words[i].height)
            .max()
            .unwrap_or(0) as f32
    };

    let mut tables = Vec::new();
    let mut l = 0;
    while l < lines.len() {
        if segs[l].len() < 2 {
            l += 1;
            continue;
        }
        // Run of consecutive multi-segment lines, vertically close.
        let mut end = l + 1;
        while end < lines.len()
            && segs[end].len() >= 2
            && top(end) - bottom(end - 1) <= 3.0 * line_h[end - 1].max(1.0)
        {
            end += 1;
        }
        if let Some(t) = grid_from(words, &segs[l..end]) {
            tables.push(t);
        }
        l = end;
    }
    tables
}

fn grid_from(words: &[OcrWord], rows: &[Vec<Vec<usize>>]) -> Option<DetectedTable> {
    if rows.len() < 3 {
        return None;
    }
    let span = |seg: &[usize]| {
        let l = seg.iter().map(|&i| words[i].left).min().unwrap_or(0);
        let r = seg
            .iter()
            .map(|&i| words[i].left + words[i].width)
            .max()
            .unwrap_or(0);
        (l, r)
    };
    // Column bands: merged x-projections of every segment.
    let mut spans: Vec<(i32, i32)> = rows.iter().flatten().map(|s| span(s)).collect();
    spans.sort();
    let mut bands: Vec<(i32, i32)> = Vec::new();
    for (l, r) in spans {
        match bands.last_mut() {
            Some((_, br)) if l <= *br => *br = (*br).max(r),
            _ => bands.push((l, r)),
        }
    }
    if bands.len() < 2 {
        return None;
    }
    let mut grid = vec![vec![String::new(); bands.len()]; rows.len()];
    let mut consumed = Vec::new();
    let mut words_per_cell = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for seg in row {
            let (l, rt) = span(seg);
            let center = (l + rt) / 2;
            let c = bands
                .iter()
                .position(|(bl, br)| center >= *bl && center <= *br)?;
            let text = seg
                .iter()
                .map(|&i| words[i].text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !grid[r][c].is_empty() {
                grid[r][c].push(' ');
            }
            grid[r][c].push_str(&text);
            words_per_cell.push(seg.len() as f32);
            consumed.extend_from_slice(seg);
        }
    }
    let filled = grid.iter().flatten().filter(|c| !c.is_empty()).count();
    if (filled as f32) < 0.6 * (grid.len() * bands.len()) as f32 || median(words_per_cell) > 4.0 {
        return None;
    }
    // A garbled OCR grid (skew, noise) is worse than the flat text: refuse
    // when the words read poorly (Tesseract confidence 0–100).
    let conf: Vec<f32> = consumed
        .iter()
        .map(|&i| words[i].confidence)
        .filter(|c| *c >= 0.0)
        .collect();
    if !conf.is_empty() && conf.iter().sum::<f32>() / (conf.len() as f32) < MIN_MEAN_CONFIDENCE {
        return None;
    }
    Some(DetectedTable {
        rows: with_header(grid),
        words: consumed,
    })
}

/// Header detection: a first row holding a number is data, not a header —
/// it gets a synthetic "Column N" header row.
pub(crate) fn with_header(mut grid: Vec<Vec<String>>) -> Vec<Vec<String>> {
    if grid
        .first()
        .is_some_and(|r| r.iter().any(|c| is_numeric_cell(c)))
    {
        let n = grid.iter().map(Vec::len).max().unwrap_or(0);
        grid.insert(0, (1..=n).map(|i| format!("Column {i}")).collect());
    }
    grid
}

/// Rows → the pipe dialect (header detection applied), ragged rows padded.
pub(crate) fn pipe_table(rows: Vec<Vec<String>>) -> String {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    with_header(rows)
        .into_iter()
        .map(|mut r| {
            r.resize(n, String::new());
            pipe_row(&r)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pipe_row(cells: &[String]) -> String {
    // shortcut: a literal '|' in a cell becomes '/', the dialect has no
    // escape; give TablePayload a direct path if exact cell text matters.
    let cells: Vec<String> = cells
        .iter()
        .map(|c| {
            c.replace('|', "/")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    format!("| {} |", cells.join(" | "))
}

/// Page text rebuilt from `words` with each detected table as pipe rows at
/// its first word's position; `None` when the page has no table (callers
/// keep their original text). Lines follow the tool's word order; a change
/// of block starts a paragraph.
pub fn text_with_tables(words: &[OcrWord]) -> Option<String> {
    let tables = detect_tables(words);
    if tables.is_empty() {
        return None;
    }
    let owner: std::collections::HashMap<usize, usize> = tables
        .iter()
        .enumerate()
        .flat_map(|(t, d)| d.words.iter().map(move |&w| (w, t)))
        .collect();
    let mut emitted: HashSet<usize> = HashSet::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut line: Vec<&str> = Vec::new();
    let mut prev: Option<&OcrWord> = None;
    let flush_line = |line: &mut Vec<&str>, para: &mut Vec<String>| {
        if !line.is_empty() {
            para.push(line.join(" "));
            line.clear();
        }
    };
    let flush_para = |para: &mut Vec<String>, blocks: &mut Vec<String>| {
        if !para.is_empty() {
            blocks.push(para.join("\n"));
            para.clear();
        }
    };
    for (i, w) in words.iter().enumerate() {
        if let Some(&t) = owner.get(&i) {
            if emitted.insert(t) {
                flush_line(&mut line, &mut para);
                flush_para(&mut para, &mut blocks);
                blocks.push(
                    tables[t]
                        .rows
                        .iter()
                        .map(|r| pipe_row(r))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            prev = None;
            continue;
        }
        if let Some(p) = prev {
            let new_block = p.block_num != w.block_num;
            let new_line = new_block
                || p.line_num != w.line_num
                || (p.top - w.top).abs() > p.height.max(w.height) / 2;
            if new_line {
                flush_line(&mut line, &mut para);
            }
            if new_block {
                flush_para(&mut para, &mut blocks);
            }
        } else {
            flush_line(&mut line, &mut para);
            flush_para(&mut para, &mut blocks);
        }
        if w.text.trim().is_empty() {
            continue; // tesseract emits blank "words" for empty regions
        }
        line.push(&w.text);
        prev = Some(w);
    }
    flush_line(&mut line, &mut para);
    flush_para(&mut para, &mut blocks);
    Some(blocks.join("\n\n"))
}

/// Poppler `pdftotext -tsv` → words per page (index = page − 1). Columns:
/// level, page_num, par_num, block_num, line_num, word_num, left, top,
/// width, height, conf, text — floats in PDF points. Each (block, par,
/// line) gets a unique line number so line identity survives.
pub(crate) fn parse_pdftotext_tsv(tsv: &str) -> Vec<Vec<OcrWord>> {
    let mut pages: Vec<Vec<OcrWord>> = Vec::new();
    let mut line_ids: std::collections::HashMap<(usize, u32, u32, u32), u32> =
        std::collections::HashMap::new();
    for row in tsv.lines().skip(1) {
        let f: Vec<&str> = row.split('\t').collect();
        if f.len() < 12 || f[0] != "5" {
            continue;
        }
        let num = |s: &str| s.parse::<f64>().unwrap_or(0.0).round() as i32;
        let Ok(page) = f[1].parse::<usize>() else {
            continue;
        };
        if page == 0 {
            continue;
        }
        let (par, block, ln) = (
            f[2].parse().unwrap_or(0),
            f[3].parse().unwrap_or(0),
            f[4].parse().unwrap_or(0),
        );
        let next = line_ids.len() as u32;
        let line_num = *line_ids.entry((page, block, par, ln)).or_insert(next);
        if pages.len() < page {
            pages.resize_with(page, Vec::new);
        }
        pages[page - 1].push(OcrWord {
            text: f[11..].join("\t"),
            confidence: 100.0,
            left: num(f[6]),
            top: num(f[7]),
            width: num(f[8]),
            height: num(f[9]),
            block_num: block,
            line_num,
        });
    }
    pages
}

/// Word boxes of every page of a PDF via `pdftotext -tsv`; `None` when
/// poppler is missing, fails or exceeds `timeout_secs`.
pub(crate) fn pdf_page_words(path: &str, timeout_secs: u64) -> Option<Vec<Vec<OcrWord>>> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let out = std::env::temp_dir().join(format!(
        "aikoql-pdftsv-{}-{}.tsv",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut child = std::process::Command::new("pdftotext")
        .args(["-tsv", "-enc", "UTF-8"])
        .arg(path)
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                std::fs::remove_file(&out).ok();
                eprintln!("pdftotext timed out after {timeout_secs}s — no table detection");
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => return None,
        }
    };
    let tsv = std::fs::read_to_string(&out).ok();
    std::fs::remove_file(&out).ok();
    if !status.success() {
        return None;
    }
    Some(parse_pdftotext_tsv(&tsv?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, left: i32, top: i32, line: u32) -> OcrWord {
        OcrWord {
            text: text.into(),
            confidence: 90.0,
            left,
            top,
            width: 12 * text.len() as i32,
            height: 20,
            block_num: 1,
            line_num: line,
        }
    }

    #[test]
    fn right_aligned_numbers_share_a_band_and_numeric_first_row_gets_a_header() {
        let words = vec![
            w("Widget", 0, 0, 0),
            w("1,200", 300, 0, 0),
            w("Gadget", 0, 40, 1),
            w("80", 336, 40, 1),
            w("Sprocket", 0, 80, 2),
            w("15,000", 288, 80, 2),
        ];
        let t = detect_tables(&words);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].rows[0], vec!["Column 1", "Column 2"]);
        assert_eq!(t[0].rows[2], vec!["Gadget", "80"]);
    }

    #[test]
    fn ruling_debris_is_ignored_and_text_keeps_other_lines() {
        let words = vec![
            w("Title", 0, 0, 0),
            w("|", 100, 0, 0),
            w("A", 0, 100, 1),
            w("B", 300, 100, 1),
            w("1", 0, 140, 2),
            w("2", 300, 140, 2),
            w("3", 0, 180, 3),
            w("4", 300, 180, 3),
            w("Footer", 0, 300, 4),
        ];
        let text = text_with_tables(&words).unwrap();
        assert_eq!(text, "Title |\n\n| A | B |\n| 1 | 2 |\n| 3 | 4 |\n\nFooter");
    }

    #[test]
    fn poppler_tsv_parses_floats_and_pages() {
        let tsv = "level\tpage_num\tpar_num\tblock_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   1\t1\t0\t0\t0\t0\t0\t0\t612\t792\t-1\t###PAGE###\n\
                   5\t1\t0\t0\t0\t0\t72.00\t43.38\t53.35\t11.10\t100\tEmployee\n\
                   5\t2\t0\t0\t0\t0\t72.49\t43.38\t3.12\t11.10\t100\tAge\n";
        let p = parse_pdftotext_tsv(tsv);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0][0].left, p[0][0].top, p[0][0].height), (72, 43, 11));
        assert_eq!(p[1][0].text, "Age");
        assert_ne!(p[0][0].line_num, p[1][0].line_num);
    }
}
