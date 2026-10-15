//! Shared POC-4 T-72 test hygiene: temp paths created by THIS test thread
//! are swept when the thread exits — including `{stem}.`-prefixed sidecars
//! like the `{db}.audit.log` the server writes next to every DB — so a panic
//! mid-test cannot leave scratch DBs or audit logs behind in the shared temp
//! dir. (Per-thread on purpose: the statics are not dropped on Windows MSVC,
//! TLS is; and a child server process must never delete paths it did not
//! create — a thread only ever sweeps what it registered.)

#![allow(dead_code)]

use std::path::{Path, PathBuf};

thread_local! {
    static TEMP_PATHS: std::cell::RefCell<TempSweeper> =
        const { std::cell::RefCell::new(TempSweeper { paths: Vec::new() }) };
}

pub struct TempSweeper {
    pub paths: Vec<PathBuf>,
}

impl Drop for TempSweeper {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_dir_all(p);
            // Sidecars created NEXT TO the registered stem (`{stem}.audit.log`):
            // the stem is pid+counter-unique, so a `{stem}.` prefix match is
            // own-files only.
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

/// Register a path this thread created: it (and any `{stem}.`-prefixed
/// sidecar) is removed when the thread exits, even on panic.
pub fn register_temp_path(p: &Path) {
    TEMP_PATHS.with(|t| t.borrow_mut().paths.push(p.to_path_buf()));
}
