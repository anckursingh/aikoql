//! Single open path for every subcommand (MRFC-0020): honors [encryption]
//! settings so no plaintext writer can open an encrypted database — that
//! would silently corrupt it. Backend selection (PR#2 review SE-01/SE-02,
//! PR6-005) is owned by the runtime's ONE authoritative module
//! (aikoql_runtime::backend); the config pipeline (defaults → TOML → env
//! → CLI) supplies the explicit choice or None for detection. The public
//! contract and per-backend profiles live in docs/STORAGE-BACKENDS.md.

use crate::config::{RuntimeEncryption, StorageBackend};
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
    backend: Option<StorageBackend>,
) -> KResult<(Kernel, Option<Arc<dyn StorageAdminApi>>)> {
    // PR6-005 — engine selection + detection live in the runtime's
    // backend module; every subcommand funnels through this open_kernel.
    let (engine, admin) =
        aikoql_runtime::backend::open_engine(std::path::Path::new(db_path), backend)?;
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
    open_kernel(db_path, &cfg.encryption, cfg.backend)
}

#[cfg(test)]
mod tests {
    use super::open_kernel_auto;
    use crate::config::ENV_LOCK;
    use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};
    use aikoql_kernel::storage::store_redb::RedbEngine;
    use std::io::Read;

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
                // Sidecars next to the registered stem (`{stem}.redb.artifacts`).
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

    /// PR6-005 — the ONE detection contract exercised through the MCP
    /// server startup path (open_kernel_auto = load() + open_kernel, the
    /// funnel every subcommand uses): an existing redb database opens
    /// through auto-detection (a v2-defaulted open would fail on the redb
    /// file — that divergence is exactly what the review forbids), and a
    /// missing path still creates a fresh v2. The full five-case detection
    /// matrix lives in aikoql_runtime::backend's tests.
    #[test]
    fn mcp_startup_path_autodetects_existing_and_fresh() {
        let _guard = ENV_LOCK.lock().unwrap(); // serializes with config tests' env windows
        std::env::remove_var("AIKOQL_BACKEND");

        let path = scratch("startup-redb");
        {
            let e = RedbEngine::open(&path).unwrap();
            let mut b = WriteBatch::new();
            b.put(b"k".to_vec(), b"v".to_vec());
            e.write_batch(&b).unwrap();
        }
        {
            let (_kernel, _admin) = open_kernel_auto(&path).unwrap();
        } // redb holds a live file lock — read the head bytes after close
        let mut head = [0u8; 4];
        std::fs::File::open(&path)
            .unwrap()
            .read_exact(&mut head)
            .unwrap();
        assert_ne!(
            &head, b"AKQL",
            "the redb file must survive the startup path unrewritten"
        );

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
