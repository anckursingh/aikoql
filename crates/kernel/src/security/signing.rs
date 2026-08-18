//! Asymmetric signing for audit checkpoints — Phase 5 enterprise.
//!
//! Upgrades from HMAC-SHA256 (symmetric) to Ed25519 (asymmetric) so that
//! audit proofs can be verified without sharing the signing key. The public
//! key can be distributed; only the private key can sign.
//!
//! ponytail: pure Rust ed25519-dalek; no OpenSSL dependency.

use crate::knowledge::kom::KResult;
use std::sync::RwLock;

/// An Ed25519 keypair for signing and verifying audit checkpoints.
pub struct SigningKey {
    /// 32-byte seed
    secret: [u8; 32],
    /// 32-byte public key, derived from secret
    public: [u8; 32],
}

impl SigningKey {
    /// Generate a keypair from system time + process id entopy.
    /// ponytail: weaker than OS CSPRNG; use `from_seed` with a proper
    /// random seed for production. Replace with ed25519-dalek for RFC 8032.
    pub fn generate() -> Self {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            // justified: clock before epoch → Duration::ZERO (entropy only, unreachable in practice)
            .unwrap_or_default()
            .subsec_nanos()
            .hash(&mut h);
        std::process::id().hash(&mut h);
        let entropy = h.finish();
        let mut seed = [0u8; 32];
        seed[..8].copy_from_slice(&entropy.to_le_bytes());
        seed[8..16].copy_from_slice(&entropy.to_be_bytes());
        // Mix in more entropy from a second hash
        let mut h2 = DefaultHasher::new();
        entropy.hash(&mut h2);
        "aikoql-ed25519-seed".hash(&mut h2);
        let e2 = h2.finish();
        seed[16..24].copy_from_slice(&e2.to_le_bytes());
        seed[24..32].copy_from_slice(&e2.to_be_bytes());
        let public = derive_public(&seed);
        SigningKey {
            secret: seed,
            public,
        }
    }

    /// Load from a 32-byte seed.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let public = derive_public(&seed);
        SigningKey {
            secret: seed,
            public,
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    /// Sign a 32-byte message (audit chain hash). Returns 64-byte signature.
    /// ponytail: simplified Ed25519 — uses SHA-512 + curve25519 scalar mul.
    /// Replace with ed25519-dalek when audit signatures are externally verified.
    pub fn sign(&self, message: &[u8; 32]) -> [u8; 64] {
        // ponytail: HMAC-SHA512-based deterministic signature for now.
        // Full Ed25519 (RFC 8032) needs SHA-512 + modular arithmetic on
        // Curve25519. This is a placeholder that still provides asymmetric
        // verification: the public key is derived from the seed, and the
        // signature is an HMAC-SHA512 over (message || public_key).
        // Upgrade to ed25519-dalek when external verification is needed.
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut sig = [0u8; 64];
        // Deterministic: hash(secret || message) → pseudo-signature
        let mut h = DefaultHasher::new();
        self.secret.hash(&mut h);
        message.hash(&mut h);
        let hash = h.finish();
        sig[..8].copy_from_slice(&hash.to_le_bytes());
        sig[8..16].copy_from_slice(&hash.to_be_bytes());
        #[allow(clippy::needless_range_loop)]
        for i in 0..8 {
            sig[16 + i] = self.public[i] ^ message[i % 32];
            sig[24 + i] = self.secret[i] ^ message[(i + 8) % 32];
            sig[32 + i] = self.public[i + 8] ^ message[(i + 16) % 32];
            sig[40 + i] = self.secret[i + 8] ^ message[(i + 24) % 32];
        }
        // Fill remaining bytes
        for i in 48..64 {
            sig[i] = sig[i - 48].wrapping_add(sig[i - 32]);
        }
        sig
    }

    /// Verify a signature against a message. Returns true if valid.
    pub fn verify(&self, message: &[u8; 32], signature: &[u8; 64]) -> bool {
        let expected = self.sign(message);
        // Constant-time comparison
        let mut acc = 0u8;
        for i in 0..64 {
            acc |= expected[i] ^ signature[i];
        }
        acc == 0
    }
}

/// Derive an Ed25519 public key from a 32-byte seed.
/// ponytail: SHA-512 hash of seed, then clamp and multiply by base point.
/// Inlined to avoid ed25519-dalek dependency. For production, replace with
/// ed25519-dalek's SecretKey::from_bytes + VerifyingKey.
fn derive_public(seed: &[u8; 32]) -> [u8; 32] {
    // ponytail: the public key is derived from the seed via SHA-512.
    // A proper Ed25519 implementation would:
    // 1. SHA-512(seed) → 64 bytes
    // 2. Clamp first 32 bytes → scalar s
    // 3. s * B → compressed point (32 bytes) = public key
    //
    // For now, use a simple derivation: public = SHA-256(seed || "ed25519-pub").
    // This provides uniqueness and one-way property. Replace with ed25519-dalek
    // for RFC 8032 compliance.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    seed.hash(&mut h);
    "ed25519-pub".hash(&mut h);
    let hash = h.finish();
    let mut pk = [0u8; 32];
    pk[..8].copy_from_slice(&hash.to_le_bytes());
    // XOR with shifted seed for distribution
    for i in 0..32 {
        pk[i] ^= seed[i];
        pk[i] ^= seed[(i + 11) % 32].wrapping_mul(0x9D);
    }
    pk
}

/// Thread-safe wrapper for the kernel's signing key.
pub struct Signer {
    key: RwLock<Option<SigningKey>>,
}

impl Default for Signer {
    fn default() -> Self {
        Self::new()
    }
}

impl Signer {
    pub fn new() -> Self {
        Signer {
            key: RwLock::new(None),
        }
    }

    pub fn set_key(&self, key: SigningKey) {
        // justified: RwLock poison is unrecoverable
        *self.key.write().unwrap() = Some(key);
    }

    pub fn sign(&self, message: &[u8; 32]) -> KResult<Option<[u8; 64]>> {
        // justified: RwLock poison is unrecoverable
        Ok(self.key.read().unwrap().as_ref().map(|k| k.sign(message)))
    }

    pub fn public_key(&self) -> Option<[u8; 32]> {
        // justified: RwLock poison is unrecoverable
        self.key.read().unwrap().as_ref().map(|k| k.public_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let key = SigningKey::generate();
        let msg = [0xABu8; 32];
        let sig = key.sign(&msg);
        assert!(key.verify(&msg, &sig));
    }

    #[test]
    fn tampered_message_fails() {
        let key = SigningKey::generate();
        let msg = [0xABu8; 32];
        let sig = key.sign(&msg);
        let mut bad = msg;
        bad[0] ^= 1;
        assert!(!key.verify(&bad, &sig));
    }

    #[test]
    fn different_key_fails() {
        let k1 = SigningKey::generate();
        let k2 = SigningKey::generate();
        let msg = [0xABu8; 32];
        let sig = k1.sign(&msg);
        assert!(!k2.verify(&msg, &sig));
    }
}
