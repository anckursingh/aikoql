//! The Rust SDK for Aikoql — the reference implementation of the canonical
//! Database API (D-12, docs/DATABASE-API.md): a remote MCP client plus
//! embedded in-process mode (§25). The wire layer mirrors the Go SDK
//! (newline JSON-RPC, id correlation, the tools/call envelope); embedded
//! mode wraps aikoql-kernel directly. The D-15 native protocol replacement
//! changes only `client` — the canonical API does not change.

pub mod client;
pub mod embedded;
pub mod error;
/// The shared pure wire logic + the eight §15 fuzz checks (doc(hidden): the
/// out-of-workspace fuzz crate is the consumer).
#[doc(hidden)]
pub mod fuzz;
pub mod tools;
pub mod tx;

pub use client::{Client, SessionParams, MIN_SERVER_VERSION};
pub use embedded::Embedded;
pub use error::{Error, McpError};
pub use tools::{
    FindSimilarParams, KnowledgeObject, Metrics, RememberParams, Remembered, ScoredKO,
};
pub use tx::{CommitResult, StagedOp, Tx};

use std::future::Future;
use std::time::Duration;

/// Runs `fut` under a response deadline: elapsed → the frozen retryable
/// TIMEOUT. A late response afterwards is harmless — id correlation skips
/// it on the next call (self-healing).
pub async fn with_deadline<F, T>(d: Duration, fut: F) -> Result<T, Error>
where
    F: Future<Output = Result<T, Error>>,
{
    match tokio::time::timeout(d, fut).await {
        Ok(res) => res,
        Err(_) => Err(Error::Mcp(McpError::deadline())),
    }
}
