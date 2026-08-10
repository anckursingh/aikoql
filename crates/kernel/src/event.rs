//! Event Manager — durable CDC subscriptions and live event broadcast.
//!
//! Owns the persisted subscriber registry and in-process dispatch channels.
//! The kernel delegates `subscribe` / `unsubscribe` / `ack` / `replay` / `notify`
//! here and calls `broadcast` after each committed `KnowledgeEvent`.
//!
//! ponytail: intentionally small. This is not a generic streaming platform; it
//! is the minimal durable CDC primitive the kernel needs. Expand (fan-out
//! queues, streaming over MCP, etc.) only when a concrete consumer appears.

use crate::knowledge::kom::{KError, KResult, KnowledgeEvent, KOID};
use crate::knowledge::notify::{EventFilter, SubscriptionRecord};
use crate::storage::repository::KnowledgeRepository;
use crate::storage::store::WriteBatch;
use std::collections::HashMap;
use std::sync::mpsc;

struct ActiveSub {
    filter: EventFilter,
    tx: mpsc::Sender<KnowledgeEvent>,
}

/// In-memory + persisted subscriber registry.
///
/// `active` holds live channels for the current process. `persisted` holds the
/// last acknowledged seq per subscription so a reconnect can replay missed
/// events from the journal.
pub struct EventManager {
    active: HashMap<String, ActiveSub>,
    persisted: HashMap<String, SubscriptionRecord>,
}

impl EventManager {
    pub fn load(repo: &KnowledgeRepository) -> KResult<Self> {
        Ok(EventManager {
            active: HashMap::new(),
            persisted: repo.scan_subscriptions()?.into_iter().collect(),
        })
    }

    fn persist(
        &self,
        repo: &KnowledgeRepository,
        id: &str,
        rec: &SubscriptionRecord,
    ) -> KResult<()> {
        let mut batch = WriteBatch::new();
        repo.put_subscription(&mut batch, id, rec);
        repo.write_batch(&batch)
    }

    pub fn subscribe(
        &mut self,
        repo: &KnowledgeRepository,
        id: String,
        filter: EventFilter,
    ) -> KResult<mpsc::Receiver<KnowledgeEvent>> {
        let (tx, rx) = mpsc::channel();
        let last_seq = match self.persisted.get(&id) {
            Some(rec) => rec.last_seq,
            None => repo.current_seq()?,
        };
        let rec = SubscriptionRecord {
            filter: filter.clone(),
            last_seq,
        };
        self.persist(repo, &id, &rec)?;
        self.persisted.insert(id.clone(), rec);
        self.active.insert(id, ActiveSub { filter, tx });
        Ok(rx)
    }

    pub fn unsubscribe(&mut self, repo: &KnowledgeRepository, id: &str) -> KResult<()> {
        self.active.remove(id);
        self.persisted.remove(id);
        let mut batch = WriteBatch::new();
        repo.delete_subscription(&mut batch, id);
        repo.write_batch(&batch)
    }

    pub fn ack(&mut self, repo: &KnowledgeRepository, id: &str, seq: u64) -> KResult<()> {
        let rec = self
            .persisted
            .get_mut(id)
            .ok_or(KError::NotFound(KOID::ZERO))?;
        rec.last_seq = seq;
        let rec = rec.clone();
        self.persist(repo, id, &rec)
    }

    pub fn broadcast(&mut self, ke: &KnowledgeEvent) {
        self.active.retain(|_id, sub| {
            if sub.filter.matches(ke) {
                sub.tx.send(ke.clone()).is_ok()
            } else {
                true
            }
        });
    }

    pub fn replay(&self, repo: &KnowledgeRepository, id: &str) -> KResult<Vec<KnowledgeEvent>> {
        let rec = self.persisted.get(id).ok_or(KError::NotFound(KOID::ZERO))?;
        Ok(repo
            .scan_events_after(rec.last_seq)?
            .into_iter()
            .filter(|ke| rec.filter.matches(ke))
            .collect())
    }
}
