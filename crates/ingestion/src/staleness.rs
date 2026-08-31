//! MRFC-0070 Phase A4: Staleness detection.
//!
//! When multiple sources (code, docs) produce facts about the same entity,
//! conflicting or divergent statements signal stale documentation.

use crate::ir::FactCandidate;

/// A staleness warning: two facts about the same entity disagree.
#[derive(Clone, Debug)]
pub struct StalenessWarning {
    /// Entity referenced by both facts.
    pub entity: String,
    /// The fact from the higher-authority source (e.g. code).
    pub authoritative: String,
    /// The fact from the lower-authority source (e.g. docs).
    pub stale: String,
    /// Source of the authoritative fact ("code", "markdown", etc.).
    pub authoritative_source: String,
    /// Source of the stale fact.
    pub stale_source: String,
    /// Severity: "conflict" (direct contradiction) or "divergence" (different detail).
    pub severity: String,
}

/// Detect staleness by comparing facts across sources.
///
/// Group facts by the entities they reference. When two facts share an entity
/// but have substantially different statements, flag the one from the
/// lower-confidence source as potentially stale.
///
/// `source_labels`: labels for each facts source (e.g. "code" or "markdown").
/// Must be same length as `facts` or empty (defaults to "unknown").
pub fn detect_staleness(facts: &[FactCandidate], source_labels: &[&str]) -> Vec<StalenessWarning> {
    let sources: Vec<&str> = if source_labels.len() == facts.len() {
        source_labels.to_vec()
    } else {
        vec!["unknown"; facts.len()]
    };

    let mut warnings: Vec<StalenessWarning> = Vec::new();

    // Group by entity
    let mut by_entity: std::collections::BTreeMap<String, Vec<(&FactCandidate, &str)>> =
        std::collections::BTreeMap::new();

    for (i, fact) in facts.iter().enumerate() {
        for entity in &fact.entities {
            by_entity
                .entry(entity.clone())
                .or_default()
                .push((fact, sources[i]));
        }
        // Also index facts with no entity list by first word (heuristic)
        if fact.entities.is_empty() {
            let key = fact
                .statement
                .split_whitespace()
                .next()
                .unwrap_or("unknown")
                .to_string();
            by_entity.entry(key).or_default().push((fact, sources[i]));
        }
    }

    // Compare facts sharing the same entity key
    for pairs in by_entity.values() {
        if pairs.len() < 2 {
            continue;
        }
        for i in 0..pairs.len() {
            for j in (i + 1)..pairs.len() {
                let (a, src_a) = pairs[i];
                let (b, src_b) = pairs[j];

                // Skip if same statement
                let a_text = a.statement.to_lowercase();
                let b_text = b.statement.to_lowercase();
                if a_text == b_text || similarity(&a_text, &b_text) > 0.85 {
                    continue;
                }

                // Higher confidence → more authoritative
                let (authoritative, stale, auth_src, stale_src) = if a.confidence >= b.confidence {
                    (a, b, src_a, src_b)
                } else {
                    (b, a, src_b, src_a)
                };

                // Determine severity
                let severity = if contains_contradiction(&authoritative.statement, &stale.statement)
                {
                    "conflict"
                } else {
                    "divergence"
                };

                // Determine the best entity label
                let entity_label = authoritative
                    .entities
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "unknown".into());

                warnings.push(StalenessWarning {
                    entity: entity_label,
                    authoritative: authoritative.statement.clone(),
                    stale: stale.statement.clone(),
                    authoritative_source: auth_src.to_string(),
                    stale_source: stale_src.to_string(),
                    severity: severity.to_string(),
                });
            }
        }
    }

    warnings
}

/// Simple Jaccard similarity for short texts.
fn similarity(a: &str, b: &str) -> f64 {
    let a_words: std::collections::BTreeSet<&str> = a.split_whitespace().collect();
    let b_words: std::collections::BTreeSet<&str> = b.split_whitespace().collect();
    let intersection = a_words.intersection(&b_words).count();
    let union = a_words.union(&b_words).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// Conservative heuristic: two statements contradict if one negates the other
/// or makes an incompatible claim about the same property.
fn contains_contradiction(a: &str, b: &str) -> bool {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();

    // Negation contradiction: one says "uses X", other says "uses Y" (different tech)
    let negation_markers = ["not ", "no longer", "instead of", "rather than"];
    for marker in &negation_markers {
        if a_lower.contains(marker) && !b_lower.contains(marker) {
            return true;
        }
        if b_lower.contains(marker) && !a_lower.contains(marker) {
            return true;
        }
    }

    // Version/technology disagreement: both mention different versions of same thing
    if a_lower.contains("uses") && b_lower.contains("uses") {
        let a_tech = a_lower.split("uses").nth(1).unwrap_or("").trim();
        let b_tech = b_lower.split("uses").nth(1).unwrap_or("").trim();
        if !a_tech.is_empty() && !b_tech.is_empty() && a_tech != b_tech {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Evidence;

    fn make_fact(statement: &str, entities: &[&str], confidence: f32) -> FactCandidate {
        FactCandidate {
            snippet: None,
            statement: statement.to_string(),
            entities: entities.iter().map(|s| s.to_string()).collect(),
            confidence,
            evidence: Evidence::default(),
        }
    }

    #[test]
    fn detects_staleness_when_facts_differ() {
        let facts = vec![
            make_fact(
                "The system uses MVCC for isolation",
                &["TransactionEngine"],
                0.85,
            ),
            make_fact(
                "The system uses HNSW for isolation",
                &["TransactionEngine"],
                0.6,
            ),
        ];
        let sources = &["code", "markdown"];
        let warnings = detect_staleness(&facts, sources);
        assert!(!warnings.is_empty(), "should detect differing facts");
        let w = &warnings[0];
        assert_eq!(w.authoritative_source, "code");
        assert_eq!(w.stale_source, "markdown");
    }

    #[test]
    fn identical_facts_produce_no_warning() {
        let facts = vec![
            make_fact("The system uses MVCC", &["Engine"], 0.8),
            make_fact("The system uses MVCC", &["Engine"], 0.7),
        ];
        let warnings = detect_staleness(&facts, &["code", "docs"]);
        assert!(warnings.is_empty(), "identical facts should not warn");
    }

    #[test]
    fn different_same_entity_facts_flag_divergence() {
        let facts = vec![
            make_fact(
                "The system uses MVCC for transaction isolation",
                &["Engine"],
                0.85,
            ),
            make_fact("MVCC is used for isolation", &["Engine"], 0.6),
        ];
        let warnings = detect_staleness(&facts, &["code", "docs"]);
        assert!(
            !warnings.is_empty(),
            "different facts about same entity should flag divergence"
        );
        assert_eq!(warnings[0].severity, "divergence");
    }

    #[test]
    fn unrelated_facts_produce_no_warning() {
        let facts = vec![
            make_fact("The system uses MVCC", &["Engine"], 0.8),
            make_fact("The team meets on Fridays", &["Team"], 0.7),
        ];
        let warnings = detect_staleness(&facts, &["code", "docs"]);
        assert!(warnings.is_empty(), "unrelated facts should not warn");
    }
}
