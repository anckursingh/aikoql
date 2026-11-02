//! Envelope Encryption — MRFC-0020 Phase 2.
//!
//! Envelope encryption: a master Key Encryption Key (KEK) wraps per-tenant
//! Data Encryption Keys (DEKs). The DEK encrypts data; the KEK encrypts the
//! DEK at rest. Key rotation re-wraps DEKs without re-encrypting all data.
//!
//! Key hierarchy: KMS/KEK → Tenant DEK → Data

use crate::security::audit::{KeyAuditLog, KeyEvent, KeyEventKind};
use crate::security::crypto::Crypto;
use crate::security::hkdf::{self, DOMAIN_DEK_WRAP};
use crate::security::kms::KeyManager;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Storage key under which wrapped DEKs are persisted (inside the store;
/// the wrapped form is KEK-encrypted and safe to sit beside the data).
pub const DEKS_STORAGE_KEY: &[u8] = b"__encryption__/deks";
/// AG-31 — the next generation's wrapped DEKs, written BEFORE the KMS
/// switches to a new KEK: after a crash the set that unwraps under the
/// installed KEK wins (`Kernel::with_field_encryption`).
pub const DEKS_NEXT_KEY: &[u8] = b"__encryption__/deks.next";

/// Storage key under which the crypto-version metadata record lives. Written
/// on first open and verified on every later open — an unknown record fails
/// closed instead of silently guessing key material (review P2-14).
pub const CRYPTO_META_KEY: &[u8] = b"__encryption__/meta";

/// Current crypto metadata record. The domain labels are the compatibility
/// contract: v1 wraps DEKs with HKDF-SHA256 under `aikoql/dek-wrap/v1`, the
/// store under `aikoql/store/v1`, and fields under `aikoql/field/v1`. Any
/// future change bumps this record and migrates through explicit versioning.
pub const CRYPTO_META_V1: &[u8] = b"aikoql/crypto/v1 kdf=argon2id-v1 \
wrap=hkdf-sha256/dek-wrap/v1 store=hkdf-sha256/store/v1 field=hkdf-sha256/field/v1";

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
    /// AG-31 — serializes DEK creation and the DEK persist with a KEK
    /// rotation: a DEK minted mid-rotation would be wrapped by the old KEK
    /// and missing from the new set. Also closes the create race (two
    /// threads minting different DEKs for one tenant).
    creation: std::sync::Mutex<()>,
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
            creation: std::sync::Mutex::new(()),
        })
    }

    /// Attach an audit log for key lifecycle event recording.
    pub fn with_audit(self, audit: Arc<KeyAuditLog>) -> Self {
        // justified: RwLock poison is unrecoverable
        *self.audit.write().unwrap() = Some(audit);
        self
    }

    /// Get (or create) a DEK for a tenant. The DEK is cached unwrapped;
    /// the wrapped form is stored for persistence.
    pub fn tenant_key(&self, tenant: &str) -> Result<[u8; 32], String> {
        // Fast path: cached.
        // justified: RwLock poison is unrecoverable
        if let Some(dek) = self.deks.read().unwrap().get(tenant) {
            return Ok(*dek);
        }
        // Slow path, under the creation lock (re-checked: another thread may
        // have minted it while we waited) — generate a new DEK, wrap it.
        // justified: Mutex poison is unrecoverable
        let _creating = self.creation.lock().unwrap();
        // justified: RwLock poison is unrecoverable
        if let Some(dek) = self.deks.read().unwrap().get(tenant) {
            return Ok(*dek);
        }
        let dek = self.crypto.generate_key();
        // justified: RwLock poison is unrecoverable
        let kek = *self.kek.read().unwrap();
        // justified: RwLock poison is unrecoverable
        let kek_id = *self.kek_id.read().unwrap();
        // The DEK is "wrapped" by encrypting it with a domain-separated
        // subkey of the KEK (never the KEK itself — the KEK never encrypts
        // data directly). AAD = tenant name binds this DEK to the tenant.
        let wrap_key = hkdf::domain_sep(&kek, DOMAIN_DEK_WRAP);
        let wrapped_key = self.crypto.encrypt(&wrap_key, &dek, tenant.as_bytes())?;
        let wrapped = WrappedDek {
            tenant: tenant.to_string(),
            kek_id,
            wrapped_key,
        };
        // justified: RwLock poison is unrecoverable
        self.deks.write().unwrap().insert(tenant.to_string(), dek);
        self.wrapped_deks
            .write()
            // justified: RwLock poison is unrecoverable
            .unwrap()
            .insert(tenant.to_string(), wrapped);
        // Audit: log key creation.
        // justified: RwLock poison is unrecoverable
        if let Some(ref audit) = *self.audit.read().unwrap() {
            let _ = audit.record(&KeyEvent::now(
                KeyEventKind::Created,
                &format!("tenant-dek:{}", tenant),
                &format!("DEK created, wrapped with KEK id {}", kek_id),
            ));
        }
        Ok(dek)
    }

    /// Unwrap a DEK under the current KEK without caching it (AEAD: a wrap
    /// made by another KEK fails authentication).
    fn unwrap_dek(&self, wrapped: &WrappedDek) -> Result<[u8; 32], String> {
        // justified: RwLock poison is unrecoverable
        let kek = *self.kek.read().unwrap();
        let wrap_key = hkdf::domain_sep(&kek, DOMAIN_DEK_WRAP);
        let dek_bytes =
            self.crypto
                .decrypt(&wrap_key, &wrapped.wrapped_key, wrapped.tenant.as_bytes())?;
        let dek: [u8; 32] = dek_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "unwrapped DEK is not 32 bytes".to_string())?;
        Ok(dek)
    }

    /// AG-31 — every wrap in `set` unwraps under the current KEK.
    pub fn unwraps_all(&self, set: &[WrappedDek]) -> bool {
        set.iter().all(|w| self.unwrap_dek(w).is_ok())
    }

    /// AG-31 — hold to keep DEK creation (and the DEK persist) out of a
    /// rotation's window.
    pub fn creation_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        // justified: Mutex poison is unrecoverable
        self.creation.lock().unwrap()
    }

    /// AG-31 — every loaded DEK wrapped by `new_kek` (the next generation),
    /// without changing this envelope: the caller persists it first.
    pub fn rewrapped(&self, new_kek: &[u8; 32]) -> Result<Vec<WrappedDek>, String> {
        let wrap_key = hkdf::domain_sep(new_kek, DOMAIN_DEK_WRAP);
        // justified: RwLock poison is unrecoverable
        let next_id = *self.kek_id.read().unwrap() + 1;
        // justified: RwLock poison is unrecoverable
        let deks = self.deks.read().unwrap();
        let mut out = Vec::with_capacity(deks.len());
        for (tenant, dek) in deks.iter() {
            out.push(WrappedDek {
                tenant: tenant.clone(),
                kek_id: next_id,
                wrapped_key: self.crypto.encrypt(&wrap_key, dek, tenant.as_bytes())?,
            });
        }
        Ok(out)
    }

    /// AG-31 — switch to `new_kek` and its wraps (after the KMS installed it).
    pub fn adopt(&self, new_kek: [u8; 32], wraps: Vec<WrappedDek>) {
        let n = wraps.len();
        // justified: RwLock poison is unrecoverable
        *self.kek.write().unwrap() = new_kek;
        // justified: RwLock poison is unrecoverable
        let id = {
            let mut id = self.kek_id.write().unwrap();
            *id += 1;
            *id
        };
        // justified: RwLock poison is unrecoverable
        *self.wrapped_deks.write().unwrap() =
            wraps.into_iter().map(|w| (w.tenant.clone(), w)).collect();
        // justified: RwLock poison is unrecoverable
        if let Some(ref audit) = *self.audit.read().unwrap() {
            let _ = audit.record(&KeyEvent::now(
                KeyEventKind::Rotated,
                "kek:master",
                &format!("KEK rotated to id {id}, {n} DEKs rewrapped"),
            ));
        }
    }

    /// Load a previously-wrapped DEK (e.g., from database metadata on startup).
    pub fn load_dek(&self, wrapped: &WrappedDek) -> Result<[u8; 32], String> {
        let dek = self.unwrap_dek(wrapped)?;
        // justified: RwLock poison is unrecoverable
        self.deks
            .write()
            .unwrap()
            .insert(wrapped.tenant.clone(), dek);
        // Keep the wrapped copy so a later wrapped_deks() persist round-trips
        // everything ever loaded (otherwise re-persisting writes an empty list).
        // justified: RwLock poison is unrecoverable
        self.wrapped_deks
            .write()
            .unwrap()
            .insert(wrapped.tenant.clone(), wrapped.clone());
        Ok(dek)
    }

    /// Return all wrapped DEKs for persistence (caller stores these alongside data).
    pub fn wrapped_deks(&self) -> Vec<WrappedDek> {
        self.wrapped_deks
            .read()
            // justified: RwLock poison is unrecoverable
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Encode wrapped DEKs for storage. Layout: u32 count, then per DEK:
    /// u16 tenant-len || tenant || u64 kek_id || u32 wrapped-len || wrapped.
    pub fn encode_wrapped_deks(deks: &[WrappedDek]) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 * deks.len());
        out.extend_from_slice(&(deks.len() as u32).to_le_bytes());
        for d in deks {
            out.extend_from_slice(&(d.tenant.len() as u16).to_le_bytes());
            out.extend_from_slice(d.tenant.as_bytes());
            out.extend_from_slice(&d.kek_id.to_le_bytes());
            out.extend_from_slice(&(d.wrapped_key.len() as u32).to_le_bytes());
            out.extend_from_slice(&d.wrapped_key);
        }
        out
    }

    /// Decode wrapped DEKs written by [`Self::encode_wrapped_deks`].
    /// Returns an error on any truncation/overflow — callers must fail closed.
    pub fn decode_wrapped_deks(bytes: &[u8]) -> Result<Vec<WrappedDek>, String> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        let take = |pos: &mut usize, n: usize| -> Result<&[u8], String> {
            if *pos > bytes.len() || bytes.len() - *pos < n {
                return Err("wrapped DEKs: truncated".into());
            }
            let s = &bytes[*pos..*pos + n];
            *pos += n;
            Ok(s)
        };
        let (count_b, _) = take(&mut pos, 4)?.split_at(4);
        let count = u32::from_le_bytes(count_b.try_into().unwrap()) as usize;
        for _ in 0..count {
            let (tlen_b, _) = take(&mut pos, 2)?.split_at(2);
            let tlen = u16::from_le_bytes(tlen_b.try_into().unwrap()) as usize;
            let tenant = String::from_utf8(take(&mut pos, tlen)?.to_vec())
                .map_err(|_| "wrapped DEKs: bad tenant utf8")?;
            let (kek_b, _) = take(&mut pos, 8)?.split_at(8);
            let kek_id = u64::from_le_bytes(kek_b.try_into().unwrap());
            let (wlen_b, _) = take(&mut pos, 4)?.split_at(4);
            let wlen = u32::from_le_bytes(wlen_b.try_into().unwrap()) as usize;
            let wrapped_key = take(&mut pos, wlen)?.to_vec();
            out.push(WrappedDek {
                tenant,
                kek_id,
                wrapped_key,
            });
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::crypto::{Aes256Gcm, CryptoProvider};
    use std::sync::RwLock;

    /// In-memory mock KMS for deterministic tests (no file I/O).
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
        let old_wraps = env.wrapped_deks();
        // AG-31: rewrap for the next KEK, then adopt it (the KMS install sits
        // between the two in KeyRing::rotate)
        let new_kek = crypto.generate_key();
        let next = env.rewrapped(&new_kek).unwrap();
        assert!(env.unwraps_all(&old_wraps) && !env.unwraps_all(&next));
        env.adopt(new_kek, next.clone());
        let dek_a_after = env.tenant_key("a").unwrap();
        assert_eq!(dek_a, dek_a_after, "the DEK itself never changes");
        assert!(
            env.unwraps_all(&next),
            "the new wraps unwrap under the new KEK"
        );
        assert!(
            !env.unwraps_all(&old_wraps),
            "the old KEK's wraps no longer do"
        );
        assert_eq!(env.wrapped_deks()[0].kek_id, 1);
    }

    /// AG-31 — a DEK roll swaps generations in place: new encrypts use the
    /// fresh key, the previous generation stays decryptable, and the
    /// previous set rewraps under a new KEK for a later KEK rotation.
    #[test]
    fn envelope_dek_roll_swaps_generations() {
        let kms = MemKms::new();
        let provider = Aes256Gcm::new();
        let crypto = std::sync::Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));

        let env = Envelope::init(&kms, "pw", crypto.clone()).unwrap();
        let old = env.tenant_key("a").unwrap();
        let old_wraps = env.wrapped_deks();

        let new_wraps = env.roll_deks().unwrap();
        assert_eq!(env.wrapped_deks().len(), 1);
        assert_eq!(
            env.prev_key("a"),
            Some(old),
            "the old generation stays decryptable"
        );
        let new = env.tenant_key("a").unwrap();
        assert_ne!(new, old, "new encrypts use a fresh DEK");
        assert_eq!(new_wraps[0].kek_id, old_wraps[0].kek_id, "the KEK is unchanged");

        // The previous set rewraps under a new KEK (a later KEK rotation):
        // a fresh envelope under the rotated KMS loads it, and the old
        // KEK's wraps no longer do.
        let prev_rewrapped = env.rewrapped_prev(&kms.rotate("pw", &provider).unwrap()).unwrap();
        let env2 = Envelope::init(&kms, "pw", crypto).unwrap();
        let loaded_prev = env2.load_prev_dek(&prev_rewrapped[0]).unwrap();
        assert_eq!(loaded_prev, old);
        assert!(env2.load_prev_dek(&old_wraps[0]).is_err(), "old KEK retired");
    }

    /// AG-31 — the create race: concurrent first calls for one tenant must
    /// agree on ONE DEK (two minted keys = ciphertext under a lost key).
    #[test]
    fn envelope_concurrent_first_use_mints_one_dek() {        let kms = MemKms::new();
        let crypto = std::sync::Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
        let env = std::sync::Arc::new(Envelope::init(&kms, "pw", crypto).unwrap());
        let keys: Vec<[u8; 32]> = (0..16)
            .map(|_| {
                let env = env.clone();
                std::thread::spawn(move || env.tenant_key("t").unwrap())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert!(keys.iter().all(|k| *k == keys[0]), "one DEK for the tenant");
        assert_eq!(env.tenant_key("t").unwrap(), keys[0]);
    }
}
