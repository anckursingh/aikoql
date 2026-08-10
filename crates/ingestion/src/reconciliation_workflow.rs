//! MRFC-0070 Phase A8: Reconciliation Workflow Engine.
//!
//! Manages the lifecycle of knowledge proposals through the pipeline:
//!   PROPOSED → VALIDATED → ACCEPTED or REJECTED.
//!
//! Validates proposals against the current KnowledgeIr state and applies
//! accepted proposals by mutating entities, facts, and relations.

use crate::aikoql_ops::{KnowledgeProposal, ProposalAction, ProposalStatus};
use crate::ir::{FactCandidate, KnowledgeIr, RelationCandidate};

/// Outcome of validating a proposal against the current IR.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ValidationResult {
    /// Whether the proposal passes validation.
    pub valid: bool,
    /// Human-readable reason for rejection.
    pub reason: Option<String>,
    /// Warnings (non-blocking issues).
    pub warnings: Vec<String>,
}

/// Result of applying a proposal.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ApplyResult {
    /// Whether the apply succeeded.
    pub applied: bool,
    /// The mutated KnowledgeIr (only if applied).
    pub ir: Option<KnowledgeIr>,
    /// Reason for failure.
    pub error: Option<String>,
}

/// Batch workflow result.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WorkflowReport {
    /// Total proposals processed.
    pub total: usize,
    /// Count by status.
    pub accepted: usize,
    pub rejected: usize,
    pub still_proposed: usize,
    /// Individual results.
    pub results: Vec<ValidatedProposal>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ValidatedProposal {
    pub proposal: KnowledgeProposal,
    pub validation: ValidationResult,
    pub applied: bool,
}

/// Validate a proposal against the current KnowledgeIr state.
pub fn validate_proposal(proposal: &KnowledgeProposal, ir: &KnowledgeIr) -> ValidationResult {
    let mut warnings = Vec::new();

    match &proposal.action {
        ProposalAction::AddFact => {
            if proposal.new_facts.is_empty() {
                return ValidationResult {
                    valid: false,
                    reason: Some("AddFact requires at least one new fact".into()),
                    warnings,
                };
            }
            if let Some(ref target) = proposal.target_entity {
                if !ir.entities.iter().any(|e| e.name == *target) {
                    return ValidationResult {
                        valid: false,
                        reason: Some(format!("target entity '{}' not found in IR", target)),
                        warnings,
                    };
                }
            }
        }
        ProposalAction::RemoveFact => {
            if proposal.remove_facts.is_empty() {
                return ValidationResult {
                    valid: false,
                    reason: Some("RemoveFact requires at least one fact to remove".into()),
                    warnings,
                };
            }
            for fact_text in &proposal.remove_facts {
                if !ir.facts.iter().any(|f| f.statement.contains(fact_text)) {
                    warnings.push(format!(
                        "fact '{}' not found in IR (may already be removed)",
                        fact_text
                    ));
                }
            }
        }
        ProposalAction::UpdateEntity => {
            if proposal.target_entity.is_none() {
                return ValidationResult {
                    valid: false,
                    reason: Some("UpdateEntity requires a target entity".into()),
                    warnings,
                };
            }
            let target = proposal.target_entity.as_ref().unwrap();
            if !ir.entities.iter().any(|e| e.name == *target) {
                return ValidationResult {
                    valid: false,
                    reason: Some(format!("target entity '{}' not found in IR", target)),
                    warnings,
                };
            }
        }
        ProposalAction::AddRelation => {
            if proposal.new_relations.is_empty() {
                return ValidationResult {
                    valid: false,
                    reason: Some("AddRelation requires at least one relation".into()),
                    warnings,
                };
            }
            for (subject, _, object) in &proposal.new_relations {
                if !ir.entities.iter().any(|e| e.name == *subject) {
                    warnings.push(format!("source entity '{}' not found", subject));
                }
                if !ir.entities.iter().any(|e| e.name == *object) {
                    warnings.push(format!("target entity '{}' not found", object));
                }
            }
        }
        ProposalAction::RemoveRelation => {
            if proposal.new_relations.is_empty() {
                return ValidationResult {
                    valid: false,
                    reason: Some("RemoveRelation requires at least one relation".into()),
                    warnings,
                };
            }
        }
    }

    ValidationResult {
        valid: true,
        reason: None,
        warnings,
    }
}

/// Apply an accepted proposal to mutate the KnowledgeIr.
pub fn apply_proposal(proposal: &KnowledgeProposal, ir: &KnowledgeIr) -> ApplyResult {
    let mut ir = ir.clone();

    match &proposal.action {
        ProposalAction::AddFact => {
            for fact_text in &proposal.new_facts {
                if !ir.facts.iter().any(|f| f.statement.contains(fact_text)) {
                    ir.facts.push(FactCandidate {
                        statement: fact_text.clone(),
                        entities: proposal.target_entity.clone().into_iter().collect(),
                        confidence: 0.7,
                        evidence: Default::default(),
                    });
                }
            }
        }
        ProposalAction::RemoveFact => {
            ir.facts.retain(|f| {
                !proposal
                    .remove_facts
                    .iter()
                    .any(|rf| f.statement.contains(rf))
            });
        }
        ProposalAction::UpdateEntity => {
            let target = match &proposal.target_entity {
                Some(t) => t,
                None => {
                    return ApplyResult {
                        applied: false,
                        ir: None,
                        error: Some("missing target_entity".into()),
                    }
                }
            };
            if let Some(entity) = ir.entities.iter_mut().find(|e| e.name == *target) {
                for fact_text in &proposal.new_facts {
                    if !entity.mentions.contains(fact_text) {
                        entity.mentions.push(fact_text.clone());
                    }
                }
            }
        }
        ProposalAction::AddRelation => {
            for (subject, predicate, object) in &proposal.new_relations {
                if !ir.relations.iter().any(|r| {
                    r.subject == *subject && r.predicate == *predicate && r.object == *object
                }) {
                    ir.relations.push(RelationCandidate {
                        subject: subject.clone(),
                        predicate: predicate.clone(),
                        object: object.clone(),
                        confidence: 0.7,
                        evidence: Default::default(),
                    });
                }
            }
        }
        ProposalAction::RemoveRelation => {
            for (subject, predicate, object) in &proposal.new_relations {
                ir.relations.retain(|r| {
                    !(r.subject == *subject && r.predicate == *predicate && r.object == *object)
                });
            }
        }
    }

    ApplyResult {
        applied: true,
        ir: Some(ir),
        error: None,
    }
}

/// Run the full reconciliation workflow: validate, then accept/reject.
pub fn process_workflow(
    proposals: &[KnowledgeProposal],
    ir: &KnowledgeIr,
) -> (KnowledgeIr, WorkflowReport) {
    let mut ir = ir.clone();
    let mut results = Vec::new();
    let mut accepted = 0;
    let mut rejected = 0;
    let still_proposed = 0;

    for prop in proposals {
        let validation = validate_proposal(prop, &ir);
        let mut prop = prop.clone();

        if validation.valid {
            let result = apply_proposal(&prop, &ir);
            if result.applied {
                if let Some(new_ir) = result.ir {
                    ir = new_ir;
                }
                prop.status = ProposalStatus::Accepted;
                accepted += 1;
                results.push(ValidatedProposal {
                    proposal: prop,
                    validation,
                    applied: true,
                });
            } else {
                prop.status = ProposalStatus::Rejected;
                rejected += 1;
                results.push(ValidatedProposal {
                    proposal: prop,
                    validation,
                    applied: false,
                });
            }
        } else {
            prop.status = ProposalStatus::Rejected;
            rejected += 1;
            results.push(ValidatedProposal {
                proposal: prop,
                validation,
                applied: false,
            });
        }
    }

    (
        ir,
        WorkflowReport {
            total: proposals.len(),
            accepted,
            rejected,
            still_proposed,
            results,
        },
    )
}

/// Generate auto-proposals from a reconcile report's stale entities.
pub fn auto_proposals_from_stale(
    stale_entities: &[String],
    agent_id: &str,
) -> Vec<KnowledgeProposal> {
    stale_entities
        .iter()
        .map(|entity| KnowledgeProposal {
            action: ProposalAction::UpdateEntity,
            target_entity: Some(entity.clone()),
            new_facts: vec![format!(
                "[AUTO] Entity '{}' flagged stale — review needed",
                entity
            )],
            remove_facts: vec![],
            new_relations: vec![],
            justification: "Auto-generated from reconciliation staleness detection".into(),
            agent_id: agent_id.to_string(),
            status: ProposalStatus::Proposed,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ir() -> KnowledgeIr {
        KnowledgeIr {
            entities: vec![
                crate::EntityCandidate {
                    name: "TransactionEngine".into(),
                    type_hint: Some("Struct".into()),
                    mentions: vec!["Handles MVCC".into()],
                    confidence: 0.9,
                    evidence: Default::default(),
                },
                crate::EntityCandidate {
                    name: "LockManager".into(),
                    type_hint: Some("Struct".into()),
                    mentions: vec![],
                    confidence: 0.8,
                    evidence: Default::default(),
                },
            ],
            facts: vec![FactCandidate {
                statement: "TransactionEngine uses MVCC".into(),
                entities: vec!["TransactionEngine".into()],
                confidence: 0.9,
                evidence: Default::default(),
            }],
            relations: vec![RelationCandidate {
                subject: "TransactionEngine".into(),
                predicate: "DEPENDS_ON".into(),
                object: "LockManager".into(),
                confidence: 0.8,
                evidence: Default::default(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn validates_add_fact_proposal() {
        let ir = sample_ir();
        let prop = KnowledgeProposal {
            action: ProposalAction::AddFact,
            target_entity: Some("TransactionEngine".into()),
            new_facts: vec!["TransactionEngine supports snapshot isolation".into()],
            remove_facts: vec![],
            new_relations: vec![],
            justification: "New feature".into(),
            agent_id: "test-agent".into(),
            status: ProposalStatus::Proposed,
        };
        let v = validate_proposal(&prop, &ir);
        assert!(v.valid);
    }

    #[test]
    fn rejects_add_fact_for_unknown_entity() {
        let ir = sample_ir();
        let prop = KnowledgeProposal {
            action: ProposalAction::AddFact,
            target_entity: Some("NonExistent".into()),
            new_facts: vec!["some fact".into()],
            remove_facts: vec![],
            new_relations: vec![],
            justification: "?".into(),
            agent_id: "test-agent".into(),
            status: ProposalStatus::Proposed,
        };
        let v = validate_proposal(&prop, &ir);
        assert!(!v.valid);
        assert!(v.reason.unwrap().contains("not found"));
    }

    #[test]
    fn applies_add_fact_to_ir() {
        let ir = sample_ir();
        let prop = KnowledgeProposal {
            action: ProposalAction::AddFact,
            target_entity: Some("TransactionEngine".into()),
            new_facts: vec!["TransactionEngine supports snapshot isolation".into()],
            remove_facts: vec![],
            new_relations: vec![],
            justification: "New feature".into(),
            agent_id: "test-agent".into(),
            status: ProposalStatus::Proposed,
        };
        let result = apply_proposal(&prop, &ir);
        assert!(result.applied);
        let new_ir = result.ir.unwrap();
        assert!(new_ir
            .facts
            .iter()
            .any(|f| f.statement.contains("snapshot isolation")));
    }

    #[test]
    fn applies_remove_fact_from_ir() {
        let ir = sample_ir();
        let prop = KnowledgeProposal {
            action: ProposalAction::RemoveFact,
            target_entity: None,
            new_facts: vec![],
            remove_facts: vec!["MVCC".into()],
            new_relations: vec![],
            justification: "Stale".into(),
            agent_id: "test-agent".into(),
            status: ProposalStatus::Proposed,
        };
        let result = apply_proposal(&prop, &ir);
        assert!(result.applied);
        let new_ir = result.ir.unwrap();
        assert!(!new_ir.facts.iter().any(|f| f.statement.contains("MVCC")));
    }

    #[test]
    fn full_workflow_accepts_and_rejects() {
        let ir = sample_ir();
        let proposals = vec![
            KnowledgeProposal {
                action: ProposalAction::AddFact,
                target_entity: Some("TransactionEngine".into()),
                new_facts: vec!["New fact".into()],
                remove_facts: vec![],
                new_relations: vec![],
                justification: "valid".into(),
                agent_id: "a1".into(),
                status: ProposalStatus::Proposed,
            },
            KnowledgeProposal {
                action: ProposalAction::AddFact,
                target_entity: Some("NonExistent".into()),
                new_facts: vec!["Bad fact".into()],
                remove_facts: vec![],
                new_relations: vec![],
                justification: "invalid".into(),
                agent_id: "a2".into(),
                status: ProposalStatus::Proposed,
            },
        ];
        let (_, report) = process_workflow(&proposals, &ir);
        assert_eq!(report.total, 2);
        assert_eq!(report.accepted, 1);
        assert_eq!(report.rejected, 1);
    }

    #[test]
    fn auto_proposals_from_stale_entities() {
        let stale = vec!["Foo".into(), "Bar".into()];
        let props = auto_proposals_from_stale(&stale, "reconciler");
        assert_eq!(props.len(), 2);
        assert_eq!(props[0].action, ProposalAction::UpdateEntity);
        assert_eq!(props[0].target_entity, Some("Foo".into()));
        assert_eq!(props[1].target_entity, Some("Bar".into()));
    }
}
