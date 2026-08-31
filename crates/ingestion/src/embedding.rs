//! D6-embedding: Lightweight text embedding for entity resolution.
//!
//! Provides a pluggable `EmbeddingProvider` trait and a zero-dependency mock
//! implementation using character n-gram hashing. Vectors are deterministic,
//! fast to compute, and suitable for entity-name similarity matching without
//! external API calls or model files.
//!
//! # Architecture
//! - `EmbeddingProvider` trait — `embed(text) → Vec<f32>`
//! - `MockEmbeddingProvider` — char trigram hashing → fixed 128-dim vectors
//! - `cosine_similarity(a, b)` — standard cosine between two vectors

// ---------------------------------------------------------------------------
// EmbeddingProvider trait
// ---------------------------------------------------------------------------

/// Pluggable text-to-vector embedding.
///
/// Implementations range from character n-grams (mock) to LLM-based embeddings
/// (OpenAI, local ONNX). The trait is the stable contract for embedding consumers.
pub trait EmbeddingProvider: Send + Sync {
    /// Human-readable name (e.g. "mock-char-ngram", "openai-text-embedding-3").
    fn name(&self) -> &str;

    /// Dimensionality of the vectors this provider produces.
    fn dimensions(&self) -> usize;

    /// Embed a piece of text into a vector.
    fn embed(&self, text: &str) -> Vec<f32>;
}

// ---------------------------------------------------------------------------
// Cosine similarity
// ---------------------------------------------------------------------------

/// Compute cosine similarity between two vectors.
/// Returns 0.0–1.0 (1.0 = identical direction). Returns 0.0 for zero vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    // Clamp to [-1, 1] to handle float imprecision.
    (dot / (norm_a * norm_b)).clamp(-1.0, 1.0)
}

// ---------------------------------------------------------------------------
// Mock embedding provider — character n-gram hashing
// ---------------------------------------------------------------------------

/// Deterministic char-trigram embedding with no external dependencies.
///
/// Strategy:
/// 1. Lowercase the text.
/// 2. Extract all character trigrams (padding with spaces at boundaries).
/// 3. Hash each trigram to one of `DIMS` buckets (128 by default).
/// 4. Build a bag-of-ngrams histogram vector.
/// 5. L2-normalize the vector.
///
/// This works well for short text (entity names, property values) because:
/// - "Acme Corp" and "Acme Corporation" share many trigrams → high cosine.
/// - "John Smith" and "J. Smith" share "smi", "mit", "ith" → moderate cosine.
/// - Completely different names have low trigram overlap → low cosine.
pub struct MockEmbeddingProvider {
    dims: usize,
}

impl MockEmbeddingProvider {
    /// Create a provider with the default 128-dimensional vectors.
    pub fn new() -> Self {
        MockEmbeddingProvider { dims: 128 }
    }

    /// Create a provider with a custom vector dimensionality.
    pub fn with_dimensions(dims: usize) -> Self {
        assert!(dims > 0, "dimensions must be positive");
        MockEmbeddingProvider { dims }
    }
}

impl EmbeddingProvider for MockEmbeddingProvider {
    fn name(&self) -> &str {
        "mock-char-ngram"
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        char_ngram_embed(text, self.dims)
    }
}

/// Embed text using character n-gram hashing into a fixed-size vector.
/// pub(crate): the mock multimodal provider (HLD §23) shares the text channel.
pub(crate) fn char_ngram_embed(text: &str, dims: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dims];
    let trimmed = text.trim();
    // Need at least 2 meaningful characters for trigrams.
    let meaningful: String = trimmed.chars().filter(|c| c.is_alphanumeric()).collect();
    if meaningful.len() < 2 {
        return vec; // zero vector for empty/too-short text
    }

    let lower = text.to_lowercase();
    // Pad with spaces so boundary characters participate in trigrams.
    let padded = format!("  {}  ", lower);
    let chars: Vec<char> = padded.chars().collect();

    let mut total: f32 = 0.0;
    for window in chars.windows(3) {
        let trigram: String = window.iter().collect();
        let idx = hash_trigram(&trigram, dims);
        vec[idx] += 1.0;
        total += 1.0;
    }

    // L2-normalize.
    if total > 0.0 {
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut vec {
                *v /= norm;
            }
        }
    }

    vec
}

/// Hash a trigram string to a bucket index using djb2 (simple, fast).
fn hash_trigram(trigram: &str, dims: usize) -> usize {
    let mut hash: u64 = 5381;
    for b in trigram.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    (hash as usize) % dims
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── EmbeddingProvider trait ──

    #[test]
    fn mock_implements_embedding_provider() {
        let p: &dyn EmbeddingProvider = &MockEmbeddingProvider::new();
        assert_eq!(p.name(), "mock-char-ngram");
        assert_eq!(p.dimensions(), 128);
    }

    // ── Vector properties ──

    #[test]
    fn embedding_has_correct_dimensions() {
        let provider = MockEmbeddingProvider::new();
        let v = provider.embed("Acme Corporation");
        assert_eq!(v.len(), 128);
    }

    #[test]
    fn custom_dimensions() {
        let provider = MockEmbeddingProvider::with_dimensions(64);
        let v = provider.embed("test");
        assert_eq!(v.len(), 64);
    }

    #[test]
    fn embedding_is_normalized() {
        let provider = MockEmbeddingProvider::new();
        let v = provider.embed("Acme Corporation");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.001,
            "norm should be 1.0, got {}",
            norm
        );
    }

    #[test]
    fn empty_text_returns_zero_vector() {
        let provider = MockEmbeddingProvider::new();
        let v = provider.embed("");
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn single_char_returns_zero_vector() {
        let provider = MockEmbeddingProvider::new();
        let v = provider.embed("a");
        assert!(v.iter().all(|x| *x == 0.0));
    }

    // ── Determinism ──

    #[test]
    fn same_text_produces_same_embedding() {
        let provider = MockEmbeddingProvider::new();
        let a = provider.embed("Acme Corporation");
        let b = provider.embed("Acme Corporation");
        assert_eq!(a, b);
    }

    // ── Semantic similarity ──

    #[test]
    fn similar_names_have_high_cosine_similarity() {
        let provider = MockEmbeddingProvider::new();
        let a = provider.embed("Acme Corporation");
        let b = provider.embed("Acme Corp");
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim > 0.5,
            "similar names should have cosine > 0.5, got {}",
            sim
        );
    }

    #[test]
    fn identical_names_have_max_similarity() {
        let provider = MockEmbeddingProvider::new();
        let a = provider.embed("John Smith");
        let b = provider.embed("John Smith");
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - 1.0).abs() < 0.001,
            "identical names should have sim 1.0, got {}",
            sim
        );
    }

    #[test]
    fn different_names_have_low_cosine_similarity() {
        let provider = MockEmbeddingProvider::new();
        let a = provider.embed("Acme Corporation");
        let b = provider.embed("Xylophone Zebra");
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim < 0.5,
            "different names should have cosine < 0.5, got {}",
            sim
        );
    }

    #[test]
    fn overlapping_names_have_moderate_similarity() {
        let provider = MockEmbeddingProvider::new();
        let a = provider.embed("John Smith");
        let b = provider.embed("John A. Smith");
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim > 0.6,
            "overlapping names should have cosine > 0.6, got {}",
            sim
        );
    }

    // ── Cosine similarity edge cases ──

    #[test]
    fn cosine_zero_vectors() {
        assert_eq!(cosine_similarity(&[0.0; 3], &[0.0; 3]), 0.0);
    }

    #[test]
    fn cosine_mismatched_dimensions() {
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0, 2.0, 3.0]), 0.0);
    }

    #[test]
    fn cosine_empty_vectors() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 0.001);
    }

    #[test]
    fn cosine_opposite_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - (-1.0)).abs() < 0.001);
    }

    // ── Case insensitivity ──

    #[test]
    fn case_differences_dont_affect_embedding() {
        let provider = MockEmbeddingProvider::new();
        let a = provider.embed("ACME CORPORATION");
        let b = provider.embed("acme corporation");
        assert_eq!(a, b);
    }

    // ── Multi-word stability ──

    #[test]
    fn abbreviation_similarity() {
        let provider = MockEmbeddingProvider::new();
        let full = provider.embed("International Business Machines");
        let abbr = provider.embed("IBM");
        let sim = cosine_similarity(&full, &abbr);
        // Not expected to be high (char n-grams differ significantly)
        // But the vector resolver can fall back to name matching.
        assert!(sim < 0.5, "IBM vs full name should differ, got {}", sim);
    }
}
