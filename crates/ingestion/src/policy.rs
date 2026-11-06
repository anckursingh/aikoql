//! HLD §55 `ProcessingPolicy` with the HLD §54 asset limits.
//!
//! Enforced in the default path: `enable_ocr`, `enable_visual_analysis`,
//! `max_pages`, `max_image_count` (raster images), `max_visual_assets`
//! (raster + vector assets), `max_image_dimension` (read from headers, never
//! by decoding), `max_ocr_seconds` (one deadline per document). A limit hit
//! is an error starting with [`LIMIT_EXCEEDED`] naming the limit — never a
//! panic and never a silent truncation.
//!
//! Carried but not enforced here: `enable_chart_analysis`,
//! `enable_diagram_analysis` (the default analyzers are cheap rules),
//! `enable_transformer_boundaries` and `max_model_calls` (no model runs in
//! the default build; the optional `vlm`/`transform` features own them).

/// Prefix of every limit error, for callers that classify errors.
pub const LIMIT_EXCEEDED: &str = "limit exceeded:";

/// True for errors produced by a `ProcessingPolicy` limit.
pub fn is_limit_error(e: &str) -> bool {
    e.contains(LIMIT_EXCEEDED)
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
// deny_unknown_fields: a server config typo must fail, not be ignored.
#[serde(default, deny_unknown_fields)]
pub struct ProcessingPolicy {
    pub enable_ocr: bool,
    pub enable_visual_analysis: bool,
    pub enable_chart_analysis: bool,
    pub enable_diagram_analysis: bool,
    pub enable_transformer_boundaries: bool,
    /// Raster + vector visual assets per document.
    pub max_visual_assets: usize,
    /// Budget for the optional VLM path; no model call happens by default.
    pub max_model_calls: usize,
    /// Pages (PDF pages, sheets, slides) per document.
    pub max_pages: usize,
    /// Raster images per document.
    pub max_image_count: usize,
    /// Longest side, in pixels, of any raster image.
    pub max_image_dimension: u32,
    /// Wall-clock OCR budget per document, in seconds.
    pub max_ocr_seconds: u64,
}

impl Default for ProcessingPolicy {
    fn default() -> Self {
        ProcessingPolicy {
            enable_ocr: true,
            enable_visual_analysis: true,
            enable_chart_analysis: true,
            enable_diagram_analysis: true,
            enable_transformer_boundaries: false,
            max_visual_assets: 2_000,
            max_model_calls: 100,
            max_pages: 2_000,
            max_image_count: 1_000,
            max_image_dimension: 20_000,
            max_ocr_seconds: 600,
        }
    }
}

pub(crate) fn exceeded(limit: &str, detail: String) -> String {
    format!("{LIMIT_EXCEEDED} {limit} ({detail})")
}

impl ProcessingPolicy {
    /// Zero for these would refuse every document (or every OCR page).
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [
            ("max_pages", self.max_pages as u64),
            ("max_image_dimension", self.max_image_dimension as u64),
            ("max_ocr_seconds", self.max_ocr_seconds),
        ] {
            if v == 0 {
                return Err(format!("{name} must be at least 1"));
            }
        }
        Ok(())
    }

    pub(crate) fn check_pages(&self, n: usize) -> Result<(), String> {
        if n > self.max_pages {
            return Err(exceeded(
                "max_pages",
                format!("{n} pages > {}", self.max_pages),
            ));
        }
        Ok(())
    }

    pub(crate) fn check_dimensions(&self, w: u32, h: u32, what: &str) -> Result<(), String> {
        if w.max(h) > self.max_image_dimension {
            return Err(exceeded(
                "max_image_dimension",
                format!("{what} is {w}x{h} px > {} px", self.max_image_dimension),
            ));
        }
        Ok(())
    }

    /// Document-level limits every extractor's output must meet.
    pub(crate) fn check_document(&self, doc: &crate::DocumentModel) -> Result<(), String> {
        self.check_pages(doc.pages.len())?;
        let images = doc.pages.iter().flat_map(|p| &p.images);
        let all = images.clone().count();
        let raster = images
            .filter(|i| {
                i.asset.mime_type.starts_with("image/") && i.asset.mime_type != "image/svg+xml"
            })
            .count();
        if raster > self.max_image_count {
            return Err(exceeded(
                "max_image_count",
                format!("{raster} images > {}", self.max_image_count),
            ));
        }
        if all > self.max_visual_assets {
            return Err(exceeded(
                "max_visual_assets",
                format!("{all} visual assets > {}", self.max_visual_assets),
            ));
        }
        Ok(())
    }
}

/// Raster image size from the file header — no decoding. PNG, GIF, BMP,
/// JPEG (SOF marker scan) and WebP (VP8/VP8L/VP8X). `None` = unknown.
// shortcut: TIFF (IFD walk) is not read, so a TIFF passes the dimension
// limit unchecked; add the IFD parse if TIFF uploads matter.
pub fn image_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let le32 = |i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?));
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"BM") {
        return Some((le32(18)?, (le32(22)? as i32).unsigned_abs()));
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
                i += 2;
                continue;
            }
            i += 2 + be16(i + 2)? as usize;
        }
        return None;
    }
    if b.len() >= 30 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return match &b[12..16] {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let v = le32(21)?;
                Some(((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => {
                let w = le32(24)? & 0xff_ffff;
                let h = le32(27)? & 0xff_ffff;
                Some((w + 1, h + 1))
            }
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_dimensions() {
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0x05, 0xdc, 0, 0, 0x02, 0x58,
        ];
        assert_eq!(image_dimensions(&png), Some((1500, 600)));
        let gif = *b"GIF89a\x40\x01\xf0\x00";
        assert_eq!(image_dimensions(&gif), Some((320, 240)));
        // JPEG: SOI, APP0 (len 4), SOF0 with height 480, width 640.
        let jpg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x01,
            0xe0, 0x02, 0x80, 0x03,
        ];
        assert_eq!(image_dimensions(&jpg), Some((640, 480)));
        assert_eq!(image_dimensions(b"not an image"), None);
    }

    #[test]
    fn limit_errors_are_classified() {
        let p = ProcessingPolicy {
            max_pages: 1,
            ..Default::default()
        };
        let e = p.check_pages(2).unwrap_err();
        assert!(is_limit_error(&e) && e.contains("max_pages"), "{e}");
        assert!(p.check_pages(1).is_ok());
    }
}
