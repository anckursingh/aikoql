//! Field-Level Encryption — MRFC-0020 Phase 3.
//!
//! Encrypts marked fields within a KnowledgeObject's PropertyMap using the
//! tenant DEK from the Envelope. Encrypted values are stored as Bytes with
//! a version prefix; on read, the original Value type is restored.
//!
//! Format: version_byte(0x01) || nonce(12) || ciphertext || tag(16)
//! Same page format as storage-layer encryption (MRFC-0020 §Page Format).

use crate::knowledge::kom::Value;
use crate::security::audit::{KeyAuditLog, KeyEvent, KeyEventKind};
use crate::security::crypto::Crypto;
use crate::security::envelope::{Envelope, WrappedDek};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// ComplianceSummary — MRFC-0020 Phase 4
// ---------------------------------------------------------------------------

/// Snapshot of encryption compliance state for audit reporting.
#[derive(Clone, Debug)]
pub struct ComplianceSummary {
    /// Whether field-level encryption is enabled on this kernel.
    pub field_encryption_enabled: bool,
    /// Number of tenant DEKs in the envelope.
    pub tenant_keys: usize,
    /// Audit event counts by kind: (Created, Rotated, Used, Failure).
    pub audit_events: Vec<(KeyEventKind, usize)>,
}

// ---------------------------------------------------------------------------
// EncryptionPolicy
// ---------------------------------------------------------------------------

/// Which fields of a schema type should be encrypted.
#[derive(Clone, Debug)]
pub struct EncryptionPolicy {
    /// Field names whose values are encrypted at the knowledge layer.
    pub fields: HashSet<String>,
}

impl EncryptionPolicy {
    pub fn new(fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        EncryptionPolicy {
            fields: fields.into_iter().map(|f| f.into()).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

// ---------------------------------------------------------------------------
// FieldCrypto
// ---------------------------------------------------------------------------

/// Encrypts/decrypts individual fields within a PropertyMap using the
/// tenant DEK. Key hierarchy: KMS → KEK → tenant DEK → field-ciphertext.
pub struct FieldCrypto {
    crypto: Arc<Crypto>,
    envelope: Arc<Envelope>,
    /// Optional audit log for key usage events (MRFC-0020 Phase 4).
    audit: RwLock<Option<Arc<KeyAuditLog>>>,
}

impl FieldCrypto {
    pub fn new(crypto: Arc<Crypto>, envelope: Arc<Envelope>) -> Self {
        FieldCrypto {
            crypto,
            envelope,
            audit: RwLock::new(None),
        }
    }

    /// Attach an audit log for key usage event recording.
    pub fn with_audit(self, audit: Arc<KeyAuditLog>) -> Self {
        // justified: RwLock poison is unrecoverable
        *self.audit.write().unwrap() = Some(audit);
        self
    }

    /// All wrapped DEKs known to the envelope (for persistence).
    pub fn wrapped_deks(&self) -> Vec<WrappedDek> {
        self.envelope.wrapped_deks()
    }

    /// Generate a compliance summary for audit reporting (MRFC-0020 Phase 4).
    pub fn compliance_summary(&self) -> Result<ComplianceSummary, String> {
        // justified: RwLock poison is unrecoverable
        let audit_events = if let Some(ref audit) = *self.audit.read().unwrap() {
            audit.counts_by_kind()?
        } else {
            vec![]
        };
        let deks = self.envelope.wrapped_deks();
        Ok(ComplianceSummary {
            field_encryption_enabled: true,
            tenant_keys: deks.len(),
            audit_events,
        })
    }

    /// Encrypt fields listed in `policy`. Fields that don't exist or are
    /// already encrypted (Bytes with version prefix) are skipped.
    pub fn encrypt_fields(
        &self,
        tenant: &str,
        _type_name: &str,
        props: &mut BTreeMap<String, Value>,
        policy: &EncryptionPolicy,
    ) -> Result<usize, String> {
        if policy.is_empty() {
            return Ok(0);
        }
        // ponytail: derive field-level key from tenant DEK (HKDF in prod).
        let key = self.envelope.tenant_key(tenant)?;
        let mut count = 0usize;

        for field_name in &policy.fields {
            if let Some(value) = props.get(field_name) {
                // Skip already-encrypted values (Bytes with version byte 0x01).
                if let Value::Bytes(b) = value {
                    if b.first() == Some(&0x01) {
                        continue;
                    }
                }
                let plaintext = value_to_bytes(value);
                let aad = field_name.as_bytes();
                let cipher_bytes = self.crypto.encrypt(&key, &plaintext, aad)?;
                props.insert(field_name.clone(), Value::Bytes(cipher_bytes));
                count += 1;
            }
        }
        // Audit: log field encryption usage (once per call, not per field).
        if count > 0 {
            // justified: RwLock poison is unrecoverable
            if let Some(ref audit) = *self.audit.read().unwrap() {
                let _ = audit.record(&KeyEvent::now(
                    KeyEventKind::Used,
                    &format!("tenant-dek:{}", tenant),
                    &format!("encrypted {} fields for type {}", count, _type_name),
                ));
            }
        }
        Ok(count)
    }

    /// Decrypt fields listed in `policy`. Only decrypts Bytes values with
    /// the version prefix — plaintext fields are left alone (idempotent).
    pub fn decrypt_fields(
        &self,
        tenant: &str,
        _type_name: &str,
        props: &mut BTreeMap<String, Value>,
        policy: &EncryptionPolicy,
    ) -> Result<usize, String> {
        if policy.is_empty() {
            return Ok(0);
        }
        let key = self.envelope.tenant_key(tenant)?;
        let mut count = 0usize;

        for field_name in &policy.fields {
            if let Some(Value::Bytes(b)) = props.get(field_name) {
                if b.first() != Some(&0x01) {
                    continue; // not encrypted
                }
                let aad = field_name.as_bytes();
                let plain_bytes = self.crypto.decrypt(&key, b, aad)?;
                // Restore the original Value type from the encrypted payload.
                // ponytail: simple tag-prefix type encoding (0x00=Text, 0x01=Int BE, ...).
                if let Some(restored) = bytes_to_value(&plain_bytes) {
                    props.insert(field_name.clone(), restored);
                    count += 1;
                }
            }
        }
        // Audit: log field decryption usage.
        if count > 0 {
            // justified: RwLock poison is unrecoverable
            if let Some(ref audit) = *self.audit.read().unwrap() {
                let _ = audit.record(&KeyEvent::now(
                    KeyEventKind::Used,
                    &format!("tenant-dek:{}", tenant),
                    &format!("decrypted {} fields for type {}", count, _type_name),
                ));
            }
        }
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// Serialization helpers
// ---------------------------------------------------------------------------

/// Type-tagged byte encoding for round-trip. Layout:
///   type_tag(1) || payload
///   tag 0x00 = Text (UTF-8)
///   tag 0x01 = Int (i64, big-endian)
///   tag 0x02 = Float (f64, little-endian)
///   tag 0x03 = Bool
///   tag 0x04 = Bytes (raw)
///   tag 0x05 = Null
fn value_to_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::Null => vec![0x05],
        Value::Bool(true) => vec![0x03, 1],
        Value::Bool(false) => vec![0x03, 0],
        Value::Int(i) => {
            let mut b = vec![0x01];
            b.extend_from_slice(&i.to_be_bytes());
            b
        }
        Value::Float(f) => {
            let mut b = vec![0x02];
            b.extend_from_slice(&f.to_le_bytes());
            b
        }
        Value::Text(s) => {
            let mut b = vec![0x00];
            b.extend_from_slice(s.as_bytes());
            b
        }
        Value::Bytes(data) => {
            let mut b = vec![0x04];
            b.extend_from_slice(data);
            b
        }
        Value::List(items) => {
            let mut b = vec![0x07];
            // ponytail: u16 length cap (65535 items); truncation doesn't apply in practice.
            let n = items.len().min(u16::MAX as usize);
            b.extend_from_slice(&(n as u16).to_le_bytes());
            for item in &items[..n] {
                let item_bytes = value_to_bytes(item);
                b.extend_from_slice(&(item_bytes.len() as u16).to_le_bytes());
                b.extend_from_slice(&item_bytes);
            }
            b
        }
        Value::Map(entries) => {
            let mut b = vec![0x08];
            let n = entries.len().min(u16::MAX as usize);
            b.extend_from_slice(&(n as u16).to_le_bytes());
            for (k, val) in entries.iter().take(n) {
                let key_bytes = k.as_bytes();
                b.extend_from_slice(&(key_bytes.len() as u16).to_le_bytes());
                b.extend_from_slice(key_bytes);
                let val_bytes = value_to_bytes(val);
                b.extend_from_slice(&(val_bytes.len() as u16).to_le_bytes());
                b.extend_from_slice(&val_bytes);
            }
            b
        }
    }
}

fn bytes_to_value(b: &[u8]) -> Option<Value> {
    if b.is_empty() {
        return None;
    }
    match b[0] {
        0x00 => String::from_utf8(b[1..].to_vec()).ok().map(Value::Text),
        0x01 if b.len() == 9 => {
            let arr: [u8; 8] = b[1..].try_into().ok()?;
            Some(Value::Int(i64::from_be_bytes(arr)))
        }
        0x02 if b.len() == 9 => {
            let arr: [u8; 8] = b[1..].try_into().ok()?;
            Some(Value::Float(f64::from_le_bytes(arr)))
        }
        0x03 if b.len() == 2 => Some(Value::Bool(b[1] != 0)),
        0x04 => Some(Value::Bytes(b[1..].to_vec())),
        0x05 => Some(Value::Null),
        0x07 => {
            // List: u16 count || [u16 len || encoded_item]*
            if b.len() < 3 {
                return None;
            }
            let n = u16::from_le_bytes([b[1], b[2]]) as usize;
            let mut items = Vec::with_capacity(n.min(1024));
            let mut pos = 3usize;
            for _ in 0..n {
                if pos + 2 > b.len() {
                    break;
                }
                let item_len = u16::from_le_bytes([b[pos], b[pos + 1]]) as usize;
                pos += 2;
                if pos + item_len > b.len() {
                    break;
                }
                if let Some(v) = bytes_to_value(&b[pos..pos + item_len]) {
                    items.push(v);
                }
                pos += item_len;
            }
            Some(Value::List(items))
        }
        0x08 => {
            // Map: u16 count || [u16 key_len || key || u16 val_len || val]*
            if b.len() < 3 {
                return None;
            }
            let n = u16::from_le_bytes([b[1], b[2]]) as usize;
            let mut map = BTreeMap::new();
            let mut pos = 3usize;
            for _ in 0..n {
                if pos + 2 > b.len() {
                    break;
                }
                let key_len = u16::from_le_bytes([b[pos], b[pos + 1]]) as usize;
                pos += 2;
                if pos + key_len > b.len() {
                    break;
                }
                let key = String::from_utf8_lossy(&b[pos..pos + key_len]).into_owned();
                pos += key_len;
                if pos + 2 > b.len() {
                    break;
                }
                let val_len = u16::from_le_bytes([b[pos], b[pos + 1]]) as usize;
                pos += 2;
                if pos + val_len > b.len() {
                    break;
                }
                if let Some(v) = bytes_to_value(&b[pos..pos + val_len]) {
                    map.insert(key, v);
                }
                pos += val_len;
            }
            Some(Value::Map(map))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::kom::Value;
    use crate::security::crypto::{Aes256Gcm, CryptoProvider};
    use crate::security::kms::KeyManager;
    use std::collections::BTreeMap;
    use std::sync::RwLock;

    struct MemKms {
        key: RwLock<[u8; 32]>,
    }
    impl MemKms {
        fn new() -> Self {
            MemKms {
                key: RwLock::new(Aes256Gcm::new().generate_key()),
            }
        }
    }
    impl KeyManager for MemKms {
        fn master_key(&self, _passphrase: &str) -> Result<[u8; 32], String> {
            // justified: RwLock poison is unrecoverable
            Ok(*self.key.read().unwrap())
        }
        fn rotate(
            &self,
            _passphrase: &str,
            provider: &dyn CryptoProvider,
        ) -> Result<[u8; 32], String> {
            let new_key = provider.generate_key();
            // justified: RwLock poison is unrecoverable
            *self.key.write().unwrap() = new_key;
            Ok(new_key)
        }
    }

    fn setup() -> (FieldCrypto, EncryptionPolicy) {
        let kms = MemKms::new();
        let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
        let env = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());
        let fc = FieldCrypto::new(crypto, env);
        let policy = EncryptionPolicy::new(vec!["salary".to_string(), "ssn".to_string()]);
        (fc, policy)
    }

    #[test]
    fn roundtrip_text_fields() {
        let (fc, policy) = setup();
        let mut props = BTreeMap::new();
        props.insert("name".into(), Value::Text("Alice".into()));
        props.insert("salary".into(), Value::Text("150000".into()));
        props.insert("city".into(), Value::Text("NYC".into()));

        // Encrypt
        let n = fc
            .encrypt_fields("acme", "employee", &mut props, &policy)
            .unwrap();
        assert_eq!(n, 1);
        // salary is now bytes
        assert!(matches!(props.get("salary"), Some(Value::Bytes(_))));
        // name and city are untouched
        assert_eq!(props.get("name"), Some(&Value::Text("Alice".into())));
        assert_eq!(props.get("city"), Some(&Value::Text("NYC".into())));

        // Decrypt
        let n = fc
            .decrypt_fields("acme", "employee", &mut props, &policy)
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(props.get("salary"), Some(&Value::Text("150000".into())));
    }

    #[test]
    fn roundtrip_mixed_types() {
        let (fc, policy) = setup();
        let mut props = BTreeMap::new();
        props.insert("x".into(), Value::Int(42));
        props.insert("salary".into(), Value::Int(99999));
        props.insert("ssn".into(), Value::Text("123-45-6789".into()));

        let n = fc.encrypt_fields("acme", "t", &mut props, &policy).unwrap();
        assert_eq!(n, 2);
        assert!(matches!(props.get("salary"), Some(Value::Bytes(_))));
        assert!(matches!(props.get("ssn"), Some(Value::Bytes(_))));
        assert_eq!(props.get("x"), Some(&Value::Int(42)));

        let n = fc.decrypt_fields("acme", "t", &mut props, &policy).unwrap();
        assert_eq!(n, 2);
        assert_eq!(props.get("salary"), Some(&Value::Int(99999)));
        assert_eq!(props.get("ssn"), Some(&Value::Text("123-45-6789".into())));
    }

    #[test]
    fn decrypt_is_idempotent() {
        let (fc, policy) = setup();
        let mut props = BTreeMap::new();
        props.insert("salary".into(), Value::Text("5".into()));
        fc.encrypt_fields("acme", "t", &mut props, &policy).unwrap();
        // Decrypt twice — second is no-op
        fc.decrypt_fields("acme", "t", &mut props, &policy).unwrap();
        let n = fc.decrypt_fields("acme", "t", &mut props, &policy).unwrap();
        assert_eq!(n, 0); // nothing to decrypt
        assert_eq!(props.get("salary"), Some(&Value::Text("5".into())));
    }

    #[test]
    fn different_tenants_different_keys() {
        let (fc, policy) = setup();
        let mut props_a = BTreeMap::new();
        props_a.insert("salary".into(), Value::Text("a-salary".into()));
        let mut props_b = BTreeMap::new();
        props_b.insert("salary".into(), Value::Text("b-salary".into()));

        fc.encrypt_fields("tenant-a", "t", &mut props_a, &policy)
            .unwrap();
        fc.encrypt_fields("tenant-b", "t", &mut props_b, &policy)
            .unwrap();

        // Ciphertexts should differ because different tenant DEKs.
        let ct_a = props_a.get("salary").cloned();
        let ct_b = props_b.get("salary").cloned();
        assert_ne!(ct_a, ct_b);

        // Each decrypts correctly with its own tenant.
        fc.decrypt_fields("tenant-a", "t", &mut props_a, &policy)
            .unwrap();
        fc.decrypt_fields("tenant-b", "t", &mut props_b, &policy)
            .unwrap();
        assert_eq!(props_a.get("salary"), Some(&Value::Text("a-salary".into())));
        assert_eq!(props_b.get("salary"), Some(&Value::Text("b-salary".into())));
    }

    #[test]
    fn empty_policy_noop() {
        let (fc, _) = setup();
        let empty = EncryptionPolicy::new(Vec::<String>::new());
        let mut props = BTreeMap::new();
        props.insert("salary".into(), Value::Text("secret".into()));
        let n = fc.encrypt_fields("acme", "t", &mut props, &empty).unwrap();
        assert_eq!(n, 0);
        assert_eq!(props.get("salary"), Some(&Value::Text("secret".into())));
    }

    #[test]
    fn missing_field_skipped() {
        let (fc, policy) = setup();
        let mut props = BTreeMap::new();
        props.insert("name".into(), Value::Text("Bob".into()));
        let n = fc.encrypt_fields("acme", "t", &mut props, &policy).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn value_type_roundtrip_all_scalars() {
        // Verify each scalar type round-trips through value_to_bytes/bytes_to_value.
        let cases: Vec<Value> = vec![
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(-42),
            Value::Int(0),
            Value::Int(i64::MAX),
            Value::Float(3.14),
            Value::Float(-0.5),
            Value::Text("hello".into()),
            Value::Text("".into()),
            Value::Bytes(vec![1, 2, 3]),
        ];
        for v in &cases {
            let bytes = value_to_bytes(v);
            let restored = bytes_to_value(&bytes).unwrap();
            assert_eq!(&restored, v, "round-trip failed for {:?}", v);
        }
    }
}
