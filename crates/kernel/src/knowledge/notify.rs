//! Notification / subscription types (MRFC-0015 pre-work).
//!
//! Kept in the `knowledge` layer so lower-level storage code (the repository)
//! can persist durable subscription state without depending on the transaction
//! orchestrator.

use crate::knowledge::kom::{EventKind, KnowledgeEvent, KOID};

/// Filter over the Knowledge Event stream.
#[derive(Clone, Debug, Default)]
pub struct EventFilter {
    pub koid: Option<KOID>,
    pub kinds: Option<Vec<EventKind>>,
}

impl EventFilter {
    pub fn matches(&self, ke: &KnowledgeEvent) -> bool {
        self.koid.map_or(true, |k| k == ke.koid)
            && self.kinds.as_ref().map_or(true, |ks| ks.contains(&ke.kind))
    }
}

/// Persisted durable subscription state.
#[derive(Clone, Debug)]
pub struct SubscriptionRecord {
    pub filter: EventFilter,
    pub last_seq: u64,
}
