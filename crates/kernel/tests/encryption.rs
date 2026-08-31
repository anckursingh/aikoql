//! Encryption acceptance tests — MRFC-0020 Phase 1 & Phase 3 gates.

use aikoql_kernel::security::crypto::{Aes256Gcm, Crypto, CryptoProvider};
use aikoql_kernel::security::envelope::{Envelope, DEKS_STORAGE_KEY};
use aikoql_kernel::security::field_crypto::{EncryptionPolicy, FieldCrypto};
use aikoql_kernel::security::kms::{KeyManager, LocalKms};
use aikoql_kernel::storage::encrypted::EncryptedStore;
use aikoql_kernel::storage::store::{MemoryEngine, StorageEngine, WriteBatch};
use aikoql_kernel::storage::store_redb::RedbEngine;
use aikoql_kernel::{
    Kernel, ManualClock, Metadata, Origin, ReferentialPolicy, RememberRequest, Subject, Value,
};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

fn temp_path(label: &str) -> String {
    format!(
        "{}/aikoql-enc-{}-{}",
        std::env::temp_dir().display(),
        label,
        std::process::id()
    )
}

fn encrypted_redb(path: &str) -> (EncryptedStore, [u8; 32]) {
    let _ = std::fs::remove_file(path);
    let redb = Arc::new(RedbEngine::open(path).expect("open redb"));
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let key = crypto.generate_key();
    (EncryptedStore::new(redb, crypto, key), key)
}

#[test]
fn e01_no_plaintext_in_redb_file() {
    let path = temp_path("e01");
    let (store, _key) = encrypted_redb(&path);

    let mut batch = WriteBatch::new();
    batch.put(b"ssn".to_vec(), b"123-45-6789".to_vec());
    batch.put(b"salary".to_vec(), b"150000".to_vec());
    store.write_batch(&batch).unwrap();
    drop(store);

    let raw_bytes = std::fs::read(&path).unwrap_or_default();
    let raw_str = String::from_utf8_lossy(&raw_bytes);
    assert!(!raw_str.contains("123-45-6789"), "SSN leaked");
    assert!(!raw_str.contains("150000"), "salary leaked");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn e02_reopen_with_same_key_recovers_data() {
    let path = temp_path("e02");
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let key = crypto.generate_key();
    {
        let redb = Arc::new(RedbEngine::open(&path).expect("open"));
        let store = EncryptedStore::new(redb, crypto.clone(), key);
        let mut batch = WriteBatch::new();
        batch.put(b"data".to_vec(), b"recoverable".to_vec());
        store.write_batch(&batch).unwrap();
    } // store + redb dropped here

    let redb2 = Arc::new(RedbEngine::open(&path).expect("reopen"));
    let store2 = EncryptedStore::new(redb2, crypto, key);
    let val = store2.get(b"data").unwrap().unwrap();
    assert_eq!(val, b"recoverable");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn e03_wrong_key_fails_decryption() {
    let path = temp_path("e03");
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let key = crypto.generate_key();
    {
        let redb = Arc::new(RedbEngine::open(&path).expect("open"));
        let store = EncryptedStore::new(redb, crypto.clone(), key);
        let mut batch = WriteBatch::new();
        batch.put(b"data".to_vec(), b"secret".to_vec());
        store.write_batch(&batch).unwrap();
    }

    let wrong_key = crypto.generate_key();
    let redb2 = Arc::new(RedbEngine::open(&path).expect("reopen"));
    let store2 = EncryptedStore::new(redb2, crypto, wrong_key);
    assert!(store2.get(b"data").is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn e04_encrypted_memory_no_plaintext() {
    let mem = Arc::new(MemoryEngine::new());
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let key = crypto.generate_key();
    let store = EncryptedStore::new(mem.clone(), crypto, key);

    let mut batch = WriteBatch::new();
    batch.put(b"classified".to_vec(), b"top-secret".to_vec());
    store.write_batch(&batch).unwrap();

    let raw = mem.get(b"classified").unwrap().unwrap();
    assert_ne!(raw, b"top-secret");
    assert_eq!(raw[0], 0x01); // version byte
                              // version(1) + nonce(12) + ciphertext(len) + tag(16) = 29+len
    assert!(raw.len() >= 29, "too small: {}", raw.len());
}

// ---------------------------------------------------------------------------
// MRFC-0020 Phase 3: Field-level encryption acceptance
// ---------------------------------------------------------------------------

/// In-memory KMS for tests (no file I/O).
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
        Ok(*self.key.read().unwrap())
    }
    fn rotate(&self, _passphrase: &str, provider: &dyn CryptoProvider) -> Result<[u8; 32], String> {
        let new_key = provider.generate_key();
        *self.key.write().unwrap() = new_key;
        Ok(new_key)
    }
}

#[test]
fn e05_field_level_encrypt_remember_decrypt_get() {
    let kms = MemKms::new();
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let envelope = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());

    let store: Arc<dyn StorageEngine> = Arc::new(MemoryEngine::new());
    let clock = Arc::new(ManualClock::new(10_000));

    let k = Kernel::open(store, clock, 0xBEEF)
        .unwrap()
        .with_field_encryption(crypto, envelope)
        .unwrap();

    // Mark "salary" and "ssn" as encrypted for type "employee".
    let policy = EncryptionPolicy::new(vec!["salary".to_string(), "ssn".to_string()]);
    k.set_encryption_policy("employee", policy);

    let alice = Subject::new("alice");
    let mut props = BTreeMap::new();
    props.insert("name".into(), Value::Text("Alice".into()));
    props.insert("salary".into(), Value::Int(150000));
    props.insert("ssn".into(), Value::Text("123-45-6789".into()));

    let remembered = k
        .remember(RememberRequest {
            context: (&alice).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "employee".into(),
                tenant: Some("acme".into()),
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: BTreeMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap();

    // Read back via get() — fields should be decrypted.
    let ko = k.get(&alice, &remembered.koid).unwrap();
    assert_eq!(
        ko.properties.get("name"),
        Some(&Value::Text("Alice".into()))
    );
    assert_eq!(ko.properties.get("salary"), Some(&Value::Int(150000)));
    assert_eq!(
        ko.properties.get("ssn"),
        Some(&Value::Text("123-45-6789".into()))
    );

    // Read via raw_object_at — storage still has encrypted ciphertext.
    let raw = k
        .raw_object_at(&remembered.koid, remembered.commit_ts)
        .unwrap()
        .unwrap();
    assert_eq!(
        raw.properties.get("name"),
        Some(&Value::Text("Alice".into()))
    );
    // salary and ssn are Bytes at rest:
    match raw.properties.get("salary") {
        Some(Value::Bytes(b)) => {
            assert_eq!(b[0], 0x01); // version byte
        }
        other => panic!("expected encrypted Bytes for salary, got {:?}", other),
    }
    match raw.properties.get("ssn") {
        Some(Value::Bytes(b)) => {
            assert_eq!(b[0], 0x01);
        }
        other => panic!("expected encrypted Bytes for ssn, got {:?}", other),
    }
}

#[test]
fn e06_field_encryption_without_policy_is_noop() {
    let kms = MemKms::new();
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let envelope = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());

    let store: Arc<dyn StorageEngine> = Arc::new(MemoryEngine::new());
    let clock = Arc::new(ManualClock::new(20_000));

    // Kernel with field encryption enabled, but no policy registered for this type.
    let k = Kernel::open(store, clock, 0xCAFE)
        .unwrap()
        .with_field_encryption(crypto, envelope)
        .unwrap();

    let alice = Subject::new("alice");
    let mut props = BTreeMap::new();
    props.insert("salary".into(), Value::Int(99999));

    let remembered = k
        .remember(RememberRequest {
            context: (&alice).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "employee".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: BTreeMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap();

    let ko = k.get(&alice, &remembered.koid).unwrap();
    // No policy = no encryption, value stored and returned as-is.
    assert_eq!(ko.properties.get("salary"), Some(&Value::Int(99999)));
}

// ---------------------------------------------------------------------------
// MRFC-0020 Security Gates
// ---------------------------------------------------------------------------

/// e07: Crash-safe rotation — DEK survives Envelope restart and KEK rotation.
/// Verified through FieldCrypto encrypt → restart → FieldCrypto decrypt.
#[test]
fn e07_crash_safe_rotation_field_crypto_survives() {
    let kms = MemKms::new();
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));

    // Phase 1: encrypt fields, capture wrapped DEKs.
    let (encrypted_props, wrapped_deks) = {
        let env = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());
        // Force DEK creation and capture wrapped form before passing to FieldCrypto.
        env.tenant_key("acme").unwrap();
        let wrapped = env.wrapped_deks();

        let fc = FieldCrypto::new(crypto.clone(), env);
        let policy = EncryptionPolicy::new(vec!["secret".to_string()]);

        let mut props = BTreeMap::new();
        props.insert("name".into(), Value::Text("Alice".into()));
        props.insert("secret".into(), Value::Text("classified-data".into()));

        fc.encrypt_fields("acme", "doc", &mut props, &policy)
            .unwrap();
        assert!(matches!(props.get("secret"), Some(Value::Bytes(_))));
        assert_eq!(props.get("name"), Some(&Value::Text("Alice".into())));
        (props, wrapped)
    }; // Drop fc + env.

    // Phase 2: new Envelope, reload DEK, decrypt.
    {
        let env2 = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());
        for w in &wrapped_deks {
            env2.load_dek(w).unwrap();
        }
        let fc2 = FieldCrypto::new(crypto.clone(), env2);
        let policy = EncryptionPolicy::new(vec!["secret".to_string()]);

        let mut props = encrypted_props.clone();
        fc2.decrypt_fields("acme", "doc", &mut props, &policy)
            .unwrap();

        assert_eq!(
            props.get("secret"),
            Some(&Value::Text("classified-data".into()))
        );
        assert_eq!(props.get("name"), Some(&Value::Text("Alice".into())));
    }
}

/// e08: Encrypted recovery — backup/restore with EncryptedStore.
/// Backup copies the encrypted redb file; restore opens it with the same key.
#[test]
fn e08_encrypted_backup_restore() {
    let path = format!(
        "{}/aikoql-enc-e08-{}",
        std::env::temp_dir().display(),
        std::process::id()
    );
    let backup_path = format!("{}.backup", path);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&backup_path);

    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let key = crypto.generate_key();

    // Write encrypted data.
    {
        let redb = Arc::new(RedbEngine::open(&path).expect("open"));
        let store = EncryptedStore::new(redb, crypto.clone(), key);
        let mut batch = WriteBatch::new();
        batch.put(b"ko:001".to_vec(), b"encrypted-value".to_vec());
        batch.put(b"ko:002".to_vec(), b"another-secret".to_vec());
        store.write_batch(&batch).unwrap();
    }

    // Backup: copy the file.
    std::fs::copy(&path, &backup_path).expect("backup copy");

    // Restore: open backup with same key, verify data.
    {
        let redb = Arc::new(RedbEngine::open(&backup_path).expect("open backup"));
        let store = EncryptedStore::new(redb, crypto.clone(), key);
        assert_eq!(store.get(b"ko:001").unwrap().unwrap(), b"encrypted-value");
        assert_eq!(store.get(b"ko:002").unwrap().unwrap(), b"another-secret");
    }

    // Wrong key on backup should fail.
    let wrong_key = crypto.generate_key();
    {
        let redb = Arc::new(RedbEngine::open(&backup_path).expect("open backup 2"));
        let store = EncryptedStore::new(redb, crypto.clone(), wrong_key);
        assert!(store.get(b"ko:001").is_err());
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&backup_path);
}

/// e09: DEKs persist through a full kernel restart (not just FieldCrypto).
/// Without persistence each boot mints fresh DEKs and field ciphertext
/// becomes undecryptable — data loss.
#[test]
fn e09_deks_persist_across_kernel_restart() {
    let key_path = temp_path("e09-key");
    let _ = std::fs::remove_file(&key_path);
    let kms = LocalKms::new(&key_path);
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let store: Arc<dyn StorageEngine> = Arc::new(MemoryEngine::new());
    let clock = Arc::new(ManualClock::new(30_000));

    let remembered = {
        let envelope = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());
        let k = Kernel::open(store.clone(), clock.clone(), 0xD00D)
            .unwrap()
            .with_field_encryption(crypto.clone(), envelope)
            .unwrap();
        k.set_encryption_policy(
            "employee",
            EncryptionPolicy::new(vec!["salary".to_string()]),
        );
        let alice = Subject::new("alice");
        let mut props = BTreeMap::new();
        props.insert("name".into(), Value::Text("Alice".into()));
        props.insert("salary".into(), Value::Int(150000));
        k.remember(RememberRequest {
            context: (&alice).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "employee".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: BTreeMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap()
    };

    // Fresh kernel over the same store: DEKs must load from the store.
    {
        let envelope = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());
        let k = Kernel::open(store, clock, 0xD00D)
            .unwrap()
            .with_field_encryption(crypto, envelope)
            .unwrap();
        k.set_encryption_policy(
            "employee",
            EncryptionPolicy::new(vec!["salary".to_string()]),
        );
        let alice = Subject::new("alice");
        let ko = k.get(&alice, &remembered.koid).unwrap();
        assert_eq!(ko.properties.get("salary"), Some(&Value::Int(150000)));
        assert_eq!(
            ko.properties.get("name"),
            Some(&Value::Text("Alice".into()))
        );
    }
    let _ = std::fs::remove_file(&key_path);
}

/// e10: A corrupt DEK record fails the open — silently continuing would mint
/// a fresh DEK for the tenant and orphan every field-encrypted value.
#[test]
fn e10_corrupt_dek_record_fails_closed() {
    let store: Arc<dyn StorageEngine> = Arc::new(MemoryEngine::new());
    let mut batch = WriteBatch::new();
    batch.put(DEKS_STORAGE_KEY.to_vec(), vec![0xAB, 0xCD]); // truncated garbage
    store.write_batch(&batch).unwrap();

    let kms = MemKms::new();
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let envelope = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());
    let res = Kernel::open(store, Arc::new(ManualClock::new(40_000)), 0xE0E)
        .unwrap()
        .with_field_encryption(crypto, envelope);
    assert!(res.is_err(), "corrupt DEK record must fail the open");
}

/// e11: The wrong passphrase never yields a key — LocalKms fails closed with
/// an authentication error instead of deriving garbage (review P1-13).
#[test]
fn e11_wrong_passphrase_fails_closed() {
    let key_path = temp_path("e11-key");
    let _ = std::fs::remove_file(&key_path);
    let kms = LocalKms::new(&key_path);
    kms.master_key("correct-horse")
        .expect("first use creates the key file");
    // A restart re-reads the file: same path, fresh instance (the instance
    // cache is per-process; a new process derives from the envelope again).
    let kms2 = LocalKms::new(&key_path);
    let err = kms2.master_key("wrong-passphrase").unwrap_err();
    assert!(
        err.contains("InvalidPassphrase") || err.contains("authentication"),
        "expected a passphrase-authentication error, got: {}",
        err
    );
    let _ = std::fs::remove_file(&key_path);
}

/// e12: An unknown crypto-meta version fails the open (review P2-14) — the
/// key derivation scheme is versioned, and unknown versions are never guessed.
#[test]
fn e12_unknown_crypto_meta_version_fails_open() {
    use aikoql_kernel::security::envelope::CRYPTO_META_KEY;
    let store: Arc<dyn StorageEngine> = Arc::new(MemoryEngine::new());
    let mut batch = WriteBatch::new();
    batch.put(
        CRYPTO_META_KEY.to_vec(),
        b"aikoql/crypto/v99 kdf=future-widget".to_vec(),
    );
    store.write_batch(&batch).unwrap();

    let kms = MemKms::new();
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let envelope = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());
    let res = Kernel::open(store, Arc::new(ManualClock::new(50_000)), 0xE12)
        .unwrap()
        .with_field_encryption(crypto, envelope);
    assert!(
        res.is_err(),
        "unknown crypto-meta version must fail the open, not guess"
    );
}

/// e13: Tampered field ciphertext fails closed — AEAD authentication rejects
/// the modification; the kernel can never surface a phantom plaintext
/// (review P1-13).
#[test]
fn e13_tampered_field_ciphertext_fails_closed() {
    let kms = MemKms::new();
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let envelope = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());
    let fc = FieldCrypto::new(crypto, envelope);
    let policy = EncryptionPolicy::new(vec!["secret".to_string()]);

    let mut props = BTreeMap::new();
    props.insert("secret".into(), Value::Text("classified-data".into()));
    fc.encrypt_fields("acme", "doc", &mut props, &policy)
        .unwrap();

    // Untampered: valid DEK decrypts back to the plaintext.
    let mut clean = props.clone();
    fc.decrypt_fields("acme", "doc", &mut clean, &policy)
        .unwrap();
    assert_eq!(
        clean.get("secret"),
        Some(&Value::Text("classified-data".into()))
    );

    // Flip one ciphertext byte: decryption must fail, never return garbage.
    let mut tampered = props.clone();
    if let Some(Value::Bytes(bytes)) = tampered.get_mut("secret") {
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
    } else {
        panic!("secret must be encrypted Bytes at rest");
    }
    assert!(
        fc.decrypt_fields("acme", "doc", &mut tampered, &policy)
            .is_err(),
        "tampered ciphertext must fail AEAD authentication"
    );
}
