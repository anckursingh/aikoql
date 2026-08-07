//! AIKOQL Diagnostics Catalog — stable error codes per MRFC-0010 §10.
//!
//! Every parser/compiler error gets a stable code. Codes never change;
//! messages can be improved. External tools can rely on codes for
//! programmatic error handling.

use std::fmt;

/// All AIKOQL error codes. Range: AIKOQL1001–AIKOQL1999.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    // ---- Lexer errors (1001–1009) ----
    /// Unexpected character in source.
    UnexpectedChar = 1001,
    /// Unterminated string literal.
    UnterminatedString = 1002,
    /// Invalid number literal.
    InvalidNumber = 1003,

    // ---- Parser errors (1010–1029) ----
    /// Unexpected token at current position.
    UnexpectedToken = 1010,
    /// Expected a specific token but found something else.
    ExpectedToken = 1011,
    /// Query ended before completing a statement.
    UnexpectedEof = 1012,
    /// Invalid comparison operator in WHERE clause.
    InvalidOperator = 1013,
    /// Conflicting clauses (e.g., both SCAN and TRAVERSE).
    ConflictingClauses = 1014,

    // ---- Semantic errors (1030–1049) ----
    /// Referenced entity type is not registered.
    UnknownType = 1030,
    /// Referenced property does not exist on the entity.
    UnknownProperty = 1031,
    /// Type mismatch in expression.
    TypeMismatch = 1032,
}

impl Code {
    /// Human-readable description. Stable for documentation; may be improved
    /// for clarity without changing the code number.
    pub fn description(self) -> &'static str {
        match self {
            Code::UnexpectedChar => "unexpected character in source",
            Code::UnterminatedString => "unterminated string literal",
            Code::InvalidNumber => "invalid number literal",
            Code::UnexpectedToken => "unexpected token",
            Code::ExpectedToken => "expected a different token",
            Code::UnexpectedEof => "unexpected end of input",
            Code::InvalidOperator => "invalid comparison operator",
            Code::ConflictingClauses => "conflicting clauses in query",
            Code::UnknownType => "unknown entity type",
            Code::UnknownProperty => "unknown property on entity",
            Code::TypeMismatch => "type mismatch in expression",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AIKOQL{:04}", *self as u16)
    }
}

/// A structured diagnostic message.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: Code,
    pub message: String,
    pub line: usize,
    pub column: usize,
    pub hint: Option<String>,
}

impl Diagnostic {
    pub fn new(code: Code, message: impl Into<String>, line: usize, column: usize) -> Self {
        Diagnostic { code, message: message.into(), line, column, hint: None }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into()); self
    }

    /// Format per MRFC-0010 §10.
    pub fn format(&self) -> String {
        let base = format!(
            "{}: {} at line {}, column {}",
            self.code, self.message, self.line, self.column
        );
        match &self.hint {
            Some(h) => format!("{}. Did you mean: {}?", base, h),
            None => format!("{}.", base),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format())
    }
}

impl std::error::Error for Diagnostic {}

// ---- Convenience constructors for parser use ----

pub fn unexpected_token(got: &str, line: usize, col: usize) -> Diagnostic {
    Diagnostic::new(Code::UnexpectedToken, format!("unexpected token '{}'", got), line, col)
}

pub fn expected_token(expected: &str, got: &str, line: usize, col: usize) -> Diagnostic {
    Diagnostic::new(Code::ExpectedToken, format!("expected {}, got '{}'", expected, got), line, col)
}

pub fn unexpected_eof(line: usize, col: usize) -> Diagnostic {
    Diagnostic::new(Code::UnexpectedEof, "query ended unexpectedly", line, col)
}

pub fn invalid_operator(got: &str, line: usize, col: usize) -> Diagnostic {
    Diagnostic::new(Code::InvalidOperator, format!("'{}' is not a valid comparison operator", got), line, col)
        .with_hint("use == != < > <= >=")
}

pub fn conflicting_clauses(a: &str, b: &str, line: usize, col: usize) -> Diagnostic {
    Diagnostic::new(Code::ConflictingClauses, format!("cannot use both '{}' and '{}'", a, b), line, col)
}

// ---- Convenience constructors for semantic analysis ----

pub fn unknown_type(name: &str, line: usize, col: usize) -> Diagnostic {
    Diagnostic::new(Code::UnknownType, format!("unknown type '{}'", name), line, col)
        .with_hint(format!("register it with CREATE TYPE {}", name))
}

pub fn unknown_property(prop: &str, entity: &str, line: usize, col: usize) -> Diagnostic {
    Diagnostic::new(Code::UnknownProperty, format!("'{}' has no property '{}'", entity, prop), line, col)
}
