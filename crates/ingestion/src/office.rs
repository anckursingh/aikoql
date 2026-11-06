//! XLSX, PPTX and CSV/TSV extractors (HLD §27). Same approach as the DOCX
//! parser: the zip crate plus string scanning of the (simple, generated)
//! OOXML parts — no XML dependency. Tables are emitted in the pipe dialect,
//! so they become Table nodes with a `TablePayload` in the canonical AST.

use crate::extractors::{DocumentExtractor, DocumentSource};
use crate::table_detect::pipe_table;
use crate::{
    attr_value, normalize_zip_path, xml_unescape, DocumentImage, DocumentModel, PageModel,
};
use std::collections::BTreeMap;
use std::io::Read;

const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PPTX_MIME: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

fn model(pages: Vec<(String, Vec<DocumentImage>)>) -> DocumentModel {
    let pages: Vec<PageModel> = pages
        .into_iter()
        .enumerate()
        .map(|(i, (text, images))| PageModel {
            page_number: i as u32 + 1,
            char_count: text.len(),
            text,
            source: "native".into(),
            ocr_confidence: None,
            images,
        })
        .collect();
    DocumentModel {
        page_count: pages.len() as u32,
        total_chars: pages.iter().map(|p| p.char_count).sum(),
        pages,
        ocr_stats: None,
    }
}

// ------------------------------------------------------------ XML helpers

/// `<tag …>inner</tag>` occurrences, non-nested, in document order:
/// (offset, open tag, inner). `tag` matches exactly (`a:t` never matches
/// `a:tbl`). Self-closing elements yield an empty inner.
fn elements<'a>(xml: &'a str, tag: &str) -> Vec<(usize, &'a str, &'a str)> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = xml[pos..].find(&open) {
        let start = pos + i;
        let after = &xml[start + open.len()..];
        if !after.starts_with(['>', ' ', '/', '\n', '\t', '\r']) {
            pos = start + open.len();
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        let open_tag = &xml[start..start + open.len() + gt + 1];
        let body_start = start + open.len() + gt + 1;
        if open_tag.ends_with("/>") {
            out.push((start, open_tag, ""));
            pos = body_start;
            continue;
        }
        let end = xml[body_start..]
            .find(&close)
            .map_or(xml.len(), |e| body_start + e);
        out.push((start, open_tag, &xml[body_start..end]));
        pos = (end + close.len()).min(xml.len());
    }
    out
}

/// Concatenated `<tag>` texts (e.g. every `a:t` run of a paragraph).
fn texts(xml: &str, tag: &str) -> String {
    elements(xml, tag)
        .into_iter()
        .map(|(_, _, t)| xml_unescape(t))
        .collect()
}

fn read_entry(archive: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut s = String::new();
    archive.by_name(name).ok()?.read_to_string(&mut s).ok()?;
    Some(s)
}

/// Relationships of an OOXML part: (Id, Type, absolute zip path). `base`
/// is the directory relative targets resolve against.
fn rels(xml: &str, base: &str) -> Vec<(String, String, String)> {
    elements(xml, "Relationship")
        .into_iter()
        .filter_map(|(_, tag, _)| {
            let target = attr_value(tag, "Target")?;
            let path = if target.starts_with('/') {
                normalize_zip_path(&target)
            } else {
                normalize_zip_path(&format!("{base}/{target}"))
            };
            Some((attr_value(tag, "Id")?, attr_value(tag, "Type")?, path))
        })
        .collect()
}

fn rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

fn dir_of(part: &str) -> &str {
    part.rsplit_once('/').map_or("", |(d, _)| d)
}

// ------------------------------------------------------------------ XLSX

pub(crate) struct XlsxExtractor;

impl DocumentExtractor for XlsxExtractor {
    fn name(&self) -> &str {
        "xlsx"
    }
    fn supports(&self, mime: &str) -> bool {
        mime == XLSX_MIME
    }
    fn extract(&self, s: &DocumentSource) -> Result<DocumentModel, String> {
        let file = std::fs::File::open(s.path).map_err(|e| format!("open xlsx: {e}"))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("xlsx zip: {e}"))?;
        let workbook =
            read_entry(&mut zip, "xl/workbook.xml").ok_or("xlsx: missing xl/workbook.xml")?;
        let wb_rels = read_entry(&mut zip, "xl/_rels/workbook.xml.rels")
            .map(|r| rels(&r, "xl"))
            .unwrap_or_default();
        let shared = read_entry(&mut zip, "xl/sharedStrings.xml")
            .map(|x| shared_strings(&x))
            .unwrap_or_default();
        let mut pages = Vec::new();
        // Workbook order, not part names: sheet10.xml may precede sheet2.xml.
        for (_, tag, _) in elements(&workbook, "sheet") {
            let name = xml_unescape(&attr_value(tag, "name").unwrap_or_default());
            let Some(rid) = attr_value(tag, "r:id") else {
                continue;
            };
            let Some((_, _, part)) = wb_rels.iter().find(|(id, _, _)| *id == rid) else {
                continue;
            };
            let Some(xml) = read_entry(&mut zip, part) else {
                continue;
            };
            let grid = sheet_grid(&xml, &shared);
            let text = if grid.is_empty() {
                format!("# {name}")
            } else {
                format!("# {name}\n\n{}", pipe_table(grid))
            };
            pages.push((text, Vec::new()));
        }
        if pages.is_empty() {
            return Err("xlsx: no readable worksheet".into());
        }
        Ok(model(pages))
    }
}

fn shared_strings(xml: &str) -> Vec<String> {
    elements(xml, "si")
        .into_iter()
        .map(|(_, _, si)| {
            // Phonetic runs (<rPh>) are reading aids, not cell text.
            let mut body = si.to_string();
            while let Some(a) = body.find("<rPh") {
                let b = body[a..].find("</rPh>").map_or(body.len(), |e| a + e + 6);
                body.replace_range(a..b, "");
            }
            texts(&body, "t")
        })
        .collect()
}

/// "BC12" → (row 12, column 55), both 1-based.
fn cell_ref(r: &str) -> Option<(usize, usize)> {
    let letters: String = r.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let row: usize = r[letters.len()..].parse().ok()?;
    let col = letters.bytes().try_fold(0usize, |acc, b| {
        Some(acc * 26 + (b.to_ascii_uppercase() - b'A' + 1) as usize)
    })?;
    (row > 0 && col > 0).then_some((row, col))
}

/// A worksheet as rows of cell text; merged ranges carry their top-left
/// value; empty rows and columns are dropped.
// shortcut: date serials print as numbers (styles.xml numFmts are not read)
// and sheets are not size-capped; add both if spreadsheets get big/dated.
fn sheet_grid(xml: &str, shared: &[String]) -> Vec<Vec<String>> {
    let mut cells: BTreeMap<(usize, usize), String> = BTreeMap::new();
    for (_, tag, body) in elements(xml, "c") {
        let Some(pos) = attr_value(tag, "r").and_then(|r| cell_ref(&r)) else {
            continue;
        };
        let raw = || elements(body, "v").first().map(|(_, _, v)| xml_unescape(v));
        let value = match attr_value(tag, "t").as_deref() {
            Some("s") => raw()
                .and_then(|i| i.trim().parse::<usize>().ok())
                .and_then(|i| shared.get(i).cloned()),
            Some("inlineStr") => Some(texts(body, "t")),
            Some("b") => raw().map(|v| if v.trim() == "1" { "TRUE" } else { "FALSE" }.into()),
            _ => raw(),
        };
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            cells.insert(pos, v.trim().to_string());
        }
    }
    for (_, tag, _) in elements(xml, "mergeCell") {
        let Some((a, b)) = attr_value(tag, "ref").and_then(|r| {
            let (a, b) = r.split_once(':')?;
            Some((cell_ref(a)?, cell_ref(b)?))
        }) else {
            continue;
        };
        if let Some(v) = cells.get(&a).cloned() {
            for r in a.0..=b.0 {
                for c in a.1..=b.1 {
                    cells.entry((r, c)).or_insert_with(|| v.clone());
                }
            }
        }
    }
    let rows: Vec<usize> = {
        let mut r: Vec<usize> = cells.keys().map(|k| k.0).collect();
        r.dedup();
        r
    };
    let mut cols: Vec<usize> = cells.keys().map(|k| k.1).collect();
    cols.sort_unstable();
    cols.dedup();
    rows.iter()
        .map(|r| {
            cols.iter()
                .map(|c| cells.get(&(*r, *c)).cloned().unwrap_or_default())
                .collect()
        })
        .collect()
}

// ------------------------------------------------------------------ PPTX

pub(crate) struct PptxExtractor;

impl DocumentExtractor for PptxExtractor {
    fn name(&self) -> &str {
        "pptx"
    }
    fn supports(&self, mime: &str) -> bool {
        mime == PPTX_MIME
    }
    fn extract(&self, s: &DocumentSource) -> Result<DocumentModel, String> {
        let file = std::fs::File::open(s.path).map_err(|e| format!("open pptx: {e}"))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("pptx zip: {e}"))?;
        let pres = read_entry(&mut zip, "ppt/presentation.xml")
            .ok_or("pptx: missing ppt/presentation.xml")?;
        let pres_rels = read_entry(&mut zip, "ppt/_rels/presentation.xml.rels")
            .map(|r| rels(&r, "ppt"))
            .unwrap_or_default();
        let mut pages = Vec::new();
        // sldIdLst order is the deck order (slide10.xml sorts before slide2.xml).
        for (_, tag, _) in elements(&pres, "p:sldId") {
            let Some(rid) = attr_value(tag, "r:id") else {
                continue;
            };
            let Some((_, _, part)) = pres_rels.iter().find(|(id, _, _)| *id == rid) else {
                continue;
            };
            let part = part.clone();
            let Some(xml) = read_entry(&mut zip, &part) else {
                continue;
            };
            let page_no = pages.len() as u32 + 1;
            let mut blocks = slide_blocks(&xml);
            let slide_rels = read_entry(&mut zip, &rels_path(&part))
                .map(|r| rels(&r, dir_of(&part)))
                .unwrap_or_default();
            let mut images = Vec::new();
            for (_, kind, target) in &slide_rels {
                if kind.ends_with("/notesSlide") {
                    if let Some(notes) = read_entry(&mut zip, target) {
                        let text = notes_text(&notes);
                        if !text.is_empty() {
                            blocks.push(format!("Notes: {text}"));
                        }
                    }
                } else if kind.ends_with("/image") {
                    let mut bytes = Vec::new();
                    if let Ok(mut e) = zip.by_name(target) {
                        if e.read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                            images.push(crate::pdf_asset_image(
                                &bytes,
                                &crate::asset_store::mime_from_extension(target),
                                page_no,
                                s.asset_dir,
                                target,
                            ));
                        }
                    }
                }
            }
            pages.push((blocks.join("\n\n"), images));
        }
        if pages.is_empty() {
            return Err("pptx: no readable slide".into());
        }
        Ok(model(pages))
    }
}

fn paragraphs(xml: &str) -> Vec<String> {
    elements(xml, "a:p")
        .into_iter()
        .map(|(_, _, p)| texts(p, "a:t").trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn placeholder_type(sp: &str) -> Option<String> {
    elements(sp, "p:ph")
        .first()
        .and_then(|(_, tag, _)| attr_value(tag, "type"))
}

/// A slide's text blocks in document order: the title as a heading, every
/// other shape as a paragraph block, tables as pipe rows.
fn slide_blocks(xml: &str) -> Vec<String> {
    let mut items: Vec<(usize, String)> = Vec::new();
    for (pos, _, sp) in elements(xml, "p:sp") {
        let lines = paragraphs(sp);
        if lines.is_empty() {
            continue;
        }
        let is_title = matches!(placeholder_type(sp).as_deref(), Some("title" | "ctrTitle"));
        let block = if is_title {
            format!("# {}", lines.join(" "))
        } else {
            lines.join("\n")
        };
        items.push((pos, block));
    }
    for (pos, _, tbl) in elements(xml, "a:tbl") {
        let grid: Vec<Vec<String>> = elements(tbl, "a:tr")
            .into_iter()
            .map(|(_, _, tr)| {
                elements(tr, "a:tc")
                    .into_iter()
                    .map(|(_, _, tc)| paragraphs(tc).join(" "))
                    .collect()
            })
            .collect();
        if !grid.is_empty() {
            items.push((pos, pipe_table(grid)));
        }
    }
    items.sort_by_key(|(p, _)| *p);
    items.into_iter().map(|(_, b)| b).collect()
}

/// Speaker notes: the body placeholder's text (not the slide number/image).
fn notes_text(xml: &str) -> String {
    elements(xml, "p:sp")
        .into_iter()
        .filter(|(_, _, sp)| placeholder_type(sp).as_deref() == Some("body"))
        .flat_map(|(_, _, sp)| paragraphs(sp))
        .collect::<Vec<_>>()
        .join(" ")
}

// --------------------------------------------------------------- CSV/TSV

pub(crate) struct CsvExtractor;

impl DocumentExtractor for CsvExtractor {
    fn name(&self) -> &str {
        "csv"
    }
    fn supports(&self, mime: &str) -> bool {
        matches!(
            mime,
            "text/csv" | "application/csv" | "text/tab-separated-values"
        )
    }
    fn extract(&self, s: &DocumentSource) -> Result<DocumentModel, String> {
        let bytes = std::fs::read(s.path).map_err(|e| format!("read csv: {e}"))?;
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim_start_matches('\u{feff}');
        let delim = sniff_delimiter(text, s.mime_type == "text/tab-separated-values");
        let rows: Vec<Vec<String>> = parse_delimited(text, delim)
            .into_iter()
            .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
            .collect();
        let body = if rows.is_empty() {
            String::new()
        } else {
            pipe_table(rows)
        };
        Ok(model(vec![(body, Vec::new())]))
    }
}

/// RFC 4180 records: quoted fields may hold the delimiter, newlines and
/// doubled quotes. Newlines inside a field become spaces.
// shortcut: whole file in memory as one page; stream + paginate if CSVs
// beyond tens of MB matter (the upload cap bounds it today).
fn parse_delimited(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                '\r' | '\n' => field.push(' '),
                _ => field.push(c),
            }
        } else if c == '"' && field.trim().is_empty() {
            field.clear();
            quoted = true;
        } else if c == delim {
            row.push(std::mem::take(&mut field).trim().to_string());
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut field).trim().to_string());
            rows.push(std::mem::take(&mut row));
        } else {
            field.push(c);
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field.trim().to_string());
        rows.push(row);
    }
    rows
}

/// The candidate delimiter whose first records agree most on a field
/// count above one. Ties keep the candidate order.
fn sniff_delimiter(text: &str, prefer_tab: bool) -> char {
    let sample: String = text.chars().take(8192).collect();
    let candidates: &[char] = if prefer_tab {
        &['\t', ',', ';', '|']
    } else {
        &[',', ';', '\t', '|']
    };
    let mut best = (candidates[0], 0.0f32);
    for &d in candidates {
        let counts: Vec<usize> = parse_delimited(&sample, d)
            .into_iter()
            .take(20)
            .map(|r| r.len())
            .collect();
        let Some(&mode) = counts
            .iter()
            .max_by_key(|n| counts.iter().filter(|m| m == n).count())
        else {
            continue;
        };
        if mode < 2 {
            continue;
        }
        let score = counts.iter().filter(|n| **n == mode).count() as f32 / counts.len() as f32;
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_strings_rich_text_and_cell_types() {
        let sst = r#"<sst><si><t>Region</t></si><si><r><t>North </t></r><r><t>America</t></r><rPh><t>x</t></rPh></si></sst>"#;
        let shared = shared_strings(sst);
        assert_eq!(shared, vec!["Region", "North America"]);
        let sheet = r#"<sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="b"><v>1</v></c></row>
            <row r="3"><c r="A3" t="s"><v>1</v></c><c r="C3"><v>12.5</v></c><c r="D3" s="1"/></row></sheetData>"#;
        assert_eq!(
            sheet_grid(sheet, &shared),
            vec![
                vec!["Region".to_string(), "TRUE".into()],
                vec!["North America".to_string(), "12.5".into()]
            ]
        );
        assert_eq!(cell_ref("AB10"), Some((10, 28)));
    }

    #[test]
    fn delimiter_sniffing() {
        assert_eq!(sniff_delimiter("a;b;c\n1;2,5;3\n", false), ';');
        assert_eq!(sniff_delimiter("a,b\n1,2\n", false), ',');
        assert_eq!(sniff_delimiter("a\tb\n1\t2\n", false), '\t');
        assert_eq!(
            parse_delimited("x,\"a, \"\"b\"\"\"\r\ny,z", ','),
            vec![vec!["x", "a, \"b\""], vec!["y", "z"]]
        );
    }

    #[test]
    fn elements_match_exact_tags() {
        let x =
            "<a:tbl><a:tr><a:tc><a:txBody><a:p><a:t>v</a:t></a:p></a:txBody></a:tc></a:tr></a:tbl>";
        assert_eq!(elements(x, "a:t").len(), 1);
        assert_eq!(texts(x, "a:t"), "v");
        assert_eq!(elements(x, "a:tc").len(), 1);
    }
}
