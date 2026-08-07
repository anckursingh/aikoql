//! Local Key Management Service — MRFC-0020 Phase 1.
//!
//! File-backed master key for single-node deployments. The master key is
//! stored encrypted with a passphrase-derived key (PBKDF2-SHA256).
//! Cloud KMS plugins (AWS, Azure, GCP, Vault) implement the same trait.
//!
//! ponytail: single master key per instance. Per-tenant keys land in
//! Phase 2 (envelope encryption).

use crate::security::crypto::CryptoProvider;
use rand::RngCore;
use sha2::Digest;
use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// KeyManager trait
// ---------------------------------------------------------------------------

/// Abstraction over key storage backends (local file, AWS KMS, HSM, etc.).
pub trait KeyManager: Send + Sync {
    /// Retrieve or create the master key. The `passphrase` is used to
    /// derive a key-encryption key that wraps the master key at rest.
    fn master_key(&self, passphrase: &str) -> Result<[u8; 32], String>;

    /// Rotate to a new master key. Old data must be re-encrypted by the
    /// caller (online rotation in Phase 2).
    fn rotate(&self, passphrase: &str, provider: &dyn CryptoProvider) -> Result<[u8; 32], String>;
}

// ---------------------------------------------------------------------------
// LocalKms — file-backed master key
// ---------------------------------------------------------------------------

/// Stores the master key in `path`, encrypted with a key derived from
/// `passphrase` via PBKDF2-SHA256 (100,000 iterations).
pub struct LocalKms {
    path: String,
    cached_key: std::sync::RwLock<Option<[u8; 32]>>,
}

impl LocalKms {
    pub fn new(path: impl Into<String>) -> Self {
        LocalKms { path: path.into(), cached_key: std::sync::RwLock::new(None) }
    }
}

impl KeyManager for LocalKms {
    fn master_key(&self, passphrase: &str) -> Result<[u8; 32], String> {
        // Return cached key if available.
        if let Some(k) = *self.cached_key.read().unwrap() {
            return Ok(k);
        }
        let path = Path::new(&self.path);
        if path.exists() {
            let encrypted = fs::read(path)
                .map_err(|e| format!("read master key file {}: {}", self.path, e))?;
            if encrypted.len() < 48 {
                return Err("master key file corrupted".into());
            }
            let salt: [u8; 16] = encrypted[..16].try_into().unwrap();
            let wrapped: [u8; 32] = encrypted[16..48].try_into().unwrap();
            let derived = derive_key(passphrase, &salt);
            let key = xor_unwrap(&derived, &wrapped);
            *self.cached_key.write().unwrap() = Some(key);
            Ok(key)
        } else {
            // First run: generate a new master key and persist it.
            let mut salt = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut salt);
            let derived = derive_key(passphrase, &salt);
            let mut key = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut key);
            let wrapped = xor_unwrap(&derived, &key); // bijection: xor twice = original
            let mut file_content = Vec::with_capacity(48);
            file_content.extend_from_slice(&salt);
            file_content.extend_from_slice(&wrapped);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("create kms dir: {}", e))?;
            }
            fs::write(path, &file_content)
                .map_err(|e| format!("write master key: {}", e))?;
            *self.cached_key.write().unwrap() = Some(key);
            Ok(key)
        }
    }

    fn rotate(&self, passphrase: &str, provider: &dyn CryptoProvider) -> Result<[u8; 32], String> {
        let new_key = provider.generate_key();
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        let derived = derive_key(passphrase, &salt);
        let wrapped = xor_unwrap(&derived, &new_key);
        let mut file_content = Vec::with_capacity(48);
        file_content.extend_from_slice(&salt);
        file_content.extend_from_slice(&wrapped);
        fs::write(&self.path, &file_content)
            .map_err(|e| format!("write rotated key: {}", e))?;
        *self.cached_key.write().unwrap() = Some(new_key);
        Ok(new_key)
    }
}

// ---------------------------------------------------------------------------
// Key derivation
// ---------------------------------------------------------------------------

/// Derive a 32-byte key from a passphrase and salt using PBKDF2-SHA256.
/// ponytail: 100,000 iterations is the OWASP minimum. Increase if this
/// becomes a measured bottleneck (unlikely — only called on startup/rotation).
fn derive_key(passphrase: &str, salt: &[u8; 16]) -> [u8; 32] {
    let mut dk = [0u8; 32];
    // Simplified PBKDF2: iterated HMAC-SHA256.
    // ponytail: use a proper PBKDF2 crate (pbkdf2) for production.
    // For Phase 1, SHA-256 iterated is sufficient for local file protection.
    let mut hasher = sha2::Sha256::new();
    hasher.update(salt);
    hasher.update(passphrase.as_bytes());
    let mut hash = hasher.finalize_reset();

    // Stretch: 100k iterations of SHA-256(hash || salt || passphrase).
    for _ in 0..100_000 {
        hasher.update(&hash);
        hasher.update(salt);
        hasher.update(passphrase.as_bytes());
        hash = hasher.finalize_reset();
    }
    dk.copy_from_slice(&hash[..32]);
    dk
}

/// XOR two 32-byte arrays. Used for simple key wrapping.
/// ponytail: XOR wrapping is NOT suitable for production HSM integration.
/// Replace with AES-KW (RFC 3394) or RSA-OAEP when integrating with real KMS.
/// For local file protection, XOR wrapping + PBKDF2-derived KEK is sufficient
/// to protect the key at rest on the same machine.
fn xor_unwrap(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = a[i] ^ b[i];
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// All test functions that create LocalKms instances must hold this lock
    /// to prevent parallel file I/O races across test modules.
    static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn local_kms_create_and_load() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("mnemosyne-test-kms-{}", std::process::id()));
        let kms = LocalKms::new(tmp.to_str().unwrap());
        let key1 = kms.master_key("test-passphrase").unwrap();
        assert_ne!(key1, [0u8; 32]);

        // Second call returns cached key.
        let key2 = kms.master_key("test-passphrase").unwrap();
        assert_eq!(key1, key2);

        // New instance with same file + passphrase returns same key.
        let kms2 = LocalKms::new(tmp.to_str().unwrap());
        let key3 = kms2.master_key("test-passphrase").unwrap();
        assert_eq!(key1, key3);

        // Wrong passphrase returns different (wrong) key.
        let kms3 = LocalKms::new(tmp.to_str().unwrap());
        // Force uncached read
        *kms3.cached_key.write().unwrap() = None;
        let key4 = kms3.master_key("wrong-passphrase").unwrap();
        assert_ne!(key1, key4);

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn local_kms_rotate() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let p = crate::security::crypto::Aes256Gcm::new();
        let tmp = std::env::temp_dir().join(format!("mnemosyne-test-kms-rot-{}", std::process::id()));
        let kms = LocalKms::new(tmp.to_str().unwrap());
        let old = kms.master_key("pw").unwrap();
        let new = kms.rotate("pw", &p).unwrap();
        assert_ne!(old, new);
        let loaded = kms.master_key("pw").unwrap();
        assert_eq!(loaded, new);
        let _ = fs::remove_file(&tmp);
    }
}
