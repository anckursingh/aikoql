//! Embedding provider plugin interface — query-time text→vector.
//!
//! Like `IndexMaintainerApi`: trait lives in the kernel, implementations live
//! in engine crates with their own deps (HTTP clients, ML runtimes).
//! ponytail: trait-only module, impls in mnemosyne-semantic.

use crate::knowledge::kom::KResult;

/// Minimal trait for text → vector embedding at query time.
/// Separate from the semantic crate's `AiProvider` (which enriches whole KOs) —
/// this is the query-time interface.
pub trait EmbeddingProvider: Send + Sync {
    /// Embed `text` as a float vector. `model` optionally overrides the
    /// provider's configured default model.
    fn embed(&self, text: &str, model: Option<&str>) -> KResult<Vec<f32>>;
}
