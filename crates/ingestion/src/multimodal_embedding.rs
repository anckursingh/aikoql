//! HLD §23: modality-aware embeddings.
//!
//! `EmbeddingProvider` stays the text contract; this module adds the
//! multimodal seam — text, image, and fused text+image inputs — so the
//! visual index (§24) and future multimodal consumers can embed image
//! bytes without changing the text path.
//!
//! # Architecture
//! - `MultimodalEmbeddingInput` — the three input shapes (§23).
//! - `MultimodalEmbeddingProvider` — `embed_text` / `embed_image` /
//!   `embed_multimodal`; the last defaults to channel dispatch so a
//!   text+image provider writes two methods and overrides the fused case
//!   only when its model truly mixes modalities.
//! - `MockMultimodalEmbeddingProvider` — deterministic zero-dependency
//!   mock: the text channel is the char-ngram embedding, the image channel
//!   hashes image bytes into the same space (content-derived, not
//!   semantic), and the fused input sums both channels (L2 re-normalized).
//!
//! A real provider is NOT part of the base build (HLD §23: "Do not require
//! this provider in the base build") — the pipeline runs on the text
//! `EmbeddingProvider` and the architecture works without any multimodal
//! model; a real provider arrives behind the §60 real-model decision.

use crate::embedding::char_ngram_embed;

/// HLD §23: the three multimodal embedding inputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MultimodalEmbeddingInput<'a> {
    Text(&'a str),
    Image(&'a [u8]),
    TextImage { text: &'a str, image: &'a [u8] },
}

/// HLD §23: pluggable modality-aware embedding provider.
pub trait MultimodalEmbeddingProvider: Send + Sync {
    /// Human-readable name (e.g. "mock-mm-char-ngram", "clip-vit-b32").
    fn name(&self) -> &str;

    /// Dimensionality of the vectors this provider produces.
    fn dimensions(&self) -> usize;

    /// Embed text.
    fn embed_text(&self, text: &str) -> Vec<f32>;

    /// Embed raw image bytes.
    fn embed_image(&self, image: &[u8]) -> Vec<f32>;

    /// Embed a multimodal input. Default: channel dispatch — `TextImage`
    /// is text-dominant because a text+image pair without a fused model
    /// degrades to its text; providers with a fused model override.
    fn embed_multimodal(&self, input: &MultimodalEmbeddingInput<'_>) -> Vec<f32> {
        match input {
            MultimodalEmbeddingInput::Text(text) => self.embed_text(text),
            MultimodalEmbeddingInput::Image(image) => self.embed_image(image),
            MultimodalEmbeddingInput::TextImage { text, .. } => self.embed_text(text),
        }
    }
}

/// Deterministic mock multimodal provider (no external dependencies).
///
/// Text channel: char-ngram (identical to `MockEmbeddingProvider`).
/// Image channel: the hex rendering of the first `IMAGE_SAMPLE` bytes runs
/// through the same char-ngram embedding — content-derived (identical bytes
/// → identical vectors), NOT semantic: a word query never matches pixels.
/// Fused input: channel sum, L2 re-normalized.
///
/// ponytail: 4 KiB image sample — enough to distinguish fixtures; a real
/// provider replaces the mock before any semantic image matching matters.
pub struct MockMultimodalEmbeddingProvider {
    dims: usize,
}

const IMAGE_SAMPLE: usize = 4096;

impl MockMultimodalEmbeddingProvider {
    /// Create a provider with the default 128-dimensional vectors.
    pub fn new() -> Self {
        MockMultimodalEmbeddingProvider { dims: 128 }
    }

    /// Create a provider with a custom vector dimensionality.
    pub fn with_dimensions(dims: usize) -> Self {
        assert!(dims > 0, "dimensions must be positive");
        MockMultimodalEmbeddingProvider { dims }
    }

    fn fused(&self, text: &str, image: &[u8]) -> Vec<f32> {
        let a = self.embed_text(text);
        let b = self.embed_image(image);
        let mut sum: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let norm: f32 = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut sum {
                *v /= norm;
            }
        }
        sum
    }
}

impl MultimodalEmbeddingProvider for MockMultimodalEmbeddingProvider {
    fn name(&self) -> &str {
        "mock-mm-char-ngram"
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn embed_text(&self, text: &str) -> Vec<f32> {
        char_ngram_embed(text, self.dims)
    }

    fn embed_image(&self, image: &[u8]) -> Vec<f32> {
        let hex: String = image[..image.len().min(IMAGE_SAMPLE)]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        char_ngram_embed(&hex, self.dims)
    }

    fn embed_multimodal(&self, input: &MultimodalEmbeddingInput<'_>) -> Vec<f32> {
        match input {
            MultimodalEmbeddingInput::Text(text) => self.embed_text(text),
            MultimodalEmbeddingInput::Image(image) => self.embed_image(image),
            MultimodalEmbeddingInput::TextImage { text, image } => self.fused(text, image),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::{cosine_similarity, EmbeddingProvider, MockEmbeddingProvider};

    #[test]
    fn mock_implements_multimodal_provider() {
        let p: &dyn MultimodalEmbeddingProvider = &MockMultimodalEmbeddingProvider::new();
        assert_eq!(p.name(), "mock-mm-char-ngram");
        assert_eq!(p.dimensions(), 128);
    }

    #[test]
    fn text_channel_matches_text_provider() {
        let mm = MockMultimodalEmbeddingProvider::new();
        let text = MockEmbeddingProvider::new();
        assert_eq!(
            mm.embed_text("Acme Corporation"),
            text.embed("Acme Corporation")
        );
    }

    #[test]
    fn default_dispatch_routes_channels() {
        // A provider that does NOT override embed_multimodal dispatches:
        // Text→text channel, Image→image channel, TextImage→text-dominant.
        struct DispatchProbe {
            text: Vec<f32>,
            image: Vec<f32>,
        }
        impl MultimodalEmbeddingProvider for DispatchProbe {
            fn name(&self) -> &str {
                "dispatch-probe"
            }
            fn dimensions(&self) -> usize {
                2
            }
            fn embed_text(&self, _text: &str) -> Vec<f32> {
                self.text.clone()
            }
            fn embed_image(&self, _image: &[u8]) -> Vec<f32> {
                self.image.clone()
            }
        }
        let p = DispatchProbe {
            text: vec![1.0, 0.0],
            image: vec![0.0, 1.0],
        };
        assert_eq!(
            p.embed_multimodal(&MultimodalEmbeddingInput::Text("hi")),
            vec![1.0, 0.0]
        );
        assert_eq!(
            p.embed_multimodal(&MultimodalEmbeddingInput::Image(&[0xAB])),
            vec![0.0, 1.0]
        );
        assert_eq!(
            p.embed_multimodal(&MultimodalEmbeddingInput::TextImage {
                text: "hi",
                image: &[0xAB]
            }),
            vec![1.0, 0.0],
            "default TextImage is text-dominant"
        );
    }

    #[test]
    fn image_channel_is_deterministic_and_sensitive() {
        let mm = MockMultimodalEmbeddingProvider::new();
        let a = mm.embed_image(&[0x00, 0x11, 0x22]);
        let b = mm.embed_image(&[0x00, 0x11, 0x22]);
        assert_eq!(a, b, "identical bytes → identical embedding");
        let c = mm.embed_image(&[0xFF, 0xEE, 0xDD]);
        assert_ne!(a, c, "different bytes → different embedding");
        assert_eq!(mm.embed_image(&[]), vec![0.0; 128], "empty image → zeros");
    }

    #[test]
    fn fused_input_combines_channels() {
        let mm = MockMultimodalEmbeddingProvider::new();
        let fused = mm.embed_multimodal(&MultimodalEmbeddingInput::TextImage {
            text: "Figure 3: Company logo",
            image: &[0xDE, 0xAD, 0xBE, 0xEF],
        });
        let text_only = mm.embed_text("Figure 3: Company logo");
        let image_only = mm.embed_image(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_ne!(fused, text_only, "fusion adds the image channel");
        assert_ne!(fused, image_only, "fusion adds the text channel");
        // Fused stays closest to its channels: higher cosine to each channel
        // than the two channels share with each other.
        assert!(
            cosine_similarity(&fused, &text_only) > cosine_similarity(&text_only, &image_only),
            "fused embedding sits between its channels"
        );
    }

    #[test]
    fn fused_input_is_normalized() {
        let mm = MockMultimodalEmbeddingProvider::new();
        let fused = mm.embed_multimodal(&MultimodalEmbeddingInput::TextImage {
            text: "logo",
            image: &[0x01, 0x02],
        });
        let norm: f32 = fused.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.001, "norm should be 1.0, got {norm}");
    }
}
