//! The storage-backend decision path — v2-only since the launch S-02
//! decommission (docs/IMPLEMENTATION-PLAN-LAUNCH.md). Every production
//! opener (aikoql-mcp, the python SDK) routes through `open_engine` here.
//!
//! ```text
//! missing path                → fresh aikoql-v2 create (the default)
//! directory with CURRENT      → existing aikoql-v2 database
//! directory without CURRENT   → error, never a silent fresh create
//! any existing FILE           → error: a legacy single-file database
//! ```
//!
//! A legacy FILE fails closed: the v1 WAL migrator
//! (`aikoql_storage_v2::migration::migrate_v1_wal`) is the only supported
//! path from a legacy single-file database; nothing opens those files
//! anymore (launch S-02).

use aikoql_kernel::storage::store::StorageEngine;
use aikoql_kernel::{KError, KResult};
use aikoql_storage_v2::engine::StorageAdminApi;
use aikoql_storage_v2::AikoqlStorageEngineV2;
use std::path::Path;
use std::sync::Arc;

/// An opened engine plus its design §22 admin capability (P3-M2).
pub type Opened = (Arc<dyn StorageEngine>, Option<Arc<dyn StorageAdminApi>>);

/// Open the engine at `path` — the ONE opener.
pub fn open_engine(path: &Path) -> KResult<Opened> {
    if path.is_dir() && !path.join("CURRENT").is_file() {
        return Err(KError::Store(format!(
            "{} is a directory but not an aikoql-v2 database (no CURRENT)",
            path.display()
        )));
    }
    if path.exists() && !path.is_dir() {
        return Err(KError::Store(format!(
            "{} is a legacy storage file — storage V2 is the only engine \
             (launch S-02): migrate a v1 WAL with \
             aikoql_storage_v2::migration::migrate_v1_wal, or start fresh at \
             a new path",
            path.display()
        )));
    }
    // P3-M2 (design §22): extract the admin capability from the CONCRETE
    // engine before the StorageEngine coercion.
    let e = Arc::new(AikoqlStorageEngineV2::open(
        path.to_string_lossy().as_ref(),
    )?);
    let admin: Arc<dyn StorageAdminApi> = e.clone();
    Ok((e, Some(admin)))
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
                // Sidecars next to the registered stem.
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

    #[test]
    fn empty_path_creates_fresh_v2() {
        let p = scratch("empty-v2");
        let (engine, admin) = open_engine(&p).unwrap();
        assert!(admin.is_some(), "v2 always carries the admin capability");
        put_get(&engine);
        drop(engine);
        assert!(
            p.join("CURRENT").is_file(),
            "a missing path must create a v2 database directory with CURRENT"
        );
    }

    #[test]
    fn existing_v2_reopens() {
        let p = scratch("v2-existing");
        {
            let e = AikoqlStorageEngineV2::open(&p).unwrap();
            let mut b = WriteBatch::new();
            b.put(b"k".to_vec(), b"v".to_vec());
            e.write_batch(&b).unwrap();
        }
        let (engine, _admin) = open_engine(&p).unwrap();
        put_get(&engine);
    }

    /// A legacy FILE (v1 database or WAL) fails closed with the
    /// migration story — never silently rewritten as v2.
    #[test]
    fn legacy_file_fails_closed() {
        let p = scratch("legacy-file");
        std::fs::write(&p, b"AKQL\x01\x00\x01\x00\x00\x00\x00").unwrap();
        let err = match open_engine(&p) {
            Err(e) => e,
            Ok(_) => panic!("a legacy file must fail closed, not open as v2"),
        };
        assert!(
            format!("{err}").contains("legacy storage file"),
            "got: {err}"
        );
        assert!(
            !p.join("CURRENT").is_file(),
            "the legacy file must stay untouched"
        );
    }

    /// A directory that is NOT a v2 database fails closed — never a
    /// silent fresh create.
    #[test]
    fn non_v2_directory_fails_closed() {
        let p = scratch("plain-dir");
        std::fs::create_dir_all(&p).unwrap();
        let err = match open_engine(&p) {
            Err(e) => e,
            Ok(_) => panic!("a non-v2 directory must fail closed, not become a fresh store"),
        };
        assert!(
            format!("{err}").contains("not an aikoql-v2 database"),
            "got: {err}"
        );
    }
}
