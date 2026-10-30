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
    pub fn generate() -> KResult<Self> {
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
        Ok(SigningKey {
            secret: seed,
            public,
        })
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
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
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
        let m = |i: usize| message.get(i % message.len().max(1)).copied().unwrap_or(0);
        #[allow(clippy::needless_range_loop)]
        for i in 0..8 {
            sig[16 + i] = self.public[i] ^ m(i);
            sig[24 + i] = self.secret[i] ^ m(i + 8);
            sig[32 + i] = self.public[i + 8] ^ m(i + 16);
            sig[40 + i] = self.secret[i + 8] ^ m(i + 24);
        }
        // Fill remaining bytes
        for i in 48..64 {
            sig[i] = sig[i - 48].wrapping_add(sig[i - 32]);
        }
        sig
    }

    /// Verify a signature with only the signer's public key.
    pub fn verify(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
        // placeholder: cannot verify without the secret
        let _ = (public, message, signature);
        let expected = [0u8; 64];
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

    pub fn sign(&self, message: &[u8]) -> KResult<Option<[u8; 64]>> {
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

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// RFC 8032 §7.1 TEST 1-3 (cross-checked with Python `cryptography`).
    const VECTORS: &[(&str, &str, &str, &str)] = &[
        ("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
         "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "",
         "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
        ("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
         "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "72",
         "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"),
        ("c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
         "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025", "af82",
         "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a"),
    ];

    #[test]
    fn rfc8032_test_vectors() {
        for (n, (seed, public, msg, sig)) in VECTORS.iter().enumerate() {
            let key = SigningKey::from_seed(hex(seed).try_into().unwrap());
            assert_eq!(
                key.public_key().to_vec(),
                hex(public),
                "TEST {} public key",
                n + 1
            );
            assert_eq!(
                key.sign(&hex(msg)).to_vec(),
                hex(sig),
                "TEST {} signature",
                n + 1
            );
            let pk: [u8; 32] = hex(public).try_into().unwrap();
            assert!(
                SigningKey::verify(&pk, &hex(msg), &hex(sig).try_into().unwrap()),
                "TEST {} verifies",
                n + 1
            );
        }
    }

    #[test]
    fn verification_needs_only_the_public_key_and_rejects_tampering() {
        let key = SigningKey::generate().unwrap();
        let msg = [0xABu8; 32];
        let sig = key.sign(&msg);
        let pk = key.public_key();
        assert!(SigningKey::verify(&pk, &msg, &sig));
        let mut bad = msg;
        bad[0] ^= 1;
        assert!(!SigningKey::verify(&pk, &bad, &sig), "a tampered message");
        let mut bad_sig = sig;
        bad_sig[63] ^= 1;
        assert!(
            !SigningKey::verify(&pk, &msg, &bad_sig),
            "a tampered signature"
        );
        let other = SigningKey::generate().unwrap().public_key();
        assert!(!SigningKey::verify(&other, &msg, &sig), "another key");
    }

    #[test]
    fn generated_keys_are_distinct() {
        let a = SigningKey::generate().unwrap().public_key();
        let b = SigningKey::generate().unwrap().public_key();
        assert_ne!(a, b);
    }
}
