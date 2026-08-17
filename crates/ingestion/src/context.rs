//! MRFC-0070 Phase A5: Context Compiler — THE KILLER FEATURE.
//!
//! Given a task description and a merged KnowledgeIr, compiles the minimum
//! sufficient context package under a token budget.
//!
//! Pipeline: Score → Rank → Pack → Trim.

use crate::ir::KnowledgeIr;

/// A context package ready for agent consumption.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ContextPackage {
    /// Entities most relevant to the task.
    pub entities: Vec<RankedEntity>,
    /// Facts/rules/claims relevant to the task.
    pub facts: Vec<RankedFact>,
    /// Relationships relevant to the task.
    pub relations: Vec<RankedRelation>,
    /// Total estimated tokens in this package.
    pub estimated_tokens: usize,
    /// Whether the package was trimmed to fit the budget.
    pub trimmed: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RankedEntity {
    pub name: String,
    pub type_hint: Option<String>,
    pub score: f32,
    pub mentions: Vec<String>,
    /// Source file this entity was extracted from (None = synthetic).
    #[serde(default)]
    pub document_id: Option<String>,
    /// Why was this entity included in the context?
    pub justification: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RankedFact {
    pub statement: String,
    pub entities: Vec<String>,
    pub score: f32,
    /// Why was this fact included?
    pub justification: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RankedRelation {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub score: f32,
    /// Why was this relation included?
    pub justification: String,
}

/// Compile a context package from merged KnowledgeIr for a task.
///
/// - `task`: natural language description of what the agent needs to do.
/// - `ir`: merged KnowledgeIr from all compilers.
/// - `token_budget`: max tokens for the context package (0 = unlimited).
pub fn compile_context(task: &str, ir: &KnowledgeIr, token_budget: usize) -> ContextPackage {
    compile_context_semantic(task, ir, token_budget, None)
}

/// Compile with optional semantic scores fused into the lexical score.
///
/// `semantic` maps "document_id::name" → cosine similarity between the
/// entity's embedding and the task embedding. Symptom-described tasks
/// ("fix the endpoint resolution bug") share no keywords with entity
/// names, so lexical matching alone misses them entirely.
pub fn compile_context_semantic(
    task: &str,
    ir: &KnowledgeIr,
    token_budget: usize,
    semantic: Option<&HashMap<String, f32>>,
) -> ContextPackage {
    compile_context_semantic_with(
        task,
        ir,
        token_budget,
        semantic,
        SEMANTIC_WEIGHT,
        SEMANTIC_MIN,
        RELATION_BOOST_FACTOR,
    )
}

/// Tunable variant for ranking experiments (see
/// crates/services/api/mcp/examples/probe_rank.rs) — production calls go
/// through [`compile_context_semantic`] with the defaults.
pub fn compile_context_semantic_with(
    task: &str,
    ir: &KnowledgeIr,
    token_budget: usize,
    semantic: Option<&HashMap<String, f32>>,
    semantic_weight: f32,
    semantic_min: f32,
    relation_boost: f32,
) -> ContextPackage {
    let task_lower = task.to_lowercase();
    let task_words: Vec<&str> = task_lower.split_whitespace().collect();

    // Score entities by name overlap + mention overlap + semantic similarity
    let mut entities: Vec<RankedEntity> = ir
        .entities
        .iter()
        .map(|e| {
            let name_score = keyword_score(&e.name.to_lowercase(), &task_words);
            let mut mention_score: f32 = 0.0;
            let mut matched_mentions = Vec::new();
            for mention in &e.mentions {
                let ms = keyword_score(&mention.to_lowercase(), &task_words) * 0.5;
                if ms > 0.0 {
                    matched_mentions.push(mention.clone());
                }
                mention_score += ms;
            }
            let semantic_score = semantic
                .and_then(|m| {
                    m.get(&format!(
                        "{}::{}",
                        e.evidence.document_id.as_deref().unwrap_or_default(),
                        e.name
                    ))
                })
                .copied()
                .unwrap_or(0.0);
            let lexical = name_score + mention_score;
            let mut score = lexical + semantic_score * semantic_weight;
            // Semantic-only matches must clear a floor to enter the package —
            // below it the similarity is noise, not signal.
            if lexical <= 0.0 && semantic_score < semantic_min {
                score = 0.0;
            }
            score *= 1.0 + (e.mentions.len() as f32 * 0.1).min(0.5);
            // Doc-section entities (headings from markdown, never code) are
            // orientation, not implementation — they must not outrank code
            // entities in a coding task and soak the whole budget.
            if PROSE_TYPES.contains(&e.type_hint.as_deref().unwrap_or("")) {
                score *= 0.5;
            }

            let justification = if name_score > 0.0 {
                format!(
                    "name matches task keywords (score: {:.1}), {} mentions",
                    name_score,
                    e.mentions.len()
                )
            } else if !matched_mentions.is_empty() {
                format!(
                    "mentions match task: {}",
                    truncate_chars(matched_mentions.first().unwrap(), 120)
                )
            } else if semantic_score >= semantic_min {
                format!(
                    "semantically relevant to task (cosine {:.2})",
                    semantic_score
                )
            } else {
                format!(
                    "type '{}' has {} mentions",
                    e.type_hint.as_deref().unwrap_or("unknown"),
                    e.mentions.len()
                )
            };

            RankedEntity {
                name: e.name.clone(),
                type_hint: e.type_hint.clone(),
                score,
                mentions: e.mentions.clone(),
                document_id: e.evidence.document_id.clone(),
                justification,
            }
        })
        .collect();

    // Relation-aware boost: when an entity ranks, its direct neighbors
    // (depends_on / tested_by / implements / contains) get a slice of its
    // score, so a fix location follows its ranked anchor into the fold at
    // low token budgets.
    let mut adjacency: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for r in &ir.relations {
        let pred_lower = r.predicate.to_lowercase();
        if !RELATION_PREDICATES.contains(&pred_lower.as_str()) {
            continue;
        }
        if pred_lower == "contains" {
            // Inbound-only: the subject is the container (file contains
            // entity). A ranked child boosts its container; a ranked
            // container must not flood all its children.
            adjacency
                .entry(r.object.as_str())
                .or_default()
                .push((r.predicate.as_str(), r.subject.as_str()));
        } else {
            adjacency
                .entry(r.subject.as_str())
                .or_default()
                .push((r.predicate.as_str(), r.object.as_str()));
            adjacency
                .entry(r.object.as_str())
                .or_default()
                .push((r.predicate.as_str(), r.subject.as_str()));
        }
    }
    let index: HashMap<String, usize> = entities
        .iter()
        .enumerate()
        .map(|(i, e)| (e.name.clone(), i))
        .collect();
    let orig: Vec<f32> = entities.iter().map(|e| e.score).collect();
    let mut boost_src: Vec<Option<(String, String)>> = vec![None; entities.len()];
    for i in 0..entities.len() {
        if orig[i] <= 0.0 {
            continue; // ponytail: no transitivity — boosts don't re-boost
        }
        let anchor_score = entities[i].score;
        let anchor_name = entities[i].name.clone();
        let mut cands: Vec<(usize, &str)> = Vec::new();
        if let Some(edges) = adjacency.get(anchor_name.as_str()) {
            for &(pred, nb) in edges {
                if let Some(&j) = index.get(nb) {
                    cands.push((j, pred));
                }
            }
        }
        // Cap fan-out by the neighbor's own pre-boost score — a fat module's
        // 30 use-targets collapse to its 5 highest-signal neighbors.
        cands.sort_by(|a, b| {
            orig[b.0]
                .partial_cmp(&orig[a.0])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        for (j, pred) in cands.into_iter().take(RELATION_MAX_NEIGHBORS) {
            let boost = anchor_score * relation_boost;
            if boost > entities[j].score {
                entities[j].score = boost;
                boost_src[j] = Some((anchor_name.clone(), pred.to_string()));
            }
        }
    }
    for (i, src) in boost_src.into_iter().enumerate() {
        if let Some((anchor, pred)) = src {
            if orig[i] <= 0.0 {
                entities[i].justification = format!("related to {} via {}", anchor, pred);
            }
        }
    }
    entities.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Score facts by statement overlap with task
    let mut facts: Vec<RankedFact> = ir
        .facts
        .iter()
        .map(|f| {
            let stmt_score = keyword_score(&f.statement.to_lowercase(), &task_words);
            // Boost facts connected to high-scoring entities
            let entity_boost: f32 = f
                .entities
                .iter()
                .map(|en| {
                    entities
                        .iter()
                        .find(|e| e.name == *en)
                        .map(|e| e.score * 0.3)
                        .unwrap_or(0.0)
                })
                .sum();
            let score = stmt_score + entity_boost.min(0.5);

            let justification = if stmt_score > 0.0 {
                format!("statement matches task keywords (score: {:.1})", stmt_score)
            } else if entity_boost > 0.0 {
                format!("connected to relevant entity: {}", f.entities.join(", "))
            } else {
                "general domain knowledge".into()
            };

            RankedFact {
                statement: f.statement.clone(),
                entities: f.entities.clone(),
                score,
                justification,
            }
        })
        .collect();
    facts.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Score relations by subject, predicate, and object overlap with task + entities
    let mut relations: Vec<RankedRelation> = ir
        .relations
        .iter()
        .map(|r| {
            let subj_score = entities
                .iter()
                .find(|e| e.name == r.subject)
                .map(|e| e.score)
                .unwrap_or(0.0);
            let obj_score = entities
                .iter()
                .find(|e| e.name == r.object)
                .map(|e| e.score)
                .unwrap_or(0.0);
            let pred_score = keyword_score(&r.predicate.to_lowercase(), &task_words);
            let score = subj_score.max(obj_score) + pred_score * 0.5;

            let justification = if pred_score > 0.0 {
                format!(
                    "predicate '{}' matches task, connected entities: {} ↔ {}",
                    r.predicate, r.subject, r.object
                )
            } else {
                format!(
                    "connects relevant entities: {} → {} (via {})",
                    r.subject, r.object, r.predicate
                )
            };

            RankedRelation {
                subject: r.subject.clone(),
                predicate: r.predicate.clone(),
                object: r.object.clone(),
                score,
                justification,
            }
        })
        .collect();
    relations.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Pack and trim to token budget
    let unlimited = token_budget == 0;
    let mut pkg = ContextPackage::default();
    let mut tokens = 0usize;

    for e in &entities {
        if e.score <= 0.0 {
            break;
        }
        // Cap mentions at pack time: the first one is the primary doc
        // comment, the second is corroboration; giant section bodies (a
        // whole markdown table in one mention) would dominate the payload.
        let mentions = pack_mentions(&e.mentions);
        let est = est_tokens(&e.name)
            + est_tokens(&e.justification)
            + mentions.iter().map(|m| est_tokens(m)).sum::<usize>();
        if !unlimited && tokens + est > token_budget {
            pkg.trimmed = true;
            break;
        }
        tokens += est;
        pkg.entities.push(RankedEntity {
            mentions,
            ..e.clone()
        });
    }

    for f in &facts {
        if f.score <= 0.0 {
            break;
        }
        let est =
            est_tokens(&f.statement) + f.entities.iter().map(|e| est_tokens(e)).sum::<usize>();
        if !unlimited && tokens + est > token_budget {
            pkg.trimmed = true;
            break;
        }
        tokens += est;
        pkg.facts.push(f.clone());
    }

    for r in &relations {
        if r.score <= 0.0 {
            break;
        }
        let est = est_tokens(&r.subject) + est_tokens(&r.predicate) + est_tokens(&r.object);
        if !unlimited && tokens + est > token_budget {
            pkg.trimmed = true;
            break;
        }
        tokens += est;
        pkg.relations.push(r.clone());
    }

    pkg.estimated_tokens = tokens;
    pkg
}

/// Type hints the markdown section classifier emits for documentation
/// sections — never code. Demoted in ranking so code entities win the
/// budget for coding tasks; they still surface for doc-oriented tasks.
const PROSE_TYPES: &[&str] = &[
    "Project",
    "Database",
    "Architecture",
    "Design",
    "Repository",
    "API",
    "Organization",
];

/// Max mentions kept per entity in a context package, and max chars per
/// mention. Capped at pack time — the IR still holds the full evidence.
const MAX_MENTIONS: usize = 2;
const MAX_MENTION_CHARS: usize = 200;

/// Semantic fusion weight (× cosine) and the minimum cosine a
/// semantically-only matched entity needs to enter the package.
// Semantic fusion weight: cosine × this is added to the lexical score.
// 3.0 is the offline-tuned value from probe_rank sweeps (see
// crates/services/api/mcp/examples/probe_rank.rs): at 2.0 a symptom task's
// exact fix location (doc-less fn, cosine ~0.38) sits below rank 40 of 47;
// at 3.0 semantic proximity visibly reorders without flipping the top fold.
const SEMANTIC_WEIGHT: f32 = 3.0;
// 0.35 (was 0.30): at 0.30 one gibberish task leaked a junk cosine
// ("impl ChaCha20Poly1305", embedding text tokenizes to subword junk, cos
// 0.315 vs "xq9 wm3 blorp zzzq"). Tokenizer-degenerate embeddings wander
// 0.28-0.36 against arbitrary tasks; 0.35 closes the observed band.
// ponytail: an ingest-side lexical stopword filter was considered and
// skipped — the degenerate case is tokenizer-level (camelCase split), not
// lexical, so a word filter would catch a different class. The compile-time
// gate here is the fix, not a stopgap.
const SEMANTIC_MIN: f32 = 0.35;

/// Relation-aware boost: a ranked anchor hands each neighbor max(own score,
/// anchor × this). Fix locations (callees, tests, containing files) ride
/// their anchors into the fold at low token budgets.
/// 0.65 (was 0.5): tuned against the live snapshot — at 0.5 a containing
/// file whose anchor scores ~6.0 still missed the budget-1000 fold when the
/// cutoff sat at ~3.6 (0.5 × 6.0 = 3.0 < 3.6). 0.65 clears that band with
/// margin while keeping control-fold top-5s unchanged.
const RELATION_BOOST_FACTOR: f32 = 0.65;
/// Predicates whose endpoints share ranking credit. `contains` is
/// inbound-only (subject = container), handled in the adjacency build.
const RELATION_PREDICATES: &[&str] = &["depends_on", "tested_by", "implements", "contains"];
/// Max neighbors an anchor boosts, chosen by the neighbor's own pre-boost
/// score (deterministic index tie-break) — a fat module's 30 use-targets
/// collapse to its 5 highest-signal neighbors.
const RELATION_MAX_NEIGHBORS: usize = 5;

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn pack_mentions(mentions: &[String]) -> Vec<String> {
    mentions
        .iter()
        .take(MAX_MENTIONS)
        .map(|m| truncate_chars(m, MAX_MENTION_CHARS))
        .collect()
}

/// Score a text against task keywords. Each exact word match adds 1.0,
/// partial (substring) match adds 0.3.
fn keyword_score(text: &str, task_words: &[&str]) -> f32 {
    let mut score: f32 = 0.0;
    for &word in task_words {
        if word.len() < 3 {
            continue; // skip short words
        }
        if text.contains(word) {
            score += 1.0;
        } else {
            // Partial match: shared prefix ≥4 chars ("truncate"/"truncation").
            // Stopword chunks ("a", "of") share no 4-char prefix, so they stop
            // handing every mention fake lexical credit — which had drowned
            // the semantic-only recall path (lexical was never 0).
            for chunk in text.split_whitespace() {
                let shared = chunk
                    .chars()
                    .zip(word.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                if shared >= 4 {
                    score += 0.3;
                    break;
                }
            }
        }
    }
    score
}

/// Conservative token estimate: 1 token ≈ 4 chars for English text.
fn est_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Render a ContextPackage as a human-readable Markdown string for agent consumption.
pub fn render_context_markdown(pkg: &ContextPackage) -> String {
    let mut md = String::new();

    if !pkg.entities.is_empty() {
        md.push_str("## Relevant Components\n\n");
        for e in &pkg.entities {
            let type_str = e.type_hint.as_deref().unwrap_or("Unknown");
            md.push_str(&format!("- **{}** ({})", e.name, type_str));
            if let Some(doc) = &e.document_id {
                md.push_str(&format!(" [`{}`]", doc));
            }
            if !e.mentions.is_empty() {
                md.push_str(&format!(": {}", e.mentions.first().unwrap()));
            }
            md.push('\n');
        }
        md.push('\n');
    }

    if !pkg.facts.is_empty() {
        md.push_str("## Relevant Facts & Rules\n\n");
        for f in &pkg.facts {
            md.push_str(&format!("- {}\n", f.statement));
        }
        md.push('\n');
    }

    if !pkg.relations.is_empty() {
        md.push_str("## Relevant Relationships\n\n");
        for r in &pkg.relations {
            md.push_str(&format!(
                "- `{}` --[{}]--> `{}`\n",
                r.subject, r.predicate, r.object
            ));
        }
        md.push('\n');
    }

    if pkg.trimmed {
        md.push_str(
            "> ⚠️ Context trimmed to fit token budget. Some relevant items were omitted.\n\n",
        );
    }

    md
}

// ---------------------------------------------------------------------------
// Progressive Context Expansion
// ---------------------------------------------------------------------------

/// Expand context for a specific entity — return ALL its facts, relations,
/// evidence details, and source information.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EntityExpansion {
    pub entity: RankedEntity,
    pub all_facts: Vec<RankedFact>,
    pub all_relations: Vec<RankedRelation>,
    pub evidence_source: Option<String>,
    pub evidence_confidence: f32,
}

/// EXPAND KO: get full details about a specific entity in the context.
pub fn expand_entity(
    entity_name: &str,
    pkg: &ContextPackage,
    ir: &KnowledgeIr,
) -> Option<EntityExpansion> {
    let entity = pkg.entities.iter().find(|e| e.name == entity_name)?.clone();

    let all_facts: Vec<RankedFact> = ir
        .facts
        .iter()
        .filter(|f| f.entities.contains(&entity_name.to_string()))
        .map(|f| RankedFact {
            statement: f.statement.clone(),
            entities: f.entities.clone(),
            score: f.confidence,
            justification: format!("references entity '{}'", entity_name),
        })
        .collect();

    let all_relations: Vec<RankedRelation> = ir
        .relations
        .iter()
        .filter(|r| r.subject == entity_name || r.object == entity_name)
        .map(|r| RankedRelation {
            subject: r.subject.clone(),
            predicate: r.predicate.clone(),
            object: r.object.clone(),
            score: r.confidence,
            justification: format!("connects '{}' with '{}'", r.subject, r.object),
        })
        .collect();

    let src_entity = ir.entities.iter().find(|e| e.name == entity_name);
    let source = src_entity.and_then(|e| e.evidence.document_id.clone());
    let confidence = src_entity.map(|e| e.confidence).unwrap_or(0.0);

    Some(EntityExpansion {
        entity,
        all_facts,
        all_relations,
        evidence_source: source,
        evidence_confidence: confidence,
    })
}

/// EXPAND RELATIONSHIP: get the full chain of relationships connected to entities
/// in the context, tracing transitive dependencies up to a given depth.
pub fn expand_relationship(
    entity_name: &str,
    ir: &KnowledgeIr,
    depth: usize,
) -> Vec<Vec<RankedRelation>> {
    let mut chains = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue: Vec<(String, usize, Vec<RankedRelation>)> =
        vec![(entity_name.to_string(), 0, vec![])];

    while let Some((current, d, chain)) = queue.pop() {
        if d >= depth || visited.contains(&current) {
            if !chain.is_empty() {
                chains.push(chain);
            }
            continue;
        }
        visited.insert(current.clone());

        let neighbors: Vec<_> = ir
            .relations
            .iter()
            .filter(|r| r.subject == current || r.object == current)
            .collect();

        if neighbors.is_empty() && !chain.is_empty() {
            chains.push(chain.clone());
        }

        for rel in &neighbors {
            let next = if rel.subject == current {
                &rel.object
            } else {
                &rel.subject
            };
            let mut new_chain = chain.clone();
            new_chain.push(RankedRelation {
                subject: rel.subject.clone(),
                predicate: rel.predicate.clone(),
                object: rel.object.clone(),
                score: rel.confidence,
                justification: format!("depth {}: {} → {}", d + 1, current, next),
            });
            queue.push((next.clone(), d + 1, new_chain));
        }
    }

    chains
}

/// EXPAND SOURCE: get all entities and facts from a specific evidence source.
pub fn expand_source(source_hint: &str, ir: &KnowledgeIr) -> (Vec<RankedEntity>, Vec<RankedFact>) {
    let entities: Vec<RankedEntity> = ir
        .entities
        .iter()
        .filter(|e| {
            e.evidence
                .document_id
                .as_deref()
                .is_some_and(|s| s.contains(source_hint))
        })
        .map(|e| RankedEntity {
            name: e.name.clone(),
            type_hint: e.type_hint.clone(),
            score: e.confidence,
            mentions: e.mentions.clone(),
            document_id: e.evidence.document_id.clone(),
            justification: format!("from source: {}", source_hint),
        })
        .collect();

    let entity_names: std::collections::HashSet<&str> =
        entities.iter().map(|e| e.name.as_str()).collect();

    let facts: Vec<RankedFact> = ir
        .facts
        .iter()
        .filter(|f| {
            f.entities
                .iter()
                .any(|en| entity_names.contains(en.as_str()))
        })
        .map(|f| RankedFact {
            statement: f.statement.clone(),
            entities: f.entities.clone(),
            score: f.confidence,
            justification: format!("connected to source: {}", source_hint),
        })
        .collect();

    (entities, facts)
}

// ---------------------------------------------------------------------------
// Context Cache
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A cache entry: compiled context + insertion time.
#[derive(Clone, Debug)]
struct CacheEntry {
    pkg: ContextPackage,
    inserted_at: Instant,
    knowledge_hash: u64,
}

/// Simple task→context cache with TTL and invalidation on knowledge change.
static CONTEXT_CACHE: LazyLock<Mutex<HashMap<String, CacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Hash the IR to detect knowledge changes (invalidates cache).
fn ir_fingerprint(ir: &KnowledgeIr) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    ir.entities.len().hash(&mut h);
    ir.facts.len().hash(&mut h);
    ir.relations.len().hash(&mut h);
    for e in &ir.entities {
        e.name.hash(&mut h);
    }
    h.finish()
}

/// Stable fingerprint of a semantic map — HashMap iteration order is random,
/// so sort keys before hashing or the cache key would change per call.
fn semantic_fingerprint(semantic: Option<&HashMap<String, f32>>) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let Some(m) = semantic else {
        return 0;
    };
    let mut h = DefaultHasher::new();
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort();
    for k in keys {
        k.hash(&mut h);
        m[k].to_bits().hash(&mut h);
    }
    h.finish()
}

/// Compile context with caching. Returns None when cache hit,
/// or Some(ContextPackage) on cache miss (caller should use the result).
pub fn compile_context_cached(
    task: &str,
    ir: &KnowledgeIr,
    token_budget: usize,
    ttl_secs: u64,
) -> ContextPackage {
    compile_context_cached_semantic(task, ir, token_budget, ttl_secs, None)
}

/// Cached compile with semantic scores. The cache key includes a fingerprint
/// of the semantic map so a different embedding model (or task embedding)
/// never reuses stale semantic packages.
pub fn compile_context_cached_semantic(
    task: &str,
    ir: &KnowledgeIr,
    token_budget: usize,
    ttl_secs: u64,
    semantic: Option<&HashMap<String, f32>>,
) -> ContextPackage {
    let fp = ir_fingerprint(ir);
    let cache_key = format!(
        "{}:{}:{}:{}",
        task,
        token_budget,
        fp,
        semantic_fingerprint(semantic)
    );

    // Check cache
    {
        let cache = CONTEXT_CACHE.lock().unwrap();
        if let Some(entry) = cache.get(&cache_key) {
            if entry.inserted_at.elapsed() < Duration::from_secs(ttl_secs)
                && entry.knowledge_hash == fp
            {
                return entry.pkg.clone();
            }
        }
    }

    // Cache miss — compile fresh
    let pkg = compile_context_semantic(task, ir, token_budget, semantic);

    // Store in cache
    {
        let mut cache = CONTEXT_CACHE.lock().unwrap();
        // ponytail: simple eviction — drop oldest entry if > 100 entries
        if cache.len() > 100 {
            let oldest_key = cache
                .iter()
                .min_by_key(|(_, v)| v.inserted_at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest_key {
                cache.remove(&k);
            }
        }
        cache.insert(
            cache_key,
            CacheEntry {
                pkg: pkg.clone(),
                inserted_at: Instant::now(),
                knowledge_hash: fp,
            },
        );
    }

    pkg
}

/// Invalidate all cached contexts (call when knowledge changes).
pub fn invalidate_context_cache() {
    let mut cache = CONTEXT_CACHE.lock().unwrap();
    cache.clear();
}

/// Get cache statistics.
pub fn context_cache_stats() -> (usize, u64) {
    let cache = CONTEXT_CACHE.lock().unwrap();
    let count = cache.len();
    let oldest = cache
        .values()
        .map(|e| e.inserted_at.elapsed().as_secs())
        .max()
        .unwrap_or(0);
    (count, oldest)
}

#[cfg(test)]
mod expansion_tests {
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
                    evidence: Evidence::default(),
                },
            ],
            facts: vec![FactCandidate {
                statement: "must use MVCC for all writes".into(),
                entities: vec!["TransactionEngine".into()],
                confidence: 0.9,
                evidence: Evidence::default(),
            }],
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
    fn expand_entity_returns_all_facts_and_relations() {
        let ir = sample_ir();
        let pkg = compile_context("transaction", &ir, 0);
        let expansion = expand_entity("TransactionEngine", &pkg, &ir).unwrap();
        assert_eq!(expansion.entity.name, "TransactionEngine");
        assert!(!expansion.all_facts.is_empty());
        assert!(!expansion.all_relations.is_empty());
        assert!(expansion.evidence_source.is_some());
    }

    #[test]
    fn expand_relationship_traces_dependencies() {
        let ir = sample_ir();
        let chains = expand_relationship("ConstraintEngine", &ir, 3);
        // Should find the ConstraintEngine → TransactionEngine chain
        assert!(!chains.is_empty());
    }

    #[test]
    fn context_cache_hits_on_repeat_task() {
        let ir = sample_ir();
        // Invalidate first to ensure clean state
        invalidate_context_cache();

        let pkg1 = compile_context_cached("transaction", &ir, 0, 300);
        let pkg2 = compile_context_cached("transaction", &ir, 0, 300);
        // Both should return the same context (from cache on second call)
        assert_eq!(pkg1.estimated_tokens, pkg2.estimated_tokens);
        assert_eq!(pkg1.entities.len(), pkg2.entities.len());
    }

    #[test]
    fn context_cache_invalidates_on_clear() {
        let ir = sample_ir();
        invalidate_context_cache();
        let _ = compile_context_cached("transaction", &ir, 0, 300);
        let (count, _) = context_cache_stats();
        assert!(count > 0, "cache should have entries after compile");
        invalidate_context_cache();
        let (count2, _) = context_cache_stats();
        assert_eq!(count2, 0, "cache should be empty after invalidate");
    }

    #[test]
    fn expand_source_filters_by_evidence_path() {
        let ir = sample_ir();
        let (entities, _facts) = expand_source("transaction.rs", &ir);
        assert!(!entities.is_empty());
        assert!(entities.iter().any(|e| e.name == "TransactionEngine"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate};

    fn sample_ir() -> KnowledgeIr {
        KnowledgeIr {
            entities: vec![
                EntityCandidate {
                    name: "TransactionEngine".into(),
                    type_hint: Some("Struct".into()),
                    mentions: vec!["Handles MVCC transaction isolation".into()],
                    confidence: 0.85,
                    evidence: Evidence::default(),
                },
                EntityCandidate {
                    name: "ConstraintEngine".into(),
                    type_hint: Some("Struct".into()),
                    mentions: vec!["Validates constraint rules".into()],
                    confidence: 0.85,
                    evidence: Evidence::default(),
                },
                EntityCandidate {
                    name: "AuthService".into(),
                    type_hint: Some("Module".into()),
                    mentions: vec!["Handles authentication".into()],
                    confidence: 0.7,
                    evidence: Evidence::default(),
                },
            ],
            facts: vec![
                FactCandidate {
                    statement: "must use MVCC for all writes".into(),
                    entities: vec!["TransactionEngine".into()],
                    confidence: 0.9,
                    evidence: Evidence::default(),
                },
                FactCandidate {
                    statement: "constraints are validated at commit time".into(),
                    entities: vec!["ConstraintEngine".into()],
                    confidence: 0.85,
                    evidence: Evidence::default(),
                },
                FactCandidate {
                    statement: "AuthService supports OAuth2 and JWT".into(),
                    entities: vec!["AuthService".into()],
                    confidence: 0.7,
                    evidence: Evidence::default(),
                },
            ],
            relations: vec![RelationCandidate {
                subject: "ConstraintEngine".into(),
                predicate: "DEPENDS_ON".into(),
                object: "TransactionEngine".into(),
                confidence: 0.8,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn compile_ranks_by_task_relevance() {
        let ir = sample_ir();
        let pkg = compile_context("add constraint validation to transaction", &ir, 0);
        assert!(!pkg.entities.is_empty());
        // Both ConstraintEngine and TransactionEngine should rank high (order depends on exact overlap)
        let names: Vec<&str> = pkg.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"ConstraintEngine"),
            "should include ConstraintEngine"
        );
        assert!(
            names.contains(&"TransactionEngine"),
            "should include TransactionEngine"
        );
        // AuthService should not rank for this task
        assert!(
            !names.contains(&"AuthService"),
            "AuthService should NOT rank for constraint task"
        );
    }

    #[test]
    fn compile_respects_token_budget() {
        let ir = sample_ir();
        let pkg = compile_context("auth", &ir, 5); // very small budget
        assert!(pkg.trimmed, "should be trimmed on tiny budget");
        assert!(pkg.estimated_tokens <= 10, "should be near the budget");
    }

    #[test]
    fn keyword_score_ignores_stopword_chunks() {
        // Stopwords must never match task words — the old bidirectional
        // containment rule gave every mention fake credit
        // ("quadruple".contains("a")) and hid semantic-only recall.
        assert_eq!(keyword_score("validate a write", &["quadruple"]), 0.0);
        assert_eq!(keyword_score("the of and", &["galvanize"]), 0.0);
        // Morphological prefix variant still scores.
        assert_eq!(keyword_score("truncation", &["truncates"]), 0.3);
    }

    #[test]
    fn render_produces_markdown() {
        let ir = sample_ir();
        let pkg = compile_context("transaction", &ir, 0);
        let md = render_context_markdown(&pkg);
        assert!(md.contains("TransactionEngine"));
        assert!(md.contains("DEPENDS_ON"));
    }

    #[test]
    fn prose_entities_demoted_below_code() {
        let ir = KnowledgeIr {
            entities: vec![
                EntityCandidate {
                    name: "ConstraintEngine".into(),
                    type_hint: Some("Struct".into()),
                    mentions: vec!["Validates constraint rules".into()],
                    confidence: 0.8,
                    evidence: Evidence::default(),
                },
                EntityCandidate {
                    name: "Constraint Overview".into(),
                    type_hint: Some("Project".into()),
                    mentions: vec![
                        "Add constraint checks here".into(),
                        "m1".into(),
                        "m2".into(),
                        "m3".into(),
                    ],
                    confidence: 0.8,
                    evidence: Evidence::default(),
                },
            ],
            ..Default::default()
        };
        let pkg = compile_context("add constraint", &ir, 0);
        let first = &pkg.entities[0];
        assert_eq!(first.name, "ConstraintEngine");
        // Prose still surfaces (score > 0), just below the code entity.
        assert!(pkg.entities.iter().any(|e| e.name == "Constraint Overview"));
        assert!(pkg.entities[1].score < first.score);
    }

    #[test]
    fn pack_caps_mentions() {
        let long: String = "x".repeat(500);
        let ir = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "Thing".into(),
                type_hint: Some("Struct".into()),
                mentions: vec![long.clone(), long.clone(), long.clone(), long.clone(), long],
                confidence: 0.8,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        };
        let pkg = compile_context("thing", &ir, 0);
        let m = &pkg.entities[0].mentions;
        assert_eq!(m.len(), 2);
        assert!(m.iter().all(|s| s.chars().count() <= MAX_MENTION_CHARS));
    }

    #[test]
    fn render_includes_document_path() {
        let mut e = sample_ir().entities[0].clone();
        e.evidence.document_id = Some("crates/kernel/src/transaction.rs".into());
        let ir = KnowledgeIr {
            entities: vec![e],
            ..Default::default()
        };
        let pkg = compile_context("transaction", &ir, 0);
        let md = render_context_markdown(&pkg);
        assert!(md.contains("crates/kernel/src/transaction.rs"));
    }

    #[test]
    fn semantic_recall_includes_lexically_unmatched_entity() {
        let mut e = EntityCandidate {
            name: "EndpointResolver".into(),
            type_hint: Some("Function".into()),
            mentions: vec!["Maps use-path subjects to KOIDs".into()],
            confidence: 0.8,
            evidence: Evidence::default(),
        };
        e.evidence.document_id = Some("mcp/main.rs".into());
        let ir = KnowledgeIr {
            entities: vec![e],
            ..Default::default()
        };
        // No keyword overlap — lexical-only compile finds nothing.
        let lexical = compile_context("fix the gateway crash", &ir, 0);
        assert!(lexical.entities.is_empty());
        // Semantic match on the entity's key ("doc::name") pulls it in.
        let mut semantic = HashMap::new();
        semantic.insert("mcp/main.rs::EndpointResolver".to_string(), 0.72);
        let pkg = compile_context_semantic("fix the gateway crash", &ir, 0, Some(&semantic));
        assert_eq!(pkg.entities.len(), 1);
        assert_eq!(pkg.entities[0].name, "EndpointResolver");
        assert!(pkg.entities[0].justification.contains("cosine"));
    }

    #[test]
    fn semantic_below_floor_excluded() {
        let ir = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "EndpointResolver".into(),
                type_hint: Some("Function".into()),
                mentions: vec![],
                confidence: 0.8,
                evidence: Evidence::default(),
            }],
            ..Default::default()
        };
        let mut semantic = HashMap::new();
        semantic.insert("::EndpointResolver".to_string(), 0.1);
        let pkg = compile_context_semantic("fix the gateway crash", &ir, 0, Some(&semantic));
        assert!(pkg.entities.is_empty());
    }

    #[test]
    fn cached_semantic_key_separates_embeddings() {
        invalidate_context_cache();
        let ir = KnowledgeIr {
            entities: vec![
                EntityCandidate {
                    name: "Alpha".into(),
                    type_hint: Some("Function".into()),
                    mentions: vec![],
                    confidence: 0.8,
                    evidence: Evidence::default(),
                },
                EntityCandidate {
                    name: "Beta".into(),
                    type_hint: Some("Function".into()),
                    mentions: vec![],
                    confidence: 0.8,
                    evidence: Evidence::default(),
                },
            ],
            ..Default::default()
        };
        let mut a = HashMap::new();
        a.insert("::Alpha".to_string(), 0.9);
        let mut b = HashMap::new();
        b.insert("::Beta".to_string(), 0.9);
        let pkg_a = compile_context_cached_semantic("task", &ir, 0, 300, Some(&a));
        let pkg_b = compile_context_cached_semantic("task", &ir, 0, 300, Some(&b));
        assert_eq!(pkg_a.entities[0].name, "Alpha");
        assert_eq!(pkg_b.entities[0].name, "Beta");
    }

    fn ent(name: &str, type_hint: &str, mentions: Vec<&str>) -> EntityCandidate {
        EntityCandidate {
            name: name.into(),
            type_hint: Some(type_hint.into()),
            mentions: mentions.into_iter().map(String::from).collect(),
            confidence: 0.8,
            evidence: Evidence::default(),
        }
    }

    fn rel(subject: &str, predicate: &str, object: &str) -> RelationCandidate {
        RelationCandidate {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            confidence: 0.8,
            evidence: Evidence::default(),
        }
    }

    #[test]
    fn relation_boost_brings_neighbor_into_fold() {
        // Uppercase predicate proves case normalization at the adjacency gate.
        let ir = KnowledgeIr {
            entities: vec![
                ent("RetryLoop", "Function", vec![]),
                ent("TimeoutPolicy", "Struct", vec![]),
            ],
            relations: vec![rel("RetryLoop", "DEPENDS_ON", "TimeoutPolicy")],
            ..Default::default()
        };
        let pkg = compile_context("retry", &ir, 0);
        let neighbor = pkg
            .entities
            .iter()
            .find(|e| e.name == "TimeoutPolicy")
            .expect("gated neighbor should be resurrected by boost");
        assert!((neighbor.score - RELATION_BOOST_FACTOR).abs() < 1e-6);
        assert_eq!(
            neighbor.justification,
            "related to RetryLoop via DEPENDS_ON"
        );
    }

    #[test]
    fn relation_boost_skips_unrelated_entity() {
        let ir = KnowledgeIr {
            entities: vec![
                ent("RetryLoop", "Function", vec![]),
                ent("TimeoutPolicy", "Struct", vec![]),
                ent("UnrelatedThing", "Struct", vec![]),
            ],
            relations: vec![rel("RetryLoop", "depends_on", "TimeoutPolicy")],
            ..Default::default()
        };
        let pkg = compile_context("retry", &ir, 0);
        let names: Vec<&str> = pkg.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"TimeoutPolicy"));
        assert!(!names.contains(&"UnrelatedThing"));
    }

    #[test]
    fn contains_boosts_parent_only() {
        let ir = KnowledgeIr {
            entities: vec![
                ent("RetryLoop", "Function", vec![]),
                ent("src/net.rs", "config", vec![]),
            ],
            relations: vec![rel("src/net.rs", "contains", "RetryLoop")],
            ..Default::default()
        };
        // Ranked child boosts its container…
        let pkg = compile_context("retry", &ir, 0);
        let file = pkg
            .entities
            .iter()
            .find(|e| e.name == "src/net.rs")
            .expect("container should be boosted by its ranked child");
        assert!((file.score - RELATION_BOOST_FACTOR).abs() < 1e-6);
        assert_eq!(file.justification, "related to RetryLoop via contains");
        // …but a ranked container does not flood its children.
        let pkg2 = compile_context("net", &ir, 0);
        assert!(!pkg2.entities.iter().any(|e| e.name == "RetryLoop"));
    }

    #[test]
    fn relation_boost_takes_max_not_sum() {
        let ir = KnowledgeIr {
            entities: vec![
                ent("RetryLoop", "Function", vec![]),
                ent("TimeoutPolicy", "Struct", vec![]),
                ent("CircuitBreaker", "Struct", vec![]),
            ],
            relations: vec![
                rel("RetryLoop", "depends_on", "CircuitBreaker"),
                rel("TimeoutPolicy", "tested_by", "CircuitBreaker"),
            ],
            ..Default::default()
        };
        let pkg = compile_context("retry timeout", &ir, 0);
        let shared = pkg
            .entities
            .iter()
            .find(|e| e.name == "CircuitBreaker")
            .expect("shared neighbor should be boosted");
        assert!(
            (shared.score - RELATION_BOOST_FACTOR).abs() < 1e-6,
            "boost must be max, not sum: {}",
            shared.score
        );
    }

    #[test]
    fn relation_boost_fanout_capped() {
        let mut entities = vec![ent("RetryLoop", "Function", vec![])];
        for i in 0..7 {
            entities.push(ent(&format!("Node{i}"), "Struct", vec![]));
        }
        let relations: Vec<RelationCandidate> = (0..7)
            .map(|i| rel("RetryLoop", "depends_on", &format!("Node{i}")))
            .collect();
        let ir = KnowledgeIr {
            entities,
            relations,
            ..Default::default()
        };
        let pkg = compile_context("retry", &ir, 0);
        // All 7 neighbors are score-0 pre-boost → the tie-break is entity
        // index, so Node0..Node4 win the 5 slots deterministically.
        let names: Vec<&str> = pkg.entities.iter().map(|e| e.name.as_str()).collect();
        for i in 0..5 {
            let name = format!("Node{i}");
            assert!(names.contains(&name.as_str()), "Node{i} should be boosted");
        }
        assert!(!names.contains(&"Node5"));
        assert!(!names.contains(&"Node6"));
        assert_eq!(pkg.entities.len(), 6);
    }

    #[test]
    fn relation_boost_keeps_own_justification() {
        let ir = KnowledgeIr {
            entities: vec![
                ent("RetryLoop", "Function", vec![]),
                ent("TimeoutPolicy", "Struct", vec![]),
            ],
            relations: vec![rel("RetryLoop", "depends_on", "TimeoutPolicy")],
            ..Default::default()
        };
        let pkg = compile_context("retry timeout", &ir, 0);
        let own = pkg
            .entities
            .iter()
            .find(|e| e.name == "TimeoutPolicy")
            .expect("lexically-ranked neighbor should be present");
        // Own lexical score (1.0) beats the 0.5 boost — max, not replace.
        assert!((own.score - 1.0).abs() < 1e-6);
        assert!(own.justification.contains("name matches"));
        assert!(!own.justification.starts_with("related to"));
    }
}
