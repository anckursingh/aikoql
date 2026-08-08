//! Structured Error Codes — MRFC-0040 Agent Experience.
//!
//! Machine-parseable error responses with retry guidance. Every error
//! returned to an agent includes a code, message, retryable flag, and
//! optional suggestion so agents can handle errors programmatically.

use serde_json::{json, Value as J};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    AccessDenied,
    VersionConflict,
    NotFound,
    ValidationError,
    RateLimited,
    Timeout,
    Internal,
    NotAProgram,
    CompileError,
}

impl ErrorCode {
    pub fn as_str(&self) -> &str {
        match self {
            ErrorCode::AccessDenied => "ACCESS_DENIED",
            ErrorCode::VersionConflict => "VERSION_CONFLICT",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::ValidationError => "VALIDATION_ERROR",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::Internal => "INTERNAL",
            ErrorCode::NotAProgram => "NOT_A_PROGRAM",
            ErrorCode::CompileError => "COMPILE_ERROR",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, ErrorCode::Timeout | ErrorCode::RateLimited | ErrorCode::VersionConflict)
    }

    pub fn suggestion(&self) -> &str {
        match self {
            ErrorCode::AccessDenied => "Request access or use a different subject with appropriate roles",
            ErrorCode::VersionConflict => "Re-read the object to get the latest version, then retry",
            ErrorCode::NotFound => "Verify the KOID exists. Use discover_schema to check available types",
            ErrorCode::ValidationError => "Check the parameter types and required fields. Use tools/list for schemas",
            ErrorCode::RateLimited => "Wait and retry with exponential backoff",
            ErrorCode::Timeout => "The operation timed out. Retry with a smaller batch or narrower query",
            ErrorCode::Internal => "An unexpected error occurred. Report this if it persists",
            ErrorCode::NotAProgram => "The KOID references an object that is not a program. Use list_programs to find valid programs",
            ErrorCode::CompileError => "The AIKOQL query has a syntax error. Check the query and retry",
        }
    }

    /// Classify an error message string into an error code.
    pub fn classify(msg: &str) -> Self {
        let lower = msg.to_lowercase();
        if lower.contains("access denied") || lower.contains("unauthorized") || lower.contains("login required") {
            ErrorCode::AccessDenied
        } else if lower.contains("version conflict") || lower.contains("conflict") {
            ErrorCode::VersionConflict
        } else if lower.contains("not_found") || lower.contains("not found") || lower.contains("notfound") {
            ErrorCode::NotFound
        } else if lower.contains("missing") || lower.contains("invalid") || lower.contains("bad") {
            ErrorCode::ValidationError
        } else if lower.contains("rate") || lower.contains("too many") {
            ErrorCode::RateLimited
        } else if lower.contains("timeout") || lower.contains("timed out") {
            ErrorCode::Timeout
        } else if lower.contains("not a program") {
            ErrorCode::NotAProgram
        } else if lower.contains("compile") || lower.contains("parse") || lower.contains("syntax") {
            ErrorCode::CompileError
        } else {
            ErrorCode::Internal
        }
    }
}

/// Wrap a tool result with structured error handling for agents.
pub fn wrap_result(result: Result<J, String>) -> J {
    match result {
        Ok(payload) => json!({"ok": true, "data": payload}),
        Err(msg) => {
            let code = ErrorCode::classify(&msg);
            json!({
                "ok": false,
                "error": {
                    "code": code.as_str(),
                    "message": msg,
                    "retryable": code.retryable(),
                    "suggestion": code.suggestion(),
                }
            })
        }
    }
}
