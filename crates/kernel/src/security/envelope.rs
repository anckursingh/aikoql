//! Envelope Encryption — MRFC-0020 Phase 2.
//!
//! Envelope encryption: a master Key Encryption Key (KEK) wraps per-tenant
//! Data Encryption Keys (DEKs). The DEK encrypts data; the KEK encrypts the
//! DEK at rest. Key rotation re-wraps DEKs without re-encrypting all data.
//!
//! Key hierarchy: KMS/KEK → Tenant DEK → Data

use crate::security::audit::{KeyAuditLog, KeyEvent, KeyEventKind};
use crate::security::crypto::{Crypto, CryptoProvider};
use crate::security::kms::KeyManager;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// A wrapped DEK: the DEK itself encrypted by the KEK.
#[derive(Clone, Debug)]
pub struct WrappedDek {
    /// Tenant this DEK belongs to.
    pub tenant: String,
    /// KEK ID that wrapped this DEK (for rotation tracking).
    pub kek_id: u64,
    /// Encrypted DEK bytes (DEK encrypted with KEK, using tenant name as AAD).
    pub wrapped_key: Vec<u8>,
}

/// Manages the full key hierarchy: KEK → DEKs → data.
pub struct Envelope {
    crypto: std::sync::Arc<Crypto>,
    /// Master KEK, loaded from KMS.
    kek: RwLock<[u8; 32]>,
    /// Current KEK generation (incremented on rotation).
    kek_id: RwLock<u64>,
    /// Per-tenant DEKs, cached unwrapped.
    deks: RwLock<HashMap<String, [u8; 32]>>,
    /// Wrapped DEKs for persistence.
    wrapped_deks: RwLock<HashMap<String, WrappedDek>>,
    /// Optional audit log for key lifecycle events (MRFC-0020 Phase 4).
    audit: RwLock<Option<Arc<KeyAuditLog>>>,
}

impl Envelope {
    /// Initialize the envelope from a KMS. The master KEK is retrieved
    /// (or created) from the KMS.
    pub fn init(
        kms: &dyn KeyManager,
        passphrase: &str,
        crypto: std::sync::Arc<Crypto>,
    ) -> Result<Self, String> {
        let kek = kms.master_key(passphrase)?;
        Ok(Envelope {
            crypto,
            kek: RwLock::new(kek),
            kek_id: RwLock::new(0),
            deks: RwLock::new(HashMap::new()),
            wrapped_deks: RwLock::new(HashMap::new()),
            audit: RwLock::new(None),
        })
    }

    /// Attach an audit log for key lifecycle event recording.
    pub fn with_audit(self, audit: Arc<KeyAuditLog>) -> Self {
        *self.audit.write().unwrap() = Some(audit);
        self
    }

    /// Get (or create) a DEK for a tenant. The DEK is cached unwrapped;
    /// the wrapped form is stored for persistence.
    pub fn tenant_key(&self, tenant: &str) -> Result<[u8; 32], String> {
        // Fast path: cached.
        if let Some(dek) = self.deks.read().unwrap().get(tenant) {
            return Ok(*dek);
        }
        // Slow path: generate new DEK, wrap it with KEK.
        let dek = self.crypto.generate_key();
        let kek = *self.kek.read().unwrap();
        let kek_id = *self.kek_id.read().unwrap();
        // The DEK is "wrapped" by encrypting it with the KEK.
        // AAD = tenant name binds this DEK to the tenant.
        let wrapped_key = self.crypto.encrypt(&kek, &dek, tenant.as_bytes())?;
        let wrapped = WrappedDek {
            tenant: tenant.to_string(),
            kek_id,
            wrapped_key,
        };
        self.deks.write().unwrap().insert(tenant.to_string(), dek);
        self.wrapped_deks.write().unwrap().insert(tenant.to_string(), wrapped);
        // Audit: log key creation.
        if let Some(ref audit) = *self.audit.read().unwrap() {
            let _ = audit.record(&KeyEvent::now(
                KeyEventKind::Created,
                &format!("tenant-dek:{}", tenant),
                &format!("DEK created, wrapped with KEK id {}", kek_id),
            ));
        }
        Ok(dek)
    }

    /// Load a previously-wrapped DEK (e.g., from database metadata on startup).
    pub fn load_dek(&self, wrapped: &WrappedDek) -> Result<[u8; 32], String> {
        let kek = *self.kek.read().unwrap();
        let dek_bytes = self.crypto.decrypt(&kek, &wrapped.wrapped_key, wrapped.tenant.as_bytes())?;
        let mut dek = [0u8; 32];
        dek.copy_from_slice(&dek_bytes);
        self.deks.write().unwrap().insert(wrapped.tenant.clone(), dek);
        Ok(dek)
    }

    /// Return all wrapped DEKs for persistence (caller stores these alongside data).
    pub fn wrapped_deks(&self) -> Vec<WrappedDek> {
        self.wrapped_deks.read().unwrap().values().cloned().collect()
    }

    /// Rotate the KEK: re-wrap all DEKs with the new KEK.
    /// This is online — old data does NOT need re-encryption.
    pub fn rotate_kek(&self, kms: &dyn KeyManager, passphrase: &str) -> Result<(), String> {
        let guard = self.crypto.inner().read().unwrap();
        let provider: &dyn CryptoProvider = &**guard;
        let new_kek = kms.rotate(passphrase, provider)?;
        let old_kek = *self.kek.read().unwrap();
        let new_id = *self.kek_id.read().unwrap() + 1;

        // Re-wrap all DEKs with the new KEK.
        let mut new_wrapped = HashMap::new();
        for (tenant, wrapped) in self.wrapped_deks.read().unwrap().iter() {
            // Decrypt DEK with old KEK.
            let dek_bytes = self.crypto.decrypt(&old_kek, &wrapped.wrapped_key, tenant.as_bytes())?;
            // Re-encrypt DEK with new KEK.
            let new_wrapped_key = self.crypto.encrypt(&new_kek, &dek_bytes, tenant.as_bytes())?;
            new_wrapped.insert(
                tenant.clone(),
                WrappedDek {
                    tenant: tenant.clone(),
                    kek_id: new_id,
                    wrapped_key: new_wrapped_key,
                },
            );
        }

        *self.kek.write().unwrap() = new_kek;
        *self.kek_id.write().unwrap() = new_id;
        let deks_count = new_wrapped.len();
        *self.wrapped_deks.write().unwrap() = new_wrapped;
        // Audit: log key rotation.
        if let Some(ref audit) = *self.audit.read().unwrap() {
            let _ = audit.record(&KeyEvent::now(
                KeyEventKind::Rotated,
                "kek:master",
                &format!("KEK rotated to id {}, {} DEKs rewrapped", new_id, deks_count),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::crypto::Aes256Gcm;
    use std::sync::RwLock;

    /// In-memory mock KMS for deterministic tests (no file I/O).
    struct MemKms {
        key: RwLock<[u8; 32]>,
    }

    impl MemKms {
        fn new() -> Self {
            MemKms { key: RwLock::new(Aes256Gcm::new().generate_key()) }
        }
    }

    impl KeyManager for MemKms {
        fn master_key(&self, _passphrase: &str) -> Result<[u8; 32], String> {
            Ok(*self.key.read().unwrap())
        }

        fn rotate(&self, _passphrase: &str, provider: &dyn CryptoProvider) -> Result<[u8; 32], String> {
            let new_key = provider.generate_key();
            *self.key.write().unwrap() = new_key;
            Ok(new_key)
        }
    }

    #[test]
    fn envelope_creates_and_recovers_tenant_key() {
        let kms = MemKms::new();
        let crypto = std::sync::Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));

        let env = Envelope::init(&kms, "ignored", crypto.clone()).unwrap();
        let dek1 = env.tenant_key("tenant-a").unwrap();
        let dek2 = env.tenant_key("tenant-a").unwrap();
        assert_eq!(dek1, dek2);

        let dek_b = env.tenant_key("tenant-b").unwrap();
        assert_ne!(dek1, dek_b);

        let mut wrapped: Vec<WrappedDek> = env.wrapped_deks();
        assert_eq!(wrapped.len(), 2);
        // Sort by tenant for deterministic lookup (HashMap iteration is non-deterministic).
        wrapped.sort_by(|a, b| a.tenant.cmp(&b.tenant));

        let env2 = Envelope::init(&kms, "ignored", crypto.clone()).unwrap();
        // Load tenant-a's DEK specifically.
        let tenant_a_wrapped = wrapped.iter().find(|w| w.tenant == "tenant-a").unwrap();
        let loaded = env2.load_dek(tenant_a_wrapped).unwrap();
        assert_eq!(loaded, dek1);

        // Also load tenant-b's DEK.
        let tenant_b_wrapped = wrapped.iter().find(|w| w.tenant == "tenant-b").unwrap();
        let loaded_b = env2.load_dek(tenant_b_wrapped).unwrap();
        assert_eq!(loaded_b, dek_b);
    }

    #[test]
    fn envelope_key_rotation_rewraps_deks() {
        let kms = MemKms::new();
        let crypto = std::sync::Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));

        let env = Envelope::init(&kms, "pw", crypto.clone()).unwrap();
        let dek_a = env.tenant_key("a").unwrap();
        env.rotate_kek(&kms, "pw").unwrap();
        let dek_a_after = env.tenant_key("a").unwrap();
        assert_eq!(dek_a, dek_a_after);

        let wrapped = env.wrapped_deks();
        assert_eq!(wrapped[0].kek_id, 1);
    }
}
