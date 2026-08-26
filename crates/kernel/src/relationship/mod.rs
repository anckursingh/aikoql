//! Relationship Manager — owns relationship index read/write and graph
//! edge lifecycle (MRFC-0005 §Knowledge Kernel).
//!
//! The manager hides the `relo/`/`reli/` key layout from the orchestrator.
//! All relationship operations route through here; no other component
//! touches relationship index keys directly.

use crate::knowledge::kom::*;
use crate::storage::repository::KnowledgeRepository;
use std::sync::Arc;

pub struct RelationshipManager {
    repo: Arc<KnowledgeRepository>,
}

impl RelationshipManager {
    pub fn new(repo: Arc<KnowledgeRepository>) -> Self {
        RelationshipManager { repo }
    }

    /// Write both outbound and inbound index entries for one edge.
    /// Idempotent: same (src, rel_type, dst) key is a no-op at the KV level.
    pub fn write_index(
        &self,
        batch: &mut crate::storage::store::WriteBatch,
        src: &KOID,
        rel_type: &str,
        dst: &KOID,
    ) {
        self.repo.write_rel_index(batch, src, rel_type, dst);
    }

    /// Remove both outbound and inbound index entries for one edge.
    /// Idempotent: deleting an absent key is a no-op at the KV level.
    pub fn delete_index(
        &self,
        batch: &mut crate::storage::store::WriteBatch,
        src: &KOID,
        rel_type: &str,
        dst: &KOID,
    ) {
        self.repo.del_rel_index(batch, src, rel_type, dst);
    }

    /// Return outbound edges from `koid`, optionally filtered by `rel_type`.
    pub fn outbound(&self, koid: &KOID, rel_type: Option<&str>) -> KResult<Vec<(String, KOID)>> {
        self.repo.scan_outbound(koid, rel_type)
    }

    /// Return inbound edges to `koid`, optionally filtered by `rel_type`.
    pub fn inbound(&self, koid: &KOID, rel_type: Option<&str>) -> KResult<Vec<(String, KOID)>> {
        self.repo.scan_inbound(koid, rel_type)
    }
}
