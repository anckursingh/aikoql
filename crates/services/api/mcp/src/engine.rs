//! Single open path for every subcommand (MRFC-0020): honors [encryption]
//! settings so no plaintext writer can open an encrypted database — that
//! would silently corrupt it. The storage open is owned by the runtime's
//! ONE authoritative opener (aikoql_runtime::backend::open_engine — v2-only
//! since the launch S-02 decommission).

use crate::config::RuntimeEncryption;
use aikoql_kernel::security::crypto::{Aes256Gcm, Crypto};
use aikoql_kernel::security::envelope::Envelope;
use aikoql_kernel::security::field_crypto::EncryptionPolicy;
use aikoql_kernel::security::keyring::KeyRing;
use aikoql_kernel::security::kms::LocalKms;
use aikoql_kernel::storage::encrypted::EncryptedStore;
use aikoql_kernel::storage::store::StorageEngine;
use aikoql_kernel::{KError, KResult, Kernel, SystemClock};
use aikoql_storage_v2::engine::StorageAdminApi;
use std::sync::Arc;

pub(crate) fn open_kernel(
    db_path: &str,
    enc: &RuntimeEncryption,
    retain_log: bool,
    standby: Option<String>,
) -> KResult<(Kernel, Option<Arc<dyn StorageAdminApi>>)> {
    open_kernel_with_ring(db_path, enc, retain_log, standby).map(|(k, a, _)| (k, a))
}

/// `open_kernel`, also returning the key ring of an encrypted database (the
/// handle a KEK rotation runs through — AG-31).
#[allow(clippy::type_complexity)]
pub(crate) fn open_kernel_with_ring(
    db_path: &str,
    enc: &RuntimeEncryption,
    retain_log: bool,
    standby: Option<String>,
) -> KResult<(
    Kernel,
    Option<Arc<dyn StorageAdminApi>>,
    Option<Arc<KeyRing>>,
)> {
    // The runtime's ONE opener; every subcommand funnels through here.
    let (engine, admin) =
        aikoql_runtime::backend::open_engine_with(std::path::Path::new(db_path), retain_log)?;
    // MRFC-0080 standby reads: a read replica's store is fenced before the
    // kernel opens — any client write (or a stray open-time one) fails
    // NOT_LEADER; the replication stream still applies.
    let open = |store: Arc<dyn StorageEngine>| match &standby {
        Some(_) => Kernel::open_standby(store, Arc::new(SystemClock), 0xA9C9),
        None => Kernel::open(store, Arc::new(SystemClock), 0xA9C9),
    };
    if let Some(why) = standby.clone() {
        let Some(a) = admin.as_ref() else {
            return Err(KError::Store(
                "a read replica needs the v2 storage admin surface".into(),
            ));
        };
        a.fence(why);
    }
    if !enc.enabled {
        let k = open(engine)?;
        return Ok((
            match crate::AUDIT_SIGNER.get() {
                // AG-30: a signing server chains an Ed25519 signature onto
                // every committed journal event (the evidence-pack head
                // signature is computed from the same key at export time).
                Some(signer) => k.with_audit_signer(std::sync::Arc::clone(signer)),
                None => k,
            },
            admin,
            None,
        ));
    }
    let Some(pass) = enc.passphrase.as_deref() else {
        return Err(KError::Store(
            "encryption enabled but no passphrase: set AIKOQL_PASSPHRASE or encryption.passphrase"
                .into(),
        ));
    };
    let kms = Arc::new(LocalKms::new(&enc.key_path));
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    // AG-31: the store key is a data key WRAPPED by the KEK (a pre-AG-31
    // database keeps its HKDF(KEK, store) value as that key), so the KEK
    // can rotate without re-encrypting the store.
    let ring = KeyRing::open(
        kms.clone(),
        pass,
        format!("{}.store-dek", enc.key_path),
        crypto.clone(),
    )
    .map_err(KError::Store)?;
    let envelope = Arc::new(Envelope::init(&*kms, pass, crypto.clone()).map_err(KError::Store)?);
    let store: Arc<dyn StorageEngine> = Arc::new(EncryptedStore::new(
        engine,
        crypto.clone(),
        ring.store_key(),
    ));
    let kernel = open(store)?.with_field_encryption(crypto, envelope)?;
    let kernel = match crate::AUDIT_SIGNER.get() {
        // AG-30: per-commit journal signing on encrypted databases too.
        Some(signer) => kernel.with_audit_signer(std::sync::Arc::clone(signer)),
        None => kernel,
    };
    for (type_name, fields) in &enc.policies {
        kernel.set_encryption_policy(type_name, EncryptionPolicy::new(fields.clone()));
    }
    Ok((kernel, admin, Some(ring)))
}

/// Subcommand variant: one config pipeline (R10, PR#2 review SE-02) —
/// encryption AND backend both come from `load()` (defaults → TOML → env;
/// subcommand flags are not server config and are not parsed).
pub(crate) fn open_kernel_auto(
    db_path: &str,
) -> KResult<(Kernel, Option<Arc<dyn StorageAdminApi>>)> {
    let cfg = crate::config::load(&[], None, None).map_err(KError::Store)?;
    open_kernel(db_path, &cfg.encryption, false, None)
}

/// AG-31 — rotate the master KEK of an encrypted database (offline: the
/// database must not be open elsewhere). Every data key is rewrapped under
/// the new KEK — nothing is re-encrypted — and the old KEK unwraps nothing
/// current afterwards (keyring.rs has the crash-safe sequence).
pub(crate) fn rotate_keys(db_path: &str, enc: &RuntimeEncryption) -> KResult<()> {
    if !enc.enabled {
        return Err(KError::Store(
            "key rotation needs [encryption] enabled".into(),
        ));
    }
    let (kernel, _admin, ring) = open_kernel_with_ring(db_path, enc, false, None)?;
    ring.expect("an encrypted open returns its key ring")
        .rotate(&kernel)
}

/// AG-31 — change the passphrase that wraps the master key of an encrypted
/// database (offline: the database must not be open elsewhere). The master
/// key itself does not change — no data key moves — and the old passphrase
/// no longer opens the database.
pub(crate) fn change_passphrase(db_path: &str, enc: &RuntimeEncryption, new: &str) -> KResult<()> {
    if !enc.enabled {
        return Err(KError::Store(
            "passphrase change needs [encryption] enabled".into(),
        ));
    }
    let old = enc
        .passphrase
        .as_deref()
        .ok_or_else(|| KError::Store("passphrase change needs a current passphrase".into()))?;
    let (_kernel, _admin, ring) = open_kernel_with_ring(db_path, enc, false, None)?;
    ring.expect("an encrypted open returns its key ring")
        .change_passphrase(old, new)
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

    // --- AG-31 — kr001: rotation keeps every object readable and retires the
    // old KEK. RED: the KMS mints a new KEK, but the store key is derived
    // from the KEK, so the database no longer opens. ---
    #[test]
    fn kr001_rotation_keeps_every_object_readable_and_retires_the_old_kek() {
        use aikoql_kernel::{Metadata, RememberRequest, Subject, Value};
        let dir = scratch("kr001");
        let key = format!("{dir}.key");
        let enc = super::RuntimeEncryption {
            enabled: true,
            key_path: key.clone(),
            passphrase: Some("pw".into()),
            policies: [("Secret".to_string(), vec!["pin".to_string()])].into(),
        };
        let alice = Subject::new("alice");
        let write = |k: &aikoql_kernel::Kernel, t: &str, body: &str| {
            let mut req = RememberRequest::create(
                alice.clone(),
                Metadata {
                    type_name: t.into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
            );
            req.properties
                .insert("pin".into(), Value::Text(body.into()));
            k.remember(req).unwrap().koid
        };
        let ids: Vec<_> = {
            let (k, _) = super::open_kernel(&dir, &enc, false, None).unwrap();
            (0..20)
                .map(|i| write(&k, "note", &format!("n{i}")))
                .chain((0..5).map(|i| write(&k, "Secret", &format!("{i}{i}{i}{i}"))))
                .collect()
        };
        let old_kek = std::fs::read(&key).unwrap();
        super::rotate_keys(&dir, &enc).unwrap();
        assert_ne!(
            std::fs::read(&key).unwrap(),
            old_kek,
            "a new KEK is in place"
        );
        {
            let (k, _) =
                super::open_kernel(&dir, &enc, false, None).expect("the rotated database opens");
            for (i, id) in ids.iter().enumerate() {
                let ko = k.get(alice.clone(), id).unwrap();
                let want = if i < 20 {
                    format!("n{i}")
                } else {
                    let j = i - 20;
                    format!("{j}{j}{j}{j}")
                };
                assert_eq!(ko.properties.get("pin"), Some(&Value::Text(want)), "{id:?}");
            }
        }
        let new_kek = std::fs::read(&key).unwrap();
        std::fs::write(&key, &old_kek).unwrap();
        assert!(
            super::open_kernel(&dir, &enc, false, None).is_err(),
            "the old KEK unwraps nothing current"
        );
        std::fs::write(&key, &new_kek).unwrap();
        TEMP_PATHS.with(|t| t.borrow_mut().paths.push(key.into()));
    }

    fn secret_enc(dir: &str) -> super::RuntimeEncryption {
        super::RuntimeEncryption {
            enabled: true,
            key_path: format!("{dir}.key"),
            passphrase: Some("pw".into()),
            policies: [("Secret".to_string(), vec!["pin".to_string()])].into(),
        }
    }

    fn ctx_of(tenant: Option<&str>) -> aikoql_kernel::KnowledgeContext {
        let mut subject = aikoql_kernel::Subject::new("alice");
        if let Some(t) = tenant {
            subject = subject.in_tenant(t);
        }
        let mut ctx = aikoql_kernel::KnowledgeContext::new(subject);
        ctx.tenant = tenant.map(String::from);
        ctx
    }

    type Written = (aikoql_kernel::KOID, Option<String>, String);

    /// One object, optionally for a tenant; returns (koid, tenant, pin).
    fn put_secret(k: &aikoql_kernel::Kernel, tenant: Option<&str>, pin: &str) -> Written {
        use aikoql_kernel::{Metadata, RememberRequest, Value};
        let ctx = ctx_of(tenant);
        let mut req = RememberRequest::create(
            ctx,
            Metadata {
                type_name: "Secret".into(),
                tenant: tenant.map(String::from),
                schema_version: 1,
                tags: vec![],
            },
        );
        req.properties.insert("pin".into(), Value::Text(pin.into()));
        (
            k.remember(req).unwrap().koid,
            tenant.map(String::from),
            pin.to_string(),
        )
    }

    fn read_all(dir: &str, enc: &super::RuntimeEncryption, want: &[Written]) {
        let (k, _) = super::open_kernel(dir, enc, false, None).expect("the database opens");
        for (id, tenant, pin) in want {
            let ko = k.get(ctx_of(tenant.as_deref()), id).unwrap();
            assert_eq!(
                ko.properties.get("pin"),
                Some(&aikoql_kernel::Value::Text(pin.clone())),
                "{id:?}"
            );
        }
    }

    /// AG-31 — kr002: a rotation stopped after any step (the crash windows)
    /// reopens with every object readable, and a later rotation succeeds.
    #[test]
    fn kr002_an_interrupted_rotation_recovers_at_every_step() {
        for step in 1..=5 {
            let dir = scratch(&format!("kr002-{step}"));
            let enc = secret_enc(&dir);
            TEMP_PATHS.with(|t| t.borrow_mut().paths.push(enc.key_path.clone().into()));
            let want: Vec<_> = {
                let (k, _) = super::open_kernel(&dir, &enc, false, None).unwrap();
                (0..4)
                    .map(|i| put_secret(&k, [None, Some("acme")][i % 2], &format!("p{i}")))
                    .collect()
            };
            {
                let (k, _, ring) = super::open_kernel_with_ring(&dir, &enc, false, None).unwrap();
                let err = ring
                    .unwrap()
                    .rotate_steps(&k, &mut |n| {
                        if n == step {
                            Err(aikoql_kernel::KError::Store("injected stop".into()))
                        } else {
                            Ok(())
                        }
                    })
                    .unwrap_err();
                assert!(
                    err.to_string().contains("injected stop"),
                    "step {step}: {err}"
                );
            }
            read_all(&dir, &enc, &want);
            super::rotate_keys(&dir, &enc).unwrap_or_else(|e| panic!("step {step}: {e}"));
            read_all(&dir, &enc, &want);
        }
    }

    /// AG-31 — kr003: rotation online, under write load — writers keep
    /// committing field-encrypted objects (new tenants included: a DEK
    /// minted mid-rotation must land in the new generation).
    #[test]
    fn kr003_rotation_under_write_load_loses_nothing() {
        let dir = scratch("kr003");
        let enc = secret_enc(&dir);
        TEMP_PATHS.with(|t| t.borrow_mut().paths.push(enc.key_path.clone().into()));
        let want = {
            let (k, _, ring) = super::open_kernel_with_ring(&dir, &enc, false, None).unwrap();
            let ring = ring.unwrap();
            let k = &k;
            std::thread::scope(|s| {
                let writers: Vec<_> = (0..4)
                    .map(|w| {
                        s.spawn(move || {
                            (0..40)
                                .map(|i| {
                                    let tenant = format!("t{w}-{}", i / 10); // new tenants as it goes
                                    put_secret(k, Some(&tenant), &format!("{w}:{i}"))
                                })
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                ring.rotate(k).unwrap();
                ring.rotate(k).unwrap(); // twice, while the writers run
                writers
                    .into_iter()
                    .flat_map(|h| h.join().unwrap())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(want.len(), 160);
        read_all(&dir, &enc, &want);
    }

    /// AG-31 — kr004 (deterministic interleave): a writer minting a NEW
    /// tenant's DEK inside the rotation window (after the rewrap snapshot,
    /// before the adopt) must wait for the rotation — otherwise its DEK is
    /// missing from the new generation and its object is unreadable.
    #[test]
    fn kr004_a_dek_minted_mid_rotation_lands_in_the_new_generation() {
        let dir = scratch("kr004");
        let enc = secret_enc(&dir);
        TEMP_PATHS.with(|t| t.borrow_mut().paths.push(enc.key_path.clone().into()));
        let mut want = Vec::new();
        {
            let (k, _, ring) = super::open_kernel_with_ring(&dir, &enc, false, None).unwrap();
            want.push(put_secret(&k, Some("old"), "before"));
            let k = &k;
            let late = std::thread::scope(|s| {
                let mut writer = None;
                ring.unwrap()
                    .rotate_steps(k, &mut |step| {
                        if step == 3 {
                            writer = Some(s.spawn(move || put_secret(k, Some("brand-new"), "mid")));
                            // a guarded rotation blocks this writer; give an
                            // unguarded one the time to let it through
                            std::thread::sleep(std::time::Duration::from_millis(300));
                        }
                        Ok(())
                    })
                    .unwrap();
                writer.unwrap().join().unwrap()
            });
            want.push(late);
        }
        read_all(&dir, &enc, &want);
    }

    /// AG-31 — kr005: a passphrase change re-wraps the master key file (the
    /// key itself never changes: every object — field ciphertext included —
    /// stays readable), the old passphrase stops opening the database, and a
    /// later KEK rotation wraps under the new passphrase.
    #[test]
    fn kr005_passphrase_change_retires_the_old_passphrase_and_keeps_everything_readable() {
        let dir = scratch("kr005");
        let enc = secret_enc(&dir);
        TEMP_PATHS.with(|t| t.borrow_mut().paths.push(enc.key_path.clone().into()));
        let want = {
            let (k, _) = super::open_kernel(&dir, &enc, false, None).unwrap();
            vec![
                put_secret(&k, Some("t1"), "alpha"),
                put_secret(&k, None, "beta"),
            ]
        };
        let key_file = std::fs::read(&enc.key_path).unwrap();

        super::change_passphrase(&dir, &enc, "pw-new").unwrap();
        assert_ne!(
            std::fs::read(&enc.key_path).unwrap(),
            key_file,
            "the envelope is rewrapped"
        );

        let enc_new =
            super::RuntimeEncryption { passphrase: Some("pw-new".into()), ..enc.clone() };
        read_all(&dir, &enc_new, &want);

        // The old passphrase no longer opens the database.
        assert!(super::open_kernel(&dir, &enc, false, None).is_err());

        // A later KEK rotation wraps under the NEW passphrase (the ring's
        // copy must stay in sync, or the rotated key file is unreadable).
        super::rotate_keys(&dir, &enc_new).unwrap();
        read_all(&dir, &enc_new, &want);
    }
}
