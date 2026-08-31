//! D6: Entity Resolution — link document entities to existing Knowledge Objects.
//!
//! Consumes `KnowledgeIr` (D4) entities and matches them against a knowledge
//! base of existing KOs. Produces resolution candidates with confidence scores
//! and evidence aggregation. Unmatched entities are flagged for creation.
//!
//! # Architecture
//! - `KnowledgeBaseEntry` — lightweight reference to an existing KO
//! - `ResolutionCandidate` — a document entity matched to one or more KOs
//! - `ResolutionResult` — container: matched, ambiguous, unmatched entities
//! - `EntityResolver` trait — pluggable resolution strategy
//! - `MockEntityResolver` — name-similarity + type-based matching

use crate::ir::{EntityCandidate, Evidence, KnowledgeIr};

// ---------------------------------------------------------------------------
// Knowledge base reference
// ---------------------------------------------------------------------------

/// A lightweight reference to an existing Knowledge Object for resolution.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeBaseEntry {
    /// KOID of the existing object.
    pub koid: String,
    /// Display name or primary label property.
    pub name: String,
    /// Ontology type (e.g. "Organization", "Person", "Invoice").
    pub type_name: String,
    /// Known aliases or alternate names.
    pub aliases: Vec<String>,
    /// Additional properties for matching context.
    pub properties: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Resolution types
// ---------------------------------------------------------------------------

/// A document entity matched to one or more knowledge base entries.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResolutionCandidate {
    /// The original document entity name.
    pub entity_name: String,
    /// The ontology class of the entity.
    pub entity_type: Option<String>,
    /// Best-match knowledge base entry (if any).
    pub matched_koid: Option<String>,
    /// All candidate matches with scores.
    pub candidates: Vec<MatchScore>,
    /// Whether this entity needs a new KO created.
    pub needs_creation: bool,
    /// Resolution confidence (0.0–1.0).
    pub confidence: f32,
    /// Evidence from the document supporting this resolution.
    pub evidence: Vec<Evidence>,
}

/// A scored match between a document entity and a knowledge base entry.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MatchScore {
    /// KOID of the matched entry.
    pub koid: String,
    /// Name of the matched entry.
    pub name: String,
    /// Match score 0.0–1.0 (1.0 = exact match).
    pub score: f32,
    /// Match method used (e.g. "exact_name", "fuzzy_name", "type_match", "alias").
    pub method: String,
}

/// Result of entity resolution across a document's Knowledge IR.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ResolutionResult {
    /// Entities successfully matched to existing KOs.
    pub matched: Vec<ResolutionCandidate>,
    /// Entities with multiple plausible matches (needs human review).
    pub ambiguous: Vec<ResolutionCandidate>,
    /// Entities with no match found (candidates for creation).
    pub unmatched: Vec<ResolutionCandidate>,
    /// Overall resolution statistics.
    pub stats: ResolutionStats,
}

/// Statistics for the resolution pass.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ResolutionStats {
    pub total_entities: usize,
    pub matched_count: usize,
    pub ambiguous_count: usize,
    pub unmatched_count: usize,
    pub average_confidence: f32,
}

// ---------------------------------------------------------------------------
// EntityResolver trait
// ---------------------------------------------------------------------------

/// Pluggable entity resolution: document entities → existing KOs.
///
/// Implementations range from simple name matching (mock) to embedding-based
/// similarity search using the vector engine.
pub trait EntityResolver: Send + Sync {
    /// Human-readable name (e.g. "mock", "vector-similarity").
    fn name(&self) -> &str;

    /// Resolve document entities against a knowledge base.
    fn resolve(
        &self,
        entities: &[EntityCandidate],
        knowledge_base: &[KnowledgeBaseEntry],
    ) -> ResolutionResult;
}

// ---------------------------------------------------------------------------
// Mock entity resolver — name-similarity matching
// ---------------------------------------------------------------------------

/// Matches document entities to knowledge base entries using name similarity.
///
/// Strategy:
/// - **Exact name match** → score 1.0, immediate resolution.
/// - **Case-insensitive match** → score 0.95.
/// - **Alias match** → score 0.9.
/// - **Substring / token overlap** → score proportional to overlap ratio.
/// - **Type filtering**: only matches entities with the same type_name if available.
/// - **Ambiguous**: 2+ candidates within 0.15 score of each other → flagged for review.
/// - **Unmatched**: no candidate above threshold → flagged for creation.
pub struct MockEntityResolver {
    /// Minimum score to consider a match (default 0.5).
    pub min_score: f32,
    /// Score difference below which matches are ambiguous (default 0.15).
    pub ambiguity_threshold: f32,
}

impl Default for MockEntityResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl MockEntityResolver {
    pub fn new() -> Self {
        MockEntityResolver {
            min_score: 0.5,
            ambiguity_threshold: 0.15,
        }
    }

    pub fn with_thresholds(min_score: f32, ambiguity_threshold: f32) -> Self {
        MockEntityResolver {
            min_score,
            ambiguity_threshold,
        }
    }
}

impl EntityResolver for MockEntityResolver {
    fn name(&self) -> &str {
        "mock"
    }

    fn resolve(
        &self,
        entities: &[EntityCandidate],
        knowledge_base: &[KnowledgeBaseEntry],
    ) -> ResolutionResult {
        let mut matched = Vec::new();
        let mut ambiguous = Vec::new();
        let mut unmatched = Vec::new();

        for entity in entities {
            let mut candidates: Vec<MatchScore> = Vec::new();

            for entry in knowledge_base {
                // Type filter: if both have types, they must match.
                if let (Some(ref etype), etype_kb) = (&entity.type_hint, &entry.type_name) {
                    if !etype_kb.is_empty() && !types_match(etype, etype_kb) {
                        continue;
                    }
                }

                // Compute best match score across name + aliases.
                let score = best_name_score(&entity.name, &entry.name, &entry.aliases);
                if score >= self.min_score {
                    candidates.push(MatchScore {
                        koid: entry.koid.clone(),
                        name: entry.name.clone(),
                        score,
                        method: match_method(&entity.name, &entry.name, &entry.aliases, score),
                    });
                }
            }

            // Sort by score descending.
            candidates.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            if candidates.is_empty() {
                unmatched.push(ResolutionCandidate {
                    entity_name: entity.name.clone(),
                    entity_type: entity.type_hint.clone(),
                    matched_koid: None,
                    candidates: vec![],
                    needs_creation: true,
                    confidence: 0.0,
                    evidence: vec![entity.evidence.clone()],
                });
            } else {
                let best_score = candidates[0].score;
                let best_koid = candidates[0].koid.clone();
                let is_ambig = is_ambiguous(&candidates, self.ambiguity_threshold);
                let rc = ResolutionCandidate {
                    entity_name: entity.name.clone(),
                    entity_type: entity.type_hint.clone(),
                    matched_koid: Some(best_koid),
                    candidates,
                    needs_creation: false,
                    confidence: best_score,
                    evidence: vec![entity.evidence.clone()],
                };
                if is_ambig {
                    ambiguous.push(rc);
                } else {
                    matched.push(rc);
                }
            }
        }

        let total = entities.len();
        let m = matched.len();
        let a = ambiguous.len();
        let u = unmatched.len();
        let avg_conf = if m + a > 0 {
            let sum: f32 = matched.iter().map(|r| r.confidence).sum::<f32>()
                + ambiguous.iter().map(|r| r.confidence).sum::<f32>();
            sum / (m + a) as f32
        } else {
            0.0
        };

        ResolutionResult {
            matched,
            ambiguous,
            unmatched,
            stats: ResolutionStats {
                total_entities: total,
                matched_count: m,
                ambiguous_count: a,
                unmatched_count: u,
                average_confidence: avg_conf,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Name matching helpers
// ---------------------------------------------------------------------------

/// Compute the best name match score across primary name and aliases.
fn best_name_score(entity_name: &str, entry_name: &str, aliases: &[String]) -> f32 {
    let mut best = name_similarity(entity_name, entry_name);
    for alias in aliases {
        let s = name_similarity(entity_name, alias);
        if s > best {
            best = s;
        }
    }
    best
}

/// Score similarity between two names (0.0–1.0).
fn name_similarity(a: &str, b: &str) -> f32 {
    // Exact match.
    if a == b {
        return 1.0;
    }
    // Case-insensitive exact match.
    if a.to_lowercase() == b.to_lowercase() {
        return 0.95;
    }
    // One is a substring of the other.
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    if a_lower.contains(&b_lower) || b_lower.contains(&a_lower) {
        let shorter = a_lower.len().min(b_lower.len()) as f32;
        let longer = a_lower.len().max(b_lower.len()) as f32;
        return 0.8 * (shorter / longer) + 0.1;
    }
    // Token overlap (Jaccard-like).
    let tokens_a: Vec<&str> = a_lower.split_whitespace().collect();
    let tokens_b: Vec<&str> = b_lower.split_whitespace().collect();
    if tokens_a.len() >= 2 && tokens_b.len() >= 2 {
        let mut overlap = 0;
        for ta in &tokens_a {
            if tokens_b.contains(ta) {
                overlap += 1;
            }
        }
        let union = tokens_a.len() + tokens_b.len() - overlap;
        if union > 0 && overlap > 0 {
            return 0.7 * (overlap as f32 / union as f32);
        }
    }
    // Single token overlap (one side).
    if tokens_a.len() == 1 || tokens_b.len() == 1 {
        for ta in &tokens_a {
            if tokens_b.contains(ta) {
                return 0.5;
            }
        }
    }
    0.0
}

/// Determine match method label based on what matched.
fn match_method(entity_name: &str, entry_name: &str, aliases: &[String], score: f32) -> String {
    if entity_name == entry_name {
        "exact_name".into()
    } else if entity_name.to_lowercase() == entry_name.to_lowercase() {
        "case_insensitive".into()
    } else if aliases.iter().any(|a| a == entity_name) {
        "alias_exact".into()
    } else if aliases
        .iter()
        .any(|a| a.to_lowercase() == entity_name.to_lowercase())
    {
        "alias_case_insensitive".into()
    } else if score >= 0.8 {
        "substring".into()
    } else if score >= 0.5 {
        "token_overlap".into()
    } else {
        "fuzzy".into()
    }
}

/// Check if two type names are a match (exact or compatible).
fn types_match(entity_type: &str, kb_type: &str) -> bool {
    let el = entity_type.to_lowercase();
    let kl = kb_type.to_lowercase();
    el == kl
        || el.contains(&kl)
        || kl.contains(&el)
        // Common aliases.
        || (el == "company" && kl == "organization")
        || (el == "organization" && kl == "company")
        || (el == "person" && kl == "individual")
        || (el == "individual" && kl == "person")
        || (el == "location" && kl == "place")
        || (el == "place" && kl == "location")
}

/// Determine if the top candidates are ambiguous (scores too close).
fn is_ambiguous(candidates: &[MatchScore], threshold: f32) -> bool {
    if candidates.len() < 2 {
        return false;
    }
    let best = candidates[0].score;
    let second = candidates[1].score;
    // Ambiguous if second-best is close to best.
    (best - second).abs() < threshold
}

// ---------------------------------------------------------------------------
// Vector entity resolver — embedding-based similarity
// ---------------------------------------------------------------------------

use crate::embedding::{cosine_similarity, EmbeddingProvider};
use std::sync::Arc;

/// Matches document entities using embedding cosine similarity.
///
/// Pre-computes embeddings for all knowledge base entries at construction time
/// and matches document entities via cosine similarity between their embeddings.
/// Falls back to name-based scoring for edge cases (very short names, etc.).
///
/// This is more robust than `MockEntityResolver` for:
/// - Abbreviations ("Acme Corp" vs "Acme Corporation")
/// - Spelling variations ("John Smith" vs "John Smyth")
/// - Multi-word reordering ("Corporation Acme" vs "Acme Corporation")
pub struct VectorEntityResolver {
    provider: Arc<dyn EmbeddingProvider>,
    /// Pre-computed embeddings for each KB entry.
    kb_embeddings: Vec<(KnowledgeBaseEntry, Vec<f32>)>,
    /// Minimum cosine similarity to consider a match (default 0.4).
    pub min_similarity: f32,
    /// Similarity difference below which matches are ambiguous (default 0.1).
    pub ambiguity_threshold: f32,
}

impl VectorEntityResolver {
    /// Build a resolver with pre-computed KB embeddings.
    pub fn new(
        provider: Arc<dyn EmbeddingProvider>,
        knowledge_base: &[KnowledgeBaseEntry],
    ) -> Self {
        let kb_embeddings: Vec<(KnowledgeBaseEntry, Vec<f32>)> = knowledge_base
            .iter()
            .map(|entry| {
                let embedding = provider.embed(&entry.name);
                (entry.clone(), embedding)
            })
            .collect();
        VectorEntityResolver {
            provider,
            kb_embeddings,
            min_similarity: 0.4,
            ambiguity_threshold: 0.1,
        }
    }

    /// Rebuild the KB embeddings from an updated knowledge base.
    pub fn rebuild(&mut self, knowledge_base: &[KnowledgeBaseEntry]) {
        self.kb_embeddings = knowledge_base
            .iter()
            .map(|entry| {
                let embedding = self.provider.embed(&entry.name);
                (entry.clone(), embedding)
            })
            .collect();
    }

    pub fn with_thresholds(mut self, min_similarity: f32, ambiguity_threshold: f32) -> Self {
        self.min_similarity = min_similarity;
        self.ambiguity_threshold = ambiguity_threshold;
        self
    }
}

impl EntityResolver for VectorEntityResolver {
    fn name(&self) -> &str {
        "vector-similarity"
    }

    fn resolve(
        &self,
        entities: &[EntityCandidate],
        _knowledge_base: &[KnowledgeBaseEntry],
    ) -> ResolutionResult {
        let mut matched = Vec::new();
        let mut ambiguous = Vec::new();
        let mut unmatched = Vec::new();

        for entity in entities {
            let entity_embedding = self.provider.embed(&entity.name);
            let entity_norm: f32 = entity_embedding.iter().map(|x| x * x).sum::<f32>().sqrt();

            // Zero vector → can't match by embedding, fall through to unmatched.
            if entity_norm == 0.0 {
                unmatched.push(ResolutionCandidate {
                    entity_name: entity.name.clone(),
                    entity_type: entity.type_hint.clone(),
                    matched_koid: None,
                    candidates: vec![],
                    needs_creation: true,
                    confidence: 0.0,
                    evidence: vec![entity.evidence.clone()],
                });
                continue;
            }

            let mut candidates: Vec<MatchScore> = Vec::new();

            for (entry, kb_emb) in &self.kb_embeddings {
                // Type filter.
                if let (Some(ref etype), etype_kb) = (&entity.type_hint, &entry.type_name) {
                    if !etype_kb.is_empty() && !types_match(etype, etype_kb) {
                        continue;
                    }
                }

                let cos_sim = cosine_similarity(&entity_embedding, kb_emb);
                // Blend with name-based score for robustness.
                let name_score = best_name_score(&entity.name, &entry.name, &entry.aliases);
                // Weighted combination: 70% embedding, 30% name matching.
                let combined = cos_sim * 0.7 + name_score * 0.3;

                if combined >= self.min_similarity {
                    candidates.push(MatchScore {
                        koid: entry.koid.clone(),
                        name: entry.name.clone(),
                        score: combined,
                        method: if cos_sim > 0.8 {
                            "embedding_strong".into()
                        } else if name_score > 0.8 {
                            "name_boosted".into()
                        } else {
                            "embedding_match".into()
                        },
                    });
                }
            }

            // Sort by score descending.
            candidates.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            if candidates.is_empty() {
                unmatched.push(ResolutionCandidate {
                    entity_name: entity.name.clone(),
                    entity_type: entity.type_hint.clone(),
                    matched_koid: None,
                    candidates: vec![],
                    needs_creation: true,
                    confidence: 0.0,
                    evidence: vec![entity.evidence.clone()],
                });
            } else {
                let best_score = candidates[0].score;
                let best_koid = candidates[0].koid.clone();
                let is_ambig = is_ambiguous(&candidates, self.ambiguity_threshold);
                let rc = ResolutionCandidate {
                    entity_name: entity.name.clone(),
                    entity_type: entity.type_hint.clone(),
                    matched_koid: Some(best_koid),
                    candidates,
                    needs_creation: false,
                    confidence: best_score,
                    evidence: vec![entity.evidence.clone()],
                };
                if is_ambig {
                    ambiguous.push(rc);
                } else {
                    matched.push(rc);
                }
            }
        }

        let total = entities.len();
        let m = matched.len();
        let a = ambiguous.len();
        let u = unmatched.len();
        let avg_conf = if m + a > 0 {
            let sum: f32 = matched.iter().map(|r| r.confidence).sum::<f32>()
                + ambiguous.iter().map(|r| r.confidence).sum::<f32>();
            sum / (m + a) as f32
        } else {
            0.0
        };

        ResolutionResult {
            matched,
            ambiguous,
            unmatched,
            stats: ResolutionStats {
                total_entities: total,
                matched_count: m,
                ambiguous_count: a,
                unmatched_count: u,
                average_confidence: avg_conf,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience: full D4→D6 pipeline
// ---------------------------------------------------------------------------

/// Resolve entities from a KnowledgeIr against a knowledge base.
pub fn resolve_entities(
    ir: &KnowledgeIr,
    resolver: &dyn EntityResolver,
    knowledge_base: &[KnowledgeBaseEntry],
) -> ResolutionResult {
    resolver.resolve(&ir.entities, knowledge_base)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::EntityCandidate;

    fn evidence() -> Evidence {
        Evidence {
            document_id: Some("doc.pdf".into()),
            page: Some(1),
            source: None,
            extractor: "mock".into(),
            model: Some("mock-v1".into()),
            confidence: 0.85,
        }
    }

    fn sample_kb() -> Vec<KnowledgeBaseEntry> {
        vec![
            KnowledgeBaseEntry {
                koid: "KO-001".into(),
                name: "Acme Corporation".into(),
                type_name: "Organization".into(),
                aliases: vec!["Acme Corp".into(), "Acme".into()],
                properties: vec![("industry".into(), "Technology".into())],
            },
            KnowledgeBaseEntry {
                koid: "KO-002".into(),
                name: "Globex Industries".into(),
                type_name: "Organization".into(),
                aliases: vec!["Globex".into()],
                properties: vec![("industry".into(), "Manufacturing".into())],
            },
            KnowledgeBaseEntry {
                koid: "KO-003".into(),
                name: "John Smith".into(),
                type_name: "Person".into(),
                aliases: vec!["John A. Smith".into()],
                properties: vec![("role".into(), "CEO".into())],
            },
            KnowledgeBaseEntry {
                koid: "KO-004".into(),
                name: "Jane Doe".into(),
                type_name: "Person".into(),
                aliases: vec![],
                properties: vec![("role".into(), "CTO".into())],
            },
            KnowledgeBaseEntry {
                koid: "KO-005".into(),
                name: "New York Office".into(),
                type_name: "Location".into(),
                aliases: vec!["NYC Office".into()],
                properties: vec![("city".into(), "New York".into())],
            },
        ]
    }

    // ── Exact match ──

    #[test]
    fn exact_name_match_resolves_entity() {
        let entities = vec![EntityCandidate {
            name: "Acme Corporation".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corporation".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.matched.len(), 1);
        assert_eq!(result.unmatched.len(), 0);
        assert_eq!(result.ambiguous.len(), 0);

        let m = &result.matched[0];
        assert_eq!(m.matched_koid.as_deref(), Some("KO-001"));
        assert!((m.confidence - 1.0).abs() < 0.001);
        assert!(!m.needs_creation);
    }

    // ── Alias match ──

    #[test]
    fn alias_match_resolves_entity() {
        let entities = vec![EntityCandidate {
            name: "Acme Corp".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corp".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.matched.len(), 1);
        let m = &result.matched[0];
        assert_eq!(m.matched_koid.as_deref(), Some("KO-001"));
        assert!(m.confidence >= 0.9); // Alias exact match.
    }

    // ── Case-insensitive match ──

    #[test]
    fn case_insensitive_match() {
        let entities = vec![EntityCandidate {
            name: "acme corporation".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["acme corporation".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.matched.len(), 1);
        assert!((result.matched[0].confidence - 0.95).abs() < 0.001);
    }

    // ── Substring match ──

    #[test]
    fn substring_match() {
        let entities = vec![EntityCandidate {
            name: "Acme".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.matched.len(), 1);
        let m = &result.matched[0];
        assert_eq!(m.matched_koid.as_deref(), Some("KO-001"));
        // "Acme" is substring of "Acme Corporation" → score ~0.8*(4/16)+0.1 ≈ 0.3
        // But also matches alias "Acme" exactly → score 1.0
        assert!(m.confidence >= 0.9);
    }

    // ── Type filtering ──

    #[test]
    fn type_mismatch_prevents_match() {
        let entities = vec![EntityCandidate {
            name: "John Smith".into(),
            type_hint: Some("Organization".into()), // Wrong type.
            mentions: vec!["John Smith".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        // "John Smith" exists as Person, but we said Organization → no match.
        assert_eq!(result.unmatched.len(), 1);
        assert!(result.unmatched[0].needs_creation);
    }

    #[test]
    fn compatible_types_match() {
        let entities = vec![EntityCandidate {
            name: "Acme Corporation".into(),
            type_hint: Some("Company".into()), // "Company" is compatible with "Organization".
            mentions: vec!["Acme Corporation".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.matched.len(), 1);
    }

    // ── Unmatched entities ──

    #[test]
    fn unknown_entity_flagged_for_creation() {
        let entities = vec![EntityCandidate {
            name: "Unknown Entity".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Unknown Entity".into()],
            confidence: 0.5,
            evidence: evidence(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.unmatched.len(), 1);
        assert!(result.unmatched[0].needs_creation);
        assert_eq!(result.unmatched[0].matched_koid, None);
    }

    // ── Ambiguous matches ──

    #[test]
    fn close_scores_produce_ambiguous_result() {
        let kb = vec![
            KnowledgeBaseEntry {
                koid: "KO-A".into(),
                name: "Acme Corporation".into(),
                type_name: "Organization".into(),
                aliases: vec![],
                properties: vec![],
            },
            KnowledgeBaseEntry {
                koid: "KO-B".into(),
                name: "Acme Corp LLC".into(),
                type_name: "Organization".into(),
                aliases: vec![],
                properties: vec![],
            },
        ];

        let entities = vec![EntityCandidate {
            name: "Acme Corp".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corp".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &kb);

        // "Acme Corp" vs "Acme Corporation" and "Acme Corp LLC" — both substring
        // matches with similar scores → ambiguous.
        assert_eq!(result.ambiguous.len(), 1);
    }

    // ── Multiple entities ──

    #[test]
    fn resolves_multiple_entities() {
        let entities = vec![
            EntityCandidate {
                name: "Acme Corporation".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Acme Corporation".into()],
                confidence: 0.9,
                evidence: evidence(),
            },
            EntityCandidate {
                name: "John Smith".into(),
                type_hint: Some("Person".into()),
                mentions: vec!["John Smith".into()],
                confidence: 0.8,
                evidence: evidence(),
            },
            EntityCandidate {
                name: "Unknown Co".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Unknown Co".into()],
                confidence: 0.5,
                evidence: evidence(),
            },
        ];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.matched.len(), 2);
        assert_eq!(result.unmatched.len(), 1);
        assert_eq!(result.stats.total_entities, 3);
        assert_eq!(result.stats.matched_count, 2);
        assert_eq!(result.stats.unmatched_count, 1);
    }

    // ── Statistics ──

    #[test]
    fn stats_computed_correctly() {
        let entities = vec![
            EntityCandidate {
                name: "Acme Corporation".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Acme Corporation".into()],
                confidence: 0.9,
                evidence: evidence(),
            },
            EntityCandidate {
                name: "Jane Doe".into(),
                type_hint: Some("Person".into()),
                mentions: vec!["Jane Doe".into()],
                confidence: 0.8,
                evidence: evidence(),
            },
        ];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.stats.total_entities, 2);
        assert_eq!(result.stats.matched_count, 2);
        assert!(result.stats.average_confidence > 0.0);
    }

    // ── KnowledgeBaseEntry ──

    #[test]
    fn knowledge_base_entry_serde_roundtrip() {
        let entry = KnowledgeBaseEntry {
            koid: "KO-001".into(),
            name: "Test Entity".into(),
            type_name: "Organization".into(),
            aliases: vec!["Test".into()],
            properties: vec![("key".into(), "value".into())],
        };

        let json = serde_json::to_string(&entry).unwrap();
        let back: KnowledgeBaseEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.koid, "KO-001");
        assert_eq!(back.name, "Test Entity");
        assert_eq!(back.aliases, vec!["Test"]);
    }

    // ── ResolutionResult ──

    #[test]
    fn resolution_result_serde_roundtrip() {
        let result = ResolutionResult {
            matched: vec![],
            ambiguous: vec![],
            unmatched: vec![ResolutionCandidate {
                entity_name: "New Entity".into(),
                entity_type: Some("Organization".into()),
                matched_koid: None,
                candidates: vec![],
                needs_creation: true,
                confidence: 0.0,
                evidence: vec![evidence()],
            }],
            stats: ResolutionStats {
                total_entities: 1,
                matched_count: 0,
                ambiguous_count: 0,
                unmatched_count: 1,
                average_confidence: 0.0,
            },
        };

        let json = serde_json::to_string(&result).unwrap();
        let back: ResolutionResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.stats.total_entities, 1);
        assert_eq!(back.unmatched.len(), 1);
        assert!(back.unmatched[0].needs_creation);
    }

    // ── EntityResolver trait ──

    #[test]
    fn mock_implements_entity_resolver_trait() {
        let resolver: &dyn EntityResolver = &MockEntityResolver::new();
        assert_eq!(resolver.name(), "mock");

        let entities = vec![EntityCandidate {
            name: "Acme Corporation".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corporation".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let result = resolver.resolve(&entities, &sample_kb());
        assert_eq!(result.matched.len(), 1);
    }

    // ── Edge cases ──

    #[test]
    fn empty_entities_produces_empty_result() {
        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&[], &sample_kb());
        assert!(result.matched.is_empty());
        assert_eq!(result.stats.total_entities, 0);
    }

    #[test]
    fn empty_knowledge_base_all_unmatched() {
        let entities = vec![EntityCandidate {
            name: "Acme Corporation".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corporation".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &[]);

        assert_eq!(result.unmatched.len(), 1);
        assert!(result.unmatched[0].needs_creation);
    }

    #[test]
    fn custom_threshold_filters_matches() {
        let entities = vec![EntityCandidate {
            name: "Acme".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        // With high threshold, even substring matches might not pass.
        let resolver = MockEntityResolver::with_thresholds(0.95, 0.1);
        let result = resolver.resolve(&entities, &sample_kb());

        // "Acme" matches alias "Acme" exactly → score 1.0, passes 0.95 threshold.
        assert_eq!(result.matched.len(), 1);
    }

    #[test]
    fn confidence_aggregated_from_entity_evidence() {
        let ev = Evidence {
            document_id: Some("doc.pdf".into()),
            page: Some(3),
            source: Some(crate::source::EvidenceSource::Region {
                bbox: crate::ast::BoundingBox {
                    page: 3,
                    x: 10.0,
                    y: 20.0,
                    width: 100.0,
                    height: 30.0,
                },
            }),
            extractor: "mock".into(),
            model: Some("mock-v1".into()),
            confidence: 0.85,
        };

        let entities = vec![EntityCandidate {
            name: "Acme Corporation".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corporation".into()],
            confidence: 0.85,
            evidence: ev.clone(),
        }];

        let resolver = MockEntityResolver::new();
        let result = resolver.resolve(&entities, &sample_kb());

        let m = &result.matched[0];
        assert_eq!(m.evidence[0].page, Some(3));
        assert_eq!(
            m.evidence[0].source,
            Some(crate::source::EvidenceSource::Region {
                bbox: crate::ast::BoundingBox {
                    page: 3,
                    x: 10.0,
                    y: 20.0,
                    width: 100.0,
                    height: 30.0,
                },
            })
        );
    }

    // ── VectorEntityResolver tests ──

    use crate::embedding::MockEmbeddingProvider;
    use std::sync::Arc;

    fn vector_resolver() -> VectorEntityResolver {
        let provider = Arc::new(MockEmbeddingProvider::new());
        VectorEntityResolver::new(provider, &sample_kb())
    }

    #[test]
    fn vector_resolver_exact_name_match() {
        let entities = vec![EntityCandidate {
            name: "Acme Corporation".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corporation".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = vector_resolver();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.matched.len(), 1);
        let m = &result.matched[0];
        assert_eq!(m.matched_koid.as_deref(), Some("KO-001"));
        // Combined embedding + name score for exact match should be very high.
        assert!(m.confidence > 0.9, "expected > 0.9, got {}", m.confidence);
    }

    #[test]
    fn vector_resolver_similar_name_match() {
        let entities = vec![EntityCandidate {
            name: "Acme Corp".into(), // Alias in KB.
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corp".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = vector_resolver();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.matched.len(), 1);
        let m = &result.matched[0];
        assert_eq!(m.matched_koid.as_deref(), Some("KO-001"));
        // "Acme Corp" matches alias exactly → name_score=0.9, embedding also high → combined high.
        assert!(m.confidence > 0.7, "expected > 0.7, got {}", m.confidence);
    }

    #[test]
    fn vector_resolver_type_filtering() {
        let entities = vec![EntityCandidate {
            name: "John Smith".into(),
            type_hint: Some("Organization".into()), // Wrong type.
            mentions: vec!["John Smith".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let resolver = vector_resolver();
        let result = resolver.resolve(&entities, &sample_kb());

        // Name match is strong but type mismatch blocks it.
        assert_eq!(result.unmatched.len(), 1);
        assert!(result.unmatched[0].needs_creation);
    }

    #[test]
    fn vector_resolver_unknown_entity() {
        let entities = vec![EntityCandidate {
            name: "Quantum Flux Dynamics".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Quantum Flux Dynamics".into()],
            confidence: 0.5,
            evidence: evidence(),
        }];

        let resolver = vector_resolver();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.unmatched.len(), 1);
        assert!(result.unmatched[0].needs_creation);
    }

    #[test]
    fn vector_resolver_handles_multiple_entities() {
        let entities = vec![
            EntityCandidate {
                name: "Acme Corporation".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Acme Corporation".into()],
                confidence: 0.9,
                evidence: evidence(),
            },
            EntityCandidate {
                name: "Jane Doe".into(),
                type_hint: Some("Person".into()),
                mentions: vec!["Jane Doe".into()],
                confidence: 0.8,
                evidence: evidence(),
            },
            EntityCandidate {
                name: "Unknown Entity".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Unknown Entity".into()],
                confidence: 0.3,
                evidence: evidence(),
            },
        ];

        let resolver = vector_resolver();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.matched.len(), 2);
        assert_eq!(result.unmatched.len(), 1);
        assert_eq!(result.stats.total_entities, 3);
    }

    #[test]
    fn vector_resolver_implements_entity_resolver_trait() {
        let provider = Arc::new(MockEmbeddingProvider::new());
        let resolver: &dyn EntityResolver = &VectorEntityResolver::new(provider, &sample_kb());
        assert_eq!(resolver.name(), "vector-similarity");

        let entities = vec![EntityCandidate {
            name: "Acme Corporation".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corporation".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];

        let result = resolver.resolve(&entities, &sample_kb());
        assert_eq!(result.matched.len(), 1);
    }

    #[test]
    fn vector_resolver_rebuild_kb() {
        let provider = Arc::new(MockEmbeddingProvider::new());
        let mut resolver = VectorEntityResolver::new(provider, &[]);

        // Empty KB → all unmatched.
        let entities = vec![EntityCandidate {
            name: "Acme Corporation".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corporation".into()],
            confidence: 0.9,
            evidence: evidence(),
        }];
        let result = resolver.resolve(&entities, &[]);
        assert_eq!(result.unmatched.len(), 1);

        // Rebuild with sample KB → should match.
        resolver.rebuild(&sample_kb());
        let result2 = resolver.resolve(&entities, &sample_kb());
        assert_eq!(result2.matched.len(), 1);
    }

    #[test]
    fn vector_resolver_empty_entity_name_unmatched() {
        let entities = vec![EntityCandidate {
            name: "".into(),
            type_hint: None,
            mentions: vec![],
            confidence: 0.0,
            evidence: evidence(),
        }];

        let resolver = vector_resolver();
        let result = resolver.resolve(&entities, &sample_kb());

        assert_eq!(result.unmatched.len(), 1);
    }
}
