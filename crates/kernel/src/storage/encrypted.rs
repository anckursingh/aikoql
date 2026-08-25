//! EncryptedStore — transparent page/WAL encryption wrapper (MRFC-0020 §Storage Encryption).
//!
//! Wraps any `StorageEngine` and encrypts values before writing. Keys remain
//! plaintext (needed for KV lookups). The storage key is used as AAD to bind
//! ciphertext to its key, preventing key-swapping attacks.
//!
//! Write path: Compress(passthrough) → Encrypt → AEAD → inner store
//! Read path:  inner store → Verify AEAD → Decrypt → Decompress(passthrough)

use super::store::{StorageEngine, WriteBatch};
use crate::knowledge::kom::KResult;
use crate::security::crypto::Crypto;
use std::path::Path;
use std::sync::Arc;

pub struct EncryptedStore {
    inner: Arc<dyn StorageEngine>,
    crypto: Arc<Crypto>,
    key: [u8; 32],
}

impl EncryptedStore {
    pub fn new(inner: Arc<dyn StorageEngine>, crypto: Arc<Crypto>, key: [u8; 32]) -> Self {
        EncryptedStore { inner, crypto, key }
    }

    pub fn inner(&self) -> &Arc<dyn StorageEngine> {
        &self.inner
    }

    pub fn crypto(&self) -> &Arc<Crypto> {
        &self.crypto
    }

    pub fn key_bytes(&self) -> [u8; 32] {
        self.key
    }
}

impl StorageEngine for EncryptedStore {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>> {
        match self.inner.get(key)? {
            Some(encrypted) => {
                let plaintext = self
                    .crypto
                    .decrypt(&self.key, &encrypted, key)
                    .map_err(|e| crate::knowledge::kom::KError::Store(format!("decrypt: {}", e)))?;
                Ok(Some(plaintext))
            }
            None => Ok(None),
        }
    }

    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let rows = self.inner.scan(prefix)?;
        let mut out = Vec::with_capacity(rows.len());
        for (k, v) in rows {
            let plaintext = self
                .crypto
                .decrypt(&self.key, &v, &k)
                .map_err(|e| crate::knowledge::kom::KError::Store(format!("decrypt: {}", e)))?;
            out.push((k, plaintext));
        }
        Ok(out)
    }

    fn write_batch(&self, batch: &WriteBatch) -> KResult<()> {
        let mut encrypted_batch = WriteBatch::new();
        for (k, v) in &batch.puts {
            let encrypted = self
                .crypto
                .encrypt(&self.key, v, k)
                .map_err(|e| crate::knowledge::kom::KError::Store(format!("encrypt: {}", e)))?;
            encrypted_batch.put(k.clone(), encrypted);
        }
        for k in &batch.dels {
            encrypted_batch.del(k.clone());
        }
        self.inner.write_batch(&encrypted_batch)
    }

    fn snapshot_to(&self, dest: &Path) -> KResult<()> {
        // Raw delegation: scan() decrypts, so the default impl would write a
        // PLAINTEXT backup. The inner engine's rows (ciphertext) must move
        // verbatim — a fresh kernel opens the backup with decryption on.
        self.inner.snapshot_to(dest)
    }

    fn restore_from(&self, src: &Path) -> KResult<()> {
        // Raw delegation: backup rows are already encrypted; writing them
        // through the encrypting layer would double-encrypt.
        self.inner.restore_from(src)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::store::MemoryEngine;
    use super::*;
    use crate::security::crypto::Aes256Gcm;

    #[test]
    fn encrypted_roundtrip() {
        let mem = Arc::new(MemoryEngine::new());
        let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
        let key = crypto.generate_key();
        let store = EncryptedStore::new(mem.clone(), crypto, key);

        let mut batch = WriteBatch::new();
        batch.put(b"hello".to_vec(), b"world".to_vec());
        store.write_batch(&batch).unwrap();

        // Inner store has encrypted data (not plaintext).
        let raw = mem.get(b"hello").unwrap().unwrap();
        assert_ne!(raw, b"world");
        assert!(raw.len() > 30); // version + nonce + ct + tag

        // Outer store decrypts transparently.
        let val = store.get(b"hello").unwrap().unwrap();
        assert_eq!(val, b"world");

        // Missing key returns None.
        assert!(store.get(b"bogus").unwrap().is_none());
    }

    #[test]
    fn encrypted_scan() {
        let mem = Arc::new(MemoryEngine::new());
        let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
        let key = crypto.generate_key();
        let store = EncryptedStore::new(mem, crypto, key);

        let mut batch = WriteBatch::new();
        batch.put(b"a".to_vec(), b"1".to_vec());
        batch.put(b"b".to_vec(), b"2".to_vec());
        store.write_batch(&batch).unwrap();

        let rows = store.scan(b"").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, b"1");
        assert_eq!(rows[1].1, b"2");
    }

    #[test]
    fn encrypted_delete() {
        let mem = Arc::new(MemoryEngine::new());
        let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
        let key = crypto.generate_key();
        let store = EncryptedStore::new(mem, crypto, key);

        let mut batch = WriteBatch::new();
        batch.put(b"x".to_vec(), b"y".to_vec());
        store.write_batch(&batch).unwrap();

        let mut del_batch = WriteBatch::new();
        del_batch.del(b"x".to_vec());
        store.write_batch(&del_batch).unwrap();

        assert!(store.get(b"x").unwrap().is_none());
    }

    #[test]
    fn encrypted_data_is_opaque_in_inner_store() {
        let mem = Arc::new(MemoryEngine::new());
        let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
        let key = crypto.generate_key();
        let store = EncryptedStore::new(mem.clone(), crypto, key);

        store
            .write_batch(&WriteBatch {
                puts: vec![(b"secret".to_vec(), b"classified".to_vec())],
                dels: vec![],
            })
            .unwrap();

        // Verify the inner store contains NO plaintext.
        let raw = mem.get(b"secret").unwrap().unwrap();
        let raw_str = String::from_utf8_lossy(&raw);
        assert!(
            !raw_str.contains("classified"),
            "plaintext leaked to inner store: {}",
            raw_str
        );
        assert!(
            !raw_str.contains("secret"),
            "key leaked to inner store: {}",
            raw_str
        );
        // Must start with version byte 0x01.
        assert_eq!(raw[0], 0x01, "version byte missing");
    }

    #[test]
    fn encrypted_snapshot_and_restore_stay_ciphertext() {
        // REC-002: the overrides must delegate to the RAW inner rows. A
        // plaintext backup would leak values and be unopenable by a fresh
        // encrypted kernel; a restore through the encrypting layer would
        // double-encrypt the already-encrypted rows.
        let mem = Arc::new(MemoryEngine::new());
        let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
        let key = crypto.generate_key();
        let store = EncryptedStore::new(mem, crypto, key);
        store
            .write_batch(&WriteBatch {
                puts: vec![(b"secret".to_vec(), b"classified".to_vec())],
                dels: vec![],
            })
            .unwrap();

        let dest =
            std::env::temp_dir().join(format!("aikoql_enc_snap_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        store.snapshot_to(&dest).unwrap();

        // Raw rows in the backup file must not contain the plaintext.
        let raw = crate::storage::store_redb::RedbEngine::open(&dest)
            .unwrap()
            .get(b"secret")
            .unwrap()
            .unwrap();
        let raw_str = String::from_utf8_lossy(&raw);
        assert!(
            !raw_str.contains("classified"),
            "plaintext leaked into backup: {}",
            raw_str
        );

        // Restoring into a fresh encrypted wrapper decrypts back to the
        // original value — ciphertext was never double-encrypted.
        let mem2 = Arc::new(MemoryEngine::new());
        let crypto2 = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
        let store2 = EncryptedStore::new(mem2, crypto2, key);
        store2.restore_from(&dest).unwrap();
        assert_eq!(store2.get(b"secret").unwrap().unwrap(), b"classified");
        let _ = std::fs::remove_file(&dest);
    }
}
