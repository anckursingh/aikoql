//! The Knowledge Kernel: commit pipeline + KS-ABI Class A syscalls (MRFC-0011).
//!
//! Design invariants (conformance-tested):
//! - Single-writer pipeline: one mutex serializes validation -> OCC -> HLC
//!   assignment -> atomic write batch (KO version + KE + journal head) -> ack.
//!   This is where "zero committed loss" is won (review §4.3).
//! - MVCC: versions keyed by (koid, commit_ts); readers pin a snapshot ts.
//! - Determinism Law (MRFC-0011 §7): no wall-clock reads except via the
//!   injected `Clock`; no external calls anywhere in this file.
//! - ACL enforcement lives HERE (kernel boundary), not in adapters (MRFC-0001 §12).

use crate::event::EventManager;
use crate::index::coordinator::IndexCoordinator;
use crate::knowledge::codec::{self, Enc};
use crate::knowledge::kom::*;
use crate::lifecycle::schema::SchemaRegistry;
use crate::object::ObjectManager;
use crate::relationship::RelationshipManager;
use crate::security::auth::{AuthManager, POLICY_TYPE, ROLE_TYPE};
use crate::security::tenant::TenantManager;
use crate::security::crypto::Crypto;
use crate::security::envelope::Envelope;
use crate::security::field_crypto::{ComplianceSummary, EncryptionPolicy, FieldCrypto};
use crate::storage::repository::KnowledgeRepository;
use crate::storage::store::{StorageEngine, WriteBatch};
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc, Mutex, RwLock};

// ---------------------------------------------------------------------------
// Clock & Hybrid Logical Clock (commit timestamps)
// ---------------------------------------------------------------------------

pub trait Clock: Send + Sync {
    fn millis(&self) -> u64;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn millis(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Deterministic clock for conformance replay.
pub struct ManualClock {
    now: Mutex<u64>,
}
impl ManualClock {
    pub fn new(t: u64) -> Self {
        ManualClock { now: Mutex::new(t) }
    }
    pub fn set(&self, t: u64) {
        *self.now.lock().unwrap() = t;
    }
    pub fn tick(&self, d: u64) {
        *self.now.lock().unwrap() += d;
    }
}
impl Clock for ManualClock {
    fn millis(&self) -> u64 {
        *self.now.lock().unwrap()
    }
}

/// HLC packed as (millis << 16) | counter. Monotone even under clock regression.
struct Hlc {
    last: Mutex<u64>,
}
impl Hlc {
    #[cfg(test)]
    fn new() -> Self {
        Hlc {
            last: Mutex::new(0),
        }
    }
    /// Re-seed from the persisted journal head so commit timestamps stay
    /// monotone across process restarts (durability requirement).
    fn starting_at(ts: u64) -> Self {
        Hlc {
            last: Mutex::new(ts),
        }
    }
    fn now(&self, clock: &dyn Clock) -> u64 {
        let mut last = self.last.lock().unwrap();
        let ms = clock.millis();
        let cur_ms = *last >> 16;
        *last = if ms > cur_ms { ms << 16 } else { *last + 1 };
        *last
    }
    fn current(&self) -> u64 {
        *self.last.lock().unwrap()
    }
}

// ---------------------------------------------------------------------------
// Audit-chain preimage: covers every field an attacker might flip.
// ---------------------------------------------------------------------------
fn audit_hash_of(
    prev: [u8; 32],
    seq: u64,
    koid: &KOID,
    version: u64,
    kind: EventKind,
    commit_ts: u64,
    payload_hash: &[u8; 32],
    signature: Option<&[u8; 32]>,
    actor: &str,
    note: Option<&str>,
) -> [u8; 32] {
    let mut e = Enc::new();
    e.hash256(&prev);
    e.u64(seq);
    e.raw(koid.as_bytes());
    e.u64(version);
    e.u8(kind.tag());
    e.u64(commit_ts);
    e.hash256(payload_hash);
    e.opt_hash256(signature);
    e.str(actor);
    e.opt_str(note);
    sha256(&e.buf)
}

// ---------------------------------------------------------------------------
// Public request/response types (KS-ABI Class A)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subject {
    pub name: String,
    pub roles: Vec<String>,
}

impl Subject {
    pub fn new(name: &str) -> Self {
        Subject {
            name: name.into(),
            roles: vec![],
        }
    }
    pub fn with_roles(name: &str, roles: &[&str]) -> Self {
        Subject {
            name: name.into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
        }
    }
    pub(crate) fn is_admin(&self) -> bool {
        self.roles.iter().any(|r| r == "admin")
    }
}

/// Runtime context carried by every kernel operation.
///
/// Groups identity, tenancy, and snapshot so syscalls do not accumulate a long
/// parameter list. Optional fields are forward-compat hooks; the kernel only
/// uses `subject` and `snapshot` today.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnowledgeContext {
    pub subject: Subject,
    pub tenant: Option<String>,
    pub agent: Option<String>,
    pub reasoning_mode: Option<String>,
    pub snapshot: Option<u64>,
}

impl KnowledgeContext {
    pub fn new(subject: Subject) -> Self {
        Self {
            subject,
            tenant: None,
            agent: None,
            reasoning_mode: None,
            snapshot: None,
        }
    }

    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }

    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    pub fn with_reasoning_mode(mut self, mode: impl Into<String>) -> Self {
        self.reasoning_mode = Some(mode.into());
        self
    }

    pub fn with_snapshot(mut self, snapshot: u64) -> Self {
        self.snapshot = Some(snapshot);
        self
    }
}

impl From<Subject> for KnowledgeContext {
    fn from(subject: Subject) -> Self {
        Self::new(subject)
    }
}

impl From<&Subject> for KnowledgeContext {
    fn from(subject: &Subject) -> Self {
        Self::new(subject.clone())
    }
}

#[derive(Clone, Debug)]
pub struct RememberRequest {
    pub context: KnowledgeContext,
    /// None => create new KO.
    pub koid: Option<KOID>,
    /// OCC guard. Create: must be None/0. Update: defaults to current head.
    pub expected_version: Option<u64>,
    /// Retried calls with the same key commit exactly once (MRFC-0011 req 10).
    pub idempotency_key: Option<String>,
    pub metadata: Metadata,
    pub properties: PropertyMap,
    pub semantic: Option<SemanticBlock>,
    pub relationships: Vec<RelationshipRef>,
    /// Create: defaults to owner=subject. Update: None keeps existing.
    pub security: Option<SecurityDescriptor>,
    pub extensions: ExtensionMap,
    pub origin: Origin,
    pub note: Option<String>,
    pub referential_policy: ReferentialPolicy,
}

impl RememberRequest {
    pub fn create(context: impl Into<KnowledgeContext>, metadata: Metadata) -> Self {
        RememberRequest {
            context: context.into(),
            koid: None,
            // insert-only semantics: conflicts deterministically if the KOID exists
            expected_version: Some(0),
            idempotency_key: None,
            metadata,
            properties: PropertyMap::new(),
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        }
    }
    pub fn update(context: impl Into<KnowledgeContext>, koid: KOID, metadata: Metadata) -> Self {
        RememberRequest {
            context: context.into(),
            koid: Some(koid),
            expected_version: None,
            idempotency_key: None,
            metadata,
            properties: PropertyMap::new(),
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        }
    }
}

/// One operation inside a multi-object transaction.
#[derive(Clone, Debug)]
pub struct TransactionOp {
    pub context: KnowledgeContext,
    pub request: RememberRequest,
}

impl TransactionOp {
    pub fn new(context: impl Into<KnowledgeContext>, request: RememberRequest) -> Self {
        TransactionOp {
            context: context.into(),
            request,
        }
    }
}

/// Compliance report for encryption audit (MRFC-0020 Phase 4).
#[derive(Clone, Debug)]
pub struct ComplianceReport {
    pub encryption_enabled: bool,
    pub policies_registered: usize,
    pub policy_types: Vec<String>,
    pub field_crypto_summary: Option<ComplianceSummary>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Remembered {
    pub koid: KOID,
    pub version: u64,
    pub commit_ts: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evolved {
    pub koid: KOID,
    pub version: u64,
    pub commit_ts: u64,
    pub state: LifecycleState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Forgotten {
    pub koid: KOID,
    pub version: u64,
    pub commit_ts: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForgetMode {
    Tombstone,
    Erase,
}

#[derive(Clone, Debug, Default)]
pub struct PropertyFilter {
    pub type_name: Option<String>,
    pub required: Vec<(String, Value)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fusion {
    VectorOnly,
    TextOnly,
    Weighted { wv: f32, wt: f32 },
    Rrf { k0: u32 },
    /// Bypass indexes entirely — exact scan-and-filter (MRFC-0009 §4).
    Exact,
}

#[derive(Clone, Debug)]
pub struct SimilarityQuery {
    pub context: KnowledgeContext,
    pub filter: Option<PropertyFilter>,
    pub text: Option<String>,
    pub vector: Option<Vec<f32>>,
    /// When set, only vectors from this embedding model are considered.
    /// When `None`, all models are searched (backward-compatible).
    pub embedding_model: Option<String>,
    pub k: usize,
    pub fusion: Fusion,
}

impl SimilarityQuery {
    pub fn new(context: impl Into<KnowledgeContext>, k: usize, fusion: Fusion) -> Self {
        Self {
            context: context.into(),
            filter: None,
            text: None,
            vector: None,
            embedding_model: None,
            k,
            fusion,
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn with_vector(mut self, vector: Vec<f32>) -> Self {
        self.vector = Some(vector);
        self
    }

    pub fn with_filter(mut self, filter: PropertyFilter) -> Self {
        self.filter = Some(filter);
        self
    }
}

#[derive(Clone, Debug)]
pub struct ScoredKO {
    pub ko: KnowledgeObject,
    pub score: f32,
    /// Staleness of any consulted index. Inc-1 computes exact inline scores
    /// over committed state, so lag is 0; async indexes (Inc-2) report real lag.
    pub index_lag_ms: u64,
}

#[derive(Clone, Debug)]
pub struct VersionRecord {
    pub version: u64,
    pub commit_ts: u64,
    pub origin: Origin,
    pub state: LifecycleState,
}

#[derive(Clone, Debug)]
pub struct Lineage {
    pub koid: KOID,
    pub versions: Vec<VersionRecord>,
    pub events: Vec<KnowledgeEvent>,
}

#[derive(Clone, Debug)]
pub struct Explanation {
    pub koid: KOID,
    pub version: u64,
    pub origin: Origin,
    pub source: Option<String>,
    pub confidence: Option<f32>,
    pub verified: bool,
    pub evidence: Vec<(String, KOID)>,
    pub event_refs: Vec<EventRef>,
}

#[derive(Clone, Debug)]
pub struct Proof {
    pub claim: KOID,
    pub events: u64,
    pub chain_valid: bool,
    pub head_audit_hash: [u8; 32],
    /// True when all KEs carrying a version signature verified against the
    /// configured signing key (or when no signatures are present).
    pub signatures_verified: bool,
}

pub use crate::knowledge::notify::{EventFilter, SubscriptionRecord};

// ---------------------------------------------------------------------------
// Kernel
// ---------------------------------------------------------------------------

struct Pipeline {
    seq: u64,
    audit: [u8; 32],
}

pub struct Kernel {
    repo: Arc<KnowledgeRepository>,
    clock: Arc<dyn Clock>,
    hlc: Arc<Hlc>,
    idgen: Arc<Mutex<IdGen>>,
    pipe: Arc<Mutex<Pipeline>>,
    events: Arc<Mutex<EventManager>>,
    auth: Arc<RwLock<AuthManager>>,
    indexes: Arc<RwLock<Option<Arc<IndexCoordinator>>>>,
    schemas: Arc<RwLock<SchemaRegistry>>,
    relationships: Arc<RelationshipManager>,
    objects: Arc<ObjectManager>,
    /// Optional 32-byte HMAC-SHA256 key for at-rest version signatures.
    signing_key: Option<[u8; 32]>,
    /// Per-tenant quota tracking and enforcement.
    tenants: Arc<TenantManager>,
    /// Optional field-level encryption (MRFC-0020 Phase 3).
    field_crypto: Option<Arc<FieldCrypto>>,
    /// Per-type encryption policies: type_name → which fields to encrypt.
    encryption_policies: Arc<RwLock<HashMap<String, EncryptionPolicy>>>,
}

impl Kernel {
    /// Open (or create) a kernel over `store`. Recovers the journal head so a
    /// restarted kernel continues the hash chain and sequence numbers.
    pub fn open(store: Arc<dyn StorageEngine>, clock: Arc<dyn Clock>, salt: u64) -> KResult<Self> {
        let repo = Arc::new(KnowledgeRepository::new(store));
        let (seq, audit, last_ts) = match repo.journal_head()? {
            Some((s, a, t)) => (s, a, t),
            None => (0, [0u8; 32], 0),
        };
        let events = EventManager::load(&repo)?;
        let auth = AuthManager::load(&repo)?;
        let relationships = Arc::new(RelationshipManager::new(repo.clone()));
        let objects = Arc::new(ObjectManager::new(repo.clone()));
        Ok(Kernel {
            repo,
            clock,
            hlc: Arc::new(Hlc::starting_at(last_ts)),
            idgen: Arc::new(Mutex::new(IdGen::new(salt))),
            pipe: Arc::new(Mutex::new(Pipeline { seq, audit })),
            events: Arc::new(Mutex::new(events)),
            auth: Arc::new(RwLock::new(auth)),
            indexes: Arc::new(RwLock::new(Some(IndexCoordinator::new()))),
            schemas: Arc::new(RwLock::new(SchemaRegistry::new())),
            relationships,
            objects,
            signing_key: None,
            tenants: Arc::new(TenantManager::new()),
            field_crypto: None,
            encryption_policies: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Enable at-rest HMAC-SHA256 version signatures. Idempotent and safe to
    /// call on a `clone_handle` before handing to another subsystem.
    pub fn with_signing_key(mut self, key: [u8; 32]) -> Self {
        self.signing_key = Some(key);
        self
    }

    /// Enable an in-memory LRU cache for heads and object versions.
    /// Only effective when called on the originally opened kernel (before any
    /// `clone_handle` shares the repository).
    pub fn with_cache(mut self, capacity: usize) -> Self {
        if let Some(repo) = Arc::get_mut(&mut self.repo) {
            repo.with_cache(capacity);
        }
        self
    }

    /// Enable field-level encryption (MRFC-0020 Phase 3).
    /// Only effective when called on the originally opened kernel.
    pub fn with_field_encryption(mut self, crypto: Arc<Crypto>, envelope: Arc<Envelope>) -> Self {
        self.field_crypto = Some(Arc::new(FieldCrypto::new(crypto, envelope)));
        self
    }

    /// Register an encryption policy for a schema type. Fields listed in the
    /// policy are encrypted on `remember` and decrypted on `get`.
    pub fn set_encryption_policy(&self, type_name: &str, policy: EncryptionPolicy) {
        self.encryption_policies.write().unwrap().insert(type_name.to_string(), policy);
    }

    /// Remove an encryption policy.
    pub fn remove_encryption_policy(&self, type_name: &str) {
        self.encryption_policies.write().unwrap().remove(type_name);
    }

    /// Generate a compliance report for encryption audit (MRFC-0020 Phase 4).
    /// Returns encryption status, policy inventory, and key audit event counts.
    pub fn compliance_report(&self) -> KResult<ComplianceReport> {
        let pol_count = self.encryption_policies.read().unwrap().len();
        let pol_types: Vec<String> = self.encryption_policies.read().unwrap().keys().cloned().collect();
        let summary = self.field_crypto.as_ref().map(|fc| fc.compliance_summary());
        Ok(ComplianceReport {
            encryption_enabled: self.field_crypto.is_some(),
            policies_registered: pol_count,
            policy_types: pol_types,
            field_crypto_summary: summary.transpose().unwrap_or(None),
        })
    }

    pub fn new_koid(&self) -> KOID {
        self.idgen.lock().unwrap().next(self.clock.millis())
    }

    /// Current read timestamp (snapshot isolation anchor, MRFC-0001 §8).
    pub fn snapshot(&self) -> u64 {
        self.hlc.current()
    }

    /// Shared-state handle for auxiliary subsystems (e.g. index maintainer
    /// threads). All pipeline state is shared — commits stay single-writer.
    pub fn clone_handle(&self) -> Kernel {
        Kernel {
            repo: self.repo.clone(),
            clock: self.clock.clone(),
            hlc: self.hlc.clone(),
            idgen: self.idgen.clone(),
            pipe: self.pipe.clone(),
            events: self.events.clone(),
            auth: self.auth.clone(),
            indexes: self.indexes.clone(),
            schemas: self.schemas.clone(),
            relationships: self.relationships.clone(),
            objects: self.objects.clone(),
            signing_key: self.signing_key,
            tenants: self.tenants.clone(),
            field_crypto: self.field_crypto.clone(),
            encryption_policies: self.encryption_policies.clone(),
        }
    }

    /// Attach an index maintainer; `find_similar` routes through it afterwards.
    pub fn attach_indexes(&self, m: Arc<dyn crate::index::IndexMaintainerApi>) {
        *self.indexes.write().unwrap() = Some(IndexCoordinator::with_maintainer(m));
    }

    /// Register a schema for automatic validation on `remember`.
    /// Schemas are currently in-memory only (MRFC-0001 Increment-1).
    pub fn register_schema(&self, schema: Schema) {
        self.schemas.write().unwrap().register(schema);
    }

    /// Version payload access for index maintenance (internal; bypasses ACL).
    #[doc(hidden)]
    pub fn raw_object_at(&self, koid: &KOID, commit_ts: u64) -> KResult<Option<KnowledgeObject>> {
        self.object_at(koid, commit_ts)
    }

    // ---- internal read helpers -------------------------------------------

    fn head_object(&self, koid: &KOID) -> KResult<Option<KnowledgeObject>> {
        self.objects.get(koid)
    }

    pub(crate) fn object_at(&self, koid: &KOID, snap_ts: u64) -> KResult<Option<KnowledgeObject>> {
        self.objects.get_at(koid, snap_ts)
    }

    pub fn scan_heads(&self) -> KResult<Vec<(KOID, u64, u64, LifecycleState)>> {
        self.objects.scan_heads()
    }

    pub(crate) fn check_access(
        &self,
        subject: &Subject,
        ko: &KnowledgeObject,
        action: Action,
    ) -> KResult<()> {
        self.auth.read().unwrap().authorize(subject, ko, action)
    }

    pub(crate) fn accessible_objects(
        &self,
        subject: &Subject,
        type_name: Option<&str>,
    ) -> KResult<Vec<KnowledgeObject>> {
        let mut out = Vec::new();
        for (koid, _version, _ts, state) in self.repo.scan_heads()? {
            if state == LifecycleState::Deleted {
                continue;
            }
            let Some(ko) = self.head_object(&koid)? else {
                continue;
            };
            if self
                .auth
                .read()
                .unwrap()
                .authorize(subject, &ko, Action::Read)
                .is_err()
            {
                continue;
            }
            if let Some(tn) = type_name {
                if &ko.metadata.type_name != tn {
                    continue;
                }
            }
            out.push(ko);
        }
        Ok(out)
    }

    // ---- verify (MRFC-0011 §6.8) ------------------------------------------

    fn refresh_auth_cache(&self) -> KResult<()> {
        self.auth.write().unwrap().refresh(&self.repo)
    }

    pub fn verify(
        &self,
        ctx: impl Into<KnowledgeContext>,
        koid: &KOID,
        action: Action,
    ) -> KResult<()> {
        let ctx = ctx.into();
        match self.head_object(koid)? {
            Some(ko) => self
                .auth
                .read()
                .unwrap()
                .authorize(&ctx.subject, &ko, action),
            None => {
                if action == Action::Write {
                    Ok(())
                } else {
                    Err(KError::NotFound(*koid))
                }
            }
        }
    }

    // ---- shared commit machinery -------------------------------------------

    /// Append one version + one KE atomically. Caller holds `pipe` lock.
    fn commit_version(
        &self,
        pipe: &mut Pipeline,
        mut ko: KnowledgeObject,
        kind: EventKind,
        origin: Origin,
        actor: &str,
        note: Option<String>,
        idem: Option<&str>,
    ) -> KResult<(u64, u64)> {
        ko.validate()?;
        let commit_ts = self.hlc.now(self.clock.as_ref());
        let seq = pipe.seq + 1;
        ko.commit_ts = commit_ts;
        ko.event_refs.push(EventRef {
            seq,
            kind,
            commit_ts,
        });
        let payload = codec::encode_ko(&ko);
        let payload_hash = sha256(&payload);
        let signature = self.signing_key.map(|key| hmac_sha256(&key, &payload));
        let audit = audit_hash_of(
            pipe.audit,
            seq,
            &ko.koid,
            ko.version,
            kind,
            commit_ts,
            &payload_hash,
            signature.as_ref(),
            actor,
            note.as_deref(),
        );
        let ke = KnowledgeEvent {
            seq,
            koid: ko.koid,
            version: ko.version,
            kind,
            origin,
            actor: actor.into(),
            commit_ts,
            payload_hash,
            prev_audit_hash: pipe.audit,
            audit_hash: audit,
            signature,
            note,
        };
        let mut batch = WriteBatch::new();
        self.repo
            .put_object_version(&mut batch, &ko.koid, commit_ts, &ko);
        // Maintain relationship index: every edge in the KO gets an outbound
        // and inbound index entry keyed by (src, rel_type, dst). Idempotent
        // — re-writing the same edge is a no-op at the KV level.
        for rel in &ko.relationships {
            let (src, dst) = match rel.direction {
                Direction::Outbound => (ko.koid, rel.target),
                Direction::Inbound => (rel.target, ko.koid),
            };
            self.relationships
                .write_index(&mut batch, &src, &rel.rel_type, &dst);
        }
        self.repo.put_head(
            &mut batch,
            &ko.koid,
            ko.version,
            commit_ts,
            ko.lifecycle.state,
        );
        self.repo.put_event(&mut batch, seq, &ke);
        self.repo.put_journal(&mut batch, seq, audit, commit_ts);
        if let Some(k) = idem {
            self.repo
                .put_idem(&mut batch, k, &ko.koid, ko.version, commit_ts);
        }
        self.repo.write_batch(&batch)?;
        pipe.seq = seq;
        pipe.audit = audit;
        self.broadcast(&ke);
        Ok((commit_ts, seq))
    }

    fn broadcast(&self, ke: &KnowledgeEvent) {
        self.events.lock().unwrap().broadcast(ke);
    }

    // ---- remember (MRFC-0011 §6.1) -----------------------------------------

    pub fn remember(&self, req: RememberRequest) -> KResult<Remembered> {
        let mut pipe = self.pipe.lock().unwrap();
        if let Some(k) = &req.idempotency_key {
            if let Some((koid, version, commit_ts)) = self.repo.get_idem(k)? {
                return Ok(Remembered {
                    koid,
                    version,
                    commit_ts,
                }); // exact-once replay
            }
        }
        let koid = match req.koid {
            Some(k) => k,
            None => self.idgen.lock().unwrap().next(self.clock.millis()),
        };
        let head = self.head_object(&koid)?;
        if head.is_none() && req.koid.is_some() && req.expected_version.is_none() {
            // explicit target without an insert guard => update on missing object
            return Err(KError::NotFound(koid));
        }
        let cur_v = head.as_ref().map(|h| h.version).unwrap_or(0);
        let expected = req.expected_version.unwrap_or(cur_v);
        if expected != cur_v {
            return Err(KError::VersionConflict {
                koid,
                expected,
                found: cur_v,
            });
        }
        let creating = head.is_none();
        let security = if creating {
            let s = req.security.clone().unwrap_or_else(|| SecurityDescriptor {
                owner: req.context.subject.name.clone(),
                acl: vec![],
                classification: None,
            });
            if s.owner != req.context.subject.name && !req.context.subject.is_admin() {
                return Err(KError::AccessDenied {
                    subject: req.context.subject.name.clone(),
                    action: Action::Write,
                    koid,
                });
            }
            s
        } else {
            let h = head.as_ref().unwrap();
            self.auth
                .read()
                .unwrap()
                .authorize(&req.context.subject, h, Action::Write)?;
            req.security.clone().unwrap_or_else(|| h.security.clone())
        };
        if req.referential_policy == ReferentialPolicy::Strict {
            for rel in &req.relationships {
                if self.head_object(&rel.target)?.is_none() {
                    return Err(KError::InvalidObject(format!(
                        "relationship target {} does not exist under strict referential policy",
                        rel.target
                    )));
                }
            }
        }
        let mut ko = KnowledgeObject {
            koid,
            version: cur_v + 1,
            commit_ts: 0,
            metadata: req.metadata.clone(),
            properties: req.properties.clone(),
            semantic: req.semantic.clone(),
            relationships: req.relationships.clone(),
            event_refs: head
                .as_ref()
                .map(|h| h.event_refs.clone())
                .unwrap_or_default(),
            security,
            lifecycle: head
                .as_ref()
                .map(|h| h.lifecycle.clone())
                .unwrap_or(Lifecycle {
                    state: LifecycleState::Draft,
                    origin: req.origin.clone(),
                }),
            extensions: req.extensions.clone(),
        };
        self.schemas.read().unwrap().validate(&ko)?;
        // Enforce tenant quota on creation (Phase 5 multi-tenancy).
        if creating {
            self.tenants.check_create(req.context.tenant.as_deref())?;
        }
        // Field-level encryption (MRFC-0020 Phase 3).
        if let Some(ref fc) = self.field_crypto {
            if let Some(policy) = self.encryption_policies.read().unwrap().get(&req.metadata.type_name) {
                let tenant = req.context.tenant.as_deref().unwrap_or("default");
                fc.encrypt_fields(tenant, &req.metadata.type_name, &mut ko.properties, policy)
                    .map_err(|e| KError::Store(format!("field encrypt: {}", e)))?;
            }
        }
        // claims via Class B keep ClaimAsserted kind
        let kind = match (&req.origin, creating) {
            (Origin::Reason, _) | (Origin::SemanticEnrichment, _) => EventKind::ClaimAsserted,
            (_, true) => EventKind::Created,
            (_, false) => EventKind::Updated,
        };
        let is_auth_meta =
            req.metadata.type_name == ROLE_TYPE || req.metadata.type_name == POLICY_TYPE;
        let (commit_ts, _seq) = self.commit_version(
            &mut pipe,
            ko,
            kind,
            req.origin.clone(),
            &req.context.subject.name,
            req.note.clone(),
            req.idempotency_key.as_deref(),
        )?;
        drop(pipe);
        if is_auth_meta {
            self.refresh_auth_cache()?;
        }
        Ok(Remembered {
            koid,
            version: cur_v + 1,
            commit_ts,
        })
    }

    // ---- transact (multi-object atomic commit) ------------------------------

    /// Atomically commit multiple remember operations as one batch.
    ///
    /// Guarantees:
    /// - all-or-nothing persistence (single StorageEngine::write_batch);
    /// - OCC checks use the snapshot taken before any write;
    /// - strict referential integrity resolves targets created within the batch;
    /// - gapless, monotone journal sequence for the whole batch.
    ///
    /// Idempotency keys inside transaction requests are not supported: a batch
    /// is already atomic and the caller can use an external idempotency token.
    pub fn transact(&self, ops: Vec<TransactionOp>) -> KResult<Vec<Remembered>> {
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = self.pipe.lock().unwrap();

        // Phase 1: resolve KOIDs and heads (snapshot before any write).
        struct Resolved {
            koid: KOID,
            head: Option<KnowledgeObject>,
            cur_v: u64,
            creating: bool,
            op: TransactionOp,
        }
        let mut resolved = Vec::with_capacity(ops.len());
        for op in ops {
            if op.request.idempotency_key.is_some() {
                return Err(KError::UnsupportedOperation(
                    "idempotency keys are not supported inside transactions".into(),
                ));
            }
            let koid = match op.request.koid {
                Some(k) => k,
                None => self.idgen.lock().unwrap().next(self.clock.millis()),
            };
            let head = self.head_object(&koid)?;
            if head.is_none() && op.request.koid.is_some() && op.request.expected_version.is_none()
            {
                return Err(KError::NotFound(koid));
            }
            let cur_v = head.as_ref().map(|h| h.version).unwrap_or(0);
            let expected = op.request.expected_version.unwrap_or(cur_v);
            if expected != cur_v {
                return Err(KError::VersionConflict {
                    koid,
                    expected,
                    found: cur_v,
                });
            }
            resolved.push(Resolved {
                koid,
                head,
                cur_v,
                creating: cur_v == 0,
                op,
            });
        }

        // Detect duplicate KOIDs in the batch -> deterministic conflict.
        let mut seen = HashSet::new();
        for r in &resolved {
            if !seen.insert(r.koid) {
                return Err(KError::VersionConflict {
                    koid: r.koid,
                    expected: r.cur_v + 1,
                    found: r.cur_v + 1,
                });
            }
        }

        // Set of KOIDs that will exist after the batch (for referential checks).
        let new_koids: HashSet<KOID> = resolved.iter().map(|r| r.koid).collect();

        // Phase 2: authorize, validate referential integrity, build object versions.
        let mut pending = Vec::with_capacity(resolved.len());
        for r in &resolved {
            let req = &r.op.request;
            let security = if r.creating {
                let s = req.security.clone().unwrap_or_else(|| SecurityDescriptor {
                    owner: r.op.context.subject.name.clone(),
                    acl: vec![],
                    classification: None,
                });
                if s.owner != r.op.context.subject.name && !r.op.context.subject.is_admin() {
                    return Err(KError::AccessDenied {
                        subject: r.op.context.subject.name.clone(),
                        action: Action::Write,
                        koid: r.koid,
                    });
                }
                s
            } else {
                let h = r.head.as_ref().unwrap();
                self.auth
                    .read()
                    .unwrap()
                    .authorize(&r.op.context.subject, h, Action::Write)?;
                req.security.clone().unwrap_or_else(|| h.security.clone())
            };
            if req.referential_policy == ReferentialPolicy::Strict {
                for rel in &req.relationships {
                    if !new_koids.contains(&rel.target) && self.head_object(&rel.target)?.is_none()
                    {
                        return Err(KError::InvalidObject(format!(
                            "relationship target {} does not exist under strict referential policy",
                            rel.target
                        )));
                    }
                }
            }
            let ko = KnowledgeObject {
                koid: r.koid,
                version: r.cur_v + 1,
                commit_ts: 0,
                metadata: req.metadata.clone(),
                properties: req.properties.clone(),
                semantic: req.semantic.clone(),
                relationships: req.relationships.clone(),
                event_refs: r
                    .head
                    .as_ref()
                    .map(|h| h.event_refs.clone())
                    .unwrap_or_default(),
                security,
                lifecycle: r
                    .head
                    .as_ref()
                    .map(|h| h.lifecycle.clone())
                    .unwrap_or(Lifecycle {
                        state: LifecycleState::Draft,
                        origin: req.origin.clone(),
                    }),
                extensions: req.extensions.clone(),
            };
            self.schemas.read().unwrap().validate(&ko)?;
            let kind = match (&req.origin, r.creating) {
                (Origin::Reason, _) | (Origin::SemanticEnrichment, _) => EventKind::ClaimAsserted,
                (_, true) => EventKind::Created,
                (_, false) => EventKind::Updated,
            };
            pending.push((
                r.koid,
                r.cur_v,
                ko,
                kind,
                req.origin.clone(),
                r.op.context.subject.name.clone(),
                req.note.clone(),
            ));
        }

        // Phase 3: assign one commit timestamp and sequential event sequence numbers.
        let commit_ts = self.hlc.now(self.clock.as_ref());
        let mut batch = WriteBatch::new();
        let mut events: Vec<KnowledgeEvent> = Vec::with_capacity(pending.len());
        let mut results = Vec::with_capacity(pending.len());
        let mut prev_audit = pipe.audit;
        let start_seq = pipe.seq;
        for (idx, (koid, cur_v, mut ko, kind, origin, actor, note)) in
            pending.into_iter().enumerate()
        {
            let seq = start_seq + idx as u64 + 1;
            ko.commit_ts = commit_ts;
            ko.event_refs.push(EventRef {
                seq,
                kind,
                commit_ts,
            });
            ko.validate()?;
            let payload = codec::encode_ko(&ko);
            let payload_hash = sha256(&payload);
            let signature = self.signing_key.map(|key| hmac_sha256(&key, &payload));
            let audit = audit_hash_of(
                prev_audit,
                seq,
                &koid,
                ko.version,
                kind,
                commit_ts,
                &payload_hash,
                signature.as_ref(),
                &actor,
                note.as_deref(),
            );
            let ke = KnowledgeEvent {
                seq,
                koid,
                version: ko.version,
                kind,
                origin,
                actor,
                commit_ts,
                payload_hash,
                prev_audit_hash: prev_audit,
                audit_hash: audit,
                signature,
                note,
            };
            self.repo
                .put_object_version(&mut batch, &koid, commit_ts, &ko);
            self.repo
                .put_head(&mut batch, &koid, ko.version, commit_ts, ko.lifecycle.state);
            self.repo.put_event(&mut batch, seq, &ke);
            events.push(ke);
            prev_audit = audit;
            results.push(Remembered {
                koid,
                version: cur_v + 1,
                commit_ts,
            });
        }
        let final_seq = start_seq + events.len() as u64;
        self.repo
            .put_journal(&mut batch, final_seq, prev_audit, commit_ts);
        self.repo.write_batch(&batch)?;
        pipe.seq = final_seq;
        pipe.audit = prev_audit;
        for ke in events {
            self.broadcast(&ke);
        }
        drop(pipe);
        if results.iter().any(|r| {
            let h = self.head_object(&r.koid).ok().flatten();
            h.map(|h| h.metadata.type_name == ROLE_TYPE || h.metadata.type_name == POLICY_TYPE)
                .unwrap_or(false)
        }) {
            self.refresh_auth_cache()?;
        }
        Ok(results)
    }

    // ---- evolve (MRFC-0011 §6.3) -------------------------------------------

    pub fn evolve(
        &self,
        ctx: impl Into<KnowledgeContext>,
        koid: &KOID,
        to: LifecycleState,
        origin: Origin,
        expected_version: Option<u64>,
        note: Option<String>,
    ) -> KResult<Evolved> {
        let ctx = ctx.into();
        let mut pipe = self.pipe.lock().unwrap();
        let head = self.head_object(koid)?.ok_or(KError::NotFound(*koid))?;
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &head, Action::Evolve)?;
        let from = head.lifecycle.state;
        if !from.can_transition(to) {
            return Err(KError::InvalidState { from, to });
        }
        let cur_v = head.version;
        let expected = expected_version.unwrap_or(cur_v);
        if expected != cur_v {
            return Err(KError::VersionConflict {
                koid: *koid,
                expected,
                found: cur_v,
            });
        }
        let mut ko = head.clone();
        ko.version = cur_v + 1;
        ko.lifecycle = Lifecycle {
            state: to,
            origin: origin.clone(),
        };
        let (commit_ts, _seq) = self.commit_version(
            &mut pipe,
            ko,
            EventKind::LifecycleChanged,
            origin,
            &ctx.subject.name,
            note,
            None,
        )?;
        Ok(Evolved {
            koid: *koid,
            version: cur_v + 1,
            commit_ts,
            state: to,
        })
    }

    // ---- forget (MRFC-0011 §6.2) -------------------------------------------

    pub fn forget(
        &self,
        ctx: impl Into<KnowledgeContext>,
        koid: &KOID,
        mode: ForgetMode,
        expected_version: Option<u64>,
        note: Option<String>,
    ) -> KResult<Forgotten> {
        let ctx = ctx.into();
        let mut pipe = self.pipe.lock().unwrap();
        let head = self.head_object(koid)?.ok_or(KError::NotFound(*koid))?;
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &head, Action::Delete)?;
        let cur_v = head.version;
        let expected = expected_version.unwrap_or(cur_v);
        if expected != cur_v {
            return Err(KError::VersionConflict {
                koid: *koid,
                expected,
                found: cur_v,
            });
        }
        match mode {
            ForgetMode::Tombstone => {
                let mut ko = head.clone();
                ko.version = cur_v + 1;
                ko.lifecycle = Lifecycle {
                    state: LifecycleState::Deleted,
                    origin: Origin::System,
                };
                let (commit_ts, _) = self.commit_version(
                    &mut pipe,
                    ko,
                    EventKind::Forgotten,
                    Origin::System,
                    &ctx.subject.name,
                    note,
                    None,
                )?;
                Ok(Forgotten {
                    koid: *koid,
                    version: cur_v + 1,
                    commit_ts,
                })
            }
            ForgetMode::Erase => {
                // Legal erasure: remove all versions + head; keep journal and a
                // hash-only stub so `prove` can still verify the chain (GDPR-class).
                let head_payload = codec::encode_ko(&head);
                let head_hash = sha256(&head_payload);
                let signature = self.signing_key.map(|key| hmac_sha256(&key, &head_payload));
                let versions: Vec<u64> = self
                    .repo
                    .scan_object_versions(koid)?
                    .into_iter()
                    .map(|(ts, _)| ts)
                    .collect();
                let commit_ts = self.hlc.now(self.clock.as_ref());
                let seq = pipe.seq + 1;
                let audit = audit_hash_of(
                    pipe.audit,
                    seq,
                    koid,
                    cur_v,
                    EventKind::Forgotten,
                    commit_ts,
                    &head_hash,
                    signature.as_ref(),
                    &ctx.subject.name,
                    note.as_deref(),
                );
                let ke = KnowledgeEvent {
                    seq,
                    koid: *koid,
                    version: cur_v,
                    kind: EventKind::Forgotten,
                    origin: Origin::System,
                    actor: ctx.subject.name.clone(),
                    commit_ts,
                    payload_hash: head_hash,
                    prev_audit_hash: pipe.audit,
                    audit_hash: audit,
                    signature,
                    note,
                };
                let mut batch = WriteBatch::new();
                self.repo.put_event(&mut batch, seq, &ke);
                self.repo.put_journal(&mut batch, seq, audit, commit_ts);
                self.repo.put_tombstone(&mut batch, koid, head_hash, seq);
                self.repo.delete_head(&mut batch, koid);
                for ts in versions {
                    self.repo.delete_object_version(&mut batch, koid, ts);
                }
                self.repo.write_batch(&batch)?;
                pipe.seq = seq;
                pipe.audit = audit;
                self.broadcast(&ke);
                Ok(Forgotten {
                    koid: *koid,
                    version: cur_v,
                    commit_ts,
                })
            }
        }
    }

    // ---- reads (snapshot-isolated, MRFC-0001 §8) ----------------------------

    pub fn get(&self, ctx: impl Into<KnowledgeContext>, koid: &KOID) -> KResult<KnowledgeObject> {
        let ctx = ctx.into();
        let mut ko = self.head_object(koid)?.ok_or(KError::NotFound(*koid))?;
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &ko, Action::Read)?;
        // Field-level decryption (MRFC-0020 Phase 3).
        if let Some(ref fc) = self.field_crypto {
            if let Some(policy) = self.encryption_policies.read().unwrap().get(&ko.metadata.type_name) {
                let tenant = ctx.tenant.as_deref().unwrap_or("default");
                fc.decrypt_fields(tenant, &ko.metadata.type_name, &mut ko.properties, policy)
                    .map_err(|e| KError::Store(format!("field decrypt: {}", e)))?;
            }
        }
        Ok(ko)
    }

    pub fn get_at(
        &self,
        ctx: impl Into<KnowledgeContext>,
        koid: &KOID,
        snap_ts: u64,
    ) -> KResult<KnowledgeObject> {
        let ctx = ctx.into();
        let ko = self
            .object_at(koid, snap_ts)?
            .ok_or(KError::NotFound(*koid))?;
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &ko, Action::Read)?;
        Ok(ko)
    }

    // ---- find_similar (MRFC-0011 §6.4) --------------------------------------

    pub fn find_similar(&self, q: SimilarityQuery) -> KResult<Vec<ScoredKO>> {
        self.indexes
            .read()
            .unwrap()
            .as_ref()
            .expect("kernel always has a coordinator")
            .search(self, q)
    }

    // ---- type scanning ---------------------------------------------------

    /// Return all readable KOs of a given type (ACL-filtered).
    /// Services around the kernel use this for enumeration without touching
    /// storage internals.
    pub fn scan_by_type(
        &self,
        subject: &Subject,
        type_name: &str,
    ) -> KResult<Vec<KnowledgeObject>> {
        let mut out = Vec::new();
        for (koid, _version, _ts, state) in self.repo.scan_heads()? {
            if state == LifecycleState::Deleted {
                continue;
            }
            let Some(ko) = self.head_object(&koid)? else {
                continue;
            };
            if ko.metadata.type_name != type_name {
                continue;
            }
            if self
                .auth
                .read()
                .unwrap()
                .authorize(subject, &ko, Action::Read)
                .is_err()
            {
                continue;
            }
            out.push(ko);
        }
        Ok(out)
    }

    /// Return all distinct type names from head objects. O(n) scan;
    /// ponytail: add a type-name index if enumeration becomes frequent.
    pub fn list_types(&self) -> KResult<Vec<String>> {
        let mut types = std::collections::BTreeSet::new();
        for (koid, _version, _ts, state) in self.repo.scan_heads()? {
            if state == LifecycleState::Deleted {
                continue;
            }
            if let Some(ko) = self.head_object(&koid)? {
                types.insert(ko.metadata.type_name);
            }
        }
        Ok(types.into_iter().collect())
    }

    // ---- relationship index queries ---------------------------------------

    /// Return outbound edges from `koid` using the relationship index.
    /// Each result is `(rel_type, target_koid)`. When `rel_type_filter` is
    /// `Some`, only edges of that type are returned.
    ///
    /// This is a fast index-only scan — it does NOT load the source KO.
    /// Callers must still verify read access on returned targets separately.
    pub fn outbound_edges(
        &self,
        koid: &KOID,
        rel_type_filter: Option<&str>,
    ) -> KResult<Vec<(String, KOID)>> {
        self.relationships.outbound(koid, rel_type_filter)
    }

    /// Return inbound edges to `koid` using the relationship index.
    /// Each result is `(rel_type, source_koid)`.
    pub fn inbound_edges(
        &self,
        koid: &KOID,
        rel_type_filter: Option<&str>,
    ) -> KResult<Vec<(String, KOID)>> {
        self.relationships.inbound(koid, rel_type_filter)
    }

    // ---- trace / explain / prove (MRFC-0011 §6.5–6.7) ------------------------

    pub fn trace(&self, ctx: impl Into<KnowledgeContext>, koid: &KOID) -> KResult<Lineage> {
        let ctx = ctx.into();
        let head = self.head_object(koid)?.ok_or(KError::NotFound(*koid))?;
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &head, Action::Read)?;
        let mut versions = Vec::new();
        for (_ts, ko) in self.repo.scan_object_versions(koid)? {
            versions.push(VersionRecord {
                version: ko.version,
                commit_ts: ko.commit_ts,
                origin: ko.lifecycle.origin.clone(),
                state: ko.lifecycle.state,
            });
        }
        let mut events = Vec::new();
        for ke in self.repo.scan_events()? {
            if ke.koid == *koid {
                events.push(ke);
            }
        }
        Ok(Lineage {
            koid: *koid,
            versions,
            events,
        })
    }

    pub fn explain(
        &self,
        ctx: impl Into<KnowledgeContext>,
        koid: &KOID,
        version: Option<u64>,
    ) -> KResult<Explanation> {
        let ctx = ctx.into();
        let ko = match version {
            None => self.head_object(koid)?.ok_or(KError::NotFound(*koid))?,
            Some(v) => {
                let mut found = None;
                for (_ts, ko) in self.repo.scan_object_versions(koid)? {
                    if ko.version == v {
                        found = Some(ko);
                        break;
                    }
                }
                found.ok_or(KError::NotFound(*koid))?
            }
        };
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &ko, Action::Read)?;
        let (source, confidence) = match &ko.semantic {
            Some(s) => (s.source.clone(), s.confidence),
            None => (None, None),
        };
        Ok(Explanation {
            koid: *koid,
            version: ko.version,
            origin: ko.lifecycle.origin.clone(),
            source,
            confidence,
            verified: ko.lifecycle.state == LifecycleState::Verified,
            evidence: ko
                .relationships
                .iter()
                .map(|r| (r.rel_type.clone(), r.target))
                .collect(),
            event_refs: ko.event_refs.clone(),
        })
    }

    pub fn prove(&self, ctx: impl Into<KnowledgeContext>, claim: &KOID) -> KResult<Proof> {
        let ctx = ctx.into();
        let head = match self.head_object(claim) {
            Ok(Some(h)) => h,
            Ok(None) => return Err(KError::NotFound(*claim)),
            Err(KError::Codec(_)) => {
                // undecodable payload is itself detectable tamper evidence
                return Ok(Proof {
                    claim: *claim,
                    events: 0,
                    chain_valid: false,
                    head_audit_hash: [0u8; 32],
                    signatures_verified: false,
                });
            }
            Err(e) => return Err(e),
        };
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &head, Action::Read)?;
        let mut prev = [0u8; 32];
        let mut valid = true;
        let mut count = 0u64;
        let mut signatures_verified = true;
        let mut signed_count = 0u64;
        for ke in self.repo.scan_events()? {
            let expect = audit_hash_of(
                prev,
                ke.seq,
                &ke.koid,
                ke.version,
                ke.kind,
                ke.commit_ts,
                &ke.payload_hash,
                ke.signature.as_ref(),
                &ke.actor,
                ke.note.as_deref(),
            );
            if expect != ke.audit_hash || ke.prev_audit_hash != prev {
                valid = false;
                break;
            }
            // payload integrity: object bytes still hash to the committed value.
            // After legal erasure (ForgetMode::Erase) per-version payloads are
            // gone BY DESIGN; a tombstone stub proves erasure was committed and
            // the audit-chain links above still protect the journal itself.
            match self.repo.get_object_version(&ke.koid, ke.commit_ts)? {
                Some(ko) => {
                    let bytes = codec::encode_ko(&ko);
                    if sha256(&bytes) != ke.payload_hash {
                        valid = false;
                        break;
                    }
                    if let Some(sig) = &ke.signature {
                        signed_count += 1;
                        if let Some(key) = self.signing_key {
                            if hmac_sha256(&key, &bytes) != *sig {
                                signatures_verified = false;
                            }
                        }
                    }
                }
                None => {
                    if self.repo.get_tombstone(&ke.koid)?.is_none() {
                        valid = false;
                        break;
                    }
                }
            }
            prev = ke.audit_hash;
            count += 1;
        }
        if valid {
            if let Some((_, audit, _)) = self.repo.journal_head()? {
                if audit != prev {
                    valid = false;
                }
            }
        }
        Ok(Proof {
            claim: *claim,
            events: count,
            chain_valid: valid,
            head_audit_hash: prev,
            signatures_verified: !self.signing_key.is_some()
                || (signatures_verified && signed_count > 0),
        })
    }

    // ---- durable CDC subscriptions (MRFC-0015 pre-work) ----------------------

    pub fn subscribe(
        &self,
        id: String,
        filter: EventFilter,
    ) -> KResult<mpsc::Receiver<KnowledgeEvent>> {
        self.events
            .lock()
            .unwrap()
            .subscribe(&self.repo, id, filter)
    }

    pub fn unsubscribe(&self, id: &str) -> KResult<()> {
        self.events.lock().unwrap().unsubscribe(&self.repo, id)
    }

    pub fn ack(&self, id: &str, seq: u64) -> KResult<()> {
        self.events.lock().unwrap().ack(&self.repo, id, seq)
    }

    pub fn replay(&self, id: &str) -> KResult<Vec<KnowledgeEvent>> {
        self.events.lock().unwrap().replay(&self.repo, id)
    }

    /// In-process notification channel (legacy; prefer `subscribe` for durability).
    pub fn notify(&self, filter: EventFilter) -> mpsc::Receiver<KnowledgeEvent> {
        let id = format!("__anon__{}", self.new_koid().to_hex());
        self.subscribe(id, filter)
            .expect("memory subscription never fails")
    }

    /// Full journal scan (conformance + debugging).
    pub fn journal(&self) -> KResult<Vec<KnowledgeEvent>> {
        self.repo.scan_events()
    }

    pub fn journal_head(&self) -> KResult<(u64, [u8; 32])> {
        match self.repo.journal_head()? {
            Some((seq, audit, _)) => Ok((seq, audit)),
            None => Ok((0, [0u8; 32])),
        }
    }

    // ---- Programs-as-KOs (MRFC-0030 Phase 7a) ----------------------------

    /// Deploy a Program KO. The program is AIKOQL stored as a Knowledge Object
    /// of type `mnemosyne:program`. Like any KO, it gets versioning, provenance,
    /// access control, and audit trail.
    pub fn deploy_program(&self, name: &str, body: &str, language: &str, subject: &Subject) -> KResult<Remembered> {
        let mut props = PropertyMap::new();
        props.insert("name".into(), Value::Text(name.to_string()));
        props.insert("body".into(), Value::Text(body.to_string()));
        props.insert("language".into(), Value::Text(language.to_string()));
        props.insert("version".into(), Value::Int(1));
        self.remember(RememberRequest {
            context: subject.clone().into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: Some(format!("deploy-program-{}", name)),
            metadata: Metadata {
                type_name: "mnemosyne:program".into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["program".into(), "active-object".into()],
            },
            properties: props,
            semantic: None, relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: subject.name.clone(), acl: vec![], classification: None,
            }),
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: Some(format!("Deployed program: {}", name)),
            referential_policy: ReferentialPolicy::Permissive,
        })
    }

    /// Update a Program KO to a new version (new body, incremented version counter).
    pub fn update_program(&self, koid: &KOID, new_body: &str, subject: &Subject) -> KResult<Remembered> {
        let ctx = KnowledgeContext::from(subject.clone());
        let ko = self.get(ctx, koid)?;
        if ko.metadata.type_name != "mnemosyne:program" {
            return Err(KError::InvalidObject("not a program".into()));
        }
        let cur_ver = match ko.properties.get("version") {
            Some(Value::Int(v)) => *v,
            _ => 1,
        };
        let mut props = ko.properties.clone();
        props.insert("body".into(), Value::Text(new_body.to_string()));
        props.insert("version".into(), Value::Int(cur_ver + 1));
        self.remember(RememberRequest {
            context: subject.clone().into(),
            koid: Some(*koid),
            expected_version: Some(ko.version),
            idempotency_key: Some(format!("update-program-{}", koid.to_hex())),
            metadata: ko.metadata.clone(),
            properties: props,
            semantic: None, relationships: ko.relationships.clone(),
            security: Some(ko.security.clone()),
            extensions: ko.extensions.clone(),
            origin: Origin::Human,
            note: Some(format!("Updated program to v{}", cur_ver + 1)),
            referential_policy: ReferentialPolicy::Permissive,
        })
    }

    /// List all deployed programs.
    pub fn list_programs(&self, subject: &Subject) -> KResult<Vec<KnowledgeObject>> {
        self.scan_by_type(subject, "mnemosyne:program")
    }

    // ---- Policy-as-KO (MRFC-0030 Phase 7b) --------------------------------

    /// Deploy a Policy KO. When evaluated, determines whether an action is allowed.
    pub fn deploy_policy(
        &self, name: &str, effect: &str, principal: &str,
        action: &str, resource_type: &str, condition: Option<&str>,
        subject: &Subject,
    ) -> KResult<Remembered> {
        let mut props = PropertyMap::new();
        props.insert("name".into(), Value::Text(name.to_string()));
        props.insert("effect".into(), Value::Text(effect.to_string()));
        props.insert("principal".into(), Value::Text(principal.to_string()));
        props.insert("action".into(), Value::Text(action.to_string()));
        props.insert("resource_type".into(), Value::Text(resource_type.to_string()));
        if let Some(c) = condition {
            props.insert("condition".into(), Value::Text(c.to_string()));
        }
        self.remember(RememberRequest {
            context: subject.clone().into(),
            koid: None, expected_version: Some(0),
            idempotency_key: Some(format!("deploy-policy-{}", name)),
            metadata: Metadata {
                type_name: "mnemosyne:policy".into(), tenant: None, schema_version: 1,
                tags: vec!["policy".into(), "active-object".into()],
            },
            properties: props,
            semantic: None, relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: subject.name.clone(), acl: vec![], classification: None,
            }),
            extensions: ExtensionMap::new(), origin: Origin::Human,
            note: Some(format!("Deployed policy: {}", name)),
            referential_policy: ReferentialPolicy::Permissive,
        })
    }

    /// Evaluate all applicable Policy KOs for a (principal, action, resource_type) tuple.
    /// Returns the first Deny or the first Allow found. Policies are evaluated in
    /// version-descending order (newest first).
    pub fn evaluate_policies(
        &self, principal: &str, action: &Action, resource_type: &str, subject: &Subject,
    ) -> KResult<Option<String>> {
        let policies = self.scan_by_type(subject, "mnemosyne:policy")?;
        for p in policies.iter() {
            let pol_principal = match p.properties.get("principal").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }) {
                Some(s) => s, None => continue,
            };
            let pol_action = match p.properties.get("action").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }) {
                Some(s) => s, None => continue,
            };
            let pol_resource = match p.properties.get("resource_type").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }) {
                Some(s) => s, None => continue,
            };
            // Match: principal, action, resource_type must all match.
            if pol_principal != principal && pol_principal != "*" { continue; }
            let action_str = format!("{:?}", action);
            if pol_action != action_str && pol_action != "*" { continue; }
            if pol_resource != resource_type && pol_resource != "*" { continue; }
            let effect = match p.properties.get("effect").and_then(|v| match v { Value::Text(s) => Some(s.as_str()), _ => None }) {
                Some(s) => s, None => continue,
            };
            if effect == "Deny" { return Ok(Some(format!("Denied by policy: {}", p.koid.to_hex()))); }
            if effect == "Allow" { return Ok(None); } // Allowed, keep checking
        }
        Ok(Some("No matching policy found".into()))
    }

    // ---- Workflow-as-KO (MRFC-0030 Phase 7b) ------------------------------

    /// Deploy a Workflow KO — a DAG of Program KOs.
    pub fn deploy_workflow(&self, name: &str, steps_json: &str, subject: &Subject) -> KResult<Remembered> {
        let mut props = PropertyMap::new();
        props.insert("name".into(), Value::Text(name.to_string()));
        props.insert("steps".into(), Value::Text(steps_json.to_string()));
        self.remember(RememberRequest {
            context: subject.clone().into(),
            koid: None, expected_version: Some(0),
            idempotency_key: Some(format!("deploy-workflow-{}", name)),
            metadata: Metadata {
                type_name: "mnemosyne:workflow".into(), tenant: None, schema_version: 1,
                tags: vec!["workflow".into(), "active-object".into()],
            },
            properties: props,
            semantic: None, relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: subject.name.clone(), acl: vec![], classification: None,
            }),
            extensions: ExtensionMap::new(), origin: Origin::Human,
            note: Some(format!("Deployed workflow: {}", name)),
            referential_policy: ReferentialPolicy::Permissive,
        })
    }

    // ---- Trigger-as-KO (MRFC-0030 Phase 7b) -------------------------------

    /// Deploy a Trigger KO — fires on matching KnowledgeEvents.
    pub fn deploy_trigger(
        &self, name: &str, event_kind: &str, type_filter: &str,
        program_koid: &str, subject: &Subject,
    ) -> KResult<Remembered> {
        let mut props = PropertyMap::new();
        props.insert("name".into(), Value::Text(name.to_string()));
        props.insert("event_kind".into(), Value::Text(event_kind.to_string()));
        props.insert("type_filter".into(), Value::Text(type_filter.to_string()));
        props.insert("program_koid".into(), Value::Text(program_koid.to_string()));
        self.remember(RememberRequest {
            context: subject.clone().into(),
            koid: None, expected_version: Some(0),
            idempotency_key: Some(format!("deploy-trigger-{}", name)),
            metadata: Metadata {
                type_name: "mnemosyne:trigger".into(), tenant: None, schema_version: 1,
                tags: vec!["trigger".into(), "active-object".into()],
            },
            properties: props,
            semantic: None, relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: subject.name.clone(), acl: vec![], classification: None,
            }),
            extensions: ExtensionMap::new(), origin: Origin::Human,
            note: Some(format!("Deployed trigger: {}", name)),
            referential_policy: ReferentialPolicy::Permissive,
        })
    }

    // ---- Agent KO (MRFC-0030 Phase 7c) ------------------------------------

    /// Deploy an Agent KO — an AI agent definition with prompt, skills, tools, policies.
    pub fn deploy_agent(
        &self, name: &str, prompt: &str, skills_json: &str,
        tools_json: &str, policies_json: &str, subject: &Subject,
    ) -> KResult<Remembered> {
        let mut props = PropertyMap::new();
        props.insert("name".into(), Value::Text(name.to_string()));
        props.insert("prompt".into(), Value::Text(prompt.to_string()));
        props.insert("skills".into(), Value::Text(skills_json.to_string()));
        props.insert("tools".into(), Value::Text(tools_json.to_string()));
        props.insert("policies".into(), Value::Text(policies_json.to_string()));
        props.insert("version".into(), Value::Int(1));
        self.remember(RememberRequest {
            context: subject.clone().into(),
            koid: None, expected_version: Some(0),
            idempotency_key: Some(format!("deploy-agent-{}", name)),
            metadata: Metadata {
                type_name: "mnemosyne:agent".into(),
                tenant: None, schema_version: 1,
                tags: vec!["agent".into(), "active-object".into()],
            },
            properties: props,
            semantic: None, relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: subject.name.clone(), acl: vec![], classification: None,
            }),
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: Some(format!("Deployed agent: {}", name)),
            referential_policy: ReferentialPolicy::Permissive,
        })
    }

    /// List all deployed agents.
    pub fn list_agents(&self, subject: &Subject) -> KResult<Vec<KnowledgeObject>> {
        self.scan_by_type(subject, "mnemosyne:agent")
    }

    // ---- Connector KO (MRFC-0030 Phase 7b) --------------------------------

    /// Deploy a Connector KO — external system import/export as a KO.
    pub fn deploy_connector(
        &self, name: &str, plugin: &str, config_json: &str,
        mapping_json: &str, subject: &Subject,
    ) -> KResult<Remembered> {
        let mut props = PropertyMap::new();
        props.insert("name".into(), Value::Text(name.to_string()));
        props.insert("plugin".into(), Value::Text(plugin.to_string()));
        props.insert("config".into(), Value::Text(config_json.to_string()));
        props.insert("mapping".into(), Value::Text(mapping_json.to_string()));
        self.remember(RememberRequest {
            context: subject.clone().into(),
            koid: None, expected_version: Some(0),
            idempotency_key: Some(format!("deploy-connector-{}", name)),
            metadata: Metadata {
                type_name: "mnemosyne:connector".into(),
                tenant: None, schema_version: 1,
                tags: vec!["connector".into(), "active-object".into()],
            },
            properties: props,
            semantic: None, relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: subject.name.clone(), acl: vec![], classification: None,
            }),
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: Some(format!("Deployed connector: {}", name)),
            referential_policy: ReferentialPolicy::Permissive,
        })
    }

    /// List all deployed connectors.
    pub fn list_connectors(&self, subject: &Subject) -> KResult<Vec<KnowledgeObject>> {
        self.scan_by_type(subject, "mnemosyne:connector")
    }

    // ---- ABI version (MRFC-0011 §9) --------------------------------------

    /// Return the ABI version of this kernel. Adapters can check this to
    /// refuse incompatible versions. Bumped on any breaking syscall change.
    pub fn abi_version(&self) -> u32 {
        1
    }

    // ---- Offline-verifiable prove (MRFC-0011 §6.7) ------------------------

    /// Export the full audit chain for a claim so it can be independently
    /// verified without a running kernel. Returns all knowledge events
    /// in the journal plus the current head audit hash.
    pub fn prove_export(&self) -> KResult<OfflineProof> {
        let events = self.repo.scan_events()?;
        let (seq, audit) = self.journal_head()?;
        Ok(OfflineProof {
            abi_version: self.abi_version(),
            journal_seq: seq,
            head_audit_hash: audit,
            events,
        })
    }

    // ---- Class B syscalls (MRFC-0011 §5, §6.10-6.13) ----------------------

    /// Execute a reasoning rule against the knowledge graph.
    /// Returns provenance-tagged claims with `origin=Reason`.
    /// ponytail: synchronous version for Phase 2; full async JobHandle in Phase 3.
    pub fn reason(&self, rule_type: &str, rule_props: PropertyMap) -> KResult<Vec<KnowledgeObject>> {
        let subject = Subject { name: "kernel-reason".into(), roles: vec!["admin".into()] };
        // Scan objects matching the rule's conditions and produce claims.
        let candidates = self.scan_by_type(&subject, rule_type)?;
        let mut claims = Vec::new();
        for ko in candidates {
            let mut match_count = 0usize;
            for (key, expected) in &rule_props {
                if let Some(v) = ko.properties.get(key) {
                    if v == expected {
                        match_count += 1;
                    }
                }
            }
            if match_count == rule_props.len() && !rule_props.is_empty() {
                let mut claim_props = ko.properties.clone();
                claim_props.insert("reasoned_from".into(), Value::Text(ko.koid.to_hex()));
                claims.push(KnowledgeObject {
                    koid: KOID::ZERO, version: 0, commit_ts: 0,
                    metadata: Metadata {
                        type_name: format!("{}-claim", rule_type),
                        tenant: None, schema_version: 1, tags: vec!["reasoned".into()],
                    },
                    properties: claim_props,
                    semantic: None, relationships: vec![], event_refs: vec![],
                    security: SecurityDescriptor {
                        owner: "kernel-reason".into(), acl: vec![], classification: None,
                    },
                    lifecycle: Lifecycle { state: LifecycleState::Draft, origin: Origin::Reason },
                    extensions: ExtensionMap::new(),
                });
            }
        }
        Ok(claims)
    }

    /// Infer new knowledge from existing objects using similarity matching.
    /// Takes a prototype type and properties, finds similar objects, and
    /// returns them with provenance.
    pub fn infer(&self, subject: &Subject, type_name: &str, similarity_text: &str) -> KResult<Vec<ScoredKO>> {
        self.find_similar(SimilarityQuery {
            context: subject.clone().into(),
            filter: Some(PropertyFilter {
                type_name: Some(type_name.to_string()),
                required: vec![],
            }),
            text: Some(similarity_text.to_string()),
            vector: None,
            embedding_model: None,
            k: 10,
            fusion: Fusion::TextOnly,
        })
    }

    /// Predict properties for a target object based on similar objects.
    /// Returns a merged property map from the top-k most similar objects.
    pub fn predict(
        &self,
        subject: &Subject,
        type_name: &str,
        target_props: &PropertyMap,
        k: usize,
    ) -> KResult<PropertyMap> {
        // Build similarity text from target properties.
        let text: String = target_props.values()
            .map(|v| match v {
                Value::Text(s) => s.clone(),
                other => format!("{:?}", other),
            })
            .collect::<Vec<_>>()
            .join(" ");
        let similar = self.infer(subject, type_name, &text)?;
        let mut merged = PropertyMap::new();
        for scored in similar.iter().take(k) {
            for (key, val) in &scored.ko.properties {
                if !merged.contains_key(key) {
                    merged.insert(key.clone(), val.clone());
                }
            }
        }
        merged.insert("predicted_from_count".into(), Value::Int(similar.len() as i64));
        Ok(merged)
    }
}

/// An independently-verifiable proof bundle (MRFC-0011 §6.7).
#[derive(Clone, Debug)]
pub struct OfflineProof {
    pub abi_version: u32,
    pub journal_seq: u64,
    pub head_audit_hash: [u8; 32],
    pub events: Vec<KnowledgeEvent>,
}

// ---------------------------------------------------------------------------
// Scoring helpers now live in `crate::index`. Re-export here for the legacy
// unit tests in this module until those tests migrate to the index module.
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) use crate::knowledge::scoring::{cosine, jaccard, tokenize};

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::store::MemoryEngine;
    use crate::storage::store_redb::RedbEngine;
    use std::collections::BTreeMap;

    fn kernel() -> (Kernel, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(1_000));
        let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 42).unwrap();
        (k, clock)
    }

    fn meta(t: &str) -> Metadata {
        Metadata {
            type_name: t.into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        }
    }

    #[test]
    fn hlc_is_monotonic_under_same_and_regressing_clock() {
        let h = Hlc::new();
        let c = ManualClock::new(100);
        let a = h.now(&c);
        let b = h.now(&c);
        assert!(b > a);
        c.set(1); // regression
        let d = h.now(&c);
        assert!(d > b);
    }

    #[test]
    fn create_then_head_and_snapshot_reads() {
        let (k, clock) = kernel();
        let alice = Subject::new("alice");
        let r = k
            .remember(RememberRequest::create(alice.clone(), meta("fact")))
            .unwrap();
        assert_eq!(r.version, 1);
        let head = k.get(&alice, &r.koid).unwrap();
        assert_eq!(head.version, 1);
        assert_eq!(head.lifecycle.state, LifecycleState::Draft);

        let snap = k.snapshot();
        clock.tick(5);
        let mut req = RememberRequest::update(alice.clone(), r.koid, meta("fact"));
        req.properties.insert("n".into(), Value::Int(2));
        k.remember(req).unwrap();
        let old = k.get_at(&alice, &r.koid, snap).unwrap();
        assert_eq!(old.version, 1);
        let new = k.get(&alice, &r.koid).unwrap();
        assert_eq!(new.version, 2);
    }

    #[test]
    fn durable_subscription_replay_and_ack() {
        let (k, _clock) = kernel();
        let alice = Subject::new("alice");
        let rx = k.subscribe("s1".into(), EventFilter::default()).unwrap();

        let r1 = k
            .remember(RememberRequest::create(alice.clone(), meta("fact")))
            .unwrap();
        let e1 = rx.recv().unwrap();
        assert_eq!(e1.koid, r1.koid);
        assert_eq!(e1.kind, EventKind::Created);

        k.ack("s1", e1.seq).unwrap();
        let replay = k.replay("s1").unwrap();
        assert!(replay.is_empty(), "acked events must not replay");

        let _r2 = k
            .remember(RememberRequest::create(alice.clone(), meta("fact")))
            .unwrap();
        let e2 = rx.recv().unwrap();
        assert!(e2.seq > e1.seq);

        // without acking e2, replay returns it
        let replay = k.replay("s1").unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].seq, e2.seq);

        k.unsubscribe("s1").unwrap();
        assert!(k.replay("s1").is_err());
    }

    #[test]
    fn durable_subscription_survives_reopen() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mnemosyne_sub_reopen_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let clock = Arc::new(ManualClock::new(1_000));
        let engine = Arc::new(RedbEngine::open(path.to_str().unwrap()).unwrap());
        let k = Kernel::open(engine.clone(), clock.clone(), 42).unwrap();
        let alice = Subject::new("alice");

        let _rx = k.subscribe("s1".into(), EventFilter::default()).unwrap();
        let r = k
            .remember(RememberRequest::create(alice.clone(), meta("fact")))
            .unwrap();
        // do not ack — subscription must replay after reopen
        drop(k);

        let k2 = Kernel::open(engine, clock, 42).unwrap();
        let replay = k2.replay("s1").unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].koid, r.koid);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cross_agent_policy_allows_via_role_inheritance() {
        let (k, _clock) = kernel();
        let admin = Subject::with_roles("admin", &["admin"]);

        let mut senior = RememberRequest::create(admin.clone(), meta(ROLE_TYPE));
        senior
            .properties
            .insert("name".into(), Value::Text("senior".into()));
        senior
            .properties
            .insert("parents".into(), Value::List(vec![]));
        k.remember(senior).unwrap();

        let mut junior = RememberRequest::create(admin.clone(), meta(ROLE_TYPE));
        junior
            .properties
            .insert("name".into(), Value::Text("junior".into()));
        junior.properties.insert(
            "parents".into(),
            Value::List(vec![Value::Text("senior".into())]),
        );
        k.remember(junior).unwrap();

        let mut policy = RememberRequest::create(admin.clone(), meta(POLICY_TYPE));
        policy
            .properties
            .insert("target_type".into(), Value::Text("shared_note".into()));
        policy.properties.insert(
            "rules".into(),
            Value::List(vec![Value::Map(BTreeMap::from([
                ("principal".into(), Value::Text("senior".into())),
                ("action".into(), Value::Text("read".into())),
                ("effect".into(), Value::Text("allow".into())),
            ]))]),
        );
        k.remember(policy).unwrap();

        let alice = Subject::with_roles("alice", &["junior"]);
        let note = k
            .remember(RememberRequest::create(alice.clone(), meta("shared_note")))
            .unwrap();

        let bob = Subject::with_roles("bob", &["junior"]);
        let got = k.get(&bob, &note.koid).unwrap();
        assert_eq!(got.metadata.type_name, "shared_note");

        let carol = Subject::new("carol");
        assert!(matches!(
            k.get(&carol, &note.koid),
            Err(KError::AccessDenied { .. })
        ));
    }

    #[test]
    fn policy_deny_overrides_allow() {
        let (k, _clock) = kernel();
        let admin = Subject::with_roles("admin", &["admin"]);

        let mut employee = RememberRequest::create(admin.clone(), meta(ROLE_TYPE));
        employee
            .properties
            .insert("name".into(), Value::Text("employee".into()));
        employee
            .properties
            .insert("parents".into(), Value::List(vec![]));
        k.remember(employee).unwrap();

        let mut intern = RememberRequest::create(admin.clone(), meta(ROLE_TYPE));
        intern
            .properties
            .insert("name".into(), Value::Text("intern".into()));
        intern.properties.insert(
            "parents".into(),
            Value::List(vec![Value::Text("employee".into())]),
        );
        k.remember(intern).unwrap();

        let mut policy = RememberRequest::create(admin.clone(), meta(POLICY_TYPE));
        policy
            .properties
            .insert("target_type".into(), Value::Text("shared_note".into()));
        policy.properties.insert(
            "rules".into(),
            Value::List(vec![
                Value::Map(BTreeMap::from([
                    ("principal".into(), Value::Text("employee".into())),
                    ("action".into(), Value::Text("read".into())),
                    ("effect".into(), Value::Text("allow".into())),
                ])),
                Value::Map(BTreeMap::from([
                    ("principal".into(), Value::Text("intern".into())),
                    ("action".into(), Value::Text("read".into())),
                    ("effect".into(), Value::Text("deny".into())),
                ])),
            ]),
        );
        k.remember(policy).unwrap();

        let alice = Subject::with_roles("alice", &["employee"]);
        let note = k
            .remember(RememberRequest::create(alice.clone(), meta("shared_note")))
            .unwrap();
        assert!(k.get(&alice, &note.koid).is_ok());

        let bob = Subject::with_roles("bob", &["intern"]);
        assert!(matches!(
            k.get(&bob, &note.koid),
            Err(KError::AccessDenied { .. })
        ));
    }

    #[test]
    fn cross_agent_acl_with_role_inheritance() {
        let (k, _clock) = kernel();
        let alice = Subject::new("alice");
        let sec = SecurityDescriptor {
            owner: "alice".into(),
            acl: vec![AclEntry {
                principal: "senior".into(),
                action: Action::Read,
                effect: Effect::Allow,
            }],
            classification: None,
        };
        let mut req = RememberRequest::create(alice.clone(), meta("shared_note"));
        req.security = Some(sec);
        let note = k.remember(req).unwrap();

        let bob = Subject::with_roles("bob", &["senior"]);
        assert!(k.get(&bob, &note.koid).is_ok());

        let carol = Subject::with_roles("carol", &["junior"]);
        assert!(matches!(
            k.get(&carol, &note.koid),
            Err(KError::AccessDenied { .. })
        ));

        let admin = Subject::with_roles("admin", &["admin"]);
        let mut junior = RememberRequest::create(admin.clone(), meta(ROLE_TYPE));
        junior
            .properties
            .insert("name".into(), Value::Text("junior".into()));
        junior.properties.insert(
            "parents".into(),
            Value::List(vec![Value::Text("senior".into())]),
        );
        k.remember(junior).unwrap();

        assert!(k.get(&carol, &note.koid).is_ok());
    }

    #[test]
    fn cosine_and_jaccard_behave() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0); // dim mismatch
        let a = tokenize("the agent remembered everything");
        let b = tokenize("agent remembered");
        let j = jaccard(&a, &b);
        assert!(j > 0.0 && j < 1.0);
    }
}
