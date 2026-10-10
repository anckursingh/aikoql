//! The SDK error surface: one classified error type (the repo's
//! classified-error idiom). Every protocol error keeps the frozen SDK-012
//! fields (code/message/retryable/suggestion) — a mapped error never loses
//! its code.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A structured error from the MCP server (MRFC-0040 error codes): the
/// tool-level ok/error envelope and RPC-level failures both carry it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub suggestion: String,
}

impl McpError {
    /// The frozen TIMEOUT (retryable): a deadline elapsed before a response.
    pub(crate) fn deadline() -> Self {
        McpError {
            code: "TIMEOUT".into(),
            message: "no response within the deadline".into(),
            retryable: true,
            suggestion: "Retry with backoff; the request may have committed.".into(),
        }
    }

    /// A call on a closed client fails observably (§7 principle 11): a dead
    /// connection never deadlocks the caller, and a fresh dial recovers it.
    pub(crate) fn unavailable() -> Self {
        McpError {
            code: "UNAVAILABLE".into(),
            message: "the client is closed".into(),
            retryable: false,
            suggestion: "Connect again.".into(),
        }
    }

    pub(crate) fn protocol_error(request_id: u64, response_id: u64) -> Self {
        McpError {
            code: "PROTOCOL_ERROR".into(),
            message: format!("response id {response_id} does not match request {request_id}"),
            retryable: false,
            suggestion: "Check SDK/server version pairing.".into(),
        }
    }

    pub(crate) fn version_mismatch(server: &str) -> Self {
        McpError {
            code: "VERSION_MISMATCH".into(),
            message: format!("server version {server:?} is older than the SDK minimum {MIN}"),
            retryable: false,
            suggestion: "Upgrade the aikoql-mcp server to a supported version".into(),
        }
    }

    pub(crate) fn invalid_argument(message: String) -> Self {
        McpError {
            code: "INVALID_ARGUMENT".into(),
            message,
            retryable: false,
            suggestion: String::new(),
        }
    }
}

const MIN: &str = crate::client::MIN_SERVER_VERSION;

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for McpError {}

/// The SDK's one classified error type.
#[derive(Debug)]
pub enum Error {
    /// A structured protocol error (frozen SDK-012 taxonomy).
    Mcp(McpError),
    /// Transport or process failure.
    Io(std::io::Error),
    /// A malformed frame or payload.
    Json(serde_json::Error),
    /// An embedded-mode kernel failure (KError debug form).
    Kernel(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Mcp(e) => write!(f, "aikoql: {e}"),
            Error::Io(e) => write!(f, "aikoql: io: {e}"),
            Error::Json(e) => write!(f, "aikoql: json: {e}"),
            Error::Kernel(e) => write!(f, "aikoql: kernel: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Mcp(e) => Some(e),
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            Error::Kernel(_) => None,
        }
    }
}

impl From<McpError> for Error {
    fn from(e: McpError) -> Self {
        Error::Mcp(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}
