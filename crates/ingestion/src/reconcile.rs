//! MRFC-0070 Phase A8: Change Reconciliation.
//!
//! After a code/document change, identify affected knowledge and flag
//! what needs updating. Maps changed files → affected entities → affected
//! relationships → stale claims.
//!
//! Pipeline: diff → affected files → affected entities → impact graph → report.

use crate::ir::KnowledgeIr;

/// An affected entity with its impact chain.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AffectedEntity {
    /// Name of the affected entity.
    pub entity_name: String,
    /// How the entity was affected (direct file change, transitive via relation, etc.).
    pub impact_path: String,
    /// Severity: direct (file changed) or indirect (related entity).
    pub severity: ImpactSeverity,
    /// Facts that reference this entity and may now be stale.
    pub stale_facts: Vec<String>,
    /// Related entities that may cascade.
    pub related_entities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ImpactSeverity {
    /// Entity's source file was directly changed.
    Direct,
    /// Entity depends on a directly-changed entity.
    Indirect,
    /// Entity is mentioned in a fact whose subject entity changed.
    Cascade,
}

/// Full reconciliation result.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReconciliationReport {
    /// Files that changed.
    pub changed_files: Vec<String>,
    /// Entities affected, ordered by severity.
    pub affected_entities: Vec<AffectedEntity>,
    /// Facts that may be stale.
    pub potentially_stale_facts: Vec<String>,
    /// Summary text for agent consumption.
    pub summary: String,
}

/// Match an entity's evidence source against a changed-file path.
///
/// Tolerates absolute vs repo-relative forms (git reports paths relative to
/// the repo root; compilers record whatever path they were handed) by falling
/// back to basename equality.
pub fn source_matches(source: &str, changed: &str) -> bool {
    if source.is_empty() {
        return false;
    }
    let basename = std::path::Path::new(source)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(source);
    source == changed || basename == changed || changed.ends_with(basename)
}

/// Reconcile a set of changed files against a KnowledgeIr.
///
/// `changed_files` should be paths relative to the repo root (e.g., from `git diff --name-only`).
/// `ir` is the merged KnowledgeIr from all compilers.
pub fn reconcile(changed_files: &[String], ir: &KnowledgeIr) -> ReconciliationReport {
    // Build file → entity mapping from evidence paths
    // Each entity whose evidence.source matches a changed file is directly affected.
    let mut affected: Vec<AffectedEntity> = Vec::new();
    let mut affected_names: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for entity in &ir.entities {
        let source = entity.evidence.document_id.as_deref().unwrap_or("");
        let is_direct = changed_files.iter().any(|f| source_matches(source, f));

        if is_direct {
            let stale_facts: Vec<String> = ir
                .facts
                .iter()
                .filter(|f| f.entities.contains(&entity.name))
                .map(|f| f.statement.clone())
                .collect();

            affected.push(AffectedEntity {
                entity_name: entity.name.clone(),
                impact_path: format!("source file changed: {}", source),
                severity: ImpactSeverity::Direct,
                stale_facts,
                related_entities: Vec::new(), // filled below
            });
            affected_names.insert(&entity.name);
        }
    }

    // Transitive: entities related to directly-affected entities
    for rel in &ir.relations {
        let subj_affected = affected_names.contains(rel.subject.as_str());
        let obj_affected = affected_names.contains(rel.object.as_str());

        if subj_affected && !affected_names.contains(rel.object.as_str()) {
            let stale_facts: Vec<String> = ir
                .facts
                .iter()
                .filter(|f| f.entities.contains(&rel.object))
                .map(|f| f.statement.clone())
                .collect();

            affected.push(AffectedEntity {
                entity_name: rel.object.clone(),
                impact_path: format!(
                    "depends on directly-changed entity '{}' via {}",
                    rel.subject, rel.predicate
                ),
                severity: ImpactSeverity::Indirect,
                stale_facts,
                related_entities: vec![rel.subject.clone()],
            });
        }
        if obj_affected && !subj_affected && !affected_names.contains(rel.subject.as_str()) {
            let stale_facts: Vec<String> = ir
                .facts
                .iter()
                .filter(|f| f.entities.contains(&rel.subject))
                .map(|f| f.statement.clone())
                .collect();

            affected.push(AffectedEntity {
                entity_name: rel.subject.clone(),
                impact_path: format!(
                    "referenced by directly-changed entity '{}' via {}",
                    rel.object, rel.predicate
                ),
                severity: ImpactSeverity::Indirect,
                stale_facts,
                related_entities: vec![rel.object.clone()],
            });
        }
    }

    // Backfill related_entities for direct-impact entries
    for entry in &mut affected {
        let name = entry.entity_name.clone();
        let related: Vec<String> = ir
            .relations
            .iter()
            .filter(|r| r.subject == name || r.object == name)
            .map(|r| {
                if r.subject == name {
                    r.object.clone()
                } else {
                    r.subject.clone()
                }
            })
            .collect();
        entry.related_entities = related;
    }

    // Collect all potentially stale facts
    let stale_facts: Vec<String> = affected
        .iter()
        .flat_map(|a| a.stale_facts.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    // Sort by severity
    affected.sort_by_key(|a| match a.severity {
        ImpactSeverity::Direct => 0,
        ImpactSeverity::Indirect => 1,
        ImpactSeverity::Cascade => 2,
    });

    let direct_count = affected
        .iter()
        .filter(|a| a.severity == ImpactSeverity::Direct)
        .count();
    let indirect_count = affected.len() - direct_count;

    let summary = format!(
        "{} files changed → {} entities directly affected, {} indirectly affected, {} potentially stale facts. Review direct impact entities first.",
        changed_files.len(),
        direct_count,
        indirect_count,
        stale_facts.len()
    );

    ReconciliationReport {
        changed_files: changed_files.to_vec(),
        affected_entities: affected,
        potentially_stale_facts: stale_facts,
        summary,
    }
}

/// Quick check: given changed file paths, which KnowledgeIr entities are stale?
/// Returns entity names whose evidence source overlaps with the changed paths.
pub fn stale_entities(changed_files: &[String], ir: &KnowledgeIr) -> Vec<String> {
    ir.entities
        .iter()
        .filter(|e| {
            let src = e.evidence.document_id.as_deref().unwrap_or("");
            changed_files.iter().any(|f| source_matches(src, f))
        })
        .map(|e| e.name.clone())
        .collect()
}

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
                    mentions: vec!["Handles MVCC transaction isolation".into()],
                    confidence: 0.85,
                    evidence: Evidence {
                        document_id: Some("crates/kernel/src/transaction.rs".into()),
                        ..Default::default()
                    },
                },
                EntityCandidate {
                    name: "ConstraintEngine".into(),
                    type_hint: Some("Struct".into()),
                    mentions: vec!["Validates constraint rules".into()],
                    confidence: 0.85,
                    evidence: Evidence {
                        document_id: Some("crates/kernel/src/constraint.rs".into()),
                        ..Default::default()
                    },
                },
                EntityCandidate {
                    name: "AuthService".into(),
                    type_hint: Some("Module".into()),
                    mentions: vec!["Handles authentication".into()],
                    confidence: 0.7,
                    evidence: Evidence {
                        document_id: Some("crates/auth/src/lib.rs".into()),
                        ..Default::default()
                    },
                },
            ],
            facts: vec![
                FactCandidate {
                    snippet: None,
                    statement: "must use MVCC for all writes".into(),
                    entities: vec!["TransactionEngine".into()],
                    confidence: 0.9,
                    evidence: Evidence::default(),
                },
                FactCandidate {
                    snippet: None,
                    statement: "constraints are validated at commit time".into(),
                    entities: vec!["ConstraintEngine".into()],
                    confidence: 0.85,
                    evidence: Evidence::default(),
                },
                FactCandidate {
                    snippet: None,
                    statement: "AuthService supports OAuth2 and JWT".into(),
                    entities: vec!["AuthService".into()],
                    confidence: 0.7,
                    evidence: Evidence::default(),
                },
            ],
            relations: vec![RelationCandidate {
                subject: "ConstraintEngine".into(),
                predicate: "depends_on".into(),
                object: "TransactionEngine".into(),
                confidence: 0.8,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn reconcile_direct_change_finds_entity() {
        let ir = sample_ir();
        let report = reconcile(&["crates/kernel/src/transaction.rs".to_string()], &ir);
        assert_eq!(report.changed_files.len(), 1);
        // TransactionEngine directly affected + ConstraintEngine indirectly (depends_on)
        let names: Vec<&str> = report
            .affected_entities
            .iter()
            .map(|e| e.entity_name.as_str())
            .collect();
        assert!(
            names.contains(&"TransactionEngine"),
            "direct entity should be affected"
        );
        assert!(
            names.contains(&"ConstraintEngine"),
            "dependent entity should cascade"
        );
    }

    #[test]
    fn reconcile_unrelated_file_no_impact() {
        let ir = sample_ir();
        let report = reconcile(&["README.md".to_string()], &ir);
        assert!(report.affected_entities.is_empty());
    }

    #[test]
    fn stale_entities_detects_changed_sources() {
        let ir = sample_ir();
        let stale = stale_entities(&["crates/auth/src/lib.rs".to_string()], &ir);
        assert_eq!(stale, vec!["AuthService"]);
    }

    #[test]
    fn reconcile_stale_facts_flagged() {
        let ir = sample_ir();
        let report = reconcile(&["crates/kernel/src/transaction.rs".to_string()], &ir);
        assert!(
            report
                .potentially_stale_facts
                .contains(&"must use MVCC for all writes".to_string()),
            "facts referencing changed entity should be flagged"
        );
    }
}
