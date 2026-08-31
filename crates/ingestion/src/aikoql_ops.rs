//! MRFC-0070 Phase A6: Aikoql Agent Operations — full implementation.
//!
//! Seven semantic query primitives exposed as library functions and MCP tools:
//! EXPLAIN, TRACE, FIND CONFLICTS, FIND STALE, VALIDATE CHANGE, PROPOSE UPDATE.
//!
//! All operate on merged KnowledgeIr from the document compilation pipeline.

use crate::ir::{FactCandidate, KnowledgeIr};
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// 1. EXPLAIN COMPONENT "name"
// ---------------------------------------------------------------------------

/// Full explanation of a component: purpose, architecture, dependencies,
/// constraints, requirements, decisions, implementation, tests, changes, conflicts.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ComponentExplanation {
    pub name: String,
    pub type_hint: Option<String>,
    pub purpose: Vec<String>,
    pub dependencies: Vec<String>,
    pub dependents: Vec<String>,
    pub facts: Vec<String>,
    pub decisions: Vec<String>,
    pub tests: Vec<String>,
}

/// Generate a component explanation from merged KnowledgeIr.
pub fn explain_component(name: &str, ir: &KnowledgeIr) -> Option<ComponentExplanation> {
    let entity = ir.entities.iter().find(|e| e.name == name)?;

    // Facts that reference this entity
    let facts: Vec<String> = ir
        .facts
        .iter()
        .filter(|f| f.entities.contains(&name.to_string()))
        .map(|f| f.statement.clone())
        .collect();

    // Dependencies (subject = this → depends on object)
    let dependencies: Vec<String> = ir
        .relations
        .iter()
        .filter(|r| r.subject == name && r.predicate == "depends_on")
        .map(|r| r.object.clone())
        .collect();

    // Dependents (object = this ← depends on subject)
    let dependents: Vec<String> = ir
        .relations
        .iter()
        .filter(|r| r.object == name && r.predicate == "depends_on")
        .map(|r| r.subject.clone())
        .collect();

    // Decisions (ADR entities) that mention this component
    let decisions: Vec<String> = ir
        .entities
        .iter()
        .filter(|e| {
            e.type_hint.as_deref() == Some("Decision")
                && e.mentions.iter().any(|m| m.contains(name))
        })
        .map(|e| e.name.clone())
        .collect();

    // Tests (entities with type_hint = Test) that test this component
    let tests: Vec<String> = ir
        .relations
        .iter()
        .filter(|r| r.subject.contains("test_") && r.predicate == "tested_by" && r.object == name)
        .map(|r| r.subject.clone())
        .collect();

    // Purpose: first mention that isn't just the name
    let purpose: Vec<String> = entity
        .mentions
        .iter()
        .filter(|m| m.len() > name.len() + 5)
        .cloned()
        .collect();

    Some(ComponentExplanation {
        name: name.to_string(),
        type_hint: entity.type_hint.clone(),
        purpose,
        dependencies,
        dependents,
        facts,
        decisions,
        tests,
    })
}

// ---------------------------------------------------------------------------
// 2. EXPLAIN DECISION "name"
// ---------------------------------------------------------------------------

/// Full explanation of an architectural decision.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DecisionExplanation {
    pub name: String,
    pub context: String,
    pub problem: Option<String>,
    pub options: Vec<String>,
    pub selected: Option<String>,
    pub rationale: Option<String>,
    pub consequences: Vec<String>,
    pub related_components: Vec<String>,
    pub facts: Vec<String>,
}

/// Generate a decision explanation from merged KnowledgeIr.
pub fn explain_decision(name: &str, ir: &KnowledgeIr) -> Option<DecisionExplanation> {
    let entity = ir
        .entities
        .iter()
        .find(|e| e.name == name && e.type_hint.as_deref() == Some("Decision"))?;

    // Parse ADR structure from mentions
    let all_text = entity.mentions.join("\n");
    let problem = extract_section(&all_text, "Problem", "Context");
    let context = extract_section(&all_text, "Context", "Decision");
    let decision_text = extract_section(&all_text, "Decision", "Consequences");
    let consequences_text = extract_section(&all_text, "Consequences", "");

    // Options: look for bullet points with option-like patterns
    let options: Vec<String> = entity
        .mentions
        .iter()
        .filter(|m| m.starts_with('-') || m.starts_with('*') || m.starts_with("Option"))
        .cloned()
        .collect();

    // Related components: entities mentioned in this decision
    let related: Vec<String> = ir
        .entities
        .iter()
        .filter(|e| e.name != name && entity.mentions.iter().any(|m| m.contains(&e.name)))
        .map(|e| e.name.clone())
        .collect();

    // Facts that reference this decision
    let facts: Vec<String> = ir
        .facts
        .iter()
        .filter(|f| f.entities.contains(&name.to_string()))
        .map(|f| f.statement.clone())
        .collect();

    Some(DecisionExplanation {
        name: name.to_string(),
        context: context.unwrap_or_else(|| "No context provided".into()),
        problem,
        options,
        selected: decision_text,
        rationale: if !facts.is_empty() {
            Some(facts.join("; "))
        } else {
            None
        },
        consequences: {
            // justified: absent consequences section → empty list
            let lines: Vec<String> = consequences_text
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.trim().to_string())
                .collect();
            if lines.is_empty() {
                vec![]
            } else {
                lines
            }
        },
        related_components: related,
        facts,
    })
}

fn extract_section(text: &str, section_name: &str, next_section: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let start_marker = format!("## {}", section_name.to_lowercase());
    let start = lower.find(&start_marker)?;

    let content_start = text[start..].find('\n').unwrap_or(0) + start + 1;

    if next_section.is_empty() {
        Some(text[content_start..].trim().to_string())
    } else {
        let end_marker = format!("## {}", next_section.to_lowercase());
        let after = &lower[content_start..];
        let end = after.find(&end_marker).unwrap_or(after.len());
        Some(text[content_start..content_start + end].trim().to_string())
    }
}

// ---------------------------------------------------------------------------
// 3. TRACE REQUIREMENT "id" TO CODE
// ---------------------------------------------------------------------------

/// Full trace from a requirement through decisions, components, modules, functions, to tests.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RequirementTrace {
    pub requirement: String,
    pub decisions: Vec<String>,
    pub components: Vec<String>,
    pub modules: Vec<String>,
    pub functions: Vec<String>,
    pub tests: Vec<String>,
    pub trace_chain: Vec<String>,
}

/// Trace a requirement through the knowledge graph to code and tests.
pub fn trace_requirement(requirement_id: &str, ir: &KnowledgeIr) -> RequirementTrace {
    // Find the requirement (fact with must/should/shall)
    let req_fact = ir.facts.iter().find(|f| {
        f.statement.contains(requirement_id)
            || f.statement
                .to_lowercase()
                .contains(&requirement_id.to_lowercase())
    });

    let req_text = req_fact
        .map(|f| f.statement.clone())
        .unwrap_or_else(|| requirement_id.to_string());

    // Find entities mentioned in this requirement
    // justified: requirement fact not found → no entities mentioned
    let req_entities: Vec<String> = req_fact.map(|f| f.entities.clone()).unwrap_or_default();

    let mut trace_chain: Vec<String> = vec![format!("Requirement: {}", req_text)];

    // Decisions that mention these entities or the requirement
    let decisions: Vec<String> = ir
        .entities
        .iter()
        .filter(|e| {
            e.type_hint.as_deref() == Some("Decision")
                && (e.mentions.iter().any(|m| {
                    req_entities.iter().any(|re| m.contains(re)) || m.contains(requirement_id)
                }))
        })
        .map(|e| {
            trace_chain.push(format!("Decision: {}", e.name));
            e.name.clone()
        })
        .collect();

    // Components (entities with type Struct/Module/Enum/Trait)
    let components: Vec<String> = req_entities
        .iter()
        .filter(|name| {
            ir.entities.iter().any(|e| {
                e.name == **name
                    && e.type_hint
                        .as_deref()
                        .is_some_and(|t| matches!(t, "Struct" | "Module" | "Enum" | "Trait"))
            })
        })
        .cloned()
        .collect();

    for c in &components {
        trace_chain.push(format!("Component: {}", c));
    }

    // Functions (Method entities, Function entities)
    let functions: Vec<String> = ir
        .entities
        .iter()
        .filter(|e| {
            e.type_hint
                .as_deref()
                .is_some_and(|t| matches!(t, "Function" | "Method" | "Impl"))
                && components
                    .iter()
                    .any(|c| e.name.contains(c) || e.mentions.iter().any(|m| m.contains(c)))
        })
        .map(|e| {
            trace_chain.push(format!("Function: {}", e.name));
            e.name.clone()
        })
        .collect();

    // Tests (Test entities) related to these functions or components
    let tests: Vec<String> = ir
        .relations
        .iter()
        .filter(|r| {
            r.predicate == "tested_by"
                && (components.contains(&r.object) || functions.contains(&r.object))
        })
        .map(|r| {
            trace_chain.push(format!("Test: {}", r.subject));
            r.subject.clone()
        })
        .collect();

    // Gather all modules
    let modules: Vec<String> = ir
        .entities
        .iter()
        .filter(|e| e.type_hint.as_deref() == Some("Module"))
        .map(|e| e.name.clone())
        .collect();

    RequirementTrace {
        requirement: req_text,
        decisions,
        components,
        modules,
        functions,
        tests,
        trace_chain,
    }
}

// ---------------------------------------------------------------------------
// 4. FIND CONFLICTS
// ---------------------------------------------------------------------------

/// A conflict between two knowledge claims.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ConflictReport {
    pub entity_name: Option<String>,
    pub contradictions: Vec<ContradictoryClaim>,
    pub ambiguous_facts: Vec<(String, String)>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ContradictoryClaim {
    pub fact_a: String,
    pub fact_b: String,
    pub reason: String,
}

/// Find conflicts related to a specific component.
pub fn find_conflicts(component: &str, ir: &KnowledgeIr) -> ConflictReport {
    let mut contradictions = Vec::new();
    let mut ambiguous = Vec::new();

    // Gather all facts that reference this component
    let component_facts: Vec<&FactCandidate> = ir
        .facts
        .iter()
        .filter(|f| f.entities.contains(&component.to_string()))
        .collect();

    // Pairwise check for contradictions
    for i in 0..component_facts.len() {
        for j in (i + 1)..component_facts.len() {
            let a = &component_facts[i].statement;
            let b = &component_facts[j].statement;

            // Negation: "must X" vs "must not X"
            // "should X" vs "should not X"
            if is_contradictory(a, b) {
                contradictions.push(ContradictoryClaim {
                    fact_a: a.clone(),
                    fact_b: b.clone(),
                    reason: "negation contradiction".into(),
                });
            }

            // Near-duplicates with subtle differences (potential ambiguity)
            let sim = jaccard_similarity(a, b);
            if sim > 0.7 && sim < 0.95 {
                ambiguous.push((a.clone(), b.clone()));
            }
        }
    }

    ConflictReport {
        entity_name: Some(component.to_string()),
        contradictions,
        ambiguous_facts: ambiguous,
    }
}

fn is_contradictory(a: &str, b: &str) -> bool {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();

    // Direct negation words
    let negation_pairs = [
        ("must", "must not"),
        ("shall", "shall not"),
        ("should", "should not"),
        ("always", "never"),
        ("required", "optional"),
        ("enabled", "disabled"),
        ("sync", "async"),
    ];

    for (pos, neg) in &negation_pairs {
        if a_lower.contains(pos) && b_lower.contains(neg) {
            // Check they're about the same topic
            let a_topic = a_lower.replace(pos, "").trim().to_string();
            let b_topic = b_lower.replace(neg, "").trim().to_string();
            if jaccard_similarity(&a_topic, &b_topic) > 0.4 {
                return true;
            }
        }
        if a_lower.contains(neg) && b_lower.contains(pos) {
            let a_topic = a_lower.replace(neg, "").trim().to_string();
            let b_topic = b_lower.replace(pos, "").trim().to_string();
            if jaccard_similarity(&a_topic, &b_topic) > 0.4 {
                return true;
            }
        }
    }
    false
}

fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let a_words: BTreeSet<&str> = a.split_whitespace().collect();
    let b_words: BTreeSet<&str> = b.split_whitespace().collect();
    let intersection = a_words.intersection(&b_words).count();
    let union = a_words.union(&b_words).count();
    if union == 0 {
        return 1.0;
    }
    intersection as f64 / union as f64
}

// ---------------------------------------------------------------------------
// 5. FIND STALE DOCUMENTATION
// ---------------------------------------------------------------------------

/// A stale documentation finding.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StaleDocumentationReport {
    pub stale_entities: Vec<StaleEntityInfo>,
    pub stale_facts: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StaleEntityInfo {
    pub entity_name: String,
    pub entity_source: String,
    pub reason: String,
}

/// Find stale documentation by comparing document-derived facts against code-derived facts.
pub fn find_stale_documentation(ir: &KnowledgeIr) -> StaleDocumentationReport {
    // Split knowledge by source: document-derived vs code-derived
    let doc_facts: Vec<&FactCandidate> = ir
        .facts
        .iter()
        .filter(|f| f.evidence.extractor.contains("markdown") || f.evidence.extractor == "unknown")
        .collect();

    let code_facts: Vec<&FactCandidate> = ir
        .facts
        .iter()
        .filter(|f| f.evidence.extractor.contains("code") || f.evidence.extractor.contains("syn"))
        .collect();

    let mut stale_entities = Vec::new();
    let mut stale_facts = Vec::new();

    // For each entity mentioned in both doc and code facts, check for divergence
    for entity in &ir.entities {
        let doc_refs: Vec<&&FactCandidate> = doc_facts
            .iter()
            .filter(|f| f.entities.contains(&entity.name))
            .collect();

        let code_refs: Vec<&&FactCandidate> = code_facts
            .iter()
            .filter(|f| f.entities.contains(&entity.name))
            .collect();

        if doc_refs.is_empty() && code_refs.is_empty() {
            continue;
        }

        // Documented but no code → stale docs (removed/renamed)
        if !doc_refs.is_empty() && code_refs.is_empty() {
            stale_entities.push(StaleEntityInfo {
                entity_name: entity.name.clone(),
                // justified: entity without a source document → ""
                entity_source: entity.evidence.document_id.clone().unwrap_or_default(),
                reason: "documented but no code reference — possibly removed or renamed".into(),
            });
        }

        // In code but not documented → missing docs
        if doc_refs.is_empty() && !code_refs.is_empty() {
            stale_entities.push(StaleEntityInfo {
                entity_name: entity.name.clone(),
                // justified: entity without a source document → ""
                entity_source: entity.evidence.document_id.clone().unwrap_or_default(),
                reason: "exists in code but has no documentation — documentation gap".into(),
            });
        }
    }

    // Stale facts: doc facts that aren't corroborated by any code fact
    for doc_fact in &doc_facts {
        let has_code_corroboration = code_facts.iter().any(|cf| {
            jaccard_similarity(&doc_fact.statement, &cf.statement) > 0.3
                && cf.entities.iter().any(|e| doc_fact.entities.contains(e))
        });

        if !has_code_corroboration && !doc_fact.statement.starts_with("must") {
            stale_facts.push(doc_fact.statement.clone());
        }
    }

    let summary = format!(
        "{} stale entities, {} potentially stale facts",
        stale_entities.len(),
        stale_facts.len()
    );

    StaleDocumentationReport {
        stale_entities,
        stale_facts,
        summary,
    }
}

// ---------------------------------------------------------------------------
// 6. VALIDATE CHANGE
// ---------------------------------------------------------------------------

/// Result of validating a proposed change.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ChangeValidation {
    pub change_description: String,
    pub affected_entities: Vec<AffectedKnowledgeInfo>,
    pub affected_facts: Vec<String>,
    pub affected_relations: Vec<String>,
    pub risks: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AffectedKnowledgeInfo {
    pub entity_name: String,
    pub impact: String,
    pub confidence: f32,
}

/// Validate a proposed change: what knowledge entities would be affected?
pub fn validate_change(description: &str, ir: &KnowledgeIr) -> ChangeValidation {
    let desc_lower = description.to_lowercase();
    let desc_words: BTreeSet<&str> = desc_lower.split_whitespace().collect();

    let mut affected_entities = Vec::new();
    let mut affected_facts = Vec::new();
    let mut affected_relations = Vec::new();
    let mut risks = Vec::new();

    // Entities whose name or mentions overlap with change description
    for entity in &ir.entities {
        let name_hit = desc_words.iter().any(|w| {
            entity.name.to_lowercase().contains(*w) || w.contains(&entity.name.to_lowercase())
        });

        let mention_hit = entity
            .mentions
            .iter()
            .any(|m| desc_words.iter().any(|w| m.to_lowercase().contains(*w)));

        if name_hit || mention_hit {
            let impact = if name_hit {
                "direct: entity name matches change description".to_string()
            } else {
                "indirect: entity mentions overlap with change".to_string()
            };

            affected_entities.push(AffectedKnowledgeInfo {
                entity_name: entity.name.clone(),
                impact,
                confidence: if name_hit { 0.9 } else { 0.6 },
            });

            // Related facts
            for fact in &ir.facts {
                if fact.entities.contains(&entity.name) {
                    affected_facts.push(fact.statement.clone());
                }
            }

            // Related relations
            for rel in &ir.relations {
                if rel.subject == entity.name || rel.object == entity.name {
                    affected_relations.push(format!(
                        "{} --[{}]--> {}",
                        rel.subject, rel.predicate, rel.object
                    ));
                }
            }
        }
    }

    // Risk assessment
    if affected_entities.len() > 5 {
        risks.push(format!(
            "HIGH: {} entities affected — wide blast radius",
            affected_entities.len()
        ));
    }
    if affected_facts.len() > 10 {
        risks.push(format!(
            "MEDIUM: {} facts may become stale",
            affected_facts.len()
        ));
    }
    if affected_entities.is_empty() {
        risks.push("LOW: no known entities affected — safe change".into());
    }

    ChangeValidation {
        change_description: description.to_string(),
        affected_entities,
        affected_facts,
        affected_relations,
        risks,
    }
}

// ---------------------------------------------------------------------------
// 7. PROPOSE KNOWLEDGE UPDATE
// ---------------------------------------------------------------------------

/// A proposed knowledge update from an agent.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeProposal {
    /// What the agent wants to change.
    pub action: ProposalAction,
    /// Target entity, if applicable.
    pub target_entity: Option<String>,
    /// Proposed new facts.
    pub new_facts: Vec<String>,
    /// Facts to remove (stale).
    pub remove_facts: Vec<String>,
    /// Proposed new relations.
    pub new_relations: Vec<(String, String, String)>,
    /// Agent's justification.
    pub justification: String,
    /// Agent identity.
    pub agent_id: String,
    /// Status in the reconciliation workflow.
    pub status: ProposalStatus,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProposalAction {
    AddFact,
    RemoveFact,
    UpdateEntity,
    AddRelation,
    RemoveRelation,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProposalStatus {
    Proposed,
    Validated,
    Accepted,
    Rejected,
}

/// Validate and create a knowledge proposal.
pub fn propose_knowledge_update(
    action: ProposalAction,
    target_entity: Option<String>,
    new_facts: Vec<String>,
    remove_facts: Vec<String>,
    new_relations: Vec<(String, String, String)>,
    justification: String,
    agent_id: String,
    ir: &KnowledgeIr,
) -> KnowledgeProposal {
    // Validation: check target entity exists if specified
    let entity_exists = target_entity
        .as_ref()
        .map(|name| ir.entities.iter().any(|e| e.name == *name))
        .unwrap_or(true);

    let status = if !entity_exists {
        ProposalStatus::Rejected
    } else {
        ProposalStatus::Proposed
    };

    KnowledgeProposal {
        action,
        target_entity,
        new_facts,
        remove_facts,
        new_relations,
        justification,
        agent_id,
        status,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{EntityCandidate, Evidence, FactCandidate, RelationCandidate};

    fn sample_ir() -> KnowledgeIr {
        KnowledgeIr {
            entities: vec![
                EntityCandidate {
                    name: "TransactionEngine".into(),
                    type_hint: Some("Struct".into()),
                    mentions: vec![
                        "Handles all write operations with MVCC isolation".into(),
                        "Coordinates with ConstraintEngine for validation".into(),
                    ],
                    confidence: 0.85,
                    evidence: Evidence::default(),
                },
                EntityCandidate {
                    name: "ConstraintEngine".into(),
                    type_hint: Some("Struct".into()),
                    mentions: vec!["Validates constraint rules before commit".into()],
                    confidence: 0.85,
                    evidence: Evidence::default(),
                },
                EntityCandidate {
                    name: "ADR-001: MVCC over locking".into(),
                    type_hint: Some("Decision".into()),
                    mentions: vec![
                        "## Context\nWe need concurrent write isolation".into(),
                        "## Decision\nUse MVCC instead of row-level locking".into(),
                        "## Consequences\nBetter read performance, no deadlock risk".into(),
                        "TransactionEngine".into(),
                    ],
                    confidence: 0.9,
                    evidence: Evidence::default(),
                },
                EntityCandidate {
                    name: "test_transaction_isolation".into(),
                    type_hint: Some("Test".into()),
                    mentions: vec!["Tests MVCC isolation guarantees".into()],
                    confidence: 0.8,
                    evidence: Evidence::default(),
                },
            ],
            facts: vec![
                FactCandidate {
                    snippet: None,
                    statement: "must use MVCC for all write operations".into(),
                    entities: vec!["TransactionEngine".into()],
                    confidence: 0.9,
                    evidence: Evidence::default(),
                },
                FactCandidate {
                    snippet: None,
                    statement: "must not use row-level locking".into(),
                    entities: vec!["TransactionEngine".into()],
                    confidence: 0.7,
                    evidence: Evidence::default(),
                },
                FactCandidate {
                    snippet: None,
                    statement: "constraints are validated at commit time".into(),
                    entities: vec!["ConstraintEngine".into()],
                    confidence: 0.85,
                    evidence: Evidence::default(),
                },
            ],
            relations: vec![
                RelationCandidate {
                    subject: "ConstraintEngine".into(),
                    predicate: "depends_on".into(),
                    object: "TransactionEngine".into(),
                    confidence: 0.8,
                    evidence: Evidence::default(),
                },
                RelationCandidate {
                    subject: "test_transaction_isolation".into(),
                    predicate: "tested_by".into(),
                    object: "TransactionEngine".into(),
                    confidence: 0.9,
                    evidence: Evidence::default(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn explain_component_finds_entity_and_deps() {
        let ir = sample_ir();
        let explanation = explain_component("TransactionEngine", &ir).unwrap();
        assert_eq!(explanation.name, "TransactionEngine");
        assert_eq!(explanation.type_hint.as_deref(), Some("Struct"));
        assert!(!explanation.purpose.is_empty());
        assert!(explanation
            .dependents
            .contains(&"ConstraintEngine".to_string()));
    }

    #[test]
    fn explain_decision_extracts_adr_structure() {
        let ir = sample_ir();
        let explanation = explain_decision("ADR-001: MVCC over locking", &ir).unwrap();
        assert!(explanation.context.contains("concurrent write isolation"));
        assert!(explanation
            .selected
            .as_ref()
            .is_some_and(|s| s.contains("MVCC")));
        assert!(!explanation.consequences.is_empty());
    }

    #[test]
    fn trace_requirement_follows_chain() {
        let ir = sample_ir();
        let trace = trace_requirement("MVCC", &ir);
        assert!(!trace.trace_chain.is_empty());
        assert!(trace.requirement.contains("MVCC"));
    }

    #[test]
    fn find_conflicts_detects_negation() {
        let ir = KnowledgeIr {
            facts: vec![
                FactCandidate {
                    snippet: None,
                    statement: "must use synchronous replication".into(),
                    entities: vec!["TransactionEngine".into()],
                    confidence: 0.9,
                    evidence: Evidence::default(),
                },
                FactCandidate {
                    snippet: None,
                    statement: "must not use synchronous replication".into(),
                    entities: vec!["TransactionEngine".into()],
                    confidence: 0.8,
                    evidence: Evidence::default(),
                },
            ],
            entities: vec![EntityCandidate {
                name: "TransactionEngine".into(),
                type_hint: Some("Struct".into()),
                mentions: vec![],
                confidence: 0.9,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        };
        let conflicts = find_conflicts("TransactionEngine", &ir);
        assert!(
            !conflicts.contradictions.is_empty(),
            "should detect negation contradiction: must X vs must not X"
        );
    }

    #[test]
    fn validate_change_identifies_affected() {
        let ir = sample_ir();
        let validation = validate_change("modify MVCC isolation level", &ir);
        assert!(!validation.affected_entities.is_empty());
        assert!(validation
            .affected_entities
            .iter()
            .any(|a| a.entity_name == "TransactionEngine"));
    }

    #[test]
    fn propose_knowledge_update_creates_proposal() {
        let ir = sample_ir();
        let proposal = propose_knowledge_update(
            ProposalAction::AddFact,
            Some("TransactionEngine".into()),
            vec!["now also supports snapshot isolation".into()],
            vec![],
            vec![],
            "Changed isolation from MVCC to snapshot".into(),
            "agent-007".into(),
            &ir,
        );
        assert_eq!(proposal.status, ProposalStatus::Proposed);
        assert_eq!(proposal.agent_id, "agent-007");
    }

    #[test]
    fn propose_update_rejects_nonexistent_entity() {
        let ir = sample_ir();
        let proposal = propose_knowledge_update(
            ProposalAction::AddFact,
            Some("NonexistentComponent".into()),
            vec!["some fact".into()],
            vec![],
            vec![],
            "test".into(),
            "agent-007".into(),
            &ir,
        );
        assert_eq!(proposal.status, ProposalStatus::Rejected);
    }

    #[test]
    fn find_stale_documentation_reports_gaps() {
        let ir = sample_ir();
        let report = find_stale_documentation(&ir);
        assert!(!report.summary.is_empty());
    }
}
