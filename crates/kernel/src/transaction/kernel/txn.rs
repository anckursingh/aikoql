//! P5-M10 (ND-10) — the public transaction contract: `begin → stage →
//! commit / rollback` over the kernel's existing OCC/MVCC/atomic-batch
//! machinery.
//!
//! Contract (docs/transaction-contract.md):
//! - isolation = SNAPSHOT only — reads pin the version set visible at begin;
//!   a transaction never sees its own staged writes;
//! - `stage` pins `expected_version` from the begin snapshot, so a moved
//!   head is the deterministic `VersionConflict { expected, found }` at
//!   commit;
//! - a commit records its outcome under the txn id in the SAME engine batch
//!   as the writes — a retry after any crash is a recorded no-op returning
//!   the original outcome (idempotent retry);
//! - rollback is pure: staged ops never touch storage.
//!
//! A child module of kernel.rs (the ops.rs precedent): `Transaction` drives
//! `Kernel::transact_with_txn` while sharing the kernel's private fields.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// P5-M10 transaction counters — kernel-lifetime atomics, shared across clones.
#[derive(Default)]
pub struct TxnMetrics {
    pub begun: AtomicU64,
    pub committed: AtomicU64,
    pub rolled_back: AtomicU64,
    pub conflicts: AtomicU64,
    pub deduped_retries: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxnMetricsSnapshot {
    pub begun: u64,
    pub committed: u64,
    pub rolled_back: u64,
    pub conflicts: u64,
    pub deduped_retries: u64,
}

// ---------------------------------------------------------------------------
// The transaction handle
// ---------------------------------------------------------------------------

/// An open transaction. No `state` field — the lifecycle is enforced by
/// construction: `commit`/`rollback` consume `self`, so a finished
/// transaction cannot be reused or double-finished.
pub struct Transaction {
    kernel: Kernel,
    id: String,
    subject: Subject,
    /// The MVCC snapshot pinned at begin: reads resolve to the newest
    /// version with commit_ts <= snapshot_ts, no matter what commits after.
    snapshot_ts: u64,
    staged: Vec<TransactionOp>,
}

impl Kernel {
    /// Begin a transaction pinning an MVCC snapshot and registering the id
    /// for idempotent retry. `txn_id` must be non-empty.
    pub fn begin_transaction(
        &self,
        subject: Subject,
        txn_id: impl Into<String>,
    ) -> KResult<Transaction> {
        let id = txn_id.into();
        if id.is_empty() {
            return Err(KError::InvalidObject(
                "transaction id must not be empty".into(),
            ));
        }
        self.txn_metrics.begun.fetch_add(1, Ordering::Relaxed);
        let snapshot_ts = self.hlc.now(self.clock.as_ref());
        Ok(Transaction {
            kernel: self.clone(),
            id,
            subject,
            snapshot_ts,
            staged: Vec::new(),
        })
    }

    /// Kernel-lifetime transaction counters.
    pub fn transaction_metrics(&self) -> TxnMetricsSnapshot {
        TxnMetricsSnapshot {
            begun: self.txn_metrics.begun.load(Ordering::Relaxed),
            committed: self.txn_metrics.committed.load(Ordering::Relaxed),
            rolled_back: self.txn_metrics.rolled_back.load(Ordering::Relaxed),
            conflicts: self.txn_metrics.conflicts.load(Ordering::Relaxed),
            deduped_retries: self.txn_metrics.deduped_retries.load(Ordering::Relaxed),
        }
    }
}

impl Transaction {
    /// Stage one write. The request's context must carry the transaction's
    /// subject; `expected_version` is pinned from the begin snapshot
    /// (caller-set values are overridden) so a moved head is the
    /// deterministic VersionConflict at commit.
    pub fn stage(&mut self, mut req: RememberRequest) -> KResult<()> {
        if req.context.subject != self.subject {
            return Err(KError::InvalidObject(format!(
                "staged request subject '{}' does not match transaction subject '{}'",
                req.context.subject.name, self.subject.name
            )));
        }
        if let Some(koid) = req.koid {
            req.expected_version = Some(
                self.kernel
                    .raw_object_at(&koid, self.snapshot_ts)?
                    .map(|ko| ko.version)
                    .unwrap_or(0),
            );
        }
        self.staged
            .push(TransactionOp::new(req.context.clone(), req));
        Ok(())
    }

    /// The MVCC snapshot pinned at begin.
    pub fn snapshot_ts(&self) -> u64 {
        self.snapshot_ts
    }

    /// Snapshot read — the version visible at begin, ACL-checked against the
    /// transaction's subject. Staged writes are NOT visible (contract).
    pub fn get(&self, koid: &KOID) -> KResult<KnowledgeObject> {
        self.kernel.get_at(
            KnowledgeContext::new(self.subject.clone()),
            koid,
            self.snapshot_ts,
        )
    }

    /// Commit: every staged write lands in ONE atomic engine batch together
    /// with the txn outcome record. A VersionConflict raises `conflicts`; a
    /// recorded retry (same id already committed) returns the original
    /// outcome and raises `deduped_retries` instead of `committed`.
    ///
    /// Returns the remembered outcomes and whether this commit was a
    /// recorded retry (`deduped`) — clients (the M11 server protocol)
    /// must be able to tell a fresh apply from a no-op re-apply.
    pub fn commit(mut self) -> KResult<(Vec<Remembered>, bool)> {
        let staged = std::mem::take(&mut self.staged);
        match self.kernel.transact_with_txn(staged, &self.id) {
            Ok((results, deduped)) => {
                if !deduped {
                    self.kernel
                        .txn_metrics
                        .committed
                        .fetch_add(1, Ordering::Relaxed);
                }
                Ok((results, deduped))
            }
            Err(e) => {
                if matches!(e, KError::VersionConflict { .. }) {
                    self.kernel
                        .txn_metrics
                        .conflicts
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(e)
            }
        }
    }

    /// Rollback: staged writes are discarded; storage is untouched.
    pub fn rollback(self) {
        self.kernel
            .txn_metrics
            .rolled_back
            .fetch_add(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Outcome record codec (the reserved row sys/txn/<id>)
// ---------------------------------------------------------------------------

/// P5-M20 (P0-03): the retry identity is txn_id + the staged BODY. The
/// fingerprint canonicalizes each op as subject, referential policy, note,
/// and the payload as a shadow KO through the one canonical body codec
/// `encode_ko`, with `expected_version` excluded — it is an OCC pin, not
/// body identity, so a retry may re-pin from a moved snapshot and still
/// dedupe. ponytail: the shadow lifecycle is a fixed constant — the state
/// the commit pipeline would stamp is derivable from `origin` (which IS
/// encoded) and irrelevant to identity.
pub(crate) fn txn_body_fingerprint(ops: &[TransactionOp]) -> [u8; 32] {
    let mut e = Enc::new();
    e.u32(ops.len() as u32);
    for op in ops {
        let subj = &op.context.subject;
        e.str(&subj.name);
        e.u32(subj.roles.len() as u32);
        for r in &subj.roles {
            e.str(r);
        }
        e.opt_str(subj.tenant.as_deref());
        e.u8(op.request.referential_policy.tag());
        e.opt_str(op.request.note.as_deref());
        let req = &op.request;
        let shadow = KnowledgeObject {
            koid: req.koid.unwrap_or(KOID::ZERO),
            version: 0,
            commit_ts: 0,
            metadata: req.metadata.clone(),
            properties: req.properties.clone(),
            semantic: req.semantic.clone(),
            relationships: req.relationships.clone(),
            event_refs: Vec::new(),
            security: req.security.clone().unwrap_or_else(|| SecurityDescriptor {
                owner: req.context.subject.name.clone(),
                acl: vec![],
                classification: None,
            }),
            lifecycle: Lifecycle {
                state: LifecycleState::Draft,
                origin: req.origin.clone(),
            },
            extensions: req.extensions.clone(),
        };
        e.raw(&codec::encode_ko(&shadow));
    }
    sha256(&e.buf)
}

/// u32 count + per entry: 16-byte koid + u64 version + u64 commit_ts.
fn encode_txn_results(results: &[Remembered]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + results.len() * (KOID_LEN + 16));
    out.extend_from_slice(&(results.len() as u32).to_be_bytes());
    for r in results {
        out.extend_from_slice(r.koid.as_bytes());
        out.extend_from_slice(&r.version.to_be_bytes());
        out.extend_from_slice(&r.commit_ts.to_be_bytes());
    }
    out
}

/// Fail closed: a short read or a length mismatch is an error, never a guess.
pub(crate) fn decode_txn_results(bytes: &[u8]) -> Result<Vec<Remembered>, String> {
    if bytes.len() < 4 {
        return Err("record too short for a count".into());
    }
    let count = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let per = KOID_LEN + 16;
    if bytes.len() != 4 + count * per {
        return Err(format!(
            "record length {} does not match count {}",
            bytes.len(),
            count
        ));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = 4 + i * per;
        let mut koid = [0u8; KOID_LEN];
        koid.copy_from_slice(&bytes[off..off + KOID_LEN]);
        out.push(Remembered {
            koid: KOID::from_bytes(koid),
            version: u64::from_be_bytes(
                bytes[off + KOID_LEN..off + KOID_LEN + 8]
                    .try_into()
                    .unwrap(),
            ),
            commit_ts: u64::from_be_bytes(bytes[off + KOID_LEN + 8..off + per].try_into().unwrap()),
        });
    }
    Ok(out)
}

/// P5-M20: the outcome row is `1 + fingerprint(32) + results`. The tag byte
/// 1 marks the fingerprinted format; any other first byte is a pre-M20
/// results-only row — those carry no body, so the fingerprint decodes as
/// `None` and the retry must fail closed (an unverifiable body is not the
/// same body).
pub(crate) fn encode_txn_record(fp: &[u8; 32], results: &[Remembered]) -> Vec<u8> {
    let mut out = vec![1u8];
    out.extend_from_slice(fp);
    out.extend_from_slice(&encode_txn_results(results));
    out
}

pub(crate) fn decode_txn_record(
    bytes: &[u8],
) -> Result<(Option<[u8; 32]>, Vec<Remembered>), String> {
    if bytes.first() == Some(&1) {
        if bytes.len() < 33 {
            return Err("record too short for a fingerprint".into());
        }
        let mut fp = [0u8; 32];
        fp.copy_from_slice(&bytes[1..33]);
        Ok((Some(fp), decode_txn_results(&bytes[33..])?))
    } else {
        Ok((None, decode_txn_results(bytes)?))
    }
}

// ---------------------------------------------------------------------------
// Crash-window parks (tx006, rule 5): env-armed only, txn path only
// ---------------------------------------------------------------------------

/// When `AIKOQL_TXN_PARK` equals `stage`, write the
/// `AIKOQL_TXN_PARK_MARKER` file and sleep forever so a parent test can
/// hard-kill the process mid-commit. Armed only when the env var matches —
/// production is untouched (no env var, no park).
pub(crate) fn txn_park(stage: &str) {
    if std::env::var("AIKOQL_TXN_PARK").as_deref() != Ok(stage) {
        return;
    }
    let Some(marker) = std::env::var_os("AIKOQL_TXN_PARK_MARKER") else {
        return;
    };
    std::fs::write(&marker, stage.as_bytes()).ok();
    // Keep the process alive until the parent kills it (1s slices).
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
