//! Single open path for every subcommand (MRFC-0020): honors [encryption]
//! settings so no plaintext writer can open an encrypted database — that
//! would silently corrupt it.

use crate::config::RuntimeEncryption;
use aikoql_kernel::security::crypto::{Aes256Gcm, Crypto};
use aikoql_kernel::security::envelope::Envelope;
use aikoql_kernel::security::field_crypto::EncryptionPolicy;
use aikoql_kernel::security::hkdf::{self, DOMAIN_STORE};
use aikoql_kernel::security::kms::LocalKms;
use aikoql_kernel::security::KeyManager;
use aikoql_kernel::storage::encrypted::EncryptedStore;
use aikoql_kernel::storage::store::StorageEngine;
use aikoql_kernel::storage::store_redb::RedbEngine;
use aikoql_kernel::{KError, KResult, Kernel, SystemClock};
use aikoql_storage::AikoqlStorageEngine;
use std::sync::Arc;

/// Production default: the AIKOQL-native engine (adoption gate passed —
/// artifacts/storage-engine/adoption-decision.md). Existing redb databases
/// keep working via AIKOQL_BACKEND=redb; the migration path is the REC-002
/// backup/restore flow (restore reads the redb snapshot format into whatever
/// engine is current). Unknown values fail closed — a mistyped backend must
/// not silently open a fresh store at the same path.
fn open_engine(db_path: &str) -> KResult<Arc<dyn StorageEngine>> {
    match std::env::var("AIKOQL_BACKEND").ok().as_deref() {
        None | Some("aikoql") => Ok(Arc::new(AikoqlStorageEngine::open(db_path)?)),
        Some("redb") => Ok(Arc::new(RedbEngine::open(db_path)?)),
        Some(other) => Err(KError::Store(format!(
            "unknown AIKOQL_BACKEND {other:?}: use \"aikoql\" or \"redb\""
        ))),
    }
}

pub(crate) fn open_kernel(db_path: &str, enc: &RuntimeEncryption) -> KResult<Kernel> {
    let engine = open_engine(db_path)?;
    if !enc.enabled {
        return Kernel::open(engine, Arc::new(SystemClock), 0xA9C9);
    }
    let Some(pass) = enc.passphrase.as_deref() else {
        return Err(KError::Store(
            "encryption enabled but no passphrase: set AIKOQL_PASSPHRASE or encryption.passphrase"
                .into(),
        ));
    };
    let kms = LocalKms::new(&enc.key_path);
    let kek = kms.master_key(pass).map_err(KError::Store)?;
    // The store key is a domain-separated subkey of the KEK — the KEK itself
    // never encrypts data directly (DEK wrapping uses its own subkey).
    let store_key = hkdf::domain_sep(&kek, DOMAIN_STORE);
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let envelope = Arc::new(Envelope::init(&kms, pass, crypto.clone()).map_err(KError::Store)?);
    let store: Arc<dyn StorageEngine> =
        Arc::new(EncryptedStore::new(engine, crypto.clone(), store_key));
    let kernel = Kernel::open(store, Arc::new(SystemClock), 0xA9C9)?
        .with_field_encryption(crypto, envelope)?;
    for (type_name, fields) in &enc.policies {
        kernel.set_encryption_policy(type_name, EncryptionPolicy::new(fields.clone()));
    }
    Ok(kernel)
}

/// Subcommand variant: discover encryption settings (TOML + env) and open.
pub(crate) fn open_kernel_auto(db_path: &str) -> KResult<Kernel> {
    let enc = RuntimeEncryption::discover().map_err(KError::Store)?;
    open_kernel(db_path, &enc)
}

#[cfg(test)]
mod tests {
    use super::open_engine;
    use aikoql_kernel::storage::store::WriteBatch;

    /// Restores AIKOQL_BACKEND on drop — mcp unit tests share one process, and
    /// a leaked backend value would change every later open_kernel call.
    struct EnvGuard {
        prev: Option<String>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("AIKOQL_BACKEND", v),
                None => std::env::remove_var("AIKOQL_BACKEND"),
            }
        }
    }

    fn scratch(tag: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("aikoql_mcp_backend_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_file(&p);
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn backend_selector_default_redb_and_fail_closed() {
        // Default (unset) and the redb opt-out both open and serve a put/get.
        for (env, tag) in [(None, "aikoql"), (Some("redb"), "redb")] {
            let prev = std::env::var("AIKOQL_BACKEND").ok();
            let guard = match env {
                Some(v) => {
                    std::env::set_var("AIKOQL_BACKEND", v);
                    EnvGuard { prev }
                }
                None => {
                    std::env::remove_var("AIKOQL_BACKEND");
                    EnvGuard { prev }
                }
            };
            let engine = open_engine(&scratch(tag)).unwrap();
            let mut b = WriteBatch::new();
            b.put(b"k".to_vec(), b"v".to_vec());
            engine.write_batch(&b).unwrap();
            assert_eq!(engine.get(b"k").unwrap(), Some(b"v".to_vec()));
            drop(guard);
        }
        // Unknown values fail closed.
        let prev = std::env::var("AIKOQL_BACKEND").ok();
        std::env::set_var("AIKOQL_BACKEND", "nope");
        let guard = EnvGuard { prev };
        assert!(open_engine(&scratch("bad")).is_err());
        drop(guard);
    }
}
