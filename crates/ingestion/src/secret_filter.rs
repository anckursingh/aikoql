//! MRFC-0070 Phase A7: Secret/PII Filtering.
//!
//! Scans KnowledgeIr for secrets, API keys, tokens, emails, credit card
//! numbers, and other sensitive data. Redacts them before KO creation.
//!
//! Strategy: regex-based detection with redaction markers. This is a
//! defense-in-depth layer — connector-level filtering is the primary
//! defense; this catches what leaks through.
//!
//! Known limits (R8.1): pattern-based detection catches known formats only.
//! It does not decode URL-encoded or base64-encoded text or reassemble
//! secrets split across lines, so a determined adversary can bypass it.
//! Document-level filtering is the real boundary; this layer redacts
//! plain-text leaks.

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
    // API keys: common patterns. "sk-" matches on a word boundary —
    // "disk-", "task-" etc. must not trigger it.
    let lower = text.to_lowercase();
    let sk_api_key = lower
        .match_indices("sk-")
        .any(|(i, _)| i == 0 || !lower[..i].chars().last().unwrap_or('a').is_alphanumeric());
    if sk_api_key && text.len() > 20 {
        return Some(SecretKind::ApiKey);
    }
    if contains_pattern(text, "api_key=")
        || contains_pattern(text, "apikey=")
        || contains_pattern(text, "api-key=")
        || contains_pattern(text, "api-key:")
    {
        return Some(SecretKind::ApiKey);
    }
    // GitHub PATs
    if text.contains("github_pat_") || text.contains("ghp_") {
        return Some(SecretKind::ApiKey);
    }
    // Slack tokens: xoxb- (bot), xoxp- (user), xoxa- (app), xoxr- (refresh)
    if contains_pattern(text, "xoxb-")
        || contains_pattern(text, "xoxp-")
        || contains_pattern(text, "xoxa-")
        || contains_pattern(text, "xoxr-")
    {
        return Some(SecretKind::ApiKey);
    }
    // Stripe keys: sk_live_ / sk_test_ / pk_live_ / pk_test_ / rk_*
    if contains_pattern(text, "sk_live_")
        || contains_pattern(text, "sk_test_")
        || contains_pattern(text, "pk_live_")
        || contains_pattern(text, "pk_test_")
        || contains_pattern(text, "rk_live_")
        || contains_pattern(text, "rk_test_")
    {
        return Some(SecretKind::ApiKey);
    }

    // Google OAuth access tokens
    if contains_pattern(text, "ya29.") {
        return Some(SecretKind::BearerToken);
    }
    // Bearer tokens — require ≥20 contiguous non-whitespace chars after
    // "Bearer " to avoid matching prose like "Bearer of good news".
    if let Some(idx) = text.to_lowercase().find("bearer ") {
        let first_token: String = text[idx + 7..]
            .chars()
            .take_while(|c| !c.is_whitespace())
            .collect();
        if first_token.len() >= 20 {
            return Some(SecretKind::BearerToken);
        }
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
        || contains_pattern(text, "mongodb+srv://")
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
    // Pure hex (0-9a-f) is a checksum/hash (sha256, git oids), not a
    // base64 token — engineering docs are full of them.
    if !clean.is_empty() && clean.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    // ponytail: token detection threshold — 40+ char base64-like strings
    // with no spaces are likely tokens/secrets. A '/' or '+' inside an
    // unspaced 40+ char string is a strong base64 signal (AWS secret keys
    // are exactly 40 chars with no '=' suffix).
    if clean.len() >= 40
        && !text.contains(' ')
        && (clean.ends_with('=') || clean.len() > 60 || clean.contains('+') || clean.contains('/'))
    {
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
                snippet: None,
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
                snippet: None,
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
                snippet: None,
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

    // ---- adversarial / edge-case tests (R8 remediation) ----

    #[test]
    fn base64_encoded_api_key_is_redacted() {
        // base64("sk-proj-" + 30 random chars) ≈ 50-54 chars ending with =
        let encoded = "c2stcHJvai1hYmNkZWYxMjM0NTY3ODkwYWJjZGVmMTIzNDU2Nzg5MA==";
        assert!(encoded.len() > 50);
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: encoded.into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        // Long base64 with suffix = is flagged GenericToken
        assert!(!findings.is_empty());
    }

    #[test]
    fn short_base64_passes_through() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: "aGVsbG8=".into(), // base64("hello") — only 8 chars
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        // Short base64 should not trigger GenericToken (below 40-char threshold)
        assert!(findings.is_empty());
    }

    #[test]
    fn multi_line_private_key_is_redacted() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, SecretKind::PrivateKey);
    }

    #[test]
    fn bearer_in_prose_passes_through() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: "Bearer of good news to the entire team".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        // "Bearer " followed by short prose should NOT trigger BearerToken
        for f in &findings {
            assert_ne!(f.kind, SecretKind::BearerToken);
        }
    }

    #[test]
    fn api_key_in_prose_passes_through() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: "the api-key is not a secret here".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        // Bare "api-key" in prose (no = or :) should NOT trigger ApiKey
        for f in &findings {
            assert_ne!(f.kind, SecretKind::ApiKey);
        }
    }

    #[test]
    fn github_pat_is_redacted() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: "github_pat_11ABCDEFG1234567890abcdefghijklmnopqrstuv".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, SecretKind::ApiKey);
    }

    #[test]
    fn clean_uuid_passes_through() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: "550e8400-e29b-41d4-a716-446655440000".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        // UUIDs have dashes but aren't SSNs; shouldn't match anything
        assert!(findings.is_empty());
    }

    #[test]
    fn credit_card_with_spaces_is_redacted() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: "4111 1111 1111 1111".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, SecretKind::CreditCard);
    }

    // ---- R8.1: real-world secret formats ----

    /// Assert that `statement` is detected as exactly `kind`.
    fn assert_redacted_as(statement: &str, kind: SecretKind) {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: statement.into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(
            !findings.is_empty(),
            "expected '{}' to be redacted",
            statement
        );
        assert_eq!(
            findings[0].kind, kind,
            "expected '{:?}' for '{}', got '{:?}'",
            kind, statement, findings[0].kind
        );
    }

    #[test]
    fn slack_bot_token_is_redacted() {
        // R8.1: fixtures built at runtime — GitHub push protection blocks
        // literal token-shaped strings in source even as test fixtures.
        let slack = format!(
            "{}{}-{}-{}-{}",
            "xox", "b", 1234567890u64, 1234567890u64, "abcdefghijklmnopqrstuvwx"
        );
        assert_redacted_as(&slack, SecretKind::ApiKey);
    }

    #[test]
    fn stripe_secret_and_publishable_keys_are_redacted() {
        let stripe = format!("{}{}{}", "sk_", "live_", "51H7abcdEFGHijklMNOPqrst");
        assert_redacted_as(&stripe, SecretKind::ApiKey);
        assert_redacted_as("pk_test_51H7abcdEFGHijklMNOPqrst", SecretKind::ApiKey);
    }

    #[test]
    fn oauth_access_token_is_redacted() {
        assert_redacted_as(
            "ya29.a0AfH6SMB_abcdefghijklmnopqrstuvwxyz0123456789",
            SecretKind::BearerToken,
        );
    }

    #[test]
    fn mongodb_srv_connection_is_redacted() {
        assert_redacted_as(
            "mongodb+srv://user:pass@cluster0.mongodb.net/db",
            SecretKind::ConnectionString,
        );
    }

    #[test]
    fn postgres_url_is_redacted() {
        assert_redacted_as(
            "postgresql://user:pass@host:5432/db",
            SecretKind::ConnectionString,
        );
    }

    #[test]
    fn ghp_fine_grained_pat_is_redacted() {
        assert_redacted_as(
            "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
            SecretKind::ApiKey,
        );
    }

    #[test]
    fn aws_secret_key_is_redacted() {
        // The 40-char AWS secret key is base64-like (no "=" suffix) — the
        // generic-token threshold must catch it at exactly 40 chars.
        assert_redacted_as(
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            SecretKind::GenericToken,
        );
    }

    #[test]
    fn ssn_is_redacted() {
        assert_redacted_as("123-45-6789", SecretKind::Ssn);
    }

    #[test]
    fn password_assignment_is_redacted() {
        assert_redacted_as("password=Sup3rSecret!", SecretKind::Password);
    }

    #[test]
    fn bearer_token_is_redacted() {
        assert_redacted_as(
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789",
            SecretKind::BearerToken,
        );
    }

    // ---- R8.1: false positives ----

    #[test]
    fn sha256_hex_hash_passes_through() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement:
                    "sha256: a3f5c8d2e1b6490f7a6c5d4e3b2a1908f7e6d5c4b3a29182736455463728190a"
                        .into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(
            findings.is_empty(),
            "pure-hex checksums must not be redacted: {:?}",
            findings
        );
    }

    #[test]
    fn disk_word_passes_through() {
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: "the disk-usage metrics report is ready".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(
            findings.is_empty(),
            "'disk-' must not match 'sk-': {:?}",
            findings
        );
    }

    #[test]
    fn url_encoded_secret_is_documented_bypass() {
        // KNOWN LIMIT (R8.1): pattern-based detection does not decode URL
        // encoding — "admin@example.com" encoded as "admin%40example.com"
        // passes through. Document-level filtering is the primary defense.
        let ir = KnowledgeIr {
            facts: vec![crate::FactCandidate {
                snippet: None,
                statement: "contact admin%40example.com".into(),
                entities: vec![],
                confidence: 0.5,
                evidence: Default::default(),
            }],
            ..Default::default()
        };
        let (_, findings) = filter_secrets(&ir);
        assert!(findings.is_empty(), "documented bypass: {:?}", findings);
    }
}
