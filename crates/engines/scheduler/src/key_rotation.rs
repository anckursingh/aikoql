//! KeyRotationJob — periodic KEK rotation (MRFC-0020 Phase 2, AG-31).
//!
//! Every `interval` the job rotates the master KEK through the database's
//! `KeyRing`: every data key is rewrapped under a new KEK and nothing is
//! re-encrypted, so writers keep running (keyring.rs has the crash-safe
//! sequence).

use crate::SchedulerJob;
use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::security::keyring::KeyRing;
use aikoql_kernel::transaction::kernel::Kernel;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How often the sleeping thread checks for shutdown.
const TICK: Duration = Duration::from_millis(50);

pub struct KeyRotationJob {
    interval: Duration,
    ring: Arc<KeyRing>,
    running: Arc<AtomicBool>,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl KeyRotationJob {
    /// `ring` — the key ring of the open encrypted database.
    /// `interval_secs` — how often to rotate (e.g., 86400 = daily).
    pub fn new(ring: Arc<KeyRing>, interval_secs: u64) -> Self {
        KeyRotationJob {
            interval: Duration::from_secs(interval_secs),
            ring,
            running: Arc::new(AtomicBool::new(false)),
            handle: Mutex::new(None),
        }
    }
}

impl SchedulerJob for KeyRotationJob {
    fn name(&self) -> &str {
        "key-rotation"
    }

    fn start(&self, kernel: &Kernel) -> KResult<()> {
        let (interval, ring, kernel) = (self.interval, self.ring.clone(), kernel.clone());
        let running = self.running.clone();
        running.store(true, Ordering::SeqCst);
        let h = thread::spawn(move || {
            let mut due = Instant::now() + interval;
            while running.load(Ordering::SeqCst) {
                thread::sleep(TICK);
                if Instant::now() < due {
                    continue;
                }
                due = Instant::now() + interval;
                if let Err(e) = ring.rotate(&kernel) {
                    eprintln!("key-rotation: {e}");
                }
            }
        });
        // justified: Mutex poison is unrecoverable
        *self.handle.lock().unwrap() = Some(h);
        Ok(())
    }

    fn shutdown(&self) {
        self.running.store(false, Ordering::SeqCst);
        // justified: Mutex poison is unrecoverable
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }

    fn checkpoint(&self, _kernel: &Kernel, _dir: &std::path::Path) -> KResult<()> {
        Ok(())
    }

    fn water(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::security::crypto::{Aes256Gcm, Crypto};
    use aikoql_kernel::security::kms::LocalKms;

    fn ring(tag: &str) -> (Arc<KeyRing>, std::path::PathBuf) {
        let key = std::env::temp_dir().join(format!("aikoql-kr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&key);
        let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
        let kms = Arc::new(LocalKms::new(key.to_str().unwrap()));
        let dek = key.with_extension("store-dek");
        let _ = std::fs::remove_file(&dek);
        (KeyRing::open(kms, "pw", dek, crypto).unwrap(), key)
    }

    fn kernel() -> Kernel {
        Kernel::open(
            Arc::new(aikoql_kernel::MemoryEngine::new()),
            Arc::new(aikoql_kernel::ManualClock::new(1)),
            1,
        )
        .unwrap()
    }

    #[test]
    fn key_rotation_job_has_name_and_starts() {
        let (r, key) = ring("name");
        let job = KeyRotationJob::new(r, 3600);
        assert_eq!(job.name(), "key-rotation");
        assert_eq!(job.water(), 0);
        job.shutdown(); // no-op if not started
        let _ = std::fs::remove_file(&key);
    }

    /// AG-31 — shutdown of a started job returns (it used to hang), and
    /// the job really rotates: the master key file changes each interval.
    #[test]
    fn kr_job_shutdown_returns_promptly() {
        let (r, key) = ring("sd");
        let before = std::fs::read(&key).unwrap();
        let job = Arc::new(KeyRotationJob::new(r, 1));
        job.start(&kernel()).unwrap();
        let t = Instant::now();
        while std::fs::read(&key).unwrap() == before && t.elapsed() < Duration::from_secs(5) {
            thread::sleep(TICK);
        }
        assert_ne!(
            std::fs::read(&key).unwrap(),
            before,
            "the job rotated the KEK"
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let j = job.clone();
        thread::spawn(move || {
            j.shutdown();
            let _ = tx.send(());
        });
        let done = rx.recv_timeout(Duration::from_secs(3)).is_ok();
        let _ = std::fs::remove_file(&key);
        let _ = std::fs::remove_file(key.with_extension("store-dek"));
        assert!(done, "shutdown of a started job must return");
    }
}
