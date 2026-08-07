//! mnemosyne-kernel — the Knowledge Kernel for AI.
//!
//! Increment 1 scope:
//! - `knowledge`: Knowledge Object Model (MRFC-0001) — canonical types, codec,
//!   lifecycle, error model.
//! - `storage`: `StorageEngine` abstraction + durable backends + the repository
//!   that hides key layout from the orchestrator.
//! - `transaction`: commit pipeline (atomic KO-version + Knowledge-Event batch,
//!   MVCC, OCC, HLC) and KS-ABI Class A syscalls (MRFC-0011).
//!
//! Determinism Law (MRFC-0011 §7): this crate performs no I/O beyond the
//! `StorageEngine` trait, reads no wall-clock time except via the injected
//! `Clock`, and calls no external services. Everything probabilistic lives
//! outside, in the (future) scheduler domain.

pub mod async_kernel;
pub mod eval;
pub mod event;
pub mod index;
pub mod ir;
pub mod object;
pub mod relationship;

pub mod knowledge {
    pub mod codec;
    pub mod kom;
    pub mod notify;
    pub mod scoring;
}

pub mod storage;

pub mod transaction {
    pub mod kernel;
}

pub mod lifecycle;
pub mod security;

// Compatibility shims for the pre-HLD module layout.
pub mod codec {
    pub use crate::knowledge::codec::*;
}
pub mod kom {
    pub use crate::knowledge::kom::*;
}
pub mod store {
    pub use crate::storage::store::*;
}
pub mod store_redb {
    pub use crate::storage::store_redb::*;
}
pub mod kernel {
    pub use crate::transaction::kernel::*;
}

pub use async_kernel::AsyncKernel;
pub use eval::{
    Contradiction, EvalContradictionQuery, EvalRecallQuery, EvalRecallReport, EvalStalenessQuery,
    EvalStalenessReport,
};
pub use index::{
    BruteForceVectorIndex, IndexCoordinator, IndexMaintainerApi, TextIndex, TokenTextIndex,
    VectorIndex,
};
pub use knowledge::kom::{
    fnv1a64, AclEntry, Action, Direction, Effect, EventKind, EventRef, ExtensionMap, IdGen, KError,
    KResult, KnowledgeEntity, KnowledgeEvent, KnowledgeObject, Lifecycle, LifecycleState, Metadata,
    Origin, PropertyMap, ReferentialPolicy, RelationshipRef, Schema, SecurityDescriptor,
    SemanticBlock, Value, KOID, KOID_LEN,
};
pub use storage::store::{MemoryEngine, StorageEngine, WriteBatch};
pub use storage::store_redb::RedbEngine;
pub use transaction::kernel::{
    Clock, ComplianceReport, EventFilter, Evolved, Explanation, ForgetMode, Forgotten, Fusion,
    Kernel, KnowledgeContext, Lineage, ManualClock, Proof, PropertyFilter, RememberRequest,
    Remembered, ScoredKO, SimilarityQuery, Subject, SubscriptionRecord, SystemClock,
    TransactionOp, VersionRecord,
};
