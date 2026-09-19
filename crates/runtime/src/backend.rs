//! PR6-005 — the ONE authoritative storage-backend decision path
//! (docs/STORAGE-BACKENDS.md, 2026-09-07 ADR).
//!
//! Every production opener (aikoql-mcp, the python SDK) routes through
//! `open_engine` here. The contract:
//!
//! ```text
//! explicit backend            → always use the explicit backend
//! no explicit backend         → detect the existing on-disk format
//! missing path                → fresh aikoql-v2 create (the default)
//! ```
//!
//! Detection: a directory with `CURRENT` is aikoql-v2; a file with the
//! native WAL magic ("AKQL") is aikoql; any other existing FILE falls
//! through to redb (redb validates its own header and fails closed on
//! anything else); a directory that is not a v2 database is an explicit
//! error — never a silent fresh create.

use aikoql_kernel::storage::store::StorageEngine;
use aikoql_kernel::storage::store_redb::RedbEngine;
use aikoql_kernel::{KError, KResult};
use aikoql_storage::AikoqlStorageEngine;
use aikoql_storage_v2::engine::StorageAdminApi;
use aikoql_storage_v2::AikoqlStorageEngineV2;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

/// An opened engine plus its optional design §22 admin capability (P3-M2 —
/// only aikoql-v2 implements StorageAdminApi today).
pub type Opened = (Arc<dyn StorageEngine>, Option<Arc<dyn StorageAdminApi>>);

/// The storage backends an opener can select. Parsing accepts exactly
/// `"redb"`, `"aikoql"`, `"aikoql-v2"` — anything else fails closed at the
/// config layer, before any file is touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Redb,
    Aikoql,
    AikoqlV2,
}

impl Backend {
    pub fn parse(v: &str) -> Result<Backend, String> {
        match v {
            "redb" => Ok(Backend::Redb),
            "aikoql" => Ok(Backend::Aikoql),
            "aikoql-v2" => Ok(Backend::AikoqlV2),
            other => Err(format!(
                "unknown storage backend {other:?}: use \"redb\", \"aikoql\" or \"aikoql-v2\""
            )),
        }
    }
}

/// Open the engine at `path`. An explicit backend opens exactly that
/// engine (the value already failed closed at the config layer); `None`
/// auto-detects via `detect_backend`.
pub fn open_engine(path: &Path, backend: Option<Backend>) -> KResult<Opened> {
    let backend = match backend {
        Some(b) => b,
        None => detect_backend(path)?,
    };
    let p = path.to_string_lossy();
    match backend {
        Backend::Redb => Ok((Arc::new(RedbEngine::open(p.as_ref())?), None)),
        Backend::Aikoql => Ok((Arc::new(AikoqlStorageEngine::open(p.as_ref())?), None)),
        Backend::AikoqlV2 => {
            // P3-M2 (design §22): extract the admin capability from the
            // CONCRETE engine before the StorageEngine coercion — only v2
            // implements it today.
            let e = Arc::new(AikoqlStorageEngineV2::open(p.as_ref())?);
            let admin: Arc<dyn StorageAdminApi> = e.clone();
            Ok((e, Some(admin)))
        }
    }
}

/// Sniff the on-disk format. A <4-byte or non-AKQL file falls through to
/// redb, whose own header validation fails closed — the native WAL parser
/// never truncates or reinterprets a non-AKQL file. A missing path is a
/// fresh aikoql-v2 create (the 2026-09-07 default flip).
pub fn detect_backend(path: &Path) -> KResult<Backend> {
    if path.is_dir() {
        if path.join("CURRENT").is_file() {
            return Ok(Backend::AikoqlV2);
        }
        return Err(KError::Store(format!(
            "{} is a directory but not an aikoql-v2 database (no CURRENT): \
             name an explicit backend (--backend / AIKOQL_BACKEND / storage.backend)",
            path.display()
        )));
    }
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut magic = [0u8; 4];
            if f.read(&mut magic).ok() == Some(4) && &magic == b"AKQL" {
                return Ok(Backend::Aikoql);
            }
            Ok(Backend::Redb)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Backend::AikoqlV2),
        Err(e) => Err(KError::Store(format!("read {}: {e}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::storage::store::WriteBatch;

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
                if let Ok(rd) = std::fs::read_dir(p.parent().unwrap_or(Path::new("."))) {
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

    fn scratch(tag: &str) -> std::path::PathBuf {
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
                if !name.starts_with("aikoql_backend_") {
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
        p.push(format!("aikoql_backend_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_file(&p);
        TEMP_PATHS.with(|t| t.borrow_mut().paths.push(p.clone()));
        p
    }

    fn put_get(engine: &Arc<dyn StorageEngine>) {
        let mut b = WriteBatch::new();
        b.put(b"k".to_vec(), b"v".to_vec());
        engine.write_batch(&b).unwrap();
        assert_eq!(engine.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    /// PR6-005 — the review's five named cases.
    #[test]
    fn empty_path_defaults_to_v2() {
        let p = scratch("empty-v2");
        let (engine, _admin) = open_engine(&p, None).unwrap();
        put_get(&engine);
        drop(engine);
        assert!(
            p.join("CURRENT").is_file(),
            "a missing path must create a v2 database directory with CURRENT"
        );
    }

    #[test]
    fn existing_redb_autodetects_redb() {
        let p = scratch("redb-existing");
        {
            let e = RedbEngine::open(&p).unwrap();
            let mut b = WriteBatch::new();
            b.put(b"k".to_vec(), b"v".to_vec());
            e.write_batch(&b).unwrap();
        }
        let (engine, _admin) = open_engine(&p, None).unwrap();
        put_get(&engine);
        drop(engine); // redb holds a live file lock — read the head bytes after close
        let mut head = [0u8; 4];
        std::fs::File::open(&p)
            .unwrap()
            .read_exact(&mut head)
            .unwrap();
        assert_ne!(
            &head, b"AKQL",
            "the redb file must not be rewritten as a native WAL"
        );
    }

    #[test]
    fn existing_aikoql_v1_autodetects_v1() {
        let p = scratch("v1-existing");
        {
            let e = AikoqlStorageEngine::open(&p).unwrap();
            let mut b = WriteBatch::new();
            b.put(b"k".to_vec(), b"v".to_vec());
            e.write_batch(&b).unwrap();
        }
        let (engine, _admin) = open_engine(&p, None).unwrap();
        put_get(&engine);
    }

    #[test]
    fn existing_aikoql_v2_autodetects_v2() {
        let p = scratch("v2-existing");
        {
            let e = AikoqlStorageEngineV2::open(&p).unwrap();
            let mut b = WriteBatch::new();
            b.put(b"k".to_vec(), b"v".to_vec());
            e.write_batch(&b).unwrap();
        }
        let (engine, _admin) = open_engine(&p, None).unwrap();
        put_get(&engine);
    }

    #[test]
    fn explicit_backend_overrides_detection() {
        // A fresh path would DETECT as v2 — an explicit backend must win.
        let p = scratch("explicit-redb");
        let (engine, _admin) = open_engine(&p, Some(Backend::Redb)).unwrap();
        put_get(&engine);
        drop(engine);
        assert!(
            p.is_file() && !p.join("CURRENT").is_file(),
            "explicit redb must create a redb file, not a v2 directory"
        );

        let p = scratch("explicit-v1");
        let (engine, _admin) = open_engine(&p, Some(Backend::Aikoql)).unwrap();
        put_get(&engine);
        drop(engine);
        let mut head = [0u8; 4];
        std::fs::File::open(&p)
            .unwrap()
            .read_exact(&mut head)
            .unwrap();
        assert_eq!(&head, b"AKQL", "explicit aikoql must create a native WAL");

        let p = scratch("explicit-v2");
        let (engine, _admin) = open_engine(&p, Some(Backend::AikoqlV2)).unwrap();
        put_get(&engine);
        drop(engine);
        assert!(p.join("CURRENT").is_file());
    }

    /// A directory that is NOT a v2 database fails closed — never a
    /// silent fresh create.
    #[test]
    fn non_v2_directory_fails_closed() {
        let p = scratch("plain-dir");
        std::fs::create_dir_all(&p).unwrap();
        let err = match open_engine(&p, None) {
            Err(e) => e,
            Ok(_) => panic!("a non-v2 directory must fail closed, not become a fresh store"),
        };
        assert!(
            format!("{err}").contains("not an aikoql-v2 database"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_accepts_exact_names_and_fails_closed() {
        assert_eq!(Backend::parse("redb"), Ok(Backend::Redb));
        assert_eq!(Backend::parse("aikoql"), Ok(Backend::Aikoql));
        assert_eq!(Backend::parse("aikoql-v2"), Ok(Backend::AikoqlV2));
        assert!(Backend::parse("rocksdb")
            .unwrap_err()
            .contains("unknown storage backend"));
    }
}
