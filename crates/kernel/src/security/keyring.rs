//! AG-31 — the key ring: the master KEK (from the KMS) wraps every data key,
//! so the KEK can rotate without re-encrypting a byte of data.
//!
//! Before AG-31 the store was encrypted with HKDF(KEK, store) — derived from
//! the KEK itself — so a new KEK made the whole database unreadable. Now the
//! store key is a data key wrapped by the KEK in `<key_path>.store-dek`; an
//! existing database keeps its derived value as that data key (nothing to
//! re-encrypt), wrapped once on first open.
//!
//! Rotation (`KeyRing::rotate`) is crash-safe by generations: every wrap
//! under the NEW KEK is durable BEFORE the KMS installs it (the commit
//! point), and on open the generation that authenticates under the
//! installed KEK wins (AEAD: a wrap made by another KEK fails to decrypt).
//!
//!   1. mint the new KEK (in memory only)
//!   2. store key wrapped by it -> `<dek>.next` (atomic write)
//!   3. tenant DEKs rewrapped by it -> DEKS_NEXT_KEY (in the store)
//!   4. KMS install (atomic rename) — the commit point
//!   5. tenant set promoted, the envelope adopts the new KEK
//!   6. `<dek>.next` renamed over `<dek>`
//!
//! A crash before 4 leaves the old KEK, whose wraps are untouched; after 4,
//! open finds the next generation and finishes 5–6. The old KEK unwraps
//! nothing current once a rotation commits.

use crate::knowledge::kom::{KError, KResult};
use crate::security::crypto::Crypto;
use crate::security::hkdf::{self, DOMAIN_STORE, DOMAIN_STORE_WRAP};
use crate::security::KeyManager;
use crate::transaction::kernel::Kernel;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const STORE_DEK_MAGIC: &[u8] = b"AKSD1";
const STORE_DEK_AAD: &[u8] = b"aikoql/store-dek/v1";

/// Write `bytes` to `path` atomically: a temp file in the same directory,
/// fsync, rename over the target, fsync the directory.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(d) = dir {
        std::fs::create_dir_all(d).map_err(|e| format!("create {}: {e}", d.display()))?;
    }
    let tmp = path.with_extension(format!(
        "{}tmp",
        path.extension()
            .map(|e| format!("{}.", e.to_string_lossy()))
            .unwrap_or_default()
    ));
    {
        let mut f =
            std::fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    sync_dir(dir)
}

fn sync_dir(dir: Option<&Path>) -> Result<(), String> {
    // Windows cannot open a directory for fsync; the rename is durable there
    #[cfg(unix)]
    {
        let d = dir.unwrap_or(Path::new("."));
        std::fs::File::open(d)
            .and_then(|f| f.sync_all())
            .map_err(|e| format!("fsync {}: {e}", d.display()))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

pub struct KeyRing {
    kms: Arc<dyn KeyManager>,
    passphrase: String,
    dek_path: PathBuf,
    crypto: Arc<Crypto>,
    store_key: [u8; 32],
    rotating: Mutex<()>,
}

impl KeyRing {
    /// The next generation's store-key file.
    fn next_path(&self) -> PathBuf {
        next_of(&self.dek_path)
    }

    /// Resolve the store key under the KMS's current KEK, finishing an
    /// interrupted rotation (a `.next` that authenticates is promoted, one
    /// that does not is a rotation that never committed). A database with
    /// no store-key file keeps HKDF(KEK, store) — the pre-AG-31 value.
    pub fn open(
        kms: Arc<dyn KeyManager>,
        passphrase: &str,
        dek_path: impl Into<PathBuf>,
        crypto: Arc<Crypto>,
    ) -> Result<Arc<Self>, String> {
        let dek_path = dek_path.into();
        let kek = kms.master_key(passphrase)?;
        let next = next_of(&dek_path);
        let store_key = if let Some(k) = read_wrapped(&crypto, &kek, &next)? {
            std::fs::rename(&next, &dek_path).map_err(|e| format!("promote store key: {e}"))?;
            sync_dir(dek_path.parent())?;
            k
        } else {
            let _ = std::fs::remove_file(&next); // an uncommitted rotation's wrap
            match read_wrapped(&crypto, &kek, &dek_path)? {
                Some(k) => k,
                None if dek_path.exists() => {
                    return Err("the store key does not unwrap under this master key".into())
                }
                None => {
                    let k = hkdf::domain_sep(&kek, DOMAIN_STORE);
                    write_atomic(&dek_path, &wrap(&crypto, &kek, &k)?)?;
                    k
                }
            }
        };
        Ok(Arc::new(KeyRing {
            kms,
            passphrase: passphrase.to_string(),
            dek_path,
            crypto,
            store_key,
            rotating: Mutex::new(()),
        }))
    }

    /// The data key the store is encrypted with — constant across rotations.
    pub fn store_key(&self) -> [u8; 32] {
        self.store_key
    }

    /// Rotate the master KEK online (writers keep running: no data key
    /// changes). `kernel` carries the field-encryption envelope whose tenant
    /// DEKs are rewrapped.
    pub fn rotate(&self, kernel: &Kernel) -> KResult<()> {
        self.rotate_steps(kernel, &mut |_| Ok(()))
    }

    /// `rotate` with a hook after each step (1–5): an `Err` stops the
    /// rotation there (the crash windows), and a hook may interleave other
    /// work deterministically (the tests' park point).
    #[doc(hidden)]
    pub fn rotate_steps(
        &self,
        kernel: &Kernel,
        after: &mut dyn FnMut(usize) -> KResult<()>,
    ) -> KResult<()> {
        let err = |e: String| KError::Store(format!("key rotation: {e}"));
        let mut stop = |step: usize| after(step);
        // justified: Mutex poison is unrecoverable
        let _one = self.rotating.lock().unwrap();
        let envelope = kernel.field_envelope();
        let _no_new_deks = envelope.as_ref().map(|e| e.creation_guard());
        let new_kek = self.crypto.generate_key(); // 1
        stop(1)?;
        write_atomic(
            &self.next_path(),
            &wrap(&self.crypto, &new_kek, &self.store_key).map_err(err)?,
        )
        .map_err(err)?; // 2
        stop(2)?;
        let wraps = match &envelope {
            Some(e) => Some(e.rewrapped(&new_kek).map_err(err)?),
            None => None,
        };
        if let Some(w) = &wraps {
            kernel.persist_next_deks(w)?; // 3
        }
        stop(3)?;
        self.kms.install(&new_kek, &self.passphrase).map_err(err)?; // 4: commit
        stop(4)?;
        if let (Some(e), Some(w)) = (&envelope, wraps) {
            kernel.promote_next_deks(&w)?; // 5
            e.adopt(new_kek, w);
        }
        stop(5)?;
        std::fs::rename(self.next_path(), &self.dek_path)
            .map_err(|e| err(format!("promote store key: {e}")))?; // 6
        sync_dir(self.dek_path.parent()).map_err(err)
    }
}

fn next_of(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".next");
    PathBuf::from(s)
}

fn wrap(crypto: &Crypto, kek: &[u8; 32], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let wk = hkdf::domain_sep(kek, DOMAIN_STORE_WRAP);
    let mut out = STORE_DEK_MAGIC.to_vec();
    out.extend(crypto.encrypt(&wk, key, STORE_DEK_AAD)?);
    Ok(out)
}

/// The store key in `path` if it exists and authenticates under `kek`.
fn read_wrapped(crypto: &Crypto, kek: &[u8; 32], path: &Path) -> Result<Option<[u8; 32]>, String> {
    let Ok(raw) = std::fs::read(path) else {
        return Ok(None);
    };
    let Some(body) = raw.strip_prefix(STORE_DEK_MAGIC) else {
        return Err(format!("{}: not a store-key file", path.display()));
    };
    let wk = hkdf::domain_sep(kek, DOMAIN_STORE_WRAP);
    match crypto.decrypt(&wk, body, STORE_DEK_AAD) {
        Ok(k) => Ok(Some(k.as_slice().try_into().map_err(|_| {
            format!("{}: store key is not 32 bytes", path.display())
        })?)),
        Err(_) => Ok(None),
    }
}
