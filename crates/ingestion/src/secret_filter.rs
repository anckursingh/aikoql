//! MRFC-0070 Phase A7: Secret/PII Filtering.
//!
//! Scans KnowledgeIr for secrets, API keys, tokens, emails, credit card
//! numbers, and other sensitive data. Redacts them before KO creation.
//!
//! Strategy: regex-based detection with redaction markers. This is a
//! defense-in-depth layer — connector-level filtering is the primary
//! defense; this catches what leaks through.

use crate::ir::KnowledgeIr;

/// A detected secret/PII item with its redacted replacement.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SecretFinding {
    /// What was found (type category).
    pub kind: SecretKind,
    /// Where it was found (entity name or fact id).
    pub location: String,
    /// The redacted text (original replaced with [REDACTED:<kind>]).
    pub redacted: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SecretKind {
    ApiKey,
    BearerToken,
    JwtToken,
    AwsKey,
    PrivateKey,
    Password,
    Email,
    CreditCard,
    Ssn,
    ConnectionString,
    GenericToken,
}

impl SecretKind {
    pub fn as_str(&self) -> &str {
        match self {
            SecretKind::ApiKey => "API_KEY",
            SecretKind::BearerToken => "BEARER_TOKEN",
            SecretKind::JwtToken => "JWT_TOKEN",
            SecretKind::AwsKey => "AWS_KEY",
            SecretKind::PrivateKey => "PRIVATE_KEY",
            SecretKind::Password => "PASSWORD",
            SecretKind::Email => "EMAIL",
            SecretKind::CreditCard => "CREDIT_CARD",
            SecretKind::Ssn => "SSN",
            SecretKind::ConnectionString => "CONNECTION_STRING",
            SecretKind::GenericToken => "GENERIC_TOKEN",
        }
    }
}

/// Scan and redact secrets from a KnowledgeIr.
/// Returns the redacted IR and a list of findings.
pub fn filter_secrets(ir: &KnowledgeIr) -> (KnowledgeIr, Vec<SecretFinding>) {
    let mut redacted = ir.clone();
    let mut findings = Vec::new();

    // Scan entity names
    for entity in &mut redacted.entities {
        if let Some(kind) = detect_secret(&entity.name) {
            let original = entity.name.clone();
            entity.name = format!("[REDACTED:{}]", kind.as_str());
            findings.push(SecretFinding {
                kind,
                location: format!("entity.name: {}", original),
                redacted: entity.name.clone(),
            });
        }
        // Scan entity mentions
        for mention in &mut entity.mentions {
            if let Some(kind) = detect_secret(mention) {
                let _original = mention.clone();
                *mention = format!("[REDACTED:{}]", kind.as_str());
                findings.push(SecretFinding {
                    kind,
                    location: format!("entity.{}.mentions", entity.name),
                    redacted: mention.clone(),
                });
            }
        }
    }

    // Scan fact statements
    for fact in &mut redacted.facts {
        if let Some(kind) = detect_secret(&fact.statement) {
            let _original = fact.statement.clone();
            fact.statement = format!("[REDACTED:{}] {}", kind.as_str(), fact.statement);
            findings.push(SecretFinding {
                kind,
                location: "fact.statement".to_string(),
                redacted: fact.statement.clone(),
            });
        }
    }

    (redacted, findings)
}

/// Detect if a string contains a secret pattern.
fn detect_secret(text: &str) -> Option<SecretKind> {
    // API keys: common patterns
    if contains_pattern(text, "sk-") && text.len() > 20 {
        return Some(SecretKind::ApiKey);
    }
    if contains_pattern(text, "api_key=")
        || contains_pattern(text, "apikey=")
        || contains_pattern(text, "api-key")
    {
        return Some(SecretKind::ApiKey);
    }

    // Bearer tokens
    if contains_pattern(text, "Bearer ") || contains_pattern(text, "bearer ") {
        return Some(SecretKind::BearerToken);
    }

    // JWT tokens (header.payload.signature pattern)
    if is_jwt(text) {
        return Some(SecretKind::JwtToken);
    }

    // AWS keys
    if contains_pattern(text, "AKIA")
        || contains_pattern(text, "ASIA")
        || (contains_pattern(text, "aws_access_key") || contains_pattern(text, "AWS_ACCESS_KEY"))
    {
        return Some(SecretKind::AwsKey);
    }

    // Private keys
    if text.contains("-----BEGIN") && (text.contains("PRIVATE KEY") || text.contains("RSA PRIVATE"))
    {
        return Some(SecretKind::PrivateKey);
    }

    // Connection strings (before password — Server=...Password= is a conn string)
    if contains_pattern(text, "mongodb://")
        || contains_pattern(text, "postgresql://")
        || contains_pattern(text, "mysql://")
        || contains_pattern(text, "postgres://")
        || (contains_pattern(text, "Server=") && contains_pattern(text, "Password="))
    {
        return Some(SecretKind::ConnectionString);
    }

    // Passwords
    if contains_pattern(text, "password=")
        || contains_pattern(text, "passwd=")
        || contains_pattern(text, "pwd=")
    {
        return Some(SecretKind::Password);
    }

    // Email addresses
    if is_email(text) {
        return Some(SecretKind::Email);
    }

    // Credit card numbers (basic Luhn-like patterns)
    if is_credit_card(text) {
        return Some(SecretKind::CreditCard);
    }

    // SSN (US)
    if is_ssn(text) {
        return Some(SecretKind::Ssn);
    }

    // Generic token: long base64-like strings
    if is_likely_token(text) {
        return Some(SecretKind::GenericToken);
    }

    None
}

fn contains_pattern(text: &str, pattern: &str) -> bool {
    text.to_lowercase().contains(&pattern.to_lowercase())
}

fn is_jwt(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts[0].len() > 10
        && parts[1].len() > 10
        && parts[2].len() > 10
        && parts[0]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        && parts[1]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

fn is_email(text: &str) -> bool {
    // Simple email detection: word@domain.tld
    let words: Vec<&str> = text.split_whitespace().collect();
    for w in &words {
        if w.contains('@') {
            let parts: Vec<&str> = w.split('@').collect();
            if parts.len() == 2
                && !parts[0].is_empty()
                && parts[1].contains('.')
                && parts[0]
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '-' || c == '+')
            {
                return true;
            }
        }
    }
    false
}

fn is_credit_card(text: &str) -> bool {
    // Look for sequences of 13-19 digits, possibly with separators
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 13 && digits.len() <= 19 {
        // Check for common patterns: 4xxx (Visa), 5xxx (MC), 3xxx (Amex)
        if digits.starts_with('4')
            || digits.starts_with('5')
            || digits.starts_with('3')
            || digits.starts_with('6')
        {
            return true;
        }
    }
    // Also check for dash-separated: XXXX-XXXX-XXXX-XXXX
    let dash_parts: Vec<&str> = text.split('-').collect();
    if dash_parts.len() == 4
        && dash_parts
            .iter()
            .all(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    false
}

fn is_ssn(text: &str) -> bool {
    // Match XXX-XX-XXXX pattern
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() == 3
        && parts[0].len() == 3
        && parts[1].len() == 2
        && parts[2].len() == 4
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    false
}

fn is_likely_token(text: &str) -> bool {
    // Long strings (>30 chars) that are mostly alphanumeric or base64 chars
    let clean: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
        .collect();
    // ponytail: token detection threshold — 40+ char base64-like strings
    // with no spaces are likely tokens/secrets
    if clean.len() > 40 && !text.contains(' ') && (clean.ends_with('=') || clean.len() > 60) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_api_key_in_entity_names() {
        let ir = KnowledgeIr {
            entities: vec![crate::EntityCandidate {
                name: "sk-proj-abcdef1234567890abcdef1234567890".into(),
                type_hint: Some("ApiKey".into()),
                mentions: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (filtered, findings) = filter_secrets(&ir);
        assert!(!findings.is_empty());
        assert!(filtered.entities[0].name.contains("REDACTED"));
    }

    #[test]
    fn redacts_email_in_mentions() {
        let ir = KnowledgeIr {
            entities: vec![crate::EntityCandidate {
                name: "Contact".into(),
                type_hint: Some("Person".into()),
                mentions: vec!["Email: admin@example.com for support".into()],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, SecretKind::Email);
    }

    #[test]
    fn redacts_jwt_in_facts() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                statement: "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8s".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, SecretKind::JwtToken);
    }

    #[test]
    fn redacts_connection_string() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                statement: "Server=prod-db.example.com;Password=superSecret123;Database=mydb"
                    .into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, SecretKind::ConnectionString);
    }

    #[test]
    fn clean_text_passes_through() {
        let ir = KnowledgeIr {
            entities: vec![crate::EntityCandidate {
                name: "TransactionEngine".into(),
                type_hint: Some("Struct".into()),
                mentions: vec!["Handles MVCC transaction isolation".into()],
                confidence: 0.9,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (filtered, findings) = filter_secrets(&ir);
        assert!(findings.is_empty());
        assert_eq!(filtered.entities[0].name, "TransactionEngine");
    }

    #[test]
    fn redacts_aws_key() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                statement: "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, SecretKind::AwsKey);
    }
}
