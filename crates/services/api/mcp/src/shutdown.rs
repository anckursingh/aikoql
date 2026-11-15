//! PROD-3 — stopping on purpose: SIGTERM / SIGINT (unix) and Ctrl-C /
//! Ctrl-Break / console close (Windows) end `serve` the way an operator
//! or an orchestrator expects, instead of as a crash.
//!
//! The order is the safety argument:
//! 1. stop accepting work (`SHUTDOWN_FLAG` — the accept loops and the
//!    stdio loop end, the drain cancels in-flight requests);
//! 2. fence client writes in the engine — nothing is acknowledged after
//!    this, on any surface;
//! 3. an HA leader waits (bounded) for its standby to hold the final seq,
//!    then marks its lease released at that seq (`ha::Held::release`), so
//!    the standby takes over on its next poll instead of waiting out a TTL;
//! 4. exit 0.
//!
//! A release that fails (the lease store is unreachable, or the lease was
//! already taken) changes nothing: the lease lapses as it would for a
//! crash. The handlers only set a flag; a watcher thread does the work.
//! No dependency: the two OS entry points are declared here.

use crate::ha;
use crate::transport::SHUTDOWN_FLAG;
use aikoql_storage_v2::engine::StorageAdminApi;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Set by the OS handler; read by the watcher.
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// How often the watcher looks at the flag.
const POLL: Duration = Duration::from_millis(50);
/// The most a stopping leader waits for its standby to hold the final seq.
const ACK_WAIT: Duration = Duration::from_secs(5);

#[cfg(unix)]
mod os {
    use super::{Ordering, REQUESTED};

    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;

    extern "C" {
        // POSIX signal(2); the handler is passed as its address
        fn signal(signum: i32, handler: usize) -> usize;
    }

    /// Async-signal-safe: one atomic store.
    extern "C" fn on_signal(_signum: i32) {
        REQUESTED.store(true, Ordering::SeqCst);
    }

    pub(super) fn install() {
        // SAFETY: `signal` is the libc function std already links; the
        // handler is an `extern "C" fn(i32)` that only stores an atomic,
        // which is async-signal-safe.
        unsafe {
            signal(SIGTERM, on_signal as extern "C" fn(i32) as usize);
            signal(SIGINT, on_signal as extern "C" fn(i32) as usize);
        }
    }
}

#[cfg(windows)]
mod os {
    use super::{Ordering, REQUESTED};

    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    /// Runs on a thread the console creates. Returning TRUE marks the
    /// event handled, so the default handler does not kill the process
    /// before the watcher finishes.
    unsafe extern "system" fn on_ctrl(_ctrl_type: u32) -> i32 {
        REQUESTED.store(true, Ordering::SeqCst);
        1
    }

    pub(super) fn install() {
        // SAFETY: the documented kernel32 call with a handler of the
        // documented signature; the handler only stores an atomic.
        unsafe {
            SetConsoleCtrlHandler(Some(on_ctrl), 1);
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod os {
    pub(super) fn install() {}
}

/// What a stop has to settle before the process may exit.
#[derive(Clone, Default)]
pub(crate) struct Stop {
    /// The storage admin surface — the write fence.
    pub(crate) admin: Option<Arc<dyn StorageAdminApi>>,
    /// The lease this node leads on, when it is an HA leader.
    pub(crate) held: Option<ha::Held>,
    /// The HA lease TTL: the bound on waiting for the renewer.
    pub(crate) ttl: Duration,
}

impl Stop {
    /// Steps 2–3 of the module header. Idempotent: a second call finds the
    /// engine fenced and the lease already released.
    pub(crate) fn settle(&self, why: &str) {
        crate::node::set_stopping(); // PROD-4: the probes withdraw readiness
        let Some(admin) = &self.admin else {
            return;
        };
        admin.fence(format!("this node is shutting down ({why})"));
        let Some(held) = &self.held else {
            return;
        };
        let last = admin.last_seq();
        if let Some(acked) = crate::replication::LEADER.get() {
            if !acked.wait(last, ACK_WAIT) {
                warn!(
                    seq = last,
                    "shutdown: no standby holds the final seq — releasing anyway; \
                     a standby behind it will not take over"
                );
            }
        }
        if held.release(self.ttl) {
            info!(seq = last, "shutdown: the lease is released");
        } else {
            warn!("shutdown: the lease was not released — it lapses after its TTL");
        }
    }
}

/// Install the OS handlers and start the watcher: on a signal, stop
/// accepting work, settle `stop`, exit 0.
pub(crate) fn watch(stop: Stop) {
    os::install();
    std::thread::spawn(move || {
        while !REQUESTED.load(Ordering::SeqCst) {
            std::thread::sleep(POLL);
        }
        info!("shutdown: signal received — refusing new work");
        SHUTDOWN_FLAG.store(true, Ordering::Relaxed);
        stop.settle("signal");
        std::process::exit(0);
    });
}
