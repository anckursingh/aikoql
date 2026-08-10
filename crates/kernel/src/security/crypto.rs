//! Crypto Provider trait + AES-256-GCM + ChaCha20-Poly1305 — MRFC-0020 Phase 1+5.
//!
//! Pluggable encryption: the `CryptoProvider` trait defines the contract;
//! `Aes256Gcm` is the primary provider. `ChaCha20Poly1305` is the secondary
//! provider for crypto agility and PQC transition readiness (MRFC-0020 §Crypto Agility).

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm as AesImpl, Nonce,
};
use chacha20poly1305::ChaCha20Poly1305 as ChaChaImpl;
use rand::RngCore;
use std::sync::RwLock;

// ---------------------------------------------------------------------------
// CryptoProvider trait
// ---------------------------------------------------------------------------

/// Pluggable symmetric encryption provider. Kernel defines the contract;
/// implementations provide the algorithm (AES-256-GCM, ChaCha20-Poly1305).
pub trait CryptoProvider: Send + Sync {
    /// Encrypt `plaintext` with `key` and `aad` (additional authenticated data).
    /// Returns `nonce || ciphertext || tag`. The nonce is 12 bytes for AES-GCM.
    fn encrypt(&self, key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String>;

    /// Decrypt `ciphertext` (format: `nonce || ciphertext || tag`) with `key`
    /// and `aad`. Returns plaintext or error on authentication failure.
    fn decrypt(&self, key: &[u8; 32], ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String>;

    /// Generate a new random 256-bit key.
    fn generate_key(&self) -> [u8; 32];

    /// Algorithm name for audit logging.
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Value encryption format — MRFC-0020 §Page Format
// ---------------------------------------------------------------------------

/// Magic version byte prepended to every encrypted value.
/// Layout: `version(1) || nonce(12) || ciphertext || tag(16)`.
/// 0x01 = AES-256-GCM, 0x02 = ChaCha20-Poly1305.
const CRYPTO_VERSION_AES: u8 = 0x01;
const CRYPTO_VERSION_CHACHA: u8 = 0x02;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16; // AEAD authentication tag (both algorithms)

/// Wrap raw `nonce || ciphertext || tag` with a version header.
fn wrap(version: u8, raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + raw.len());
    out.push(version);
    out.extend_from_slice(raw);
    out
}

/// Strip the version header. Returns `Some(version, payload)` on success,
/// or `None` if the version is unsupported or data is truncated.
fn unwrap(data: &[u8]) -> Option<(u8, &[u8])> {
    if data.len() < 1 + NONCE_LEN + TAG_LEN {
        return None;
    }
    match data[0] {
        CRYPTO_VERSION_AES | CRYPTO_VERSION_CHACHA => Some((data[0], &data[1..])),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Aes256Gcm — primary provider
// ---------------------------------------------------------------------------

pub struct Aes256Gcm {
    /// Cache: (last_key, cipher) to avoid per-call key expansion.
    cache: RwLock<Option<([u8; 32], AesImpl)>>,
}

impl Default for Aes256Gcm {
    fn default() -> Self {
        Self::new()
    }
}

impl Aes256Gcm {
    pub fn new() -> Self {
        Aes256Gcm {
            cache: RwLock::new(None),
        }
    }

    fn get_cipher(&self, key: &[u8; 32]) -> Result<AesImpl, String> {
        // Fast path: cached key matches.
        if let Some((cached_key, ref cached_cipher)) = *self.cache.read().unwrap() {
            if &cached_key == key {
                return Ok(cached_cipher.clone());
            }
        }
        // Slow path: compute key expansion once per key.
        let cipher = AesImpl::new_from_slice(key).map_err(|e| format!("aes-gcm init: {}", e))?;
        *self.cache.write().unwrap() = Some((*key, cipher.clone()));
        Ok(cipher)
    }
}

impl CryptoProvider for Aes256Gcm {
    fn encrypt(&self, key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let cipher = self.get_cipher(key)?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let raw = cipher
            .encrypt(
                nonce,
                aes_gcm::aead::Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|e| format!("aes-gcm encrypt: {}", e))?;
        let mut combined = Vec::with_capacity(NONCE_LEN + raw.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&raw);
        Ok(wrap(CRYPTO_VERSION_AES, &combined))
    }

    fn decrypt(&self, key: &[u8; 32], data: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let (version, raw) = unwrap(data).ok_or("unsupported crypto version or truncated data")?;
        if version != CRYPTO_VERSION_AES {
            return Err(format!(
                "expected AES-GCM version 0x{:02x}, got 0x{:02x}",
                CRYPTO_VERSION_AES, version
            ));
        }
        if raw.len() < NONCE_LEN + TAG_LEN {
            return Err("ciphertext too short".into());
        }
        let nonce = Nonce::from_slice(&raw[..NONCE_LEN]);
        let ciphertext_and_tag = &raw[NONCE_LEN..];
        let cipher = self.get_cipher(key)?;
        cipher
            .decrypt(
                nonce,
                aes_gcm::aead::Payload {
                    msg: ciphertext_and_tag,
                    aad,
                },
            )
            .map_err(|e| format!("aes-gcm decrypt: {}", e))
    }

    fn generate_key(&self) -> [u8; 32] {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        key
    }

    fn name(&self) -> &str {
        "AES-256-GCM"
    }
}

// ---------------------------------------------------------------------------
// ChaCha20Poly1305 — secondary provider (MRFC-0020 Phase 5)
// ---------------------------------------------------------------------------

/// ChaCha20-Poly1305 AEAD provider. Same 256-bit key + 12-byte nonce as
/// AES-GCM, but uses the ChaCha20 stream cipher with Poly1305 MAC.
/// Useful for platforms without AES-NI and for crypto agility.
pub struct ChaCha20Poly1305 {
    cache: RwLock<Option<([u8; 32], ChaChaImpl)>>,
}

impl Default for ChaCha20Poly1305 {
    fn default() -> Self {
        Self::new()
    }
}

impl ChaCha20Poly1305 {
    pub fn new() -> Self {
        ChaCha20Poly1305 {
            cache: RwLock::new(None),
        }
    }

    fn get_cipher(&self, key: &[u8; 32]) -> Result<ChaChaImpl, String> {
        if let Some((cached_key, ref cached_cipher)) = *self.cache.read().unwrap() {
            if &cached_key == key {
                return Ok(cached_cipher.clone());
            }
        }
        let cipher = ChaChaImpl::new_from_slice(key)
            .map_err(|e| format!("chacha20-poly1305 init: {}", e))?;
        *self.cache.write().unwrap() = Some((*key, cipher.clone()));
        Ok(cipher)
    }
}

impl CryptoProvider for ChaCha20Poly1305 {
    fn encrypt(&self, key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let cipher = self.get_cipher(key)?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = chacha20poly1305::Nonce::from_slice(&nonce_bytes);
        let raw = cipher
            .encrypt(
                nonce,
                chacha20poly1305::aead::Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|e| format!("chacha20-poly1305 encrypt: {}", e))?;
        let mut combined = Vec::with_capacity(NONCE_LEN + raw.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&raw);
        Ok(wrap(CRYPTO_VERSION_CHACHA, &combined))
    }

    fn decrypt(&self, key: &[u8; 32], data: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let (version, raw) = unwrap(data).ok_or("unsupported crypto version or truncated data")?;
        if version != CRYPTO_VERSION_CHACHA {
            return Err(format!(
                "expected ChaCha20-Poly1305 version 0x{:02x}, got 0x{:02x}",
                CRYPTO_VERSION_CHACHA, version
            ));
        }
        if raw.len() < NONCE_LEN + TAG_LEN {
            return Err("ciphertext too short".into());
        }
        let nonce = chacha20poly1305::Nonce::from_slice(&raw[..NONCE_LEN]);
        let ciphertext_and_tag = &raw[NONCE_LEN..];
        let cipher = self.get_cipher(key)?;
        cipher
            .decrypt(
                nonce,
                chacha20poly1305::aead::Payload {
                    msg: ciphertext_and_tag,
                    aad,
                },
            )
            .map_err(|e| format!("chacha20-poly1305 decrypt: {}", e))
    }

    fn generate_key(&self) -> [u8; 32] {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        key
    }

    fn name(&self) -> &str {
        "ChaCha20-Poly1305"
    }
}

// ---------------------------------------------------------------------------
// Thread-safe provider holder
// ---------------------------------------------------------------------------

/// Holds the active `CryptoProvider` behind a lock for runtime algorithm
/// switching (MRFC-0020 §Crypto Agility).
pub struct Crypto {
    provider: RwLock<Box<dyn CryptoProvider>>,
}

impl Crypto {
    pub fn new(provider: Box<dyn CryptoProvider>) -> Self {
        Crypto {
            provider: RwLock::new(provider),
        }
    }

    pub fn encrypt(&self, key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        self.provider.read().unwrap().encrypt(key, plaintext, aad)
    }

    pub fn decrypt(&self, key: &[u8; 32], data: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        self.provider.read().unwrap().decrypt(key, data, aad)
    }

    pub fn generate_key(&self) -> [u8; 32] {
        self.provider.read().unwrap().generate_key()
    }

    /// Access the underlying provider (for passing to KMS rotation, etc.).
    pub fn inner(&self) -> &RwLock<Box<dyn CryptoProvider>> {
        &self.provider
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let p = Aes256Gcm::new();
        let key = p.generate_key();
        let plaintext = b"hello world";
        let aad = b"my-key-001";
        let ct = p.encrypt(&key, plaintext, aad).unwrap();
        // Ciphertext should be different from plaintext and contain version byte.
        assert_ne!(&ct, plaintext);
        assert_eq!(ct[0], CRYPTO_VERSION_AES);
        let pt = p.decrypt(&key, &ct, aad).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn tampered_ciphertext_fails_aead() {
        let p = Aes256Gcm::new();
        let key = p.generate_key();
        let mut ct = p.encrypt(&key, b"secret", b"k").unwrap();
        // Flip a bit in the ciphertext (after nonce).
        ct[14] ^= 1;
        assert!(p.decrypt(&key, &ct, b"k").is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let p = Aes256Gcm::new();
        let k1 = p.generate_key();
        let k2 = p.generate_key();
        let ct = p.encrypt(&k1, b"secret", b"k").unwrap();
        assert!(p.decrypt(&k2, &ct, b"k").is_err());
    }

    #[test]
    fn wrong_aad_fails() {
        let p = Aes256Gcm::new();
        let key = p.generate_key();
        let ct = p.encrypt(&key, b"secret", b"key-a").unwrap();
        assert!(p.decrypt(&key, &ct, b"key-b").is_err());
    }

    #[test]
    fn generate_key_is_unique() {
        let p = Aes256Gcm::new();
        let k1 = p.generate_key();
        let k2 = p.generate_key();
        assert_ne!(k1, k2);
    }

    #[test]
    fn truncated_data_fails() {
        let p = Aes256Gcm::new();
        let key = p.generate_key();
        assert!(p.decrypt(&key, &[0x01, 0x00], b"k").is_err());
    }

    #[test]
    fn crypto_wrapper_delegates() {
        let c = Crypto::new(Box::new(Aes256Gcm::new()));
        let key = c.generate_key();
        let ct = c.encrypt(&key, b"test", b"aad").unwrap();
        let pt = c.decrypt(&key, &ct, b"aad").unwrap();
        assert_eq!(pt, b"test");
    }

    // -- ChaCha20-Poly1305 tests (MRFC-0020 Phase 5) ----------------------

    #[test]
    fn chacha_encrypt_decrypt_roundtrip() {
        let p = ChaCha20Poly1305::new();
        let key = p.generate_key();
        let pt = b"hello from chacha";
        let ct = p.encrypt(&key, pt, b"key-001").unwrap();
        assert_eq!(ct[0], CRYPTO_VERSION_CHACHA);
        let decrypted = p.decrypt(&key, &ct, b"key-001").unwrap();
        assert_eq!(decrypted, pt);
    }

    #[test]
    fn chacha_tampered_ciphertext_fails() {
        let p = ChaCha20Poly1305::new();
        let key = p.generate_key();
        let mut ct = p.encrypt(&key, b"secret", b"k").unwrap();
        ct[15] ^= 1; // flip a bit after version+nonce
        assert!(p.decrypt(&key, &ct, b"k").is_err());
    }

    #[test]
    fn chacha_wrong_key_fails() {
        let p = ChaCha20Poly1305::new();
        let k1 = p.generate_key();
        let k2 = p.generate_key();
        let ct = p.encrypt(&k1, b"data", b"aad").unwrap();
        assert!(p.decrypt(&k2, &ct, b"aad").is_err());
    }

    #[test]
    fn chacha_wrong_aad_fails() {
        let p = ChaCha20Poly1305::new();
        let key = p.generate_key();
        let ct = p.encrypt(&key, b"data", b"aad-1").unwrap();
        assert!(p.decrypt(&key, &ct, b"aad-2").is_err());
    }

    #[test]
    fn cross_provider_rejects_wrong_version() {
        let aes = Aes256Gcm::new();
        let chacha = ChaCha20Poly1305::new();
        let key = aes.generate_key();
        // AES encrypt, try ChaCha decrypt → version mismatch.
        let ct = aes.encrypt(&key, b"data", b"aad").unwrap();
        assert!(chacha.decrypt(&key, &ct, b"aad").is_err());
        // ChaCha encrypt, try AES decrypt → version mismatch.
        let ct2 = chacha.encrypt(&key, b"data", b"aad").unwrap();
        assert!(aes.decrypt(&key, &ct2, b"aad").is_err());
    }

    #[test]
    fn crypto_wrapper_supports_chacha() {
        let c = Crypto::new(Box::new(ChaCha20Poly1305::new()));
        let key = c.generate_key();
        let ct = c.encrypt(&key, b"test", b"aad").unwrap();
        assert_eq!(ct[0], CRYPTO_VERSION_CHACHA);
        let pt = c.decrypt(&key, &ct, b"aad").unwrap();
        assert_eq!(pt, b"test");
    }
}
