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
}
