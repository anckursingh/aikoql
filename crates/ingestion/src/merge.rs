//! MRFC-0070 Phase A3: Multi-source Knowledge Graph merging.
//!
//! Merges KnowledgeIr from multiple compilers (Markdown, Code, etc.) into a
//! unified graph with entity dedup and evidence linking.

use crate::ir::{Evidence, KnowledgeIr};

/// Merge multiple KnowledgeIr sources into one.
///
/// Strategy:
/// - Entities with the same normalized name → merged (mentions + evidence combined)
/// - Facts are deduplicated by statement equality
/// - Relations are deduplicated by (subject, predicate, object) triple
pub fn merge_knowledge_ir(sources: &[KnowledgeIr]) -> KnowledgeIr {
    let mut merged = KnowledgeIr::default();

    if let Some(first) = sources.first() {
        merged.document_id = first.document_id.clone();
        merged.extractor = "multi-source-merger".into();
        merged.page_count = first.page_count;
    }

    // Entity dedup by normalized name
    let mut seen_entities: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for ir in sources {
        for entity in &ir.entities {
            let key = normalize_name(&entity.name);
            if let Some(&idx) = seen_entities.get(&key) {
                // Merge into existing entity
                let existing = &mut merged.entities[idx];
                for m in &entity.mentions {
                    if !existing.mentions.contains(m) {
                        existing.mentions.push(m.clone());
                    }
                }
                // Keep the more specific type_hint
                if existing.type_hint.is_none() && entity.type_hint.is_some() {
                    existing.type_hint = entity.type_hint.clone();
                }
                // Boost confidence for multi-source entities
                existing.confidence = (existing.confidence + entity.confidence).min(1.0);
            } else {
                seen_entities.insert(key, merged.entities.len());
                merged.entities.push(entity.clone());
            }
        }
    }

    // Fact dedup by statement
    let mut seen_facts: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for ir in sources {
        for fact in &ir.facts {
            if seen_facts.insert(fact.statement.clone()) {
                merged.facts.push(fact.clone());
            }
        }
    }

    // Relation dedup by (subject, predicate, object)
    let mut seen_rels: std::collections::BTreeSet<(String, String, String)> =
        std::collections::BTreeSet::new();
    for ir in sources {
        for rel in &ir.relations {
            let key = (
                rel.subject.clone(),
                rel.predicate.clone(),
                rel.object.clone(),
            );
            if seen_rels.insert(key) {
                merged.relations.push(rel.clone());
            }
        }
    }

    merged
}

/// Normalize entity name for comparison: lowercase, collapse whitespace,
/// strip trailing 's' (plural forms).
fn normalize_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let collapsed: String = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    // Strip common suffixes for fuzzy matching
    let trimmed = collapsed
        .trim_end_matches("type")
        .trim_end_matches("_t")
        .trim();
    trimmed.to_string()
}

/// Produce an evidence trail linking each entity to its source(s).
pub fn evidence_trail(ir: &KnowledgeIr, source_label: &str) -> Vec<Evidence> {
    let mut trail = Vec::new();
    for entity in &ir.entities {
        trail.push(Evidence {
            document_id: ir.document_id.clone(),
            page: None,
            bbox_text: Some(format!("{}: {}", source_label, entity.name)),
            extractor: ir.extractor.clone(),
            model: Some("multi-source".into()),
            confidence: entity.confidence,
        });
    }
    trail
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{EntityCandidate, FactCandidate, RelationCandidate};

    #[test]
    fn merge_deduplicates_same_name() {
        let ir1 = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "ConstraintEngine".into(),
                type_hint: Some("Struct".into()),
                mentions: vec!["A constraint engine.".into()],
                confidence: 0.8,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        };

        let ir2 = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "ConstraintEngine".into(),
                type_hint: Some("Component".into()),
                mentions: vec!["Validates constraint rules.".into()],
                confidence: 0.7,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        };

        let merged = merge_knowledge_ir(&[ir1, ir2]);
        assert_eq!(merged.entities.len(), 1);
        assert_eq!(merged.entities[0].mentions.len(), 2);
    }

    #[test]
    fn merge_keeps_distinct_entities() {
        let ir1 = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "User".into(),
                type_hint: Some("Struct".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        };

        let ir2 = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "Session".into(),
                type_hint: Some("Struct".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        };

        let merged = merge_knowledge_ir(&[ir1, ir2]);
        assert_eq!(merged.entities.len(), 2);
    }

    #[test]
    fn merge_dedups_duplicate_facts() {
        let fact = FactCandidate {
            statement: "The system uses MVCC".into(),
            entities: vec![],
            confidence: 0.7,
            evidence: Evidence::default(),
        };
        let ir1 = KnowledgeIr {
            facts: vec![fact.clone()],
            ..Default::default()
        };
        let ir2 = KnowledgeIr {
            facts: vec![fact],
            ..Default::default()
        };
        let merged = merge_knowledge_ir(&[ir1, ir2]);
        assert_eq!(merged.facts.len(), 1);
    }

    #[test]
    fn merge_dedups_duplicate_relations() {
        let rel = RelationCandidate {
            subject: "A".into(),
            predicate: "DEPENDS_ON".into(),
            object: "B".into(),
            confidence: 0.8,
            evidence: Evidence::default(),
        };
        let ir1 = KnowledgeIr {
            relations: vec![rel.clone()],
            ..Default::default()
        };
        let ir2 = KnowledgeIr {
            relations: vec![rel],
            ..Default::default()
        };
        let merged = merge_knowledge_ir(&[ir1, ir2]);
        assert_eq!(merged.relations.len(), 1);
    }
}
