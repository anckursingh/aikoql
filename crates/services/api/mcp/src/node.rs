//! PROD-4 — what this node is right now, for the probes and the status
//! report: its replication role, its term, and whether it is stopping.
//!
//! Set once by `serve` after the role is decided. A role only changes by
//! the process ending — a standby that wins the lease re-executes, a
//! leader that loses it exits — so the one moving part is `stopping`,
//! set by a planned stop (`shutdown::Stop::settle`).
//!
//! Three questions, kept apart (a standby is ready for reads and not for
//! writes; one boolean cannot say that):
//! - liveness: the process answers HTTP (`/livez`, no state consulted);
//! - read-readiness: `/readyz`;
//! - write-readiness: `/readyz/write`.

use serde_json::{json, Value as J};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;

/// The node's replication role, as an operator names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeRole {
    /// `serve` has not decided yet.
    Starting = 0,
    /// No replication configured: the only writer there is.
    Standalone = 1,
    /// The writer of a replicated pair (the lease holder under HA).
    Leader = 2,
    /// Follows a leader: converging reads, writes refused.
    Standby = 3,
    /// Deposed by a newer term and not yet rejoined: its data may have
    /// diverged, so it is ready for nothing until an operator (or its
    /// supervisor) rejoins it.
    Fenced = 4,
}

impl NodeRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            NodeRole::Starting => "starting",
            NodeRole::Standalone => "standalone",
            NodeRole::Leader => "leader",
            NodeRole::Standby => "standby",
            NodeRole::Fenced => "fenced",
        }
    }

    fn from_u8(v: u8) -> NodeRole {
        match v {
            1 => NodeRole::Standalone,
            2 => NodeRole::Leader,
            3 => NodeRole::Standby,
            4 => NodeRole::Fenced,
            _ => NodeRole::Starting,
        }
    }
}

static ROLE: AtomicU8 = AtomicU8::new(NodeRole::Starting as u8);
static TERM: AtomicU64 = AtomicU64::new(0);
static STOPPING: AtomicBool = AtomicBool::new(false);

/// Record the role `serve` decided, and the replication term it holds at.
pub(crate) fn set(role: NodeRole, term: u64) {
    TERM.store(term, Ordering::SeqCst);
    ROLE.store(role as u8, Ordering::SeqCst);
}

/// A planned stop began: not ready for anything from here on.
pub(crate) fn set_stopping() {
    STOPPING.store(true, Ordering::SeqCst);
}

/// How the node asks its storage engine why it refuses writes: the reason,
/// or None while it writes. Set once by `serve` when the engine is open.
type EngineProbe = Box<dyn Fn() -> Option<String> + Send + Sync>;
static ENGINE: OnceLock<EngineProbe> = OnceLock::new();

/// PROD-4 review: the probes read the engine too. A leader whose WAL tail
/// cannot be rolled back, or whose group committer died, is alive and holds
/// the lease — only the engine knows it cannot write.
pub(crate) fn watch_engine(probe: EngineProbe) {
    let _ = ENGINE.set(probe);
}

/// The engine's own fence reason, when it has one.
pub(crate) fn engine_fence() -> Option<String> {
    ENGINE.get().and_then(|probe| probe())
}

/// A leader that must stop renewing its lease: its engine is fenced by a
/// fault and no planned stop is in progress (a stop fences on purpose and
/// releases the lease itself).
pub(crate) fn unwritable_leader(engine_fenced: bool, stopping: bool) -> bool {
    engine_fenced && !stopping
}

/// A consistent-enough reading for a probe (three independent atomics; a
/// role never changes after `set`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NodeState {
    pub(crate) role: NodeRole,
    pub(crate) term: u64,
    pub(crate) stopping: bool,
    /// The storage engine refuses client writes (see `watch_engine`).
    pub(crate) engine_fenced: bool,
}

pub(crate) fn state() -> NodeState {
    NodeState {
        role: NodeRole::from_u8(ROLE.load(Ordering::SeqCst)),
        term: TERM.load(Ordering::SeqCst),
        stopping: STOPPING.load(Ordering::SeqCst),
        engine_fenced: engine_fence().is_some(),
    }
}

impl NodeState {
    /// Reads may be routed here.
    pub(crate) fn read_ready(self) -> bool {
        !self.stopping
            && matches!(
                self.role,
                NodeRole::Standalone | NodeRole::Leader | NodeRole::Standby
            )
    }

    /// Writes may be routed here.
    pub(crate) fn write_ready(self) -> bool {
        !self.stopping
            && !self.engine_fenced
            && matches!(self.role, NodeRole::Standalone | NodeRole::Leader)
    }

    /// The probe body: the role, and readiness by traffic class.
    pub(crate) fn to_json(self) -> J {
        json!({
            "role": self.role.as_str(),
            "term": self.term,
            "stopping": self.stopping,
            "engine_fenced": self.engine_fenced,
            "ready": {"read": self.read_ready(), "write": self.write_ready()},
        })
    }
}

/// PROD-5 — how this node's dependencies are doing: the HA lease it
/// renews and the archive it uploads to. Written by their loops (one
/// atomic store each), read by `/status` and the scrape. A dependency
/// that was never started reports nothing.
#[derive(Default)]
pub(crate) struct Deps {
    lease_on: AtomicBool,
    lease_last_renew_ms: AtomicU64,
    lease_renew_failures: AtomicU64,
    archive_on: AtomicBool,
    archive_last_success_ms: AtomicU64,
    archive_uploaded_seq: AtomicU64,
    archive_last_snapshot_ms: AtomicU64,
    archive_snapshot_seq: AtomicU64,
    archive_failures: AtomicU64,
}

/// The serving process's dependencies (tests build their own `Deps`).
pub(crate) static DEPS: Deps = Deps {
    lease_on: AtomicBool::new(false),
    lease_last_renew_ms: AtomicU64::new(0),
    lease_renew_failures: AtomicU64::new(0),
    archive_on: AtomicBool::new(false),
    archive_last_success_ms: AtomicU64::new(0),
    archive_uploaded_seq: AtomicU64::new(0),
    archive_last_snapshot_ms: AtomicU64::new(0),
    archive_snapshot_seq: AtomicU64::new(0),
    archive_failures: AtomicU64::new(0),
};

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Deps {
    /// A lease write landed (the win, the resume, every renewal).
    pub(crate) fn lease_renewed(&self) {
        self.lease_on.store(true, Ordering::SeqCst);
        self.lease_last_renew_ms.store(unix_ms(), Ordering::SeqCst);
    }

    /// A renewal could not be written (the store did not answer).
    pub(crate) fn lease_renew_failed(&self) {
        self.lease_renew_failures.fetch_add(1, Ordering::SeqCst);
    }

    /// The archive loop started.
    pub(crate) fn archive_started(&self) {
        self.archive_on.store(true, Ordering::SeqCst);
    }

    /// An archive pass ended without an error: the log is uploaded up to
    /// `seq` (a pass with nothing new to upload still counts — the lag
    /// series says how far behind the head that is).
    pub(crate) fn archive_passed(&self, seq: u64) {
        self.archive_uploaded_seq.store(seq, Ordering::SeqCst);
        self.archive_last_success_ms
            .store(unix_ms(), Ordering::SeqCst);
    }

    /// A snapshot at `seq` is in the archive.
    pub(crate) fn archive_snapshotted(&self, seq: u64) {
        self.archive_snapshot_seq.store(seq, Ordering::SeqCst);
        self.archive_last_snapshot_ms
            .store(unix_ms(), Ordering::SeqCst);
    }

    /// A pass, a snapshot or a prune failed.
    pub(crate) fn archive_failed(&self) {
        self.archive_failures.fetch_add(1, Ordering::SeqCst);
    }

    /// The lease, as `/status` reports it; null when this node runs none.
    pub(crate) fn lease_json(&self) -> J {
        if !self.lease_on.load(Ordering::SeqCst) {
            return J::Null;
        }
        json!({
            "last_renew_unix_ms": self.lease_last_renew_ms.load(Ordering::SeqCst),
            "renew_failures": self.lease_renew_failures.load(Ordering::SeqCst),
        })
    }

    /// The archive, as `/status` reports it (`head` = this node's newest
    /// seq); null when this node runs none.
    pub(crate) fn archive_json(&self, head: u64) -> J {
        if !self.archive_on.load(Ordering::SeqCst) {
            return J::Null;
        }
        let uploaded = self.archive_uploaded_seq.load(Ordering::SeqCst);
        json!({
            "uploaded_seq": uploaded,
            "lag_frames": head.saturating_sub(uploaded),
            "last_success_unix_ms": self.archive_last_success_ms.load(Ordering::SeqCst),
            "last_snapshot_unix_ms": self.archive_last_snapshot_ms.load(Ordering::SeqCst),
            "snapshot_seq": self.archive_snapshot_seq.load(Ordering::SeqCst),
            "failures": self.archive_failures.load(Ordering::SeqCst),
        })
    }

    /// The node and its dependencies in Prometheus text format. Timestamps
    /// are unix seconds (0 = never), so `time() - series` is an age.
    pub(crate) fn metrics(&self, node: NodeState) -> String {
        let mut out = String::from(
            "# HELP aikoql_node_role This node's replication role (1 on the role it has).\n\
             # TYPE aikoql_node_role gauge\n",
        );
        for role in [
            NodeRole::Standalone,
            NodeRole::Leader,
            NodeRole::Standby,
            NodeRole::Fenced,
        ] {
            out.push_str(&format!(
                "aikoql_node_role{{role=\"{}\"}} {}\n",
                role.as_str(),
                u8::from(node.role == role)
            ));
        }
        let mut gauge = |name: &str, kind: &str, help: &str, value: String| {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}\n"
            ));
        };
        let secs = |ms: &AtomicU64| format!("{:.3}", ms.load(Ordering::SeqCst) as f64 / 1000.0);
        let count = |n: &AtomicU64| n.load(Ordering::SeqCst).to_string();
        gauge(
            "aikoql_node_stopping",
            "gauge",
            "1 once a planned stop began.",
            u8::from(node.stopping).to_string(),
        );
        gauge(
            "aikoql_node_engine_fenced",
            "gauge",
            "1 while the storage engine refuses client writes (a fault fence, a deposed term, a planned stop).",
            u8::from(node.engine_fenced).to_string(),
        );
        gauge(
            "aikoql_replication_term",
            "gauge",
            "The replication term this node serves at.",
            node.term.to_string(),
        );
        if self.lease_on.load(Ordering::SeqCst) {
            gauge(
                "aikoql_ha_lease_last_renew_timestamp_seconds",
                "gauge",
                "When this node last wrote its HA lease (unix seconds).",
                secs(&self.lease_last_renew_ms),
            );
            gauge(
                "aikoql_ha_lease_renew_failures_total",
                "counter",
                "Lease renewals the store did not answer.",
                count(&self.lease_renew_failures),
            );
        }
        if self.archive_on.load(Ordering::SeqCst) {
            gauge(
                "aikoql_archive_last_success_timestamp_seconds",
                "gauge",
                "When an archive pass last ended without an error (unix seconds, 0 = never).",
                secs(&self.archive_last_success_ms),
            );
            gauge(
                "aikoql_archive_uploaded_seq",
                "gauge",
                "The newest log seq in the archive.",
                count(&self.archive_uploaded_seq),
            );
            gauge(
                "aikoql_archive_last_snapshot_timestamp_seconds",
                "gauge",
                "When the archive last took a snapshot (unix seconds, 0 = never).",
                secs(&self.archive_last_snapshot_ms),
            );
            gauge(
                "aikoql_archive_failures_total",
                "counter",
                "Archive passes, snapshots and prunes that failed.",
                count(&self.archive_failures),
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(role: NodeRole, stopping: bool) -> NodeState {
        NodeState {
            role,
            term: 3,
            stopping,
            engine_fenced: false,
        }
    }

    /// PROD-4 review: a leader whose engine stopped taking writes (a WAL
    /// tail that cannot be rolled back, a dead group committer) is still
    /// alive and serves reads — but it must leave the write path.
    #[test]
    fn an_engine_that_cannot_write_withdraws_write_readiness_only() {
        let s = NodeState {
            engine_fenced: true,
            ..at(NodeRole::Leader, false)
        };
        assert!(s.read_ready(), "reads still work");
        assert!(!s.write_ready(), "writes are refused by the engine");
        assert_eq!(
            s.to_json(),
            json!({"role": "leader", "term": 3, "stopping": false, "engine_fenced": true,
                   "ready": {"read": true, "write": false}})
        );
    }

    /// The probe the engine reports through: `None` until set, then the
    /// engine's own answer on every read.
    #[test]
    fn the_engine_probe_is_read_on_every_state() {
        assert_eq!(engine_fence(), None, "no probe, no fence");
        let fenced = std::sync::Arc::new(AtomicBool::new(false));
        let f = std::sync::Arc::clone(&fenced);
        watch_engine(Box::new(move || {
            f.load(Ordering::SeqCst).then(|| "disk full".to_string())
        }));
        assert_eq!(engine_fence(), None);
        fenced.store(true, Ordering::SeqCst);
        assert_eq!(engine_fence().as_deref(), Some("disk full"));
        assert!(state().engine_fenced);
    }

    /// The renewer must keep a planned stop's release, and drop only a
    /// lease whose engine is fenced by a fault.
    #[test]
    fn only_a_fault_fence_makes_the_leader_give_up_its_lease() {
        assert!(!unwritable_leader(false, false), "healthy");
        assert!(!unwritable_leader(true, true), "a planned stop releases it");
        assert!(unwritable_leader(true, false), "a fault: stop renewing");
    }

    /// Readiness by traffic class, for every role — and a planned stop
    /// withdraws both.
    #[test]
    fn readiness_is_by_traffic_class() {
        let ready = |role| {
            let s = at(role, false);
            (s.read_ready(), s.write_ready())
        };
        assert_eq!(ready(NodeRole::Starting), (false, false));
        assert_eq!(ready(NodeRole::Standalone), (true, true));
        assert_eq!(ready(NodeRole::Leader), (true, true));
        assert_eq!(ready(NodeRole::Standby), (true, false));
        assert_eq!(ready(NodeRole::Fenced), (false, false));
        for role in [NodeRole::Standalone, NodeRole::Leader, NodeRole::Standby] {
            let s = at(role, true);
            assert!(!s.read_ready() && !s.write_ready(), "{role:?} stopping");
        }
        assert_eq!(
            at(NodeRole::Standby, false).to_json(),
            json!({"role": "standby", "term": 3, "stopping": false, "engine_fenced": false,
                   "ready": {"read": true, "write": false}})
        );
    }

    /// PROD-5 — a dependency that never started reports nothing; one that
    /// did reports its last success, its position and its failures, in
    /// the status JSON and as series.
    #[test]
    fn dependencies_report_only_once_started() {
        let deps = Deps::default();
        let node = at(NodeRole::Leader, false);
        assert_eq!(deps.lease_json(), J::Null);
        assert_eq!(deps.archive_json(10), J::Null);
        let quiet = deps.metrics(node);
        assert!(
            quiet.contains("aikoql_node_role{role=\"leader\"} 1"),
            "{quiet}"
        );
        assert!(
            quiet.contains("aikoql_node_role{role=\"standby\"} 0"),
            "{quiet}"
        );
        assert!(quiet.contains("aikoql_replication_term 3"), "{quiet}");
        assert!(quiet.contains("aikoql_node_engine_fenced 0"), "{quiet}");
        let faulted = NodeState {
            engine_fenced: true,
            ..node
        };
        assert!(
            deps.metrics(faulted)
                .contains("aikoql_node_engine_fenced 1"),
            "an engine that refuses writes is a series an alert can read"
        );
        assert!(!quiet.contains("aikoql_archive_"), "{quiet}");
        assert!(!quiet.contains("aikoql_ha_lease_"), "{quiet}");

        deps.lease_renewed();
        deps.lease_renew_failed();
        deps.archive_started();
        deps.archive_failed();
        let never = deps.archive_json(10);
        assert_eq!(never["last_success_unix_ms"], json!(0), "never passed");
        assert_eq!(never["lag_frames"], json!(10));
        deps.archive_passed(7);
        deps.archive_snapshotted(5);
        let archive = deps.archive_json(10);
        assert_eq!(archive["uploaded_seq"], json!(7));
        assert_eq!(archive["lag_frames"], json!(3));
        assert_eq!(archive["snapshot_seq"], json!(5));
        assert_eq!(archive["failures"], json!(1));
        assert!(archive["last_success_unix_ms"].as_u64().unwrap() > 0);
        assert_eq!(deps.lease_json()["renew_failures"], json!(1));
        let busy = deps.metrics(node);
        for series in [
            "aikoql_archive_uploaded_seq 7",
            "aikoql_archive_failures_total 1",
            "aikoql_ha_lease_renew_failures_total 1",
            "# TYPE aikoql_archive_failures_total counter",
        ] {
            assert!(busy.contains(series), "no {series:?}:\n{busy}");
        }
    }
}
