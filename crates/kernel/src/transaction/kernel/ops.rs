//! v0.3 K4 — Knowledge transactions: the seven semantic ops plus conflict
//! resolution, implemented as first-class kernel operations.
//!
//! Anti-CRUD-cosplay (reviewer H6): each op enforces its own semantics —
//! - `observe` / `assert`: evidence (and authority) are REQUIRED — an
//!   unbacked claim is rejected, not silently downgraded;
//! - `verify_knowledge`: never reduces to a status flip — requires evidence,
//!   bumps the confidence context (confirmations + last_verified), records
//!   the epistemic transition;
//! - `contradict`: registers the counter-claim AND a persisted Conflict KO
//!   (Unresolved) with per-assertion authority/evidence/timestamp/scope
//!   snapshots — the original claim's status is untouched until a decision;
//! - `supersede`: new claim + old → Superseded + SUPERSEDES edge + valid_to
//!   stamping + dependent sweep, all under one pipe lock;
//! - `merge`: property folding per strategy over ≥2 sources, wired as a
//!   first-class derivation (operation "merge");
//! - `invalidate`: target → Contradicted (where legal) + invalidation stamp +
//!   valid_to=now, then BFS-propagates the stamp through DERIVED_FROM
//!   dependents — the K4 answer to "what derived knowledge became stale".
//!
//! Composite ops preflight everything (heads, versions, transitions, ACL)
//! before the first commit, then commit sequentially under the pipe lock —
//! the same validate-then-commit transaction discipline as `transact()`.

use super::*;
use crate::knowledge::evidence::Evidence;
use std::collections::{HashSet, VecDeque};

// ---------------------------------------------------------------------------
// Requests & results
// ---------------------------------------------------------------------------

/// Record a direct observation of the world. Evidence is mandatory.
pub struct ObservationRequest {
    pub context: KnowledgeContext,
    pub type_name: String,
    pub properties: PropertyMap,
    pub evidence: Vec<Evidence>,
    /// Observation instant (epoch millis). Defaults to now.
    pub valid_from: Option<u64>,
    /// Optional ACL override. Defaults to owner-only.
    pub security: Option<SecurityDescriptor>,
    pub note: Option<String>,
}

impl ObservationRequest {
    pub fn new(context: impl Into<KnowledgeContext>, type_name: impl Into<String>) -> Self {
        ObservationRequest {
            context: context.into(),
            type_name: type_name.into(),
            properties: PropertyMap::new(),
            evidence: Vec::new(),
            valid_from: None,
            security: None,
            note: None,
        }
    }
}

/// Assert knowledge on explicit authority. Evidence and a valid authority are
/// mandatory.
pub struct AssertionRequest {
    pub context: KnowledgeContext,
    pub type_name: String,
    pub properties: PropertyMap,
    /// Authority level (e.g. "owner", "architect"). Required.
    pub authority: Option<String>,
    pub evidence: Vec<Evidence>,
    /// Assertion instant (epoch millis). Defaults to now.
    pub valid_from: Option<u64>,
    /// Optional ACL override. Defaults to owner-only.
    pub security: Option<SecurityDescriptor>,
    pub note: Option<String>,
}

impl AssertionRequest {
    pub fn new(context: impl Into<KnowledgeContext>, type_name: impl Into<String>) -> Self {
        AssertionRequest {
            context: context.into(),
            type_name: type_name.into(),
            properties: PropertyMap::new(),
            authority: None,
            evidence: Vec::new(),
            valid_from: None,
            security: None,
            note: None,
        }
    }
}

/// Independently verify a KO. Evidence is mandatory — `VERIFY X` must not
/// reduce to `X.status = VERIFIED`.
pub struct VerificationRequest {
    pub context: KnowledgeContext,
    pub koid: KOID,
    pub evidence: Vec<Evidence>,
    /// Optional verification confidence; never lowers the existing score.
    pub confidence: Option<f32>,
    pub note: Option<String>,
}

impl VerificationRequest {
    pub fn new(context: impl Into<KnowledgeContext>, koid: KOID) -> Self {
        VerificationRequest {
            context: context.into(),
            koid,
            evidence: Vec::new(),
            confidence: None,
            note: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VerificationResult {
    pub koid: KOID,
    pub version: u64,
    pub commit_ts: u64,
    pub status: EpistemicStatus,
    pub confirmations: u32,
    pub last_verified: Option<u64>,
}

/// Register a competing assertion against an existing claim. Creates the
/// counter-claim (CONTRADICTS edge) and a persisted Conflict KO — the
/// original claim is NOT touched until a resolution decision.
pub struct ContradictionRequest {
    pub context: KnowledgeContext,
    pub claim: KOID,
    /// Type of the counter-claim KO. Defaults to "Claim".
    pub counter_type: String,
    pub counter_props: PropertyMap,
    /// Authority level of the counter-assertion (e.g. "documentation").
    /// Defaults to the origin-derived authority for agent assertions.
    pub authority: Option<String>,
    pub evidence: Vec<Evidence>,
    pub note: Option<String>,
}

impl ContradictionRequest {
    pub fn new(context: impl Into<KnowledgeContext>, claim: KOID) -> Self {
        ContradictionRequest {
            context: context.into(),
            claim,
            counter_type: "Claim".into(),
            counter_props: PropertyMap::new(),
            authority: None,
            evidence: Vec::new(),
            note: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContradictionResult {
    pub counter: KOID,
    pub conflict: KOID,
}

/// Replace a claim with a new generation. Preserves the old KO (history,
/// evidence, edges), transitions it to Superseded with valid_to=now, and
/// sweeps its derived dependents for staleness.
pub struct SupersedeRequest {
    pub context: KnowledgeContext,
    pub old: KOID,
    pub type_name: String,
    pub properties: PropertyMap,
    pub evidence: Vec<Evidence>,
    pub reason: Option<String>,
    pub note: Option<String>,
}

impl SupersedeRequest {
    pub fn new(
        context: impl Into<KnowledgeContext>,
        old: KOID,
        type_name: impl Into<String>,
    ) -> Self {
        SupersedeRequest {
            context: context.into(),
            old,
            type_name: type_name.into(),
            properties: PropertyMap::new(),
            evidence: Vec::new(),
            reason: None,
            note: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SupersedeResult {
    pub old: KOID,
    pub new: KOID,
    /// Derived dependents stamped invalidated (stale) by the sweep.
    pub invalidated_dependents: Vec<KOID>,
}

/// Property-folding strategy for merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Caller supplies the merged result (`properties` is required).
    Manual,
    /// Later commits win; sources folded in commit order.
    NewestWins,
    /// Stronger authority wins; sources folded weakest-first.
    AuthorityWins,
}

/// Merge ≥2 sources into one KO — a first-class derivation (operation
/// "merge") with DERIVED_FROM edges to every source.
pub struct MergeRequest {
    pub context: KnowledgeContext,
    pub type_name: String,
    pub sources: Vec<KOID>,
    /// Required for `MergeStrategy::Manual`.
    pub properties: Option<PropertyMap>,
    pub strategy: MergeStrategy,
    pub evidence: Vec<Evidence>,
    pub reason: Option<String>,
    pub note: Option<String>,
}

impl MergeRequest {
    pub fn new(
        context: impl Into<KnowledgeContext>,
        type_name: impl Into<String>,
        sources: Vec<KOID>,
    ) -> Self {
        MergeRequest {
            context: context.into(),
            type_name: type_name.into(),
            sources,
            properties: None,
            strategy: MergeStrategy::Manual,
            evidence: Vec::new(),
            reason: None,
            note: None,
        }
    }
}

/// Withdraw support for a KO and everything derived from it. Evidence is
/// mandatory. The target transitions to Contradicted where legal; every
/// dependent gets the invalidation stamp + valid_to=now (stale, dropped from
/// current truth) but keeps its epistemic status — propagation is a
/// dependency effect, not a claim about the dependents themselves.
pub struct InvalidationRequest {
    pub context: KnowledgeContext,
    pub koid: KOID,
    pub evidence: Vec<Evidence>,
    pub reason: Option<String>,
    pub note: Option<String>,
}

impl InvalidationRequest {
    pub fn new(context: impl Into<KnowledgeContext>, koid: KOID) -> Self {
        InvalidationRequest {
            context: context.into(),
            koid,
            evidence: Vec::new(),
            reason: None,
            note: None,
        }
    }
}

/// The KOIDs invalidated, in BFS order: target first, then its dependents.
#[derive(Clone, Debug, PartialEq)]
pub struct InvalidationResult {
    pub invalidated: Vec<KOID>,
}

/// Resolve a persisted Conflict KO. `decision` must be a resolved state and
/// `rationale` is mandatory — the kernel never silently picks a side.
pub struct ConflictResolutionRequest {
    pub context: KnowledgeContext,
    pub conflict: KOID,
    pub decision: ConflictResolution,
    pub rationale: String,
    /// Required for `ResolvedReplaced`.
    pub replacement: Option<KOID>,
}

/// What a resolution decision did to the claims.
#[derive(Clone, Debug, PartialEq)]
pub struct ConflictResolutionOutcome {
    pub conflict: KOID,
    pub decision: ConflictResolution,
    /// (koid, new epistemic status) for every claim the decision moved.
    pub effects: Vec<(KOID, EpistemicStatus)>,
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn require_evidence(ev: &[Evidence]) -> KResult<()> {
    if ev.is_empty() {
        return Err(KError::InvalidObject(
            "this knowledge operation requires at least one evidence record — an unbacked claim is rejected, not silently downgraded".into(),
        ));
    }
    Ok(())
}

fn evidence_value(ev: &[Evidence]) -> Value {
    KnowledgeObject::evidence_value(ev)
}

/// Rank of a KO's stamped authority (0 when absent/unparseable).
fn ko_authority_rank(ko: &KnowledgeObject) -> u32 {
    ko.extensions
        .get("authority")
        .and_then(|v| match v {
            Value::Text(s) => Authority::from_str(s),
            _ => None,
        })
        .map(|a| a.rank() as u32)
        .unwrap_or(0)
}

/// Per-assertion snapshot for a Conflict KO: authority/evidence/timestamp/
/// scope at contradiction time (the adversarial "3 observations ≠ 3 truths"
/// requirement — the conflict record carries what each side claimed and why).
fn assertion_snapshot(ko: &KnowledgeObject) -> Value {
    let mut m = PropertyMap::new();
    m.insert(
        "authority".into(),
        ko.extensions
            .get("authority")
            .cloned()
            .unwrap_or(Value::Null),
    );
    m.insert(
        "evidence".into(),
        ko.extensions
            .get(KnowledgeObject::EXT_EVIDENCE)
            .cloned()
            .unwrap_or(Value::Null),
    );
    m.insert("timestamp".into(), Value::Int(ko.commit_ts as i64));
    m.insert(
        "scope".into(),
        ko.extensions.get("scope").cloned().unwrap_or(Value::Null),
    );
    Value::Map(m)
}

fn snapshot_authority<'a>(conflict: &'a KnowledgeObject, side: &str) -> Option<&'a Value> {
    conflict.extensions.get("assertions").and_then(|v| match v {
        Value::Map(m) => m.get(side),
        _ => None,
    })
}

fn snapshot_authority_rank(snapshot: Option<&Value>) -> u32 {
    snapshot
        .and_then(|v| match v {
            Value::Map(m) => m.get("authority"),
            _ => None,
        })
        .and_then(|a| match a {
            Value::Text(s) => Authority::from_str(s),
            _ => None,
        })
        .map(|a| a.rank() as u32)
        .unwrap_or(0)
}

/// The two claim KOIDs recorded on a Conflict KO.
fn conflict_claims(conflict: &KnowledgeObject) -> KResult<(KOID, KOID)> {
    let parse = |key: &str| {
        conflict
            .properties
            .get(key)
            .and_then(|v| match v {
                Value::Text(s) => KOID::from_hex(s).ok(),
                _ => None,
            })
            .ok_or_else(|| KError::InvalidObject(format!("conflict KO missing {}", key)))
    };
    Ok((parse("claim_a")?, parse("claim_b")?))
}

impl Kernel {
    // ---- observe ---------------------------------------------------------

    /// Record a direct observation. Evidence is mandatory; the KO is stamped
    /// epistemic Observed (regardless of the calling subject) and anchored at
    /// the observation instant.
    pub fn observe(&self, req: ObservationRequest) -> KResult<Remembered> {
        require_evidence(&req.evidence)?;
        let at = self.clock_now();
        let mut extensions = ExtensionMap::new();
        extensions.insert(
            KnowledgeObject::EXT_EPISTEMIC_STATUS.into(),
            Value::Text("observed".into()),
        );
        extensions.insert(
            KnowledgeObject::EXT_EVIDENCE.into(),
            evidence_value(&req.evidence),
        );
        extensions.insert(
            KnowledgeObject::EXT_VALID_FROM.into(),
            Value::Int(req.valid_from.unwrap_or(at) as i64),
        );
        let rr = RememberRequest {
            context: req.context.clone(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: req.type_name,
                tenant: req.context.tenant.clone(),
                schema_version: 1,
                tags: vec![],
            },
            properties: req.properties,
            semantic: None,
            relationships: vec![],
            security: req.security,
            extensions,
            origin: Origin::System,
            note: req.note,
            referential_policy: ReferentialPolicy::default(),
        };
        self.remember(rr)
    }

    // ---- assert ----------------------------------------------------------

    /// Assert knowledge on explicit authority. Evidence AND a valid authority
    /// level are mandatory — an assertion without them is a bare claim.
    pub fn assert_knowledge(&self, req: AssertionRequest) -> KResult<Remembered> {
        require_evidence(&req.evidence)?;
        let authority = req
            .authority
            .as_deref()
            .and_then(Authority::from_str)
            .ok_or_else(|| {
                KError::InvalidObject("assert requires a valid authority level".into())
            })?;
        let at = self.clock_now();
        let mut extensions = ExtensionMap::new();
        extensions.insert(
            KnowledgeObject::EXT_EPISTEMIC_STATUS.into(),
            Value::Text("asserted".into()),
        );
        extensions.insert("authority".into(), Value::Text(authority.as_str().into()));
        extensions.insert(
            KnowledgeObject::EXT_EVIDENCE.into(),
            evidence_value(&req.evidence),
        );
        extensions.insert(
            KnowledgeObject::EXT_VALID_FROM.into(),
            Value::Int(req.valid_from.unwrap_or(at) as i64),
        );
        let rr = RememberRequest {
            context: req.context.clone(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: req.type_name,
                tenant: req.context.tenant.clone(),
                schema_version: 1,
                tags: vec![],
            },
            properties: req.properties,
            semantic: None,
            relationships: vec![],
            security: req.security,
            extensions,
            origin: Origin::Agent(req.context.subject.name.clone()),
            note: req.note,
            referential_policy: ReferentialPolicy::default(),
        };
        self.remember(rr)
    }

    // ---- verify ----------------------------------------------------------

    /// Independently verify a KO. Not a status flip: evidence is mandatory,
    /// the confidence context is bumped (confirmations + last_verified, score
    /// never lowered), and the epistemic transition is recorded. Verifying an
    /// already-Verified KO is a confirmation bump, not a no-op.
    pub fn verify_knowledge(&self, req: VerificationRequest) -> KResult<VerificationResult> {
        require_evidence(&req.evidence)?;
        let ctx = req.context.clone();
        let mut pipe = self.pipe.lock().unwrap();
        let head = self
            .head_object(&req.koid)?
            .ok_or(KError::NotFound(req.koid))?;
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &head, Action::Write)?;
        let cur_status = head.epistemic_status();
        let transitioning = cur_status != EpistemicStatus::Verified;
        if transitioning && !cur_status.can_transition(EpistemicStatus::Verified) {
            return Err(KError::InvalidEpistemic {
                from: cur_status,
                to: EpistemicStatus::Verified,
            });
        }
        let at = self.clock_now();
        let existing = head.confidence_context().unwrap_or(ConfidenceContext {
            score: 0.0,
            confirmations: 0,
            last_verified: None,
        });
        let new_conf = ConfidenceContext {
            score: req.confidence.unwrap_or(existing.score).max(existing.score),
            confirmations: existing.confirmations + 1,
            last_verified: Some(at),
        };
        let reason = req.note.clone();
        let mut version = head.version;
        let mut commit_ts = 0;
        if transitioning {
            let changed = self.transition_epistemic_locked(
                &mut pipe,
                &ctx,
                &req.koid,
                EpistemicStatus::Verified,
                Origin::Agent(ctx.subject.name.clone()),
                None,
                Some(head.version),
                reason.clone(),
            )?;
            version = changed.version;
            commit_ts = changed.commit_ts;
        }
        // Rebuild extensions from the POST-transition head so the update never
        // writes the pre-transition status back (carry-forward only fills
        // missing keys — a present-but-stale value would win).
        let new_head = self
            .head_object(&req.koid)?
            .ok_or(KError::NotFound(req.koid))?;
        let mut extensions = new_head.extensions.clone();
        extensions.insert(
            KnowledgeObject::EXT_CONFIDENCE.into(),
            confidence_to_value(&new_conf),
        );
        match extensions.get(KnowledgeObject::EXT_EVIDENCE).cloned() {
            Some(Value::List(mut existing)) => {
                if let Value::List(fresh) = evidence_value(&req.evidence) {
                    existing.extend(fresh);
                }
                extensions.insert(KnowledgeObject::EXT_EVIDENCE.into(), Value::List(existing));
            }
            _ => {
                extensions.insert(
                    KnowledgeObject::EXT_EVIDENCE.into(),
                    evidence_value(&req.evidence),
                );
            }
        }
        let rr = RememberRequest {
            context: ctx.clone(),
            koid: Some(req.koid),
            expected_version: Some(new_head.version),
            idempotency_key: None,
            metadata: new_head.metadata.clone(),
            properties: new_head.properties.clone(),
            semantic: None,
            relationships: new_head.relationships.clone(),
            security: None,
            extensions,
            origin: Origin::Agent(ctx.subject.name.clone()),
            note: reason,
            referential_policy: ReferentialPolicy::default(),
        };
        let rem = self.remember_locked(&mut pipe, &rr)?;
        Ok(VerificationResult {
            koid: req.koid,
            version: rem.version.max(version),
            commit_ts: rem.commit_ts.max(commit_ts),
            status: EpistemicStatus::Verified,
            confirmations: new_conf.confirmations,
            last_verified: new_conf.last_verified,
        })
    }

    // ---- contradict ------------------------------------------------------

    /// Register a competing assertion. Creates the counter-claim with a
    /// CONTRADICTS edge and a persisted Conflict KO (type `aikoql:conflict`,
    /// resolution `unresolved`) carrying per-assertion authority/evidence/
    /// timestamp/scope snapshots. The original claim's status is untouched —
    /// the conflict is symmetric until a decision.
    pub fn contradict(&self, req: ContradictionRequest) -> KResult<ContradictionResult> {
        require_evidence(&req.evidence)?;
        let ctx = req.context.clone();
        let mut pipe = self.pipe.lock().unwrap();
        let claim = self
            .head_object(&req.claim)?
            .ok_or(KError::NotFound(req.claim))?;
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &claim, Action::Read)?;
        if claim.properties == req.counter_props {
            return Err(KError::InvalidObject(
                "counter-claim must differ from the contradicted claim".into(),
            ));
        }
        if claim.invalidation().is_some() || claim.valid_to().is_some() {
            return Err(KError::InvalidObject(
                "cannot contradict non-current knowledge".into(),
            ));
        }
        let at = self.clock_now();
        // 1. Counter-claim: asserted, evidenced, CONTRADICTS edge to the claim.
        let mut counter_ext = ExtensionMap::new();
        counter_ext.insert(
            KnowledgeObject::EXT_EPISTEMIC_STATUS.into(),
            Value::Text("asserted".into()),
        );
        if let Some(a) = &req.authority {
            Authority::from_str(a).ok_or_else(|| {
                KError::InvalidObject("contradict requires a valid authority level".into())
            })?;
            counter_ext.insert("authority".into(), Value::Text(a.clone()));
        }
        counter_ext.insert(
            KnowledgeObject::EXT_EVIDENCE.into(),
            evidence_value(&req.evidence),
        );
        counter_ext.insert(
            KnowledgeObject::EXT_VALID_FROM.into(),
            Value::Int(at as i64),
        );
        let counter = self.remember_locked(
            &mut pipe,
            &RememberRequest {
                context: ctx.clone(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: req.counter_type,
                    tenant: ctx.tenant.clone(),
                    schema_version: 1,
                    tags: vec![],
                },
                properties: req.counter_props,
                semantic: None,
                relationships: vec![RelationshipRef {
                    rel_type: CONTRADICTS.into(),
                    target: req.claim,
                    direction: Direction::Outbound,
                }],
                security: None,
                extensions: counter_ext,
                origin: Origin::Agent(ctx.subject.name.clone()),
                note: req.note.clone(),
                referential_policy: ReferentialPolicy::default(),
            },
        )?;
        // 2. Conflict KO: the persisted record of the contradiction.
        let counter_ko = self
            .head_object(&counter.koid)?
            .ok_or(KError::NotFound(counter.koid))?;
        let mut props = PropertyMap::new();
        props.insert(
            "description".into(),
            Value::Text(format!(
                "Contradictory claims: {} vs {}",
                req.claim.to_hex(),
                counter.koid.to_hex()
            )),
        );
        props.insert("claim_a".into(), Value::Text(req.claim.to_hex()));
        props.insert("claim_b".into(), Value::Text(counter.koid.to_hex()));
        let mut assertions = PropertyMap::new();
        assertions.insert("a".into(), assertion_snapshot(&claim));
        assertions.insert("b".into(), assertion_snapshot(&counter_ko));
        let mut conflict_ext = ExtensionMap::new();
        conflict_ext.insert("resolution".into(), Value::Text("unresolved".into()));
        conflict_ext.insert("assertions".into(), Value::Map(assertions));
        let conflict = self.remember_locked(
            &mut pipe,
            &RememberRequest {
                context: ctx.clone(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: "aikoql:conflict".into(),
                    tenant: ctx.tenant.clone(),
                    schema_version: 1,
                    tags: vec![],
                },
                properties: props,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: conflict_ext,
                origin: Origin::System,
                note: req.note,
                referential_policy: ReferentialPolicy::default(),
            },
        )?;
        Ok(ContradictionResult {
            counter: counter.koid,
            conflict: conflict.koid,
        })
    }

    // ---- supersede -------------------------------------------------------

    /// Replace a claim with a new generation, preserving the old. Composes
    /// new-claim + supersession transition + dependent sweep under one lock.
    pub fn supersede(&self, req: SupersedeRequest) -> KResult<SupersedeResult> {
        require_evidence(&req.evidence)?;
        let ctx = req.context.clone();
        let mut pipe = self.pipe.lock().unwrap();
        let old = self
            .head_object(&req.old)?
            .ok_or(KError::NotFound(req.old))?;
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &old, Action::Write)?;
        if old.epistemic_status() == EpistemicStatus::Superseded {
            return Err(KError::InvalidObject(
                "already superseded — supersede the successor instead".into(),
            ));
        }
        let at = self.clock_now();
        let mut ext = ExtensionMap::new();
        ext.insert(
            KnowledgeObject::EXT_EPISTEMIC_STATUS.into(),
            Value::Text("asserted".into()),
        );
        ext.insert(
            KnowledgeObject::EXT_EVIDENCE.into(),
            evidence_value(&req.evidence),
        );
        ext.insert(
            KnowledgeObject::EXT_VALID_FROM.into(),
            Value::Int(at as i64),
        );
        let new = self.remember_locked(
            &mut pipe,
            &RememberRequest {
                context: ctx.clone(),
                koid: None,
                expected_version: Some(0),
                idempotency_key: None,
                metadata: Metadata {
                    type_name: req.type_name,
                    tenant: ctx.tenant.clone(),
                    schema_version: 1,
                    tags: vec![],
                },
                properties: req.properties,
                semantic: None,
                relationships: vec![],
                security: None,
                extensions: ext,
                origin: Origin::Agent(ctx.subject.name.clone()),
                note: req.note,
                referential_policy: ReferentialPolicy::default(),
            },
        )?;
        let reason = req
            .reason
            .clone()
            .unwrap_or_else(|| format!("superseded by {}", new.koid.to_hex()));
        self.transition_epistemic_locked(
            &mut pipe,
            &ctx,
            &req.old,
            EpistemicStatus::Superseded,
            Origin::Agent(ctx.subject.name.clone()),
            Some(new.koid),
            Some(old.version),
            Some(reason.clone()),
        )?;
        let roots = self.outbound_edges(&req.old, Some(DERIVED_FROM))?;
        let invalidated = self.invalidate_dependents_locked(
            &mut pipe,
            &ctx,
            roots,
            &format!("premise {} was superseded", req.old.to_hex()),
        )?;
        Ok(SupersedeResult {
            old: req.old,
            new: new.koid,
            invalidated_dependents: invalidated,
        })
    }

    // ---- merge -----------------------------------------------------------

    /// Merge ≥2 sources into one KO, folded per strategy, wired as a
    /// first-class derivation (operation "merge").
    pub fn merge(&self, req: MergeRequest) -> KResult<Remembered> {
        if req.sources.len() < 2 {
            return Err(KError::InvalidObject(
                "merge requires at least two sources".into(),
            ));
        }
        let ctx = req.context.clone();
        // Preflight: every source exists and is readable.
        for s in &req.sources {
            let ko = self.head_object(s)?.ok_or(KError::NotFound(*s))?;
            self.auth
                .read()
                .unwrap()
                .authorize(&ctx.subject, &ko, Action::Read)?;
        }
        let props = match req.strategy {
            MergeStrategy::Manual => req.properties.clone().ok_or_else(|| {
                KError::InvalidObject("Manual merge requires caller-provided properties".into())
            })?,
            MergeStrategy::NewestWins | MergeStrategy::AuthorityWins => {
                let mut heads = Vec::with_capacity(req.sources.len());
                for s in &req.sources {
                    heads.push((*s, self.head_object(s)?.ok_or(KError::NotFound(*s))?));
                }
                match req.strategy {
                    MergeStrategy::NewestWins => heads.sort_by_key(|(_, ko)| ko.commit_ts),
                    // weakest first so the strongest authority last-writes.
                    _ => heads.sort_by_key(|(_, ko)| ko_authority_rank(ko)),
                }
                let mut merged = PropertyMap::new();
                for (_, ko) in heads {
                    for (k, v) in &ko.properties {
                        merged.insert(k.clone(), v.clone());
                    }
                }
                merged
            }
        };
        let mut dr = DeriveRequest::new(ctx.clone(), req.type_name.clone());
        dr.properties = props;
        dr.sources = req.sources;
        dr.operation = "merge".into();
        dr.actor = ctx.subject.name.clone();
        dr.reason = req.reason;
        dr.evidence = req.evidence;
        self.derive(dr)
    }

    // ---- invalidate ------------------------------------------------------

    /// Withdraw support for a KO and everything derived from it. The target
    /// transitions to Contradicted where legal and is stamped + valid_to=now;
    /// dependents are stamped the same way (BFS over DERIVED_FROM) but keep
    /// their epistemic status — propagation is a dependency effect.
    pub fn invalidate(&self, req: InvalidationRequest) -> KResult<InvalidationResult> {
        require_evidence(&req.evidence)?;
        let ctx = req.context.clone();
        let mut pipe = self.pipe.lock().unwrap();
        let head = self
            .head_object(&req.koid)?
            .ok_or(KError::NotFound(req.koid))?;
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &head, Action::Write)?;
        if head.invalidation().is_some() {
            return Err(KError::InvalidObject("already invalidated".into()));
        }
        let at = self.clock_now();
        let reason = req
            .reason
            .clone()
            .unwrap_or_else(|| format!("invalidated by {}", ctx.subject.name));
        let mut invalidated = Vec::new();
        // Target: epistemic transition where legal (a Superseded KO is already
        // non-current and gets the stamp only).
        if head
            .epistemic_status()
            .can_transition(EpistemicStatus::Contradicted)
        {
            self.transition_epistemic_locked(
                &mut pipe,
                &ctx,
                &req.koid,
                EpistemicStatus::Contradicted,
                Origin::Agent(ctx.subject.name.clone()),
                None,
                Some(head.version),
                Some(reason.clone()),
            )?;
        }
        let new_head = self
            .head_object(&req.koid)?
            .ok_or(KError::NotFound(req.koid))?;
        let mut ko = new_head.clone();
        ko.set_invalidated(at, &ctx.subject.name, &reason);
        if ko.valid_to().is_none() {
            ko.set_valid_time(ko.valid_from(), Some(at));
        }
        let mut extensions = ko.extensions.clone();
        match extensions.get(KnowledgeObject::EXT_EVIDENCE).cloned() {
            Some(Value::List(mut existing)) => {
                if let Value::List(fresh) = evidence_value(&req.evidence) {
                    existing.extend(fresh);
                }
                extensions.insert(KnowledgeObject::EXT_EVIDENCE.into(), Value::List(existing));
            }
            _ => {
                extensions.insert(
                    KnowledgeObject::EXT_EVIDENCE.into(),
                    evidence_value(&req.evidence),
                );
            }
        }
        let rr = RememberRequest {
            context: ctx.clone(),
            koid: Some(req.koid),
            expected_version: Some(new_head.version),
            idempotency_key: None,
            metadata: ko.metadata.clone(),
            properties: ko.properties.clone(),
            semantic: None,
            relationships: ko.relationships.clone(),
            security: None,
            extensions,
            origin: Origin::System,
            note: req.note.clone().or_else(|| Some(reason.clone())),
            referential_policy: ReferentialPolicy::default(),
        };
        self.remember_locked(&mut pipe, &rr)?;
        invalidated.push(req.koid);
        let roots = self.outbound_edges(&req.koid, Some(DERIVED_FROM))?;
        invalidated.extend(self.invalidate_dependents_locked(
            &mut pipe,
            &ctx,
            roots,
            &format!("premise {} was invalidated", req.koid.to_hex()),
        )?);
        Ok(InvalidationResult { invalidated })
    }

    // ---- conflict resolution ---------------------------------------------

    /// Apply a resolution decision to a persisted Conflict KO and its claims.
    pub fn resolve_conflict(
        &self,
        req: ConflictResolutionRequest,
    ) -> KResult<ConflictResolutionOutcome> {
        if !req.decision.is_resolved() {
            return Err(KError::InvalidObject(
                "a resolution decision must be a resolved state".into(),
            ));
        }
        if req.rationale.trim().is_empty() {
            return Err(KError::InvalidObject(
                "a resolution requires a rationale".into(),
            ));
        }
        if req.decision == ConflictResolution::ResolvedReplaced {
            let replacement = req.replacement.ok_or_else(|| {
                KError::InvalidObject("ResolvedReplaced requires a replacement KO".into())
            })?;
            if self.head_object(&replacement)?.is_none() {
                return Err(KError::NotFound(replacement));
            }
        }
        let ctx = req.context.clone();
        let mut pipe = self.pipe.lock().unwrap();
        let conflict = self
            .head_object(&req.conflict)?
            .ok_or(KError::NotFound(req.conflict))?;
        if conflict.metadata.type_name != "aikoql:conflict" {
            return Err(KError::InvalidObject("not a conflict KO".into()));
        }
        self.auth
            .read()
            .unwrap()
            .authorize(&ctx.subject, &conflict, Action::Write)?;
        if conflict
            .extensions
            .get("resolution")
            .and_then(|v| match v {
                Value::Text(s) => ConflictResolution::from_str(s),
                _ => None,
            })
            .is_some_and(|c| c.is_resolved())
        {
            return Err(KError::InvalidObject("conflict already resolved".into()));
        }
        let (claim_a, claim_b) = conflict_claims(&conflict)?;
        let mut effects = Vec::new();
        match req.decision {
            ConflictResolution::ResolvedAPreferred => {
                self.transition_claim_if_legal(
                    &mut pipe,
                    &ctx,
                    &claim_b,
                    EpistemicStatus::Contradicted,
                    &req.rationale,
                    &mut effects,
                )?;
            }
            ConflictResolution::ResolvedBPreferred => {
                self.transition_claim_if_legal(
                    &mut pipe,
                    &ctx,
                    &claim_a,
                    EpistemicStatus::Contradicted,
                    &req.rationale,
                    &mut effects,
                )?;
            }
            ConflictResolution::ResolvedBothValid => {}
            ConflictResolution::ResolvedReplaced => {
                let replacement = req.replacement.expect("preflighted above");
                self.transition_claim_if_legal(
                    &mut pipe,
                    &ctx,
                    &claim_a,
                    EpistemicStatus::Superseded,
                    &req.rationale,
                    &mut effects,
                )?;
                self.transition_claim_if_legal(
                    &mut pipe,
                    &ctx,
                    &claim_b,
                    EpistemicStatus::Superseded,
                    &req.rationale,
                    &mut effects,
                )?;
                let _ = replacement;
            }
            ConflictResolution::Unresolved | ConflictResolution::UnderReview => unreachable!(),
        }
        // Record the decision on the Conflict KO itself (extensions — the
        // canonical storage for the resolution state machine).
        let new_conflict = self
            .head_object(&req.conflict)?
            .ok_or(KError::NotFound(req.conflict))?;
        let mut extensions = new_conflict.extensions.clone();
        extensions.insert(
            "resolution".into(),
            Value::Text(req.decision.as_str().into()),
        );
        extensions.insert(
            "resolution_rationale".into(),
            Value::Text(req.rationale.clone()),
        );
        if let Some(r) = req.replacement {
            extensions.insert("replacement".into(), Value::Text(r.to_hex()));
        }
        let rr = RememberRequest {
            context: ctx.clone(),
            koid: Some(req.conflict),
            expected_version: Some(new_conflict.version),
            idempotency_key: None,
            metadata: new_conflict.metadata.clone(),
            properties: new_conflict.properties.clone(),
            semantic: None,
            relationships: new_conflict.relationships.clone(),
            security: None,
            extensions,
            origin: Origin::System,
            note: Some(req.rationale.clone()),
            referential_policy: ReferentialPolicy::default(),
        };
        self.remember_locked(&mut pipe, &rr)?;
        Ok(ConflictResolutionOutcome {
            conflict: req.conflict,
            decision: req.decision,
            effects,
        })
    }

    /// Resolve by the recorded authority of each assertion. Higher authority
    /// wins. A tie is an error — the kernel never silently picks a side.
    pub fn resolve_conflict_by_authority(
        &self,
        ctx: impl Into<KnowledgeContext>,
        conflict: KOID,
        rationale: String,
    ) -> KResult<ConflictResolutionOutcome> {
        let ctx = ctx.into();
        let ko = self.get(ctx.clone(), &conflict)?;
        if ko.metadata.type_name != "aikoql:conflict" {
            return Err(KError::InvalidObject("not a conflict KO".into()));
        }
        let rank_a = snapshot_authority_rank(snapshot_authority(&ko, "a"));
        let rank_b = snapshot_authority_rank(snapshot_authority(&ko, "b"));
        let decision = match rank_a.cmp(&rank_b) {
            std::cmp::Ordering::Greater => ConflictResolution::ResolvedAPreferred,
            std::cmp::Ordering::Less => ConflictResolution::ResolvedBPreferred,
            std::cmp::Ordering::Equal => {
                return Err(KError::InvalidObject(
                    "authority tie — an explicit decision is required; the kernel will not silently pick a side"
                        .into(),
                ))
            }
        };
        self.resolve_conflict(ConflictResolutionRequest {
            context: ctx,
            conflict,
            decision,
            rationale,
            replacement: None,
        })
    }

    // ---- internals -------------------------------------------------------

    /// Transition a claim if the state machine allows; already-dead claims
    /// (Contradicted/Superseded) are skipped, anything illegal is an error.
    fn transition_claim_if_legal(
        &self,
        pipe: &mut Pipeline,
        ctx: &KnowledgeContext,
        koid: &KOID,
        to: EpistemicStatus,
        reason: &str,
        effects: &mut Vec<(KOID, EpistemicStatus)>,
    ) -> KResult<()> {
        let head = self.head_object(koid)?.ok_or(KError::NotFound(*koid))?;
        let from = head.epistemic_status();
        if from == to
            || (to == EpistemicStatus::Contradicted && from == EpistemicStatus::Superseded)
            || (to == EpistemicStatus::Superseded && from == EpistemicStatus::Superseded)
        {
            return Ok(());
        }
        if !from.can_transition(to) {
            return Err(KError::InvalidEpistemic { from, to });
        }
        self.transition_epistemic_locked(
            pipe,
            ctx,
            koid,
            to,
            Origin::System,
            None,
            Some(head.version),
            Some(reason.into()),
        )?;
        effects.push((*koid, to));
        Ok(())
    }

    /// BFS over DERIVED_FROM dependents: stamp invalidation + valid_to=now on
    /// each. Kernel-enforced dependency effect — no per-dependent ACL (the
    /// sweep only stamps staleness; properties are untouched). Caller holds
    /// the pipe lock. Cycle-safe via the visited set.
    fn invalidate_dependents_locked(
        &self,
        pipe: &mut Pipeline,
        ctx: &KnowledgeContext,
        roots: Vec<(String, KOID)>,
        reason: &str,
    ) -> KResult<Vec<KOID>> {
        let at = self.clock_now();
        let mut out = Vec::new();
        let mut visited: HashSet<KOID> = HashSet::new();
        let mut queue: VecDeque<KOID> = roots.into_iter().map(|(_, k)| k).collect();
        while let Some(k) = queue.pop_front() {
            if !visited.insert(k) {
                continue;
            }
            let head = match self.head_object(&k)? {
                Some(h) => h,
                // dangling edge from a forgotten KO — nothing to sweep.
                None => continue,
            };
            if head.invalidation().is_some() {
                continue; // already stamped (idempotent sweep)
            }
            for (_, dep) in self.outbound_edges(&k, Some(DERIVED_FROM))? {
                queue.push_back(dep);
            }
            let mut ko = head.clone();
            ko.set_invalidated(at, &ctx.subject.name, reason);
            if ko.valid_to().is_none() {
                ko.set_valid_time(ko.valid_from(), Some(at));
            }
            let rr = RememberRequest {
                context: ctx.clone(),
                koid: Some(k),
                expected_version: Some(head.version),
                idempotency_key: None,
                metadata: ko.metadata.clone(),
                properties: ko.properties.clone(),
                semantic: None,
                relationships: ko.relationships.clone(),
                security: None,
                extensions: ko.extensions.clone(),
                origin: Origin::System,
                note: Some(reason.into()),
                referential_policy: ReferentialPolicy::default(),
            };
            self.remember_locked(pipe, &rr)?;
            out.push(k);
        }
        Ok(out)
    }
}
