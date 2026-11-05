//! AG-31 cloud KMS `install` — the real AWS KMS KeyManager over the
//! `KmsOps` seam. The unit tests drive a mock (no network, no cost);
//! the SDK-backed ops are exercised once against real AWS by hand.
//!
//! Contract: `install` creates the CMK and wraps the master key under it
//! into a KMS pointer file; `master_key` unlocks through Decrypt (AWS IAM
//! gates the key, not the passphrase); `rotate` mints a fresh key and
//! re-wraps it under the same CMK.

use aikoql_kernel::security::crypto::Aes256Gcm;
use aikoql_kernel::security::{AwsKms, KeyManager, KmsOps};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

// --- mock KMS -----------------------------------------------------------------

/// An in-memory stand-in for the SDK: create_key mints ARNs, encrypt wraps
/// by XORing a per-key pad (round-trips, fails on the wrong key), decrypt
/// unwraps. Records the call sequence for the wiring asserts.
struct MockKms {
    next: Mutex<u32>,
    wraps: Mutex<HashMap<String, Vec<u8>>>,
    calls: Mutex<Vec<String>>,
}

impl MockKms {
    fn new() -> Self {
        MockKms {
            next: Mutex::new(0),
            wraps: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<String> {
        // justified: Mutex poison is unrecoverable
        self.calls.lock().unwrap().clone()
    }
}

impl KmsOps for MockKms {
    fn create_key(&self, alias: &str) -> Result<String, String> {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        self.calls
            .lock()
            .unwrap()
            .push(format!("create_key:{alias}"));
        Ok(format!(
            "arn:aws:kms:eu-north-1:123456789012:key/mock-{}",
            *n
        ))
    }

    fn encrypt(&self, key_id: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let mut wraps = self.wraps.lock().unwrap();
        let pad = wraps.entry(key_id.into()).or_insert_with(|| {
            (0..plaintext.len())
                .map(|i| (i * 7 + 3) as u8)
                .collect::<Vec<u8>>()
        });
        self.calls.lock().unwrap().push(format!("encrypt:{key_id}"));
        Ok(plaintext
            .iter()
            .zip(pad.iter())
            .map(|(a, b)| a ^ b)
            .collect())
    }

    fn decrypt(&self, key_id: &str, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        let pad = self
            .wraps
            .lock()
            .unwrap()
            .get(key_id)
            .cloned()
            .ok_or("the CMK has no key material (wrong key or region)")?;
        self.calls.lock().unwrap().push(format!("decrypt:{key_id}"));
        Ok(ciphertext
            .iter()
            .zip(pad.iter())
            .map(|(a, b)| a ^ b)
            .collect())
    }
}

fn tmp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aikoql-aws-kms-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("master.key")
}

// --- aws01 — install wraps the key, master_key unlocks through KMS -------------

#[test]
fn aws01_install_wraps_and_master_key_unlocks_through_kms() {
    let path = tmp_path("aws01");
    let mock = std::sync::Arc::new(MockKms::new());
    let kms = AwsKms::with_ops("", path.clone(), mock.clone());

    let key = [0xAB; 32];
    kms.install(&key, "the-passphrase").unwrap();
    let calls = mock.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("create_key:")),
        "install created the CMK: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("encrypt:")),
        "install wrapped the key: {calls:?}"
    );
    assert!(path.exists(), "the KMS pointer file was written");

    // the passphrase no longer gates the key — any string unlocks the same
    // master key once it is KMS-held
    assert_eq!(kms.master_key("wrong-passphrase").unwrap(), key);
    assert_eq!(kms.master_key("").unwrap(), key);
}

// --- aws02 — a torn or tampered pointer fails closed ---------------------------

#[test]
fn aws02_a_tampered_pointer_file_fails_closed() {
    let path = tmp_path("aws02");
    let mock = std::sync::Arc::new(MockKms::new());
    let kms = AwsKms::with_ops("", path.clone(), mock);
    kms.install(&[0x42; 32], "pw").unwrap();

    let raw = std::fs::read(&path).unwrap();
    let mut torn = raw.clone();
    let last = torn.len() - 1;
    torn[last] ^= 0x01;
    std::fs::write(&path, &torn).unwrap();
    assert!(
        kms.master_key("pw").is_err(),
        "a flipped byte must not yield a key"
    );
    assert!(
        kms.master_key("pw").is_err(),
        "and never caches the bad read"
    );
}

// --- aws03 — rotate mints a fresh key and re-wraps it under the same CMK -------

#[test]
fn aws03_rotate_mints_and_rewraps_under_the_same_cmk() {
    let path = tmp_path("aws03");
    let mock = std::sync::Arc::new(MockKms::new());
    let kms = AwsKms::with_ops("", path.clone(), mock.clone());
    let first = [0x11; 32];
    kms.install(&first, "pw").unwrap();

    let second = kms.rotate("pw", &Aes256Gcm::new()).unwrap();
    assert_ne!(second, first, "rotation mints a fresh master key");
    let encrypts = mock
        .calls()
        .iter()
        .filter(|c| c.starts_with("encrypt:"))
        .count();
    assert_eq!(encrypts, 2, "the new key was re-wrapped under the same CMK");
    assert_eq!(kms.master_key("pw").unwrap(), second);
}

// --- aws04 — the pointer file format is pinned ---------------------------------

#[test]
fn aws04_the_pointer_file_format_is_pinned() {
    let path = tmp_path("aws04");
    let mock = std::sync::Arc::new(MockKms::new());
    let kms = AwsKms::with_ops("", path.clone(), mock);
    kms.install(&[0x99; 32], "pw").unwrap();

    let raw = std::fs::read(&path).unwrap();
    assert!(
        raw.starts_with(b"AIKOQL-KMS1"),
        "magic header pins the format: {:?}",
        &raw[..raw.len().min(16)]
    );
    // the ARN the install minted is carried in the file so open knows
    // which CMK to Decrypt with
    let body = String::from_utf8_lossy(&raw);
    assert!(
        body.contains("arn:aws:kms:eu-north-1:123456789012:key/mock-1"),
        "the pointer carries the CMK arn: {body}"
    );
}
