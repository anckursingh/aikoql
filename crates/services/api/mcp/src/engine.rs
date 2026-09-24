//! Single open path for every subcommand (MRFC-0020): honors [encryption]
//! settings so no plaintext writer can open an encrypted database — that
//! would silently corrupt it. The storage open is owned by the runtime's
//! ONE authoritative opener (aikoql_runtime::backend::open_engine — v2-only
//! since the launch S-02 decommission).

use crate::config::RuntimeEncryption;
use aikoql_kernel::security::crypto::{Aes256Gcm, Crypto};
use aikoql_kernel::security::envelope::Envelope;
use aikoql_kernel::security::field_crypto::EncryptionPolicy;
use aikoql_kernel::security::hkdf::{self, DOMAIN_STORE};
use aikoql_kernel::security::kms::LocalKms;
use aikoql_kernel::security::KeyManager;
use aikoql_kernel::storage::encrypted::EncryptedStore;
use aikoql_kernel::storage::store::StorageEngine;
use aikoql_kernel::{KError, KResult, Kernel, SystemClock};
use aikoql_storage_v2::engine::StorageAdminApi;
use std::sync::Arc;

pub(crate) fn open_kernel(
    db_path: &str,
    enc: &RuntimeEncryption,
) -> KResult<(Kernel, Option<Arc<dyn StorageAdminApi>>)> {
    // The runtime's ONE opener; every subcommand funnels through here.
    let (engine, admin) = aikoql_runtime::backend::open_engine(std::path::Path::new(db_path))?;
    if !enc.enabled {
        return Ok((Kernel::open(engine, Arc::new(SystemClock), 0xA9C9)?, admin));
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
    Ok((kernel, admin))
}

/// Subcommand variant: one config pipeline (R10, PR#2 review SE-02) —
/// encryption AND backend both come from `load()` (defaults → TOML → env;
/// subcommand flags are not server config and are not parsed).
pub(crate) fn open_kernel_auto(
    db_path: &str,
) -> KResult<(Kernel, Option<Arc<dyn StorageAdminApi>>)> {
    let cfg = crate::config::load(&[], None, None).map_err(KError::Store)?;
    open_kernel(db_path, &cfg.encryption)
}

#[cfg(test)]
mod tests {
    use super::open_kernel_auto;
    use crate::config::ENV_LOCK;
    use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};

    // Temp db paths written by THIS test thread, swept when the thread exits
    // (the main thread's destructor runs at process exit — statics are NOT
    // dropped on Windows MSVC, TLS is).
    thread_local! {
        static TEMP_PATHS: std::cell::RefCell<TempSweeper> =
            const { std::cell::RefCell::new(TempSweeper { paths: Vec::new() }) };
    }

    struct TempSweeper {
        paths: Vec<std::path::PathBuf>,
    }
    impl Drop for TempSweeper {
        fn drop(&mut self) {
            for p in &self.paths {
                let _ = std::fs::remove_file(p);
                let _ = std::fs::remove_dir_all(p);
                // Sidecars next to the registered stem.
                let Some(name) = p.file_name() else { continue };
                if let Ok(rd) = std::fs::read_dir(p.parent().unwrap_or(std::path::Path::new("."))) {
                    let prefix = format!("{}.", name.to_string_lossy());
                    for e in rd.flatten() {
                        if e.file_name().to_string_lossy().starts_with(&prefix) {
                            let _ = std::fs::remove_file(e.path());
                            let _ = std::fs::remove_dir_all(e.path());
                        }
                    }
                }
            }
        }
    }

    fn scratch(tag: &str) -> String {
        // Killed runs never reach TLS drop — sweep their corpses at the
        // next startup (only entries older than a day, so a concurrent
        // live run's fresh files are untouched).
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
                return;
            };
            let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
            for e in rd.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("aikoql_mcp_backend_") {
                    continue;
                }
                let stale = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .is_some_and(|t| t < cutoff);
                if stale {
                    let _ = std::fs::remove_file(e.path());
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        });
        let mut p = std::env::temp_dir();
        p.push(format!("aikoql_mcp_backend_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_file(&p);
        TEMP_PATHS.with(|t| t.borrow_mut().paths.push(p.clone()));
        p.to_string_lossy().into_owned()
    }

    /// Launch S-02 — the startup path (open_kernel_auto = load() +
    /// open_kernel, the funnel every subcommand uses): an existing v2
    /// database reopens, and a missing path creates a fresh v2 directory.
    /// A legacy FILE fails closed (pinned in aikoql_runtime::backend).
    #[test]
    fn mcp_startup_path_reopens_existing_and_creates_fresh() {
        let _guard = ENV_LOCK.lock().unwrap(); // serializes with config tests' env windows

        let path = scratch("startup-existing");
        {
            let e = aikoql_storage_v2::AikoqlStorageEngineV2::open(&path).unwrap();
            let mut b = WriteBatch::new();
            b.put(b"k".to_vec(), b"v".to_vec());
            e.write_batch(&b).unwrap();
        }
        {
            let (_kernel, _admin) = open_kernel_auto(&path).unwrap();
        }

        let fresh = scratch("startup-fresh");
        {
            let (_kernel, _admin) = open_kernel_auto(&fresh).unwrap();
        }
        assert!(
            std::path::Path::new(&fresh).join("CURRENT").is_file(),
            "a missing path must create a v2 database directory through the startup path"
        );
    }
}
