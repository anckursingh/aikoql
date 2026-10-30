//! Ed25519 (RFC 8032) signing for audit checkpoints — Phase 5 enterprise.
//!
//! Asymmetric, so an audit proof is verified with the public key alone; the
//! private seed never leaves the key. Backed by `ring` (already linked
//! through rustls), checked against the RFC 8032 §7.1 vectors and against
//! an independent implementation (Python `cryptography`).

use crate::knowledge::kom::{KError, KResult};
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use std::sync::RwLock;

/// An Ed25519 keypair for signing audit checkpoints.
pub struct SigningKey {
    pair: Ed25519KeyPair,
}

impl SigningKey {
    /// A fresh keypair from the operating system's CSPRNG.
    pub fn generate() -> KResult<Self> {
        let mut seed = [0u8; 32];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut seed)
            .map_err(|_| KError::Store("ed25519: the OS random source failed".into()))?;
        Ok(Self::from_seed(seed))
    }

    /// The keypair of an RFC 8032 32-byte private seed.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        // justified: any 32 bytes are a valid Ed25519 seed, so this cannot fail
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("a 32-byte ed25519 seed");
        SigningKey { pair }
    }

    pub fn public_key(&self) -> [u8; 32] {
        let mut pk = [0u8; 32];
        pk.copy_from_slice(self.pair.public_key().as_ref());
        pk
    }

    /// The RFC 8032 signature of `message`.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let mut sig = [0u8; 64];
        sig.copy_from_slice(self.pair.sign(message).as_ref());
        sig
    }

    /// Verify a signature with only the signer's public key.
    pub fn verify(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
        UnparsedPublicKey::new(&ED25519, public)
            .verify(message, signature)
            .is_ok()
    }
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
