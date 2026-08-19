//! Local Key Management Service — MRFC-0020 Phase 1, hardened 2026-08-10.
//!
//! File-backed master key for single-node deployments. The master key is
//! stored encrypted with a passphrase-derived key (Argon2id → ChaCha20-Poly1305).
//! Cloud KMS plugins (AWS, Azure, GCP, Vault) implement the same trait.
//!
//! # Envelope format
//!
//! **v2 (current):** `version(1) || kdf(1) || params(9) || salt(16) || aead(1) || nonce(12) || ct(32) || tag(16)` = 88 bytes
//! **v1 (legacy, 48 bytes):** `salt(16) || xor_wrapped(32)` — auto-migrated on read.

use crate::security::crypto::CryptoProvider;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305,
};
use rand::RngCore;
use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// Envelope constants
// ---------------------------------------------------------------------------

/// Current envelope version.
const ENVELOPE_VERSION: u8 = 0x02;

/// KDF algorithm identifiers.
const KDF_ARGON2ID: u8 = 0x01;

/// AEAD algorithm identifiers.
const AEAD_CHACHA20_POLY1305: u8 = 0x01;

/// Argon2id parameters (OWASP recommended minimums).
const ARGON2_MEMORY_KB: u32 = 65536; // 64 MiB
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_PARALLELISM: u8 = 4;

/// Sizes for the v2 envelope layout.
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12; // ChaCha20-Poly1305 nonce
const TAG_LEN: usize = 16; // Poly1305 authentication tag
const MASTER_KEY_LEN: usize = 32;
const V2_ENVELOPE_LEN: usize = 1 + 1 + 9 + SALT_LEN + 1 + NONCE_LEN + MASTER_KEY_LEN + TAG_LEN; // 88 bytes
const V1_ENVELOPE_LEN: usize = 48; // salt(16) || xor_wrapped(32)

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
/// `passphrase` via Argon2id. The derived key then wraps the master key
/// with ChaCha20-Poly1305 AEAD so that a wrong passphrase produces an
/// explicit authentication failure rather than silent garbage decryption.
pub struct LocalKms {
    path: String,
    cached_key: std::sync::RwLock<Option<[u8; 32]>>,
}

impl LocalKms {
    pub fn new(path: impl Into<String>) -> Self {
        LocalKms {
            path: path.into(),
            cached_key: std::sync::RwLock::new(None),
        }
    }
}

impl KeyManager for LocalKms {
    fn master_key(&self, passphrase: &str) -> Result<[u8; 32], String> {
        // Return cached key if available.
        // justified: RwLock poison is unrecoverable
        if let Some(k) = *self.cached_key.read().unwrap() {
            return Ok(k);
        }
        let path = Path::new(&self.path);
        if path.exists() {
            let raw =
                fs::read(path).map_err(|e| format!("read master key file {}: {}", self.path, e))?;

            // Detect and migrate legacy v1 format (48 bytes: salt || xor-wrapped).
            if raw.len() == V1_ENVELOPE_LEN {
                return self.migrate_from_v1(&raw, passphrase);
            }

            // v2 format: parse and decrypt.
            if raw.len() != V2_ENVELOPE_LEN {
                return Err(format!(
                    "master key file corrupted: expected {} or {} bytes, got {}",
                    V1_ENVELOPE_LEN,
                    V2_ENVELOPE_LEN,
                    raw.len()
                ));
            }
            let key = decrypt_v2_envelope(&raw, passphrase)?;
            // justified: RwLock poison is unrecoverable
            *self.cached_key.write().unwrap() = Some(key);
            Ok(key)
        } else {
            // First run: generate a new master key and persist in v2 format.
            let mut key = [0u8; MASTER_KEY_LEN];
            rand::thread_rng().fill_bytes(&mut key);
            let envelope = encrypt_v2_envelope(&key, passphrase)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("create kms dir: {}", e))?;
            }
            fs::write(path, &envelope).map_err(|e| format!("write master key: {}", e))?;
            // justified: RwLock poison is unrecoverable
            *self.cached_key.write().unwrap() = Some(key);
            Ok(key)
        }
    }

    fn rotate(&self, passphrase: &str, provider: &dyn CryptoProvider) -> Result<[u8; 32], String> {
        let new_key = provider.generate_key();
        let envelope = encrypt_v2_envelope(&new_key, passphrase)?;
        fs::write(&self.path, &envelope).map_err(|e| format!("write rotated key: {}", e))?;
        // justified: RwLock poison is unrecoverable
        *self.cached_key.write().unwrap() = Some(new_key);
        Ok(new_key)
    }
}

impl LocalKms {
    /// Read a legacy v1 envelope (48 bytes: salt || xor-wrapped), decrypt
    /// with the old scheme, then immediately re-encrypt in v2 format.
    fn migrate_from_v1(&self, raw: &[u8], passphrase: &str) -> Result<[u8; 32], String> {
        // justified: length checked by caller (raw.len() == V1_ENVELOPE_LEN = 48)
        let salt: [u8; SALT_LEN] = raw[..16].try_into().unwrap();
        let wrapped: [u8; MASTER_KEY_LEN] = raw[16..48].try_into().unwrap();
        let derived = derive_key_v1(passphrase, &salt);
        let key = xor_unwrap(&derived, &wrapped);

        // Re-encrypt in v2 format and persist.
        let envelope = encrypt_v2_envelope(&key, passphrase)?;
        fs::write(&self.path, &envelope).map_err(|e| format!("migrate v1→v2: {}", e))?;
        *self.cached_key.write().unwrap() = Some(key);
        Ok(key)
    }
}

// ---------------------------------------------------------------------------
// v2 envelope: Argon2id → ChaCha20-Poly1305 AEAD wrap
// ---------------------------------------------------------------------------

/// Serialize the KDF parameters block: `memory_kb(4 LE) || iterations(4 LE) || parallelism(1)`.
fn encode_kdf_params(mem: u32, iters: u32, par: u8) -> [u8; 9] {
    let mut buf = [0u8; 9];
    buf[0..4].copy_from_slice(&mem.to_le_bytes());
    buf[4..8].copy_from_slice(&iters.to_le_bytes());
    buf[8] = par;
    buf
}

/// Parse the KDF parameters block.
fn decode_kdf_params(buf: &[u8; 9]) -> (u32, u32, u8) {
    let mem = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let iters = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let par = buf[8];
    (mem, iters, par)
}

/// Build a v2 envelope: `version || kdf_id || kdf_params || salt || aead_id || nonce || ct || tag`.
fn encrypt_v2_envelope(key: &[u8; MASTER_KEY_LEN], passphrase: &str) -> Result<Vec<u8>, String> {
    let mut salt = [0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);

    let derived = derive_key_argon2id(
        passphrase,
        &salt,
        ARGON2_MEMORY_KB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
    )?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = chacha20poly1305::Nonce::from_slice(&nonce_bytes);

    let cipher = ChaCha20Poly1305::new_from_slice(&derived)
        .map_err(|e| format!("chacha20-poly1305 init: {}", e))?;
    let ct_and_tag = cipher
        .encrypt(nonce, key.as_slice())
        .map_err(|e| format!("chacha20-poly1305 encrypt: {}", e))?;

    let kdf_params = encode_kdf_params(ARGON2_MEMORY_KB, ARGON2_ITERATIONS, ARGON2_PARALLELISM);

    let mut envelope = Vec::with_capacity(V2_ENVELOPE_LEN);
    envelope.push(ENVELOPE_VERSION);
    envelope.push(KDF_ARGON2ID);
    envelope.extend_from_slice(&kdf_params);
    envelope.extend_from_slice(&salt);
    envelope.push(AEAD_CHACHA20_POLY1305);
    envelope.extend_from_slice(&nonce_bytes);
    envelope.extend_from_slice(&ct_and_tag); // 48 bytes: ct(32) || tag(16)

    Ok(envelope)
}

/// Parse a v2 envelope and recover the master key. Returns `InvalidPassphrase`
/// on AEAD authentication failure (wrong passphrase or tampered ciphertext).
fn decrypt_v2_envelope(raw: &[u8], passphrase: &str) -> Result<[u8; MASTER_KEY_LEN], String> {
    if raw.len() != V2_ENVELOPE_LEN {
        return Err(format!(
            "v2 envelope must be {} bytes, got {}",
            V2_ENVELOPE_LEN,
            raw.len()
        ));
    }

    let version = raw[0];
    if version != ENVELOPE_VERSION {
        return Err(format!("unsupported envelope version 0x{:02x}", version));
    }

    let kdf_id = raw[1];
    if kdf_id != KDF_ARGON2ID {
        return Err(format!("unsupported KDF 0x{:02x}", kdf_id));
    }

    // justified: length checked above (raw.len() != V2_ENVELOPE_LEN → Err)
    let kdf_params: [u8; 9] = raw[2..11].try_into().unwrap();
    let (mem, iters, par) = decode_kdf_params(&kdf_params);

    // justified: length checked above (raw.len() != V2_ENVELOPE_LEN → Err)
    let salt: [u8; SALT_LEN] = raw[11..27].try_into().unwrap();

    let aead_id = raw[27];
    if aead_id != AEAD_CHACHA20_POLY1305 {
        return Err(format!("unsupported AEAD 0x{:02x}", aead_id));
    }

    // justified: length checked above (raw.len() != V2_ENVELOPE_LEN → Err)
    let nonce_bytes: [u8; NONCE_LEN] = raw[28..40].try_into().unwrap();
    let nonce = chacha20poly1305::Nonce::from_slice(&nonce_bytes);

    let ct_and_tag = &raw[40..88]; // 48 bytes

    let derived = derive_key_argon2id(passphrase, &salt, mem, iters, par)?;

    let cipher = ChaCha20Poly1305::new_from_slice(&derived)
        .map_err(|e| format!("chacha20-poly1305 init: {}", e))?;
    let plaintext = cipher.decrypt(nonce, ct_and_tag).map_err(|_| {
        "InvalidPassphrase: authentication failed — wrong passphrase or corrupted key file"
            .to_string()
    })?;

    if plaintext.len() != MASTER_KEY_LEN {
        return Err(format!(
            "decrypted master key must be {} bytes, got {}",
            MASTER_KEY_LEN,
            plaintext.len()
        ));
    }
    let mut key = [0u8; MASTER_KEY_LEN];
    key.copy_from_slice(&plaintext);
    Ok(key)
}

// ---------------------------------------------------------------------------
// Argon2id key derivation
// ---------------------------------------------------------------------------

/// Derive a 32-byte key-encryption key from a passphrase and salt using
/// Argon2id (memory-hard, side-channel resistant).
fn derive_key_argon2id(
    passphrase: &str,
    salt: &[u8; SALT_LEN],
    memory_kb: u32,
    iterations: u32,
    parallelism: u8,
) -> Result<[u8; 32], String> {
    let params = argon2::Params::new(memory_kb, iterations, parallelism as u32, Some(32))
        .map_err(|e| format!("argon2 params: {}", e))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut dk = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut dk)
        .map_err(|e| format!("argon2id hash: {}", e))?;
    Ok(dk)
}

// ---------------------------------------------------------------------------
// Legacy v1 KDF + unwrap (for migration only)
// ---------------------------------------------------------------------------

/// Legacy PBKDF2-SHA256-style KDF (iterated SHA-256, 100K rounds).
/// Only used to read v1 envelopes during migration; new writes use Argon2id.
fn derive_key_v1(passphrase: &str, salt: &[u8; 16]) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(salt);
    hasher.update(passphrase.as_bytes());
    let mut hash = hasher.finalize_reset();
    for _ in 0..100_000 {
        hasher.update(hash);
        hasher.update(salt);
        hasher.update(passphrase.as_bytes());
        hash = hasher.finalize_reset();
    }
    let mut dk = [0u8; 32];
    dk.copy_from_slice(&hash[..32]);
    dk
}

/// Legacy XOR key wrapping. Bijection — used for both wrap and unwrap.
fn xor_unwrap(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = a[i] ^ b[i];
    }
    out
}

// ---------------------------------------------------------------------------
// Cloud KMS stubs — these implement the KeyManager trait for cloud providers.
// Full SDK integration (aws-sdk-kms, azure_security_keyvault, google-cloud-kms)
// deferred. Stubs accept a key_uri and return the key for MVP.
// ---------------------------------------------------------------------------

/// AWS KMS stub. Set AIKOQL_AWS_KMS_KEY=<hex-key> for MVP.
pub struct AwsKms {
    pub key_id: String,
    cached_key: std::sync::RwLock<Option<[u8; 32]>>,
}

impl AwsKms {
    pub fn new(key_id: impl Into<String>) -> Self {
        AwsKms {
            key_id: key_id.into(),
            cached_key: std::sync::RwLock::new(None),
        }
    }
}

impl KeyManager for AwsKms {
    fn master_key(&self, _passphrase: &str) -> Result<[u8; 32], String> {
        // justified: RwLock poison is unrecoverable
        if let Some(k) = *self.cached_key.read().unwrap() {
            return Ok(k);
        }
        if let Ok(hex) = std::env::var("AIKOQL_AWS_KMS_KEY") {
            let bytes = hex_decode(hex.trim()).map_err(|e| format!("invalid hex key: {}", e))?;
            if bytes.len() != 32 {
                return Err("AWS KMS key must be 32 bytes (64 hex chars)".into());
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            // justified: RwLock poison is unrecoverable
            *self.cached_key.write().unwrap() = Some(key);
            Ok(key)
        } else {
            Err("AWS KMS not configured. Set AIKOQL_AWS_KMS_KEY env var or use LocalKms.".into())
        }
    }

    fn rotate(&self, _passphrase: &str, provider: &dyn CryptoProvider) -> Result<[u8; 32], String> {
        let new_key = provider.generate_key();
        // justified: RwLock poison is unrecoverable
        *self.cached_key.write().unwrap() = Some(new_key);
        Ok(new_key)
    }
}

/// Azure Key Vault stub. Set AIKOQL_AZURE_KV_KEY=<hex-key> for MVP.
pub struct AzureKeyVault {
    pub vault_url: String,
    pub key_name: String,
    cached_key: std::sync::RwLock<Option<[u8; 32]>>,
}

impl AzureKeyVault {
    pub fn new(vault_url: impl Into<String>, key_name: impl Into<String>) -> Self {
        AzureKeyVault {
            vault_url: vault_url.into(),
            key_name: key_name.into(),
            cached_key: std::sync::RwLock::new(None),
        }
    }
}

impl KeyManager for AzureKeyVault {
    fn master_key(&self, _passphrase: &str) -> Result<[u8; 32], String> {
        // justified: RwLock poison is unrecoverable
        if let Some(k) = *self.cached_key.read().unwrap() {
            return Ok(k);
        }
        if let Ok(hex) = std::env::var("AIKOQL_AZURE_KV_KEY") {
            let bytes = hex_decode(hex.trim()).map_err(|e| format!("invalid hex key: {}", e))?;
            if bytes.len() != 32 {
                return Err("Azure KV key must be 32 bytes".into());
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            // justified: RwLock poison is unrecoverable
            *self.cached_key.write().unwrap() = Some(key);
            Ok(key)
        } else {
            Err(
                "Azure Key Vault not configured. Set AIKOQL_AZURE_KV_KEY env var or use LocalKms."
                    .into(),
            )
        }
    }

    fn rotate(&self, _passphrase: &str, provider: &dyn CryptoProvider) -> Result<[u8; 32], String> {
        let new_key = provider.generate_key();
        // justified: RwLock poison is unrecoverable
        *self.cached_key.write().unwrap() = Some(new_key);
        Ok(new_key)
    }
}

/// GCP Cloud KMS stub. Set AIKOQL_GCP_KMS_KEY=<hex-key> for MVP.
pub struct GcpKeyManager {
    pub project_id: String,
    pub location: String,
    pub key_ring: String,
    pub key_name: String,
    cached_key: std::sync::RwLock<Option<[u8; 32]>>,
}

impl GcpKeyManager {
    pub fn new(
        project: impl Into<String>,
        location: impl Into<String>,
        key_ring: impl Into<String>,
        key_name: impl Into<String>,
    ) -> Self {
        GcpKeyManager {
            project_id: project.into(),
            location: location.into(),
            key_ring: key_ring.into(),
            key_name: key_name.into(),
            cached_key: std::sync::RwLock::new(None),
        }
    }
}

impl KeyManager for GcpKeyManager {
    fn master_key(&self, _passphrase: &str) -> Result<[u8; 32], String> {
        // justified: RwLock poison is unrecoverable
        if let Some(k) = *self.cached_key.read().unwrap() {
            return Ok(k);
        }
        if let Ok(hex) = std::env::var("AIKOQL_GCP_KMS_KEY") {
            let bytes = hex_decode(hex.trim()).map_err(|e| format!("invalid hex key: {}", e))?;
            if bytes.len() != 32 {
                return Err("GCP KMS key must be 32 bytes".into());
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            // justified: RwLock poison is unrecoverable
            *self.cached_key.write().unwrap() = Some(key);
            Ok(key)
        } else {
            Err(
                "GCP Cloud KMS not configured. Set AIKOQL_GCP_KMS_KEY env var or use LocalKms."
                    .into(),
            )
        }
    }

    fn rotate(&self, _passphrase: &str, provider: &dyn CryptoProvider) -> Result<[u8; 32], String> {
        let new_key = provider.generate_key();
        // justified: RwLock poison is unrecoverable
        *self.cached_key.write().unwrap() = Some(new_key);
        Ok(new_key)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decode a hex string into bytes. Returns an error on non-hex characters
/// or odd-length strings.
fn hex_decode(hex: &str) -> Result<Vec<u8>, String> {
    let hex = hex.trim();
    if !hex.len().is_multiple_of(2) {
        return Err("hex string has odd length".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| format!("invalid hex: {}", e)))
        .collect()
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

    // -----------------------------------------------------------------------
    // v2 envelope: correct passphrase, wrong passphrase, tampering
    // -----------------------------------------------------------------------

    #[test]
    fn v2_correct_passphrase_roundtrip() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let envelope = encrypt_v2_envelope(&key, "hunter2").unwrap();
        assert_eq!(envelope.len(), V2_ENVELOPE_LEN);
        assert_eq!(envelope[0], ENVELOPE_VERSION);
        let recovered = decrypt_v2_envelope(&envelope, "hunter2").unwrap();
        assert_eq!(key, recovered);
    }

    #[test]
    fn v2_wrong_passphrase_is_auth_error() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let envelope = encrypt_v2_envelope(&key, "correct-password").unwrap();
        let err = decrypt_v2_envelope(&envelope, "wrong-password").unwrap_err();
        assert!(
            err.contains("InvalidPassphrase"),
            "expected InvalidPassphrase, got: {}",
            err
        );
    }

    #[test]
    fn v2_tampered_ciphertext_detected() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let mut envelope = encrypt_v2_envelope(&key, "pw").unwrap();
        // Flip a bit in the ciphertext portion (byte 40 = first byte after nonce).
        envelope[40] ^= 0x01;
        let err = decrypt_v2_envelope(&envelope, "pw").unwrap_err();
        assert!(
            err.contains("InvalidPassphrase") || err.contains("authentication"),
            "expected auth error for tampered ct, got: {}",
            err
        );
    }

    #[test]
    fn v2_tampered_nonce_detected() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let mut envelope = encrypt_v2_envelope(&key, "pw").unwrap();
        // Flip a bit in the nonce (byte 28).
        envelope[28] ^= 0x01;
        let err = decrypt_v2_envelope(&envelope, "pw").unwrap_err();
        assert!(
            err.contains("InvalidPassphrase") || err.contains("authentication"),
            "expected auth error for tampered nonce, got: {}",
            err
        );
    }

    #[test]
    fn v2_tampered_salt_detected() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let mut envelope = encrypt_v2_envelope(&key, "pw").unwrap();
        // Flip a bit in the salt (byte 11).
        envelope[11] ^= 0x01;
        let err = decrypt_v2_envelope(&envelope, "pw").unwrap_err();
        assert!(
            err.contains("InvalidPassphrase") || err.contains("authentication"),
            "expected auth error for tampered salt, got: {}",
            err
        );
    }

    #[test]
    fn v2_tampered_metadata_detected() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let mut envelope = encrypt_v2_envelope(&key, "pw").unwrap();
        // Flip a bit in the KDF params header (byte 2).
        envelope[2] ^= 0x01;
        let err = decrypt_v2_envelope(&envelope, "pw").unwrap_err();
        assert!(
            err.contains("InvalidPassphrase") || err.contains("authentication"),
            "expected auth error for tampered metadata, got: {}",
            err
        );
    }

    #[test]
    fn v2_independent_salts_produce_different_ciphertexts() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let e1 = encrypt_v2_envelope(&key, "pw").unwrap();
        let e2 = encrypt_v2_envelope(&key, "pw").unwrap();
        // Salt regions (bytes 11..27) must differ.
        assert_ne!(&e1[11..27], &e2[11..27]);
        // Ciphertext regions (bytes 40..88) must differ (different nonce + salt).
        assert_ne!(&e1[40..88], &e2[40..88]);
        // But both must decrypt to the same key.
        assert_eq!(
            decrypt_v2_envelope(&e1, "pw").unwrap(),
            decrypt_v2_envelope(&e2, "pw").unwrap()
        );
    }

    #[test]
    fn v2_truncated_envelope_rejected() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let envelope = encrypt_v2_envelope(&key, "pw").unwrap();
        assert!(decrypt_v2_envelope(&envelope[..44], "pw").is_err()); // truncate tag
        assert!(decrypt_v2_envelope(&envelope[..87], "pw").is_err()); // truncate one byte
    }

    // -----------------------------------------------------------------------
    // v2 key recovery is idempotent (different salt → same key ok)
    // -----------------------------------------------------------------------

    #[test]
    fn v2_idempotent_recovery() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let e1 = encrypt_v2_envelope(&key, "pw").unwrap();
        let e2 = encrypt_v2_envelope(&key, "pw").unwrap();
        let k1 = decrypt_v2_envelope(&e1, "pw").unwrap();
        let k2 = decrypt_v2_envelope(&e2, "pw").unwrap();
        assert_eq!(k1, key);
        assert_eq!(k2, key);
    }

    // -----------------------------------------------------------------------
    // Legacy v1 → v2 migration
    // -----------------------------------------------------------------------

    fn make_v1_envelope(key: &[u8; 32], passphrase: &str) -> Vec<u8> {
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        let derived = derive_key_v1(passphrase, &salt);
        let wrapped = xor_unwrap(&derived, key);
        let mut buf = Vec::with_capacity(48);
        buf.extend_from_slice(&salt);
        buf.extend_from_slice(&wrapped);
        buf
    }

    #[test]
    fn v1_migration_roundtrip() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("aikoql-test-kms-v1-{}", std::process::id()));

        // Simulate a v1 key file.
        let mut original_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut original_key);
        let v1 = make_v1_envelope(&original_key, "migrate-me");
        fs::write(&tmp, &v1).unwrap();

        // Open with LocalKms: should detect v1, decrypt, re-encrypt as v2.
        let kms = LocalKms::new(tmp.to_str().unwrap());
        let key = kms.master_key("migrate-me").unwrap();
        assert_eq!(key, original_key);

        // File on disk is now v2 format.
        let on_disk = fs::read(&tmp).unwrap();
        assert_eq!(on_disk.len(), V2_ENVELOPE_LEN);
        assert_eq!(on_disk[0], ENVELOPE_VERSION);

        // Re-open: cached key returned.
        let key2 = kms.master_key("migrate-me").unwrap();
        assert_eq!(key2, original_key);

        let _ = fs::remove_file(&tmp);
    }

    // -----------------------------------------------------------------------
    // LocalKms create + load + rotate (v2 format)
    // -----------------------------------------------------------------------

    #[test]
    fn local_kms_create_and_load() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("aikoql-test-kms-{}", std::process::id()));
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

        // Check file is v2 format.
        let on_disk = fs::read(&tmp).unwrap();
        assert_eq!(on_disk.len(), V2_ENVELOPE_LEN);
        assert_eq!(on_disk[0], ENVELOPE_VERSION);

        // Wrong passphrase → explicit auth error.
        let kms3 = LocalKms::new(tmp.to_str().unwrap());
        *kms3.cached_key.write().unwrap() = None; // force uncached read
        let err = kms3.master_key("wrong-passphrase").unwrap_err();
        assert!(
            err.contains("InvalidPassphrase"),
            "expected InvalidPassphrase, got: {}",
            err
        );

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn local_kms_rotate() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let p = crate::security::crypto::Aes256Gcm::new();
        let tmp = std::env::temp_dir().join(format!("aikoql-test-kms-rot-{}", std::process::id()));
        let kms = LocalKms::new(tmp.to_str().unwrap());
        let old = kms.master_key("pw").unwrap();
        let new = kms.rotate("pw", &p).unwrap();
        assert_ne!(old, new);
        let loaded = kms.master_key("pw").unwrap();
        assert_eq!(loaded, new);

        // Rotated file is still v2 format.
        let on_disk = fs::read(&tmp).unwrap();
        assert_eq!(on_disk.len(), V2_ENVELOPE_LEN);

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn local_kms_new_file_is_v2_format() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("aikoql-test-kms-new-{}", std::process::id()));
        let kms = LocalKms::new(tmp.to_str().unwrap());
        let _ = kms.master_key("fresh").unwrap();
        let on_disk = fs::read(&tmp).unwrap();
        assert_eq!(on_disk.len(), V2_ENVELOPE_LEN);
        assert_eq!(on_disk[0], ENVELOPE_VERSION);
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn corrupted_envelope_size_rejected() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("aikoql-test-kms-corr-{}", std::process::id()));
        // Write 50 bytes of garbage (not 48 for v1, not 88 for v2).
        fs::write(&tmp, [0u8; 50]).unwrap();
        let kms = LocalKms::new(tmp.to_str().unwrap());
        let err = kms.master_key("pw").unwrap_err();
        assert!(err.contains("corrupted"), "got: {}", err);
        let _ = fs::remove_file(&tmp);
    }
}
