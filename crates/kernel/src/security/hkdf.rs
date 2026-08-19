//! HKDF-SHA256 key derivation (RFC 5869) for cryptographic domain separation.
//!
//! Every cipher in the hierarchy gets its own derived key so a single key is
//! never used for two purposes:
//!
//! ```text
//! KEK ──HKDF("aikoql/dek-wrap/v1")──> DEK wrap/unwrap key
//! KEK ──HKDF("aikoql/store/v1")─────> whole-store encryption key
//! DEK ──HKDF("aikoql/field/v1")─────> field-level encryption key
//! ```

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Domain labels. The `v1` suffix is the migration hook: a future key-usage
/// change switches to `…/v2` and existing data is re-derived via its version
/// metadata (see `CRYPTO_META_V1` in `envelope.rs`).
pub const DOMAIN_DEK_WRAP: &[u8] = b"aikoql/dek-wrap/v1";
pub const DOMAIN_STORE: &[u8] = b"aikoql/store/v1";
pub const DOMAIN_FIELD: &[u8] = b"aikoql/field/v1";

/// HKDF-SHA256 (RFC 5869): extract then expand to a 32-byte key.
/// `salt` is caller-controlled — production call sites use a zeroed salt
/// because their IKM is already a uniform 32-byte key (extraction provides
/// domain separation, not entropy stretching — RFC 5869 §3.3).
pub fn derive(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    // Extract: PRK = HMAC-SHA256(salt, IKM).
    let mut mac = <HmacSha256 as Mac>::new_from_slice(salt).expect("HMAC accepts any key length");
    mac.update(ikm);
    let prk = mac.finalize().into_bytes();

    // Expand: T(1) = HMAC-SHA256(PRK, info || 0x01) is all we need for 32 bytes.
    let mut t = <HmacSha256 as Mac>::new_from_slice(&prk).expect("HMAC accepts any key length");
    t.update(info);
    t.update(&[0x01]);
    let mut out = [0u8; 32];
    out.copy_from_slice(&t.finalize().into_bytes());
    out
}

/// Production shorthand: domain-separated subkey of a uniform 32-byte key
/// (zeroed salt).
pub fn domain_sep(ikm: &[u8], domain: &[u8]) -> [u8; 32] {
    derive(ikm, &[0u8; 32], domain)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc5869_test_vector_a1() {
        // RFC 5869 Appendix A.1 (SHA-256), L=32: first 32 bytes of OKM.
        let ikm = [0x0bu8; 22];
        let salt = [
            0x00u8, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        ];
        let info = [0xf0u8, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
        let okm = derive(&ikm, &salt, &info);
        let expected: [u8; 32] = [
            0x3c, 0xb2, 0x5f, 0x25, 0xfa, 0xac, 0xd5, 0x7a, 0x90, 0x43, 0x4f, 0x64, 0xd0, 0x36,
            0x2f, 0x2a, 0x2d, 0x2d, 0x0a, 0x90, 0xcf, 0x1a, 0x5a, 0x4c, 0x5d, 0xb0, 0x2d, 0x56,
            0xec, 0xc4, 0xc5, 0xbf,
        ];
        assert_eq!(okm, expected);
    }

    #[test]
    fn domain_separation_yields_distinct_keys() {
        let ikm = [7u8; 32];
        let wrap = domain_sep(&ikm, DOMAIN_DEK_WRAP);
        let store = domain_sep(&ikm, DOMAIN_STORE);
        let field = domain_sep(&ikm, DOMAIN_FIELD);
        // Distinct purposes must never share a key, and derivation is
        // deterministic for the same (ikm, domain).
        assert_ne!(wrap, store);
        assert_ne!(wrap, field);
        assert_ne!(store, field);
        assert_eq!(wrap, domain_sep(&ikm, DOMAIN_DEK_WRAP));
        assert_eq!(field, domain_sep(&ikm, DOMAIN_FIELD));
    }
}
