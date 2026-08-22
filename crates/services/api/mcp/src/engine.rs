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
use std::sync::Arc;

pub(crate) fn open_kernel(db_path: &str, enc: &RuntimeEncryption) -> KResult<Kernel> {
    let engine = RedbEngine::open(db_path)?;
    if !enc.enabled {
        return Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0xA9C9);
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
    let store: Arc<dyn StorageEngine> = Arc::new(EncryptedStore::new(
        Arc::new(engine),
        crypto.clone(),
        store_key,
    ));
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
