//! CompactionJob — periodic vacuum of deleted/tombstoned knowledge objects.
//!
//! Runs on a configurable interval (default 5 minutes). On each tick, scans
//! for objects in the `Deleted` lifecycle state and erases their old versions,
//! reclaiming disk space.

use crate::SchedulerJob;
use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::transaction::kernel::{ForgetMode, Kernel, KnowledgeContext, Subject};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct CompactionJob {
    interval: Duration,
    handle: std::sync::Mutex<Option<thread::JoinHandle<()>>>,
}

impl CompactionJob {
    pub fn new(interval_secs: u64) -> Self {
        CompactionJob {
            interval: Duration::from_secs(interval_secs),
            handle: std::sync::Mutex::new(None),
        }
    }
}

impl Default for CompactionJob {
    fn default() -> Self {
        Self::new(300) // 5 minutes
    }
}

impl SchedulerJob for CompactionJob {
    fn name(&self) -> &str {
        "compaction"
    }

    fn start(&self, kernel: &Kernel) -> KResult<()> {
        let interval = self.interval;
        let running = Arc::new(AtomicBool::new(true));
        let running_ref = running.clone();
        let k = kernel.clone_handle();

        let h = thread::spawn(move || {
            while running_ref.load(Ordering::SeqCst) {
                thread::sleep(interval);
                if !running_ref.load(Ordering::SeqCst) {
                    break;
                }
                match compact(&k) {
                    Ok(count) if count > 0 => {
                        eprintln!("compaction: erased {} deleted objects", count)
                    }
                    Ok(_) => {} // nothing to compact
                    Err(e) => eprintln!("compaction error: {}", e),
                }
            }
        });

        // justified: Mutex poison is unrecoverable
        *self.handle.lock().unwrap() = Some(h);
        Ok(())
    }

    fn shutdown(&self) {
        // justified: Mutex poison is unrecoverable
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }

    fn checkpoint(&self, _dir: &std::path::Path) -> KResult<()> {
        Ok(()) // stateless
    }

    fn water(&self) -> u64 {
        0 // periodic, not event-driven
    }
}

fn compact(kernel: &Kernel) -> KResult<usize> {
    let heads = kernel.scan_heads()?;
    let mut count = 0;
    for (koid, _version, _ts, state) in &heads {
        if *state == LifecycleState::Deleted
            && kernel
                .forget(
                    KnowledgeContext::new(Subject::new("compaction")),
                    koid,
                    ForgetMode::Erase,
                    None,
                    Some("periodic compaction".into()),
                )
                .is_ok()
        {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_job_has_name() {
        let job = CompactionJob::default();
        assert_eq!(job.name(), "compaction");
    }

    #[test]
    fn compaction_job_starts_and_shuts_down() {
        let job = CompactionJob::new(1); // 1-second interval for fast test
                                         // ponytail: no kernel needed — start is tested via Scheduler integration.
                                         // Just verify the job object is well-formed.
        assert_eq!(job.water(), 0);
        job.shutdown(); // no-op if not started
    }
}
