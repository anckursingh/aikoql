//! Object Manager — owns Knowledge Object read operations and head
//! resolution (MRFC-0005 §Knowledge Kernel).
//!
//! All KO reads route through here. The commit pipeline (write path)
//! stays in the Kernel orchestrator; this manager handles the read side.

use crate::knowledge::kom::*;
use crate::storage::repository::KnowledgeRepository;
use std::sync::Arc;

pub struct ObjectManager {
    repo: Arc<KnowledgeRepository>,
}

impl ObjectManager {
    pub fn new(repo: Arc<KnowledgeRepository>) -> Self {
        ObjectManager { repo }
    }

    /// Resolve the head pointer for a KOID.
    pub fn head(&self, koid: &KOID) -> KResult<Option<(u64, u64, LifecycleState)>> {
        self.repo.get_head(koid)
    }

    /// Load the current head version of a KO.
    pub fn get(&self, koid: &KOID) -> KResult<Option<KnowledgeObject>> {
        match self.repo.get_head(koid)? {
            Some((_version, ts, _state)) => self.repo.get_object_version(koid, ts),
            None => Ok(None),
        }
    }

    /// Load a KO at a specific snapshot timestamp.
    pub fn get_at(&self, koid: &KOID, snap_ts: u64) -> KResult<Option<KnowledgeObject>> {
        self.repo.get_object_at(koid, snap_ts)
    }

    /// Load a KO at a specific commit timestamp (bypasses head pointer).
    /// Used by IndexMaintainer and other internal services.
    pub fn raw_at(&self, koid: &KOID, commit_ts: u64) -> KResult<Option<KnowledgeObject>> {
        self.repo.get_object_version(koid, commit_ts)
    }

    /// Enumerate all head pointers (KOID, version, ts, state).
    pub fn scan_heads(&self) -> KResult<Vec<(KOID, u64, u64, LifecycleState)>> {
        self.repo.scan_heads()
    }

    /// Enumerate all versions of a single KOID.
    pub fn scan_versions(&self, koid: &KOID) -> KResult<Vec<(u64, KnowledgeObject)>> {
        self.repo.scan_object_versions(koid)
    }
}
