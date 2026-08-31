//! D7: Knowledge Commit — reconciliation and commit planning.
//!
//! Consumes the outputs of D4 (KnowledgeIr), D5 (OntologyProposal), and D6
//! (ResolutionResult) to produce a `KnowledgeCommitPlan` — a dry-run blueprint
//! of what KOs to create/update/skip. The plan is handed to the pipeline
//! orchestrator for execution against the kernel.
//!
//! # Architecture
//! - `Conflict` — a detected disagreement between document facts and existing KO properties
//! - `CommitAction` — CreateKO, UpdateKO, Skip, NeedsReview
//! - `KnowledgeCommitPlan` — the reconciled blueprint
//! - `KnowledgeReconciler` trait — pluggable reconciliation strategy
//! - `MockKnowledgeReconciler` — rule-based mock for testing

use crate::ir::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr};
use crate::ontology::OntologyProposal;
use crate::resolution::{KnowledgeBaseEntry, ResolutionResult};

// ---------------------------------------------------------------------------
// Conflict detection
// ---------------------------------------------------------------------------

/// Severity of a conflict between document evidence and existing knowledge.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConflictSeverity {
    /// Minor: casing differences, trailing whitespace, equivalent values.
    Info,
    /// Notable: different values for the same property from different sources.
    Warning,
    /// Cannot auto-resolve: contradictory core facts (e.g. two sources disagree on status).
    Blocking,
}

/// A single conflict between document evidence and an existing KO property.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Conflict {
    /// The document entity name involved.
    pub entity_name: String,
    /// KOID of the existing KO (None if entity unresolved).
    pub koid: Option<String>,
    /// Property or fact name with the conflict.
    pub property_name: String,
    /// Value extracted from the document.
    pub document_value: String,
    /// Current value in the knowledge base (None if the property doesn't exist yet).
    pub existing_value: Option<String>,
    /// How severe this conflict is.
    pub severity: ConflictSeverity,
    /// Evidence from the document supporting the document value.
    pub evidence: Vec<Evidence>,
}

// ---------------------------------------------------------------------------
// Commit actions
// ---------------------------------------------------------------------------

/// A single planned action for the kernel.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CommitAction {
    /// Create a new Knowledge Object from document entities.
    CreateKO {
        /// The document entity to create.
        entity_name: String,
        /// Ontology class to assign (from D5 discovery).
        class_name: Option<String>,
        /// Properties derived from facts about this entity.
        properties: Vec<(String, String)>,
        /// Evidence backing this creation.
        evidence: Vec<Evidence>,
    },
    /// Update an existing KO with new properties from the document.
    UpdateKO {
        /// KOID of the existing KO to update.
        koid: String,
        /// Entity name from the document.
        entity_name: String,
        /// Properties to add or update.
        properties: Vec<(String, String)>,
        /// Conflicts detected between document and existing values.
        conflicts: Vec<Conflict>,
        /// Evidence backing this update.
        evidence: Vec<Evidence>,
    },
    /// No action needed (entity already matches existing KO with no changes).
    Skip {
        entity_name: String,
        koid: String,
        reason: String,
    },
    /// Needs human review — conflicts or ambiguity prevent automatic resolution.
    NeedsReview {
        entity_name: String,
        koid: Option<String>,
        /// Conflicts that need human judgment.
        conflicts: Vec<Conflict>,
        /// Reason this needs review (e.g. "ambiguous match", "conflicting properties").
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Commit plan
// ---------------------------------------------------------------------------

/// Statistics for a commit plan.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommitStats {
    pub total_actions: usize,
    pub creates: usize,
    pub updates: usize,
    pub skips: usize,
    pub needs_review: usize,
    pub total_conflicts: usize,
}

/// The reconciled blueprint: what to create, update, skip, or review.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeCommitPlan {
    /// Ordered list of actions to execute.
    pub actions: Vec<CommitAction>,
    /// Summary statistics.
    pub stats: CommitStats,
    /// Source document identifier.
    pub document_id: Option<String>,
}

impl KnowledgeCommitPlan {
    /// Convenience: iterate only the CreateKO actions.
    pub fn creates(&self) -> impl Iterator<Item = &CommitAction> {
        self.actions
            .iter()
            .filter(|a| matches!(a, CommitAction::CreateKO { .. }))
    }

    /// Convenience: iterate only the UpdateKO actions.
    pub fn updates(&self) -> impl Iterator<Item = &CommitAction> {
        self.actions
            .iter()
            .filter(|a| matches!(a, CommitAction::UpdateKO { .. }))
    }

    /// Convenience: count actions needing human review.
    pub fn needs_review_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, CommitAction::NeedsReview { .. }))
            .count()
    }
}

// ---------------------------------------------------------------------------
// KnowledgeReconciler trait
// ---------------------------------------------------------------------------

/// Pluggable reconciliation: IR + ontology + resolution → commit plan.
///
/// Implementations range from simple rule-based (mock) to ML-driven conflict
/// resolution using embedding similarity and LLM judgment.
pub trait KnowledgeReconciler: Send + Sync {
    /// Human-readable name (e.g. "mock", "llm-juror").
    fn name(&self) -> &str;

    /// Produce a commit plan from all upstream pipeline outputs.
    fn reconcile(
        &self,
        ir: &KnowledgeIr,
        ontology: &OntologyProposal,
        resolution: &ResolutionResult,
        existing_kos: &[KnowledgeBaseEntry],
    ) -> KnowledgeCommitPlan;
}

// ---------------------------------------------------------------------------
// Mock reconciler — rule-based reconciliation
// ---------------------------------------------------------------------------

/// Rule-based reconciler for testing and simple document ingestion.
///
/// Strategy:
/// - **Unmatched entities**: CreateKO with ontology-derived class + properties from facts.
/// - **Matched entities**: Compare document facts against existing KO properties.
///   If new properties → UpdateKO. If conflicting values → NeedsReview.
///   If everything matches → Skip.
/// - **Ambiguous entities**: NeedsReview with all candidate info.
/// - **Relations**: CreateKO if both endpoints exist or will be created.
/// - **Temporal assertions**: Attached as date properties to relevant entities.
pub struct MockKnowledgeReconciler {
    /// Treat property values differing only by case/whitespace as the same.
    pub normalize_values: bool,
}

impl Default for MockKnowledgeReconciler {
    fn default() -> Self {
        Self::new()
    }
}

impl MockKnowledgeReconciler {
    pub fn new() -> Self {
        MockKnowledgeReconciler {
            normalize_values: true,
        }
    }
}

impl KnowledgeReconciler for MockKnowledgeReconciler {
    fn name(&self) -> &str {
        "mock"
    }

    fn reconcile(
        &self,
        ir: &KnowledgeIr,
        ontology: &OntologyProposal,
        resolution: &ResolutionResult,
        existing_kos: &[KnowledgeBaseEntry],
    ) -> KnowledgeCommitPlan {
        let mut actions: Vec<CommitAction> = Vec::new();
        let mut total_conflicts = 0usize;

        // Build lookups.
        let ekos: std::collections::HashMap<&str, &KnowledgeBaseEntry> =
            existing_kos.iter().map(|e| (e.koid.as_str(), e)).collect();

        // Build fact lookup: entity name → fact statements about it.
        let facts_for_entity = build_fact_map(&ir.facts);

        // Build property mapping: entity → (prop_name, prop_value) from ontology proposals.
        let props_for_entity = build_property_map(ontology, ir);

        // --- Process matched entities ---
        for m in &resolution.matched {
            let entity = ir
                .entities
                .iter()
                .find(|e| e.name == m.entity_name)
                .cloned();

            let koid = match &m.matched_koid {
                Some(k) => k,
                None => {
                    // Matched but no KOID? Unusual — treat as create.
                    actions.push(build_create(
                        &m.entity_name,
                        &m.entity_type,
                        &facts_for_entity,
                        &props_for_entity,
                        entity.as_ref(),
                    ));
                    continue;
                }
            };

            let existing = ekos.get(koid.as_str());
            let facts: &[&FactCandidate] = facts_for_entity
                .get(m.entity_name.as_str())
                .map(|v| v.as_slice())
                .unwrap_or(&[]);

            if let Some(eko) = existing {
                // Compare facts vs existing properties.
                let conflicts = detect_conflicts(
                    &m.entity_name,
                    Some(koid),
                    facts,
                    &eko.properties,
                    self.normalize_values,
                    entity.as_ref(),
                );

                // Collect new properties that aren't in the existing KO.
                let new_props = new_properties(facts, &eko.properties);

                if !conflicts.is_empty() {
                    let conflict_count = conflicts.len();
                    total_conflicts += conflict_count;
                    actions.push(CommitAction::NeedsReview {
                        entity_name: m.entity_name.clone(),
                        koid: Some(koid.clone()),
                        conflicts,
                        reason: format!(
                            "{} conflicting propert{} found",
                            conflict_count,
                            if conflict_count == 1 { "y" } else { "ies" }
                        ),
                    });
                } else if !new_props.is_empty() {
                    actions.push(CommitAction::UpdateKO {
                        koid: koid.clone(),
                        entity_name: m.entity_name.clone(),
                        properties: new_props,
                        conflicts: vec![],
                        evidence: entity
                            .as_ref()
                            .map(|e| vec![e.evidence.clone()])
                            // justified: no source entity → empty evidence set
                            .unwrap_or_default(),
                    });
                } else {
                    actions.push(CommitAction::Skip {
                        entity_name: m.entity_name.clone(),
                        koid: koid.clone(),
                        reason: "entity matches existing KO with no new properties".into(),
                    });
                }
            } else {
                // Matched KOID not in the provided KB snapshot — treat as create.
                actions.push(build_create(
                    &m.entity_name,
                    &m.entity_type,
                    &facts_for_entity,
                    &props_for_entity,
                    entity.as_ref(),
                ));
            }
        }

        // --- Process ambiguous entities ---
        for a in &resolution.ambiguous {
            let conflicts: Vec<Conflict> = a
                .candidates
                .iter()
                .map(|c| Conflict {
                    entity_name: a.entity_name.clone(),
                    koid: Some(c.koid.clone()),
                    property_name: "identity".into(),
                    document_value: a.entity_name.clone(),
                    existing_value: Some(c.name.clone()),
                    severity: ConflictSeverity::Warning,
                    evidence: a.evidence.clone(),
                })
                .collect();
            total_conflicts += conflicts.len();
            actions.push(CommitAction::NeedsReview {
                entity_name: a.entity_name.clone(),
                koid: None,
                conflicts,
                reason: format!(
                    "ambiguous: {} candidate{} with scores {:?}",
                    a.candidates.len(),
                    if a.candidates.len() == 1 { "" } else { "s" },
                    a.candidates
                        .iter()
                        .map(|c| format!("{:.2}", c.score))
                        .collect::<Vec<_>>()
                ),
            });
        }

        // --- Process unmatched entities ---
        for u in &resolution.unmatched {
            let entity = ir
                .entities
                .iter()
                .find(|e| e.name == u.entity_name)
                .cloned();
            actions.push(build_create(
                &u.entity_name,
                &u.entity_type,
                &facts_for_entity,
                &props_for_entity,
                entity.as_ref(),
            ));
        }

        // --- Process relations ---
        // Collect names of entities that will exist after commit (existing + planned creates).
        let mut known_entity_names: std::collections::HashSet<String> =
            existing_kos.iter().map(|e| e.name.clone()).collect();
        for action in &actions {
            if let CommitAction::CreateKO {
                ref entity_name, ..
            } = action
            {
                known_entity_names.insert(entity_name.clone());
            }
        }

        for rel in &ir.relations {
            let subj_exists = known_entity_names.contains(&rel.subject);
            let obj_exists = known_entity_names.contains(&rel.object);

            if subj_exists && obj_exists {
                // Both endpoints exist → create a property on the subject referencing the object.
                // ponytail: use a simple property rather than a full Relationship KO;
                // the pipeline orchestrator can upgrade to relate() when needed.
                actions.push(CommitAction::CreateKO {
                    entity_name: format!("rel:{}->{}", rel.subject, rel.object),
                    class_name: Some("Relationship".into()),
                    properties: vec![
                        ("subject".into(), rel.subject.clone()),
                        ("predicate".into(), rel.predicate.clone()),
                        ("object".into(), rel.object.clone()),
                    ],
                    evidence: vec![rel.evidence.clone()],
                });
            }
            // If an endpoint doesn't exist, skip the relation silently.
        }

        // --- Compute stats ---
        let stats = CommitStats {
            total_actions: actions.len(),
            creates: actions
                .iter()
                .filter(|a| matches!(a, CommitAction::CreateKO { .. }))
                .count(),
            updates: actions
                .iter()
                .filter(|a| matches!(a, CommitAction::UpdateKO { .. }))
                .count(),
            skips: actions
                .iter()
                .filter(|a| matches!(a, CommitAction::Skip { .. }))
                .count(),
            needs_review: actions
                .iter()
                .filter(|a| matches!(a, CommitAction::NeedsReview { .. }))
                .count(),
            total_conflicts,
        };

        KnowledgeCommitPlan {
            actions,
            stats,
            document_id: ir.document_id.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Mock helpers
// ---------------------------------------------------------------------------

/// Map entity names to fact candidates that mention them.
fn build_fact_map<'a>(
    facts: &'a [FactCandidate],
) -> std::collections::HashMap<String, Vec<&'a FactCandidate>> {
    let mut map: std::collections::HashMap<String, Vec<&'a FactCandidate>> =
        std::collections::HashMap::new();
    for f in facts {
        for entity_name in &f.entities {
            map.entry(entity_name.clone()).or_default().push(f);
        }
    }
    map
}

/// Extract (property_name, property_value) pairs from ontology proposals for each entity.
fn build_property_map(
    ontology: &OntologyProposal,
    ir: &KnowledgeIr,
) -> std::collections::HashMap<String, Vec<(String, String)>> {
    let mut map: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();

    // For each temporal assertion, attach to entities on the same page.
    for t in &ir.temporal {
        let page = t.evidence.page;
        for entity in &ir.entities {
            if entity.evidence.page == page {
                let prop_name = if t.start_time.is_some() {
                    "date".into()
                } else {
                    "temporal".into()
                };
                map.entry(entity.name.clone())
                    .or_default()
                    .push((prop_name, t.text.clone()));
            }
        }
    }

    // Property proposals from ontology discovery.
    for prop in &ontology.properties {
        let class = &prop.class_name;
        // Find entities whose type_hint matches this class.
        for entity in &ir.entities {
            if entity.type_hint.as_deref() == Some(class.as_str()) {
                map.entry(entity.name.clone())
                    .or_default()
                    .push((prop.name.clone(), prop.value_type.clone()));
            }
        }
    }

    map
}

/// Detect conflicts between document facts and existing KO properties.
fn detect_conflicts(
    entity_name: &str,
    koid: Option<&str>,
    facts: &[&FactCandidate],
    existing_properties: &[(String, String)],
    normalize: bool,
    entity: Option<&EntityCandidate>,
) -> Vec<Conflict> {
    let mut conflicts: Vec<Conflict> = Vec::new();

    // Build a map of existing property name → value.
    let existing_map: std::collections::HashMap<&str, &str> = existing_properties
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    for fact in facts {
        // The statement contains the fact. Extract the first "property-like" token from it.
        // ponytail: simple heuristic — treat the heading/fact statement as a property
        // value keyed by a simplified statement prefix.
        let prop_name = simplify_statement_key(&fact.statement);
        let doc_value = &fact.statement;

        if let Some(&existing_val) = existing_map.get(prop_name.as_str()) {
            if !values_equivalent(doc_value, existing_val, normalize) {
                conflicts.push(Conflict {
                    entity_name: entity_name.into(),
                    koid: koid.map(|s| s.to_string()),
                    property_name: prop_name,
                    document_value: doc_value.clone(),
                    existing_value: Some(existing_val.to_string()),
                    severity: ConflictSeverity::Warning,
                    // justified: no source entity → empty evidence set
                    evidence: entity.map(|e| vec![e.evidence.clone()]).unwrap_or_default(),
                });
            }
        }
    }

    // Also check entity-level type mismatch.
    if let Some(ent) = entity {
        if let Some(ref type_hint) = ent.type_hint {
            if let Some(&existing_type) = existing_map.get("type") {
                if !values_equivalent(type_hint, existing_type, normalize) {
                    conflicts.push(Conflict {
                        entity_name: entity_name.into(),
                        koid: koid.map(|s| s.to_string()),
                        property_name: "type".into(),
                        document_value: type_hint.clone(),
                        existing_value: Some(existing_type.to_string()),
                        severity: ConflictSeverity::Blocking,
                        evidence: vec![ent.evidence.clone()],
                    });
                }
            }
        }
    }

    conflicts
}

/// Collect new properties from facts that don't already exist on the KO.
fn new_properties(
    facts: &[&FactCandidate],
    existing_properties: &[(String, String)],
) -> Vec<(String, String)> {
    let existing_keys: std::collections::HashSet<&str> = existing_properties
        .iter()
        .map(|(k, _)| k.as_str())
        .collect();

    let mut props: Vec<(String, String)> = Vec::new();
    for fact in facts {
        let key = simplify_statement_key(&fact.statement);
        if !existing_keys.contains(key.as_str()) {
            props.push((key, fact.statement.clone()));
        }
    }
    props
}

/// Reduce a fact statement to a short property key.
/// ponytail: first few words, lowercased, snake_cased.
fn simplify_statement_key(statement: &str) -> String {
    let words: Vec<&str> = statement.split_whitespace().take(3).collect();
    words.join("_").to_lowercase()
}

/// Compare two property values, optionally normalizing case/whitespace.
fn values_equivalent(a: &str, b: &str, normalize: bool) -> bool {
    if normalize {
        a.trim().to_lowercase() == b.trim().to_lowercase()
    } else {
        a == b
    }
}

/// Build a CreateKO action from entity info + derived properties.
fn build_create(
    entity_name: &str,
    entity_type: &Option<String>,
    facts_for_entity: &std::collections::HashMap<String, Vec<&FactCandidate>>,
    props_for_entity: &std::collections::HashMap<String, Vec<(String, String)>>,
    entity: Option<&EntityCandidate>,
) -> CommitAction {
    let mut properties: Vec<(String, String)> = Vec::new();

    // Add fact statements as properties.
    if let Some(facts) = facts_for_entity.get(entity_name) {
        for f in facts {
            let key = simplify_statement_key(&f.statement);
            properties.push((key, f.statement.clone()));
        }
    }

    // Add ontology-derived properties.
    if let Some(extra) = props_for_entity.get(entity_name) {
        for (k, v) in extra {
            // Avoid duplicates.
            if !properties.iter().any(|(pk, _)| pk == k) {
                properties.push((k.clone(), v.clone()));
            }
        }
    }

    // Add type if available.
    if let Some(ref t) = entity_type {
        if !properties.iter().any(|(k, _)| k == "type") {
            properties.push(("type".into(), t.clone()));
        }
    }

    let class_name = entity_type
        .clone()
        .or_else(|| entity.and_then(|e| e.type_hint.clone()));

    CommitAction::CreateKO {
        entity_name: entity_name.into(),
        class_name,
        properties,
        // justified: no source entity → empty evidence set
        evidence: entity.map(|e| vec![e.evidence.clone()]).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Convenience: full reconciliation pipeline
// ---------------------------------------------------------------------------

/// Run the full reconciliation pipeline: IR + ontology + resolution → commit plan.
pub fn reconcile_and_plan(
    ir: &KnowledgeIr,
    ontology: &OntologyProposal,
    resolution: &ResolutionResult,
    existing_kos: &[KnowledgeBaseEntry],
    reconciler: &dyn KnowledgeReconciler,
) -> KnowledgeCommitPlan {
    reconciler.reconcile(ir, ontology, resolution, existing_kos)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate};
    use crate::ontology::OntologyProposal;
    use crate::resolution::{MatchScore, ResolutionCandidate, ResolutionResult, ResolutionStats};

    // ── Helpers ──

    fn evidence() -> Evidence {
        Evidence {
            document_id: Some("doc-001".into()),
            page: Some(1),
            source: None,
            extractor: "mock".into(),
            model: Some("mock-v1".into()),
            confidence: 0.85,
        }
    }

    fn entity(name: &str, type_hint: Option<&str>) -> EntityCandidate {
        EntityCandidate {
            name: name.into(),
            type_hint: type_hint.map(|s| s.into()),
            mentions: vec![name.into()],
            confidence: 0.85,
            evidence: evidence(),
        }
    }

    fn fact(statement: &str, entities: Vec<&str>) -> FactCandidate {
        FactCandidate {
            snippet: None,
            statement: statement.into(),
            entities: entities.into_iter().map(|s| s.into()).collect(),
            confidence: 0.85,
            evidence: evidence(),
        }
    }

    fn kb_entry(
        koid: &str,
        name: &str,
        type_name: &str,
        properties: Vec<(&str, &str)>,
    ) -> KnowledgeBaseEntry {
        KnowledgeBaseEntry {
            koid: koid.into(),
            name: name.into(),
            type_name: type_name.into(),
            aliases: vec![],
            properties: properties
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    fn mk_resolved(
        entity_name: &str,
        type_hint: Option<&str>,
        koid: &str,
        score: f32,
    ) -> ResolutionCandidate {
        ResolutionCandidate {
            entity_name: entity_name.into(),
            entity_type: type_hint.map(|s| s.into()),
            matched_koid: Some(koid.into()),
            candidates: vec![MatchScore {
                koid: koid.into(),
                name: entity_name.into(),
                score,
                method: "exact_name".into(),
            }],
            needs_creation: false,
            confidence: score,
            evidence: vec![evidence()],
        }
    }

    fn mk_ambiguous(entity_name: &str) -> ResolutionCandidate {
        ResolutionCandidate {
            entity_name: entity_name.into(),
            entity_type: None,
            matched_koid: None,
            candidates: vec![
                MatchScore {
                    koid: "ko-a".into(),
                    name: format!("{} A", entity_name),
                    score: 0.75,
                    method: "fuzzy".into(),
                },
                MatchScore {
                    koid: "ko-b".into(),
                    name: format!("{} B", entity_name),
                    score: 0.72,
                    method: "fuzzy".into(),
                },
            ],
            needs_creation: false,
            confidence: 0.5,
            evidence: vec![evidence()],
        }
    }

    fn mk_unmatched(entity_name: &str, type_hint: Option<&str>) -> ResolutionCandidate {
        ResolutionCandidate {
            entity_name: entity_name.into(),
            entity_type: type_hint.map(|s| s.into()),
            matched_koid: None,
            candidates: vec![],
            needs_creation: true,
            confidence: 0.0,
            evidence: vec![evidence()],
        }
    }

    // ── Tests ──

    #[test]
    fn mock_reconciler_has_name() {
        let r = MockKnowledgeReconciler::new();
        assert_eq!(r.name(), "mock");
    }

    // ── CreateKO for unmatched ──

    #[test]
    fn unmatched_entity_creates_ko() {
        let ir = KnowledgeIr {
            entities: vec![entity("Acme Corporation", Some("Organization"))],
            facts: vec![fact(
                "Acme Corporation Reports Revenue",
                vec!["Acme Corporation"],
            )],
            document_id: Some("doc-001".into()),
            extractor: "mock".into(),
            ..Default::default()
        };

        let ontology = OntologyProposal::default();
        let resolution = ResolutionResult {
            matched: vec![],
            ambiguous: vec![],
            unmatched: vec![mk_unmatched("Acme Corporation", Some("Organization"))],
            stats: ResolutionStats::default(),
        };
        let kb = vec![];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &ontology, &resolution, &kb);

        assert_eq!(plan.stats.creates, 1);
        assert_eq!(plan.stats.needs_review, 0);

        let create = &plan.actions[0];
        match create {
            CommitAction::CreateKO {
                entity_name,
                class_name,
                properties,
                ..
            } => {
                assert_eq!(entity_name, "Acme Corporation");
                assert_eq!(class_name.as_deref(), Some("Organization"));
                assert!(!properties.is_empty());
            }
            _ => panic!("expected CreateKO"),
        }
    }

    #[test]
    fn unmatched_without_type_infers_from_entity() {
        let ir = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "Globex Industries".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Globex Industries".into()],
                confidence: 0.85,
                evidence: evidence(),
            }],
            ..Default::default()
        };

        let ontology = OntologyProposal::default();
        let resolution = ResolutionResult {
            matched: vec![],
            ambiguous: vec![],
            unmatched: vec![mk_unmatched("Globex Industries", None)],
            stats: ResolutionStats::default(),
        };
        let kb = vec![];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &ontology, &resolution, &kb);

        match &plan.actions[0] {
            CommitAction::CreateKO { class_name, .. } => {
                assert_eq!(class_name.as_deref(), Some("Organization"));
            }
            _ => panic!("expected CreateKO"),
        }
    }

    // ── Skip for matched with no new properties ──

    #[test]
    fn matched_no_new_properties_skips() {
        let ir = KnowledgeIr {
            entities: vec![entity("Acme Corporation", Some("Organization"))],
            ..Default::default()
        };

        let ontology = OntologyProposal::default();
        let resolution = ResolutionResult {
            matched: vec![mk_resolved(
                "Acme Corporation",
                Some("Organization"),
                "ko-1",
                1.0,
            )],
            ambiguous: vec![],
            unmatched: vec![],
            stats: ResolutionStats::default(),
        };
        let kb = vec![kb_entry("ko-1", "Acme Corporation", "Organization", vec![])];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &ontology, &resolution, &kb);

        assert_eq!(plan.stats.skips, 1);
        match &plan.actions[0] {
            CommitAction::Skip {
                entity_name, koid, ..
            } => {
                assert_eq!(entity_name, "Acme Corporation");
                assert_eq!(koid, "ko-1");
            }
            _ => panic!("expected Skip"),
        }
    }

    // ── UpdateKO for matched with new properties ──

    #[test]
    fn matched_with_new_facts_updates() {
        let ir = KnowledgeIr {
            entities: vec![entity("Acme Corporation", Some("Organization"))],
            facts: vec![fact(
                "Acme Corporation Founded 2019",
                vec!["Acme Corporation"],
            )],
            ..Default::default()
        };

        let ontology = OntologyProposal::default();
        let resolution = ResolutionResult {
            matched: vec![mk_resolved(
                "Acme Corporation",
                Some("Organization"),
                "ko-1",
                1.0,
            )],
            ambiguous: vec![],
            unmatched: vec![],
            stats: ResolutionStats::default(),
        };
        let kb = vec![kb_entry("ko-1", "Acme Corporation", "Organization", vec![])];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &ontology, &resolution, &kb);

        assert_eq!(plan.stats.updates, 1);
        match &plan.actions[0] {
            CommitAction::UpdateKO {
                koid,
                entity_name,
                properties,
                conflicts,
                ..
            } => {
                assert_eq!(koid, "ko-1");
                assert_eq!(entity_name, "Acme Corporation");
                assert!(!properties.is_empty());
                assert!(conflicts.is_empty());
            }
            _ => panic!("expected UpdateKO"),
        }
    }

    // ── NeedsReview for conflicts ──

    #[test]
    fn conflicting_property_triggers_review() {
        let ir = KnowledgeIr {
            entities: vec![entity("Acme Corporation", Some("Organization"))],
            facts: vec![fact(
                "Acme Corporation Status Active",
                vec!["Acme Corporation"],
            )],
            ..Default::default()
        };

        let ontology = OntologyProposal::default();
        let resolution = ResolutionResult {
            matched: vec![mk_resolved(
                "Acme Corporation",
                Some("Organization"),
                "ko-1",
                1.0,
            )],
            ambiguous: vec![],
            unmatched: vec![],
            stats: ResolutionStats::default(),
        };

        // Existing KO has the same key ("acme_corporation_status") but different value.
        // simplify_statement_key("Acme Corporation Status Active") = "acme_corporation_status"
        let kb = vec![kb_entry(
            "ko-1",
            "Acme Corporation",
            "Organization",
            vec![(
                "acme_corporation_status",
                "Acme Corporation Status Terminated",
            )],
        )];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &ontology, &resolution, &kb);

        assert_eq!(plan.stats.needs_review, 1);
        assert_eq!(plan.stats.total_conflicts, 1);
        match &plan.actions[0] {
            CommitAction::NeedsReview {
                entity_name,
                koid,
                conflicts,
                ..
            } => {
                assert_eq!(entity_name, "Acme Corporation");
                assert_eq!(koid.as_deref(), Some("ko-1"));
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].severity, ConflictSeverity::Warning);
            }
            _ => panic!("expected NeedsReview"),
        }
    }

    // ── Type mismatch triggers Blocking conflict ──

    #[test]
    fn type_mismatch_is_blocking() {
        let ir = KnowledgeIr {
            entities: vec![entity("John Smith", Some("Person"))],
            ..Default::default()
        };

        let ontology = OntologyProposal::default();
        let resolution = ResolutionResult {
            matched: vec![mk_resolved("John Smith", Some("Person"), "ko-2", 0.95)],
            ambiguous: vec![],
            unmatched: vec![],
            stats: ResolutionStats::default(),
        };
        let kb = vec![kb_entry(
            "ko-2",
            "John Smith",
            "Organization", // Existing KO says Organization, document says Person
            vec![("type", "Organization")],
        )];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &ontology, &resolution, &kb);

        assert_eq!(plan.stats.needs_review, 1);
        match &plan.actions[0] {
            CommitAction::NeedsReview { conflicts, .. } => {
                let type_conflict = conflicts
                    .iter()
                    .find(|c| c.property_name == "type")
                    .unwrap();
                assert_eq!(type_conflict.severity, ConflictSeverity::Blocking);
            }
            _ => panic!("expected NeedsReview"),
        }
    }

    // ── Ambiguous → NeedsReview ──

    #[test]
    fn ambiguous_entity_needs_review() {
        let ir = KnowledgeIr {
            entities: vec![entity("Generic Corp", None)],
            ..Default::default()
        };

        let ontology = OntologyProposal::default();
        let resolution = ResolutionResult {
            matched: vec![],
            ambiguous: vec![mk_ambiguous("Generic Corp")],
            unmatched: vec![],
            stats: ResolutionStats::default(),
        };
        let kb = vec![];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &ontology, &resolution, &kb);

        assert_eq!(plan.stats.needs_review, 1);
        match &plan.actions[0] {
            CommitAction::NeedsReview {
                entity_name,
                conflicts,
                reason,
                ..
            } => {
                assert_eq!(entity_name, "Generic Corp");
                assert!(!conflicts.is_empty());
                assert!(reason.contains("ambiguous"));
            }
            _ => panic!("expected NeedsReview"),
        }
    }

    // ── Relation handling ──

    #[test]
    fn relation_between_known_entities_creates_relation_ko() {
        let ir = KnowledgeIr {
            entities: vec![
                entity("Acme Corporation", Some("Organization")),
                entity("Globex Industries", Some("Organization")),
            ],
            relations: vec![RelationCandidate {
                subject: "Acme Corporation".into(),
                predicate: "partnered_with".into(),
                object: "Globex Industries".into(),
                confidence: 0.7,
                evidence: evidence(),
            }],
            ..Default::default()
        };

        let ontology = OntologyProposal::default();
        let resolution = ResolutionResult {
            matched: vec![
                mk_resolved("Acme Corporation", Some("Organization"), "ko-1", 1.0),
                mk_resolved("Globex Industries", Some("Organization"), "ko-2", 1.0),
            ],
            ambiguous: vec![],
            unmatched: vec![],
            stats: ResolutionStats::default(),
        };
        let kb = vec![
            kb_entry("ko-1", "Acme Corporation", "Organization", vec![]),
            kb_entry("ko-2", "Globex Industries", "Organization", vec![]),
        ];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &ontology, &resolution, &kb);

        // Should have 2 skips + 1 relation create = 3 actions
        let rel_action = plan
            .actions
            .iter()
            .find(|a| {
                matches!(a, CommitAction::CreateKO { entity_name, .. }
                    if entity_name.starts_with("rel:"))
            })
            .unwrap();
        match rel_action {
            CommitAction::CreateKO {
                entity_name,
                properties,
                ..
            } => {
                assert!(entity_name.contains("Acme Corporation"));
                assert!(entity_name.contains("Globex Industries"));
                let has_subject = properties
                    .iter()
                    .any(|(k, v)| k == "subject" && v == "Acme Corporation");
                let has_object = properties
                    .iter()
                    .any(|(k, v)| k == "object" && v == "Globex Industries");
                assert!(has_subject);
                assert!(has_object);
            }
            _ => panic!("expected CreateKO for relation"),
        }
    }

    // ── Full pipeline ──

    #[test]
    fn full_reconciliation_mixed_scenario() {
        let ir = KnowledgeIr {
            entities: vec![
                entity("Acme Corporation", Some("Organization")),
                entity("New Startup", Some("Organization")),
                entity("Ambiguous LLC", None),
            ],
            facts: vec![
                fact("Acme Corporation Revenue 10M", vec!["Acme Corporation"]),
                fact("New Startup Founded 2025", vec!["New Startup"]),
            ],
            ..Default::default()
        };

        let ontology = OntologyProposal::default();
        let resolution = ResolutionResult {
            matched: vec![mk_resolved(
                "Acme Corporation",
                Some("Organization"),
                "ko-1",
                1.0,
            )],
            ambiguous: vec![mk_ambiguous("Ambiguous LLC")],
            unmatched: vec![mk_unmatched("New Startup", Some("Organization"))],
            stats: ResolutionStats::default(),
        };

        // Existing KO has no prior revenue property → update.
        let kb = vec![kb_entry("ko-1", "Acme Corporation", "Organization", vec![])];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &ontology, &resolution, &kb);

        assert_eq!(plan.stats.creates, 1); // New Startup
        assert_eq!(plan.stats.updates, 1); // Acme Corporation
        assert_eq!(plan.stats.needs_review, 1); // Ambiguous LLC
    }

    // ── Convenience function ──

    #[test]
    fn reconcile_and_plan_convenience() {
        let ir = KnowledgeIr {
            entities: vec![entity("Test Corp", Some("Organization"))],
            ..Default::default()
        };

        let plan = reconcile_and_plan(
            &ir,
            &OntologyProposal::default(),
            &ResolutionResult {
                matched: vec![],
                ambiguous: vec![],
                unmatched: vec![mk_unmatched("Test Corp", Some("Organization"))],
                stats: ResolutionStats::default(),
            },
            &[],
            &MockKnowledgeReconciler::new(),
        );

        assert_eq!(plan.stats.creates, 1);
    }

    // ── Stats ──

    #[test]
    fn commit_stats_are_accurate() {
        let ir = KnowledgeIr {
            entities: vec![
                entity("E1", None),
                entity("E2", None),
                entity("E3", None),
                entity("E4", None),
            ],
            ..Default::default()
        };

        let resolution = ResolutionResult {
            matched: vec![mk_resolved("E1", None, "ko-1", 1.0)],
            ambiguous: vec![mk_ambiguous("E2")],
            unmatched: vec![mk_unmatched("E3", None), mk_unmatched("E4", None)],
            stats: ResolutionStats::default(),
        };

        let kb = vec![kb_entry("ko-1", "E1", "Thing", vec![])];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &OntologyProposal::default(), &resolution, &kb);

        assert_eq!(plan.stats.total_actions, 4);
        assert_eq!(plan.stats.creates, 2); // E3, E4
        assert_eq!(plan.stats.updates, 0);
        assert_eq!(plan.stats.skips, 1); // E1
        assert_eq!(plan.stats.needs_review, 1); // E2
    }

    // ── KnowledgeCommitPlan iterators ──

    #[test]
    fn plan_iterators_filter_correctly() {
        let ir = KnowledgeIr {
            entities: vec![entity("Create Me", None), entity("Update Me", None)],
            facts: vec![fact("Update Me New Fact", vec!["Update Me"])],
            ..Default::default()
        };

        let resolution = ResolutionResult {
            matched: vec![mk_resolved("Update Me", None, "ko-1", 1.0)],
            ambiguous: vec![],
            unmatched: vec![mk_unmatched("Create Me", None)],
            stats: ResolutionStats::default(),
        };
        let kb = vec![kb_entry("ko-1", "Update Me", "Thing", vec![])];

        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(&ir, &OntologyProposal::default(), &resolution, &kb);

        assert_eq!(plan.creates().count(), 1);
        assert_eq!(plan.updates().count(), 1);
        assert_eq!(plan.needs_review_count(), 0);
    }

    // ── Edge cases ──

    #[test]
    fn empty_ir_produces_empty_plan() {
        let reconciler = MockKnowledgeReconciler::new();
        let plan = reconciler.reconcile(
            &KnowledgeIr::default(),
            &OntologyProposal::default(),
            &ResolutionResult::default(),
            &[],
        );

        assert_eq!(plan.stats.total_actions, 0);
    }

    #[test]
    fn case_difference_not_a_conflict_when_normalized() {
        let ir = KnowledgeIr {
            entities: vec![entity("Acme Corp", Some("Organization"))],
            facts: vec![fact("Acme Corp Status active", vec!["Acme Corp"])],
            ..Default::default()
        };

        let resolution = ResolutionResult {
            matched: vec![mk_resolved("Acme Corp", Some("Organization"), "ko-1", 1.0)],
            ambiguous: vec![],
            unmatched: vec![],
            stats: ResolutionStats::default(),
        };
        // Same key, different case in value.
        let kb = vec![kb_entry(
            "ko-1",
            "Acme Corp",
            "Organization",
            vec![("acme_corp_status", "Acme Corp Status ACTIVE")],
        )];

        let reconciler = MockKnowledgeReconciler::new(); // normalize_values: true
        let plan = reconciler.reconcile(&ir, &OntologyProposal::default(), &resolution, &kb);

        // Normalized: "acme corp status active" == "acme corp status active" → Skip.
        assert_eq!(plan.stats.skips, 1);
    }

    #[test]
    fn case_difference_is_conflict_when_not_normalized() {
        let ir = KnowledgeIr {
            entities: vec![entity("Acme Corp", Some("Organization"))],
            facts: vec![fact("Acme Corp Status active", vec!["Acme Corp"])],
            ..Default::default()
        };

        let resolution = ResolutionResult {
            matched: vec![mk_resolved("Acme Corp", Some("Organization"), "ko-1", 1.0)],
            ambiguous: vec![],
            unmatched: vec![],
            stats: ResolutionStats::default(),
        };
        let kb = vec![kb_entry(
            "ko-1",
            "Acme Corp",
            "Organization",
            vec![("acme_corp_status", "Acme Corp Status ACTIVE")],
        )];

        let reconciler = MockKnowledgeReconciler {
            normalize_values: false,
        };
        let plan = reconciler.reconcile(&ir, &OntologyProposal::default(), &resolution, &kb);

        // Not normalized: "Acme Corp Status active" != "Acme Corp Status ACTIVE" → conflict.
        assert_eq!(plan.stats.needs_review, 1);
    }

    // ── KnowledgeReconciler trait object ──

    #[test]
    fn mock_implements_reconciler_trait() {
        let reconciler: &dyn KnowledgeReconciler = &MockKnowledgeReconciler::new();
        assert_eq!(reconciler.name(), "mock");

        let plan = reconciler.reconcile(
            &KnowledgeIr::default(),
            &OntologyProposal::default(),
            &ResolutionResult::default(),
            &[],
        );
        assert_eq!(plan.stats.total_actions, 0);
    }
}
