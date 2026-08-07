//! Knowledge Object Model (KOM) — canonical types per MRFC-0001.
//!
//! Normative mapping:
//! - MRFC-0001 §4 req 1–3: every persisted entity is a KO with one immutable KOID;
//!   every mutation creates a new logical version (enforced by the commit pipeline).
//! - MRFC-0001 §5: canonical KO blocks (Identity..Extensions).
//! - MRFC-0001 §6: lifecycle state machine, illegal transitions => deterministic error.
//! - MRFC-0001 §11: error model (extended by MRFC-0011 §8).
//!
//! This module is std-only and free of I/O so it stays deterministic and
//! model-checkable (`loom` in later increments).

use std::collections::{BTreeMap, HashSet};
use std::fmt;

// ---------------------------------------------------------------------------
// Referential integrity policy (MRFC-0001 §7)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReferentialPolicy {
    /// Every RelationshipRef target must resolve to an existing head object.
    Strict,
    /// Relationship targets are not validated; dangling refs are allowed.
    #[default]
    Permissive,
}

impl ReferentialPolicy {
    pub fn tag(self) -> u8 {
        match self {
            ReferentialPolicy::Strict => 0,
            ReferentialPolicy::Permissive => 1,
        }
    }
    pub fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(ReferentialPolicy::Strict),
            1 => Some(ReferentialPolicy::Permissive),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Identity (MRFC-0001 §3: KOID = immutable global identifier)
// ---------------------------------------------------------------------------

pub const KOID_LEN: usize = 16;

/// Immutable global Knowledge Object identifier.
/// Layout (big-endian): 48-bit epoch millis | 32-bit per-millis counter | 48-bit generator salt.
/// Time-ordered so KOIDs have good locality in ordered KV stores.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KOID(pub [u8; KOID_LEN]);

impl KOID {
    pub const ZERO: KOID = KOID([0u8; KOID_LEN]);

    pub fn from_bytes(b: [u8; KOID_LEN]) -> Self {
        KOID(b)
    }

    pub fn as_bytes(&self) -> &[u8; KOID_LEN] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(KOID_LEN * 2);
        for b in &self.0 {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }

    /// Parse a 32-char hex string (as produced by `to_hex`) back into a KOID.
    pub fn from_hex(s: &str) -> KResult<Self> {
        let b = s.as_bytes();
        if b.len() != KOID_LEN * 2 {
            return Err(KError::InvalidObject(format!(
                "koid hex must be {} chars, got {}",
                KOID_LEN * 2,
                b.len()
            )));
        }
        let mut out = [0u8; KOID_LEN];
        for i in 0..KOID_LEN {
            let hi = (b[i * 2] as char).to_digit(16);
            let lo = (b[i * 2 + 1] as char).to_digit(16);
            match (hi, lo) {
                (Some(h), Some(l)) => out[i] = ((h << 4) | l) as u8,
                _ => {
                    return Err(KError::InvalidObject(
                        "koid hex contains non-hex char".into(),
                    ))
                }
            }
        }
        Ok(KOID(out))
    }
}

impl fmt::Debug for KOID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KOID({})", self.to_hex())
    }
}

impl fmt::Display for KOID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// Monotonic, seedable KOID generator. Deterministic given the same salt and
/// clock sequence — a hard requirement for conformance replay (MRFC-0011 §11).
pub struct IdGen {
    salt: u64,
    last_ms: u64,
    counter: u32,
}

impl IdGen {
    pub fn new(salt: u64) -> Self {
        IdGen {
            salt,
            last_ms: 0,
            counter: 0,
        }
    }

    pub fn next(&mut self, now_ms: u64) -> KOID {
        let ms = if now_ms > self.last_ms {
            self.counter = 0;
            now_ms
        } else {
            self.counter = self.counter.wrapping_add(1);
            self.last_ms
        };
        self.last_ms = ms;
        let mut b = [0u8; KOID_LEN];
        b[0..6].copy_from_slice(&ms.to_be_bytes()[2..8]);
        b[6..10].copy_from_slice(&self.counter.to_be_bytes());
        b[10..16].copy_from_slice(&self.salt.to_be_bytes()[2..8]);
        KOID(b)
    }
}

// ---------------------------------------------------------------------------
// Properties (MRFC-0001 §5: Properties + Extensions blocks)
// ---------------------------------------------------------------------------

/// Canonical property value. Map keys are sorted (BTreeMap) so encoding is canonical.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

pub type PropertyMap = BTreeMap<String, Value>;

/// Unknown extension fields MUST survive round-trip serialization (MRFC-0001 req 9).
pub type ExtensionMap = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub struct Metadata {
    pub type_name: String,
    pub tenant: Option<String>,
    pub schema_version: u32,
    pub tags: Vec<String>,
}

/// Semantic metadata is OPTIONAL (MRFC-0001 req 8) and never mutated by storage
/// (MRFC-0001 §13). Vectors are namespaced by `embedding_model` (review R7).
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticBlock {
    pub embedding_model: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub confidence: Option<f32>,
    pub source: Option<String>,
    pub summary: Option<String>,
}

// ---------------------------------------------------------------------------
// Relationships & Events (MRFC-0001 §3: KR / KE)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Outbound,
    Inbound,
}

impl Direction {
    pub fn tag(self) -> u8 {
        match self {
            Direction::Outbound => 0,
            Direction::Inbound => 1,
        }
    }
    pub fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(Direction::Outbound),
            1 => Some(Direction::Inbound),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RelationshipRef {
    pub rel_type: String,
    pub target: KOID,
    pub direction: Direction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Created,
    Updated,
    Forgotten,
    LifecycleChanged,
    ClaimAsserted,
    Audit,
}

impl EventKind {
    pub fn tag(self) -> u8 {
        match self {
            EventKind::Created => 0,
            EventKind::Updated => 1,
            EventKind::Forgotten => 2,
            EventKind::LifecycleChanged => 3,
            EventKind::ClaimAsserted => 4,
            EventKind::Audit => 5,
        }
    }
    pub fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(EventKind::Created),
            1 => Some(EventKind::Updated),
            2 => Some(EventKind::Forgotten),
            3 => Some(EventKind::LifecycleChanged),
            4 => Some(EventKind::ClaimAsserted),
            5 => Some(EventKind::Audit),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventRef {
    pub seq: u64,
    pub kind: EventKind,
    pub commit_ts: u64,
}

/// Who produced a version. Claims from Class B syscalls re-enter the store
/// tagged with non-Human origins (MRFC-0011 §6.10–6.13).
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Human,
    Agent(String),
    SemanticEnrichment,
    Reason,
    System,
}

// ---------------------------------------------------------------------------
// Security (MRFC-0001 §12)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Read,
    Write,
    Evolve,
    Delete,
    Admin,
}

impl Action {
    pub fn tag(self) -> u8 {
        match self {
            Action::Read => 0,
            Action::Write => 1,
            Action::Evolve => 2,
            Action::Delete => 3,
            Action::Admin => 4,
        }
    }
    pub fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(Action::Read),
            1 => Some(Action::Write),
            2 => Some(Action::Evolve),
            3 => Some(Action::Delete),
            4 => Some(Action::Admin),
            _ => None,
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Action::Read),
            "write" => Some(Action::Write),
            "evolve" => Some(Action::Evolve),
            "delete" => Some(Action::Delete),
            "admin" => Some(Action::Admin),
            _ => None,
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Action::Read => "read",
            Action::Write => "write",
            Action::Evolve => "evolve",
            Action::Delete => "delete",
            Action::Admin => "admin",
        };
        write!(f, "{}", s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Allow,
    Deny,
}

impl Effect {
    pub fn tag(self) -> u8 {
        match self {
            Effect::Allow => 0,
            Effect::Deny => 1,
        }
    }
    pub fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(Effect::Allow),
            1 => Some(Effect::Deny),
            _ => None,
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Effect::Allow),
            "deny" => Some(Effect::Deny),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AclEntry {
    /// Principal name or role name.
    pub principal: String,
    pub action: Action,
    pub effect: Effect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SecurityDescriptor {
    pub owner: String,
    pub acl: Vec<AclEntry>,
    pub classification: Option<String>,
}

// ---------------------------------------------------------------------------
// Lifecycle (MRFC-0001 §6): Draft -> Active -> Verified -> Archived -> Deleted
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleState {
    Draft,
    Active,
    Verified,
    Archived,
    Deleted,
}

impl LifecycleState {
    pub fn tag(self) -> u8 {
        match self {
            LifecycleState::Draft => 0,
            LifecycleState::Active => 1,
            LifecycleState::Verified => 2,
            LifecycleState::Archived => 3,
            LifecycleState::Deleted => 4,
        }
    }
    pub fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(LifecycleState::Draft),
            1 => Some(LifecycleState::Active),
            2 => Some(LifecycleState::Verified),
            3 => Some(LifecycleState::Archived),
            4 => Some(LifecycleState::Deleted),
            _ => None,
        }
    }

    /// Strict chain per MRFC-0001 §6. Any other transition is illegal and the
    /// kernel MUST return `INVALID_STATE` deterministically.
    pub fn can_transition(self, to: LifecycleState) -> bool {
        use LifecycleState::*;
        matches!(
            (self, to),
            (Draft, Active) | (Active, Verified) | (Verified, Archived) | (Archived, Deleted)
        )
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            LifecycleState::Draft => "draft",
            LifecycleState::Active => "active",
            LifecycleState::Verified => "verified",
            LifecycleState::Archived => "archived",
            LifecycleState::Deleted => "deleted",
        };
        write!(f, "{}", s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lifecycle {
    pub state: LifecycleState,
    pub origin: Origin,
}

// ---------------------------------------------------------------------------
// Canonical Knowledge Object (MRFC-0001 §5)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct KnowledgeObject {
    // Identity block
    pub koid: KOID,
    pub version: u64,
    pub commit_ts: u64,
    // Metadata block
    pub metadata: Metadata,
    // Properties block
    pub properties: PropertyMap,
    // Semantic block (optional)
    pub semantic: Option<SemanticBlock>,
    // RelationshipRefs block
    pub relationships: Vec<RelationshipRef>,
    // EventRefs block
    pub event_refs: Vec<EventRef>,
    // Security block
    pub security: SecurityDescriptor,
    // Lifecycle block
    pub lifecycle: Lifecycle,
    // Extensions block (unknown fields preserved)
    pub extensions: ExtensionMap,
}

impl KnowledgeObject {
    pub fn new(koid: KOID, metadata: Metadata, security: SecurityDescriptor) -> Self {
        KnowledgeObject {
            koid,
            version: 0,
            commit_ts: 0,
            metadata,
            properties: PropertyMap::new(),
            semantic: None,
            relationships: Vec::new(),
            event_refs: Vec::new(),
            security,
            lifecycle: Lifecycle {
                state: LifecycleState::Draft,
                origin: Origin::System,
            },
            extensions: ExtensionMap::new(),
        }
    }

    /// MRFC-0001 §10 validation rules (subset enforceable at the type boundary;
    /// duplicate property identifiers are impossible by construction — BTreeMap).
    pub fn validate(&self) -> KResult<()> {
        if self.metadata.type_name.trim().is_empty() {
            return Err(KError::InvalidObject(
                "metadata.type_name must be non-empty".into(),
            ));
        }
        if self.security.owner.trim().is_empty() {
            return Err(KError::InvalidObject(
                "security.owner must be non-empty".into(),
            ));
        }
        for t in &self.metadata.tags {
            if t.trim().is_empty() {
                return Err(KError::InvalidObject(
                    "metadata.tags must not contain empty entries".into(),
                ));
            }
        }
        for r in &self.relationships {
            if r.rel_type.trim().is_empty() {
                return Err(KError::InvalidObject(
                    "relationships[].rel_type must be non-empty".into(),
                ));
            }
        }
        for entry in &self.security.acl {
            if entry.principal.trim().is_empty() {
                return Err(KError::InvalidObject(
                    "security.acl[].principal must be non-empty".into(),
                ));
            }
        }
        Ok(())
    }

    /// Validate this KO against a registered schema. Makes `KError::InvalidSchema`
    /// reachable and enforces type/version/required-property/unknown-core-field
    /// invariants.
    pub fn validate_against(&self, schema: &Schema) -> KResult<()> {
        schema.ensure_allowed_includes_required();
        if self.metadata.type_name != schema.type_name {
            return Err(KError::InvalidSchema(format!(
                "type_name mismatch: expected '{}', got '{}'",
                schema.type_name, self.metadata.type_name
            )));
        }
        if self.metadata.schema_version != schema.schema_version {
            return Err(KError::InvalidSchema(format!(
                "schema_version mismatch: expected {}, got {}",
                schema.schema_version, self.metadata.schema_version
            )));
        }
        for req in &schema.required_properties {
            if !self.properties.contains_key(req) {
                return Err(KError::InvalidSchema(format!(
                    "missing required property: '{}'",
                    req
                )));
            }
        }
        if let Some(allowed) = &schema.allowed_properties {
            for key in self.properties.keys() {
                if !allowed.contains(key) {
                    return Err(KError::InvalidSchema(format!(
                        "unknown core field: '{}'",
                        key
                    )));
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Schema (MRFC-0001 §10 schema validation / INVALID_SCHEMA)
// ---------------------------------------------------------------------------

/// A lightweight schema definition against which KOs can be validated.
/// This is the Increment-1 subset: type, version, required property keys, and
/// optional closed-world allowed-property set. Future increments add property
/// types, relationship cardinality, and semantic constraints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schema {
    pub type_name: String,
    pub schema_version: u32,
    pub required_properties: Vec<String>,
    /// If `Some`, the schema is "closed": any core property not listed here is
    /// treated as an unknown core field and rejected (MRFC-0001 §10).
    /// If `None`, unknown core properties are allowed (open-world default).
    pub allowed_properties: Option<HashSet<String>>,
}

impl Schema {
    pub fn new(type_name: &str, schema_version: u32) -> Self {
        Schema {
            type_name: type_name.into(),
            schema_version,
            required_properties: Vec::new(),
            allowed_properties: None,
        }
    }

    pub fn require(mut self, prop: &str) -> Self {
        self.required_properties.push(prop.into());
        self
    }

    /// Enable closed-world validation: only the listed properties are permitted
    /// in the KO `properties` block. Automatically includes required properties.
    pub fn allow(mut self, prop: &str) -> Self {
        self.allowed_properties
            .get_or_insert_with(HashSet::new)
            .insert(prop.into());
        self
    }

    fn ensure_allowed_includes_required(&self) {
        if let Some(allowed) = &self.allowed_properties {
            for req in &self.required_properties {
                assert!(
                    allowed.contains(req),
                    "required property '{}' must also be allowed",
                    req
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public trait (MRFC-0001 §9)
// ---------------------------------------------------------------------------

/// Canonical abstraction exposed by every KOM implementation.
pub trait KnowledgeEntity {
    fn id(&self) -> KOID;
    fn metadata(&self) -> &Metadata;
    fn properties(&self) -> &PropertyMap;
    fn relationships(&self) -> &[RelationshipRef];
    fn events(&self) -> &[EventRef];
    fn security(&self) -> &SecurityDescriptor;
    fn semantic(&self) -> Option<&SemanticBlock>;
}

impl KnowledgeEntity for KnowledgeObject {
    fn id(&self) -> KOID {
        self.koid
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
    fn properties(&self) -> &PropertyMap {
        &self.properties
    }
    fn relationships(&self) -> &[RelationshipRef] {
        &self.relationships
    }
    fn events(&self) -> &[EventRef] {
        &self.event_refs
    }
    fn security(&self) -> &SecurityDescriptor {
        &self.security
    }
    fn semantic(&self) -> Option<&SemanticBlock> {
        self.semantic.as_ref()
    }
}

// ---------------------------------------------------------------------------
// Knowledge Event (append-only journal entry, MRFC-0001 §4 req 5)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct KnowledgeEvent {
    /// Journal sequence number (monotone, gapless per kernel).
    pub seq: u64,
    pub koid: KOID,
    pub version: u64,
    pub kind: EventKind,
    pub origin: Origin,
    pub actor: String,
    pub commit_ts: u64,
    /// SHA-256 of the committed object payload. Protects audit integrity
    /// per MRFC-0001 §12 (replaces the earlier FNV-1a-64 placeholder).
    pub payload_hash: [u8; 32],
    /// Hash-chain links for tamper evidence.
    pub prev_audit_hash: [u8; 32],
    pub audit_hash: [u8; 32],
    /// Optional HMAC-SHA256 signature of the payload. Enabled when the kernel
    /// is opened with a signing key; proves payload integrity independently of
    /// the audit chain (MRFC-0011 §6.7).
    pub signature: Option<[u8; 32]>,
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// Error model (MRFC-0001 §11 + MRFC-0011 §8)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum KError {
    InvalidObject(String),
    InvalidSchema(String),
    InvalidQuery(String),
    VersionConflict {
        koid: KOID,
        expected: u64,
        found: u64,
    },
    AccessDenied {
        subject: String,
        action: Action,
        koid: KOID,
    },
    InvalidState {
        from: LifecycleState,
        to: LifecycleState,
    },
    NotFound(KOID),
    UnsupportedOperation(String),
    IndexLagExceeded,
    JobRejected(String),
    Store(String),
    Codec(String),
}

impl fmt::Display for KError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KError::InvalidObject(m) => write!(f, "INVALID_OBJECT: {}", m),
            KError::InvalidSchema(m) => write!(f, "INVALID_SCHEMA: {}", m),
            KError::InvalidQuery(m) => write!(f, "INVALID_QUERY: {}", m),
            KError::VersionConflict {
                koid,
                expected,
                found,
            } => write!(
                f,
                "VERSION_CONFLICT: {} expected version {} found {}",
                koid, expected, found
            ),
            KError::AccessDenied {
                subject,
                action,
                koid,
            } => {
                write!(f, "ACCESS_DENIED: {} cannot {} {}", subject, action, koid)
            }
            KError::InvalidState { from, to } => {
                write!(f, "INVALID_STATE: {} -> {}", from, to)
            }
            KError::NotFound(k) => write!(f, "NOT_FOUND: {}", k),
            KError::UnsupportedOperation(m) => write!(f, "UNSUPPORTED_OPERATION: {}", m),
            KError::IndexLagExceeded => write!(f, "INDEX_LAG_EXCEEDED"),
            KError::JobRejected(m) => write!(f, "JOB_REJECTED: {}", m),
            KError::Store(m) => write!(f, "STORE: {}", m),
            KError::Codec(m) => write!(f, "CODEC: {}", m),
        }
    }
}

impl std::error::Error for KError {}

pub type KResult<T> = Result<T, KError>;

// ---------------------------------------------------------------------------
// Deterministic hashes
// ---------------------------------------------------------------------------

/// FNV-1a 64-bit. Deterministic across platforms and processes.
/// Retained for non-audit use cases; the audit stream uses SHA-256.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// SHA-256 integrity hash. Used for audit-chain integrity (MRFC-0001 §12).
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// HMAC-SHA256 keyed signature. Used for at-rest version signatures
/// when a signing key is configured (MRFC-0011 §6.7).
pub fn hmac_sha256(key: &[u8; 32], bytes: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(bytes);
    mac.finalize().into_bytes().into()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_legal_transitions() {
        use LifecycleState::*;
        let legal = [
            (Draft, Active),
            (Active, Verified),
            (Verified, Archived),
            (Archived, Deleted),
        ];
        for (a, b) in legal {
            assert!(a.can_transition(b), "{} -> {} must be legal", a, b);
        }
    }

    #[test]
    fn lifecycle_illegal_transitions() {
        use LifecycleState::*;
        let states = [Draft, Active, Verified, Archived, Deleted];
        for from in states {
            for to in states {
                let legal = matches!(
                    (from, to),
                    (Draft, Active)
                        | (Active, Verified)
                        | (Verified, Archived)
                        | (Archived, Deleted)
                );
                assert_eq!(from.can_transition(to), legal, "{} -> {}", from, to);
            }
        }
    }

    #[test]
    fn idgen_is_monotonic_and_unique() {
        let mut g = IdGen::new(7);
        let a = g.next(1000);
        let b = g.next(1000); // same ms -> counter bump
        let c = g.next(1001);
        assert!(a < b);
        assert!(b < c);
        assert_ne!(a, b);
        // clock going backwards must not reuse ids
        let d = g.next(5);
        assert!(c < d);
    }

    #[test]
    fn idgen_salts_diverge() {
        let mut g1 = IdGen::new(1);
        let mut g2 = IdGen::new(2);
        assert_ne!(g1.next(1000), g2.next(1000));
    }

    #[test]
    fn fnv_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"mnemosyne"), fnv1a64(b"mnemosyne"));
        assert_ne!(fnv1a64(b"a"), fnv1a64(b"b"));
    }

    #[test]
    fn validate_rejects_empty_acl_principal() {
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![AclEntry {
                    principal: "  ".into(),
                    action: Action::Read,
                    effect: Effect::Allow,
                }],
                classification: None,
            },
        );
        assert!(matches!(ko.validate(), Err(KError::InvalidObject(_))));
        ko.security.acl[0].principal = "bob".into();
        assert!(ko.validate().is_ok());
    }

    #[test]
    fn new_ko_passes_basic_mandatory_validation() {
        let ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![],
                classification: None,
            },
        );
        assert!(ko.validate().is_ok());
    }

    #[test]
    fn knowledge_entity_trait_exposes_all_blocks() {
        use super::KnowledgeEntity;
        let ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![],
                classification: None,
            },
        );
        assert_eq!(ko.id(), KOID::ZERO);
        assert_eq!(ko.metadata().type_name, "fact");
        assert!(ko.properties().is_empty());
        assert!(ko.relationships().is_empty());
        assert!(ko.events().is_empty());
        assert_eq!(ko.security().owner, "alice");
        assert!(ko.semantic().is_none());
    }

    #[test]
    fn validate_against_schema_rejects_type_mismatch() {
        let ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![],
                classification: None,
            },
        );
        let schema = Schema::new("claim", 1);
        assert!(matches!(
            ko.validate_against(&schema),
            Err(KError::InvalidSchema(_))
        ));
    }

    #[test]
    fn validate_against_schema_rejects_version_mismatch() {
        let ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![],
                classification: None,
            },
        );
        let schema = Schema::new("fact", 2);
        assert!(matches!(
            ko.validate_against(&schema),
            Err(KError::InvalidSchema(_))
        ));
    }

    #[test]
    fn validate_against_schema_rejects_unknown_core_field() {
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("title".into(), Value::Text("hello".into()));
        ko.properties
            .insert("extra".into(), Value::Text("surprise".into()));
        let schema = Schema::new("fact", 1).require("title").allow("title");
        assert!(matches!(
            ko.validate_against(&schema),
            Err(KError::InvalidSchema(_))
        ));

        ko.properties.remove("extra");
        assert!(ko.validate_against(&schema).is_ok());
    }

    #[test]
    fn validate_open_world_schema_allows_unknown_core_fields() {
        let mut ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![],
                classification: None,
            },
        );
        ko.properties
            .insert("anything".into(), Value::Text("goes".into()));
        let schema = Schema::new("fact", 1);
        assert!(ko.validate_against(&schema).is_ok());
    }
}
