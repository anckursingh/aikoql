//! D4: Knowledge boundary detection — semantic segmentation of the AST.
//!
//! Split from retrieval chunking (HLD §22/§37): the boundary detector owns
//! *semantic* segmentation ("which coherent knowledge units exist?"), the
//! chunker owns *retrieval* packaging ("how do chunks serve a backend?").
//!
//! Production detector: `RuleBoundaryDetector` — hard structural boundaries
//! (heading/table/figure/code/list blocks, page transitions) plus heading-path
//! context. PR-H adds `EmbeddingBoundaryDetector` — the rule structure plus a
//! semantic-similarity layer over the `EmbeddingProvider` seam (HLD §16/§60):
//! adjacent Text fragments above `merge_threshold` join; long Text fragments
//! split at sentence boundaries where consecutive sentences drop below
//! `split_threshold`. It exists to be measured against the rule baseline.
//! PR-I adds `HybridBoundaryDetector` — §16's first production target:
//! structure + sentence boundaries + semantic similarity + modality
//! transitions + an optional transformer score, where the boundary policy
//! (this detector) owns the final decision and the scorer only proposes.
//! PR-J adds `TransformerBoundaryDetector` — the rule base plus the scorer's
//! P(same unit): no embedding layer, so an unconfigured scorer degrades to
//! the rule detector (DoD row 10: the transformer is optional).

use crate::ast::{table_payload_from_node, AstNode, BlockType, DocumentAst};
use crate::chunking::fragment_text;
use crate::embedding::{cosine_similarity, EmbeddingProvider};
use crate::fragment::{FragmentContent, FragmentContext, FragmentModality, KnowledgeFragment};
use crate::ir::Evidence;
use crate::source::{EvidenceSource, SourceSpan};

/// Splits a DocumentAst into coherent knowledge units.
///
/// Implementations: RuleBoundaryDetector, EmbeddingBoundaryDetector (PR-H),
/// HybridBoundaryDetector (PR-I), TransformerBoundaryDetector (PR-J) — the
/// transformer is a pluggable implementation, not an architectural
/// dependency (HLD §16).
pub trait KnowledgeBoundaryDetector: Send + Sync {
    fn name(&self) -> &str;

    fn detect(&self, ast: &DocumentAst) -> Result<Vec<KnowledgeFragment>, BoundaryError>;
}

#[derive(Debug)]
pub enum BoundaryError {
    Construction(String),
}

impl std::fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoundaryError::Construction(msg) => {
                write!(f, "fragment construction failed: {}", msg)
            }
        }
    }
}

impl std::error::Error for BoundaryError {}

/// HLD §16/§17: the transformer's answer to "does this boundary separate two
/// semantically distinct knowledge units?" — a score, not a decision. The
/// boundary policy (the detector) makes the final call.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BoundaryScore {
    /// P(same knowledge unit) across the boundary; near 0 = hard boundary.
    pub probability: f32,
    pub model: String,
}

/// Optional scorer seam behind the hybrid detector. A transformer
/// implementation is a pluggable provider of scores, not an architectural
/// dependency (HLD §16); returning `None` means "no opinion on this
/// boundary".
pub trait BoundaryScorer: Send + Sync {
    fn score_boundary(
        &self,
        prev: &KnowledgeFragment,
        next: &KnowledgeFragment,
    ) -> Option<BoundaryScore>;
}

/// Structural boundary detector: one fragment per top-level block, with
/// headings tracked as context instead of emitted fragments (their text
/// reaches consumers through `FragmentContext.heading_path`).
pub struct RuleBoundaryDetector;

impl KnowledgeBoundaryDetector for RuleBoundaryDetector {
    fn name(&self) -> &str {
        "rule-boundary"
    }

    fn detect(&self, ast: &DocumentAst) -> Result<Vec<KnowledgeFragment>, BoundaryError> {
        let mut fragments: Vec<KnowledgeFragment> = Vec::new();
        let mut heading_path: Vec<String> = Vec::new();

        for (page_idx, page_node) in ast.pages.iter().enumerate() {
            let page = page_idx as u32 + 1;
            for (block_idx, block) in page_node.children.iter().enumerate() {
                emit_block(block, page, block_idx, &mut heading_path, &mut fragments);
            }
            heading_path.clear(); // headings do not cross page boundaries
        }

        // PR-C: document-hash prefix — fragment ids are globally unique
        // across documents (`frag-{hash8}-p{page}-b{block}`), and the
        // context carries the document identity.
        if let Some(doc_id) = ast.document_id.as_deref() {
            let hash8: String = doc_id.chars().take(8).collect();
            if !hash8.is_empty() {
                for frag in &mut fragments {
                    let short = frag.fragment_id.strip_prefix("frag-").unwrap_or("");
                    frag.fragment_id = format!("frag-{}-{}", hash8, short);
                    frag.context.document_id = Some(doc_id.to_string());
                }
            }
        }

        finalize_neighbors(&mut fragments);
        Ok(fragments)
    }
}

/// Semantic-similarity boundary detector (PR-H, HLD §16/§60).
///
/// Structure comes from `RuleBoundaryDetector`; the embedding layer then
/// (1) merges adjacent Text fragments whose rendered texts embed above
/// `merge_threshold` (topic continuity across block boundaries) and
/// (2) splits long Text fragments at sentence boundaries where consecutive
/// sentences drop below `split_threshold` (topic shift inside a block).
/// Non-Text fragments are modality boundaries and are never merged or split.
/// The vector source is the pluggable `EmbeddingProvider` seam — a real
/// model provider later swaps in without touching the detector.
pub struct EmbeddingBoundaryDetector<'a> {
    provider: &'a dyn EmbeddingProvider,
    merge_threshold: f32,
    split_threshold: f32,
}

impl<'a> EmbeddingBoundaryDetector<'a> {
    pub fn new(provider: &'a dyn EmbeddingProvider) -> Self {
        // Defaults tuned to the mock char-ngram provider: its cosine
        // between any two English sentences lands in a tight 0.16–0.51 band
        // with no topic gap (measured), so the split layer must only fire on
        // strong divergence to avoid over-fragmenting. A real model provider
        // widens the band and can raise both thresholds.
        EmbeddingBoundaryDetector {
            provider,
            merge_threshold: 0.55,
            split_threshold: 0.10,
        }
    }

    /// Tuned thresholds: merge joins fragments at or above `merge`; a long
    /// text fragment splits where consecutive sentences fall below `split`.
    pub fn with_thresholds(provider: &'a dyn EmbeddingProvider, merge: f32, split: f32) -> Self {
        EmbeddingBoundaryDetector {
            provider,
            merge_threshold: merge,
            split_threshold: split,
        }
    }
}

impl KnowledgeBoundaryDetector for EmbeddingBoundaryDetector<'_> {
    fn name(&self) -> &str {
        "embedding-boundary"
    }

    fn detect(&self, ast: &DocumentAst) -> Result<Vec<KnowledgeFragment>, BoundaryError> {
        let mut fragments = RuleBoundaryDetector.detect(ast)?;
        self.merge(&mut fragments);
        self.split(&mut fragments);
        finalize_neighbors(&mut fragments);
        Ok(fragments)
    }
}

impl EmbeddingBoundaryDetector<'_> {
    /// Join adjacent Text fragments that stay on the same topic. Only
    /// Text+Text pairs merge: modality boundaries are hard (§16 Hybrid
    /// owns modality merging). The merged fragment keeps the first
    /// fragment's id/source/context; both evidence trails are kept.
    fn merge(&self, fragments: &mut Vec<KnowledgeFragment>) {
        let mut i = 0;
        while i + 1 < fragments.len() {
            let (a_text, b_text) = match (&fragments[i].content, &fragments[i + 1].content) {
                (FragmentContent::Text(a), FragmentContent::Text(b)) => (a.clone(), b.clone()),
                _ => {
                    i += 1;
                    continue;
                }
            };
            let sim =
                cosine_similarity(&self.provider.embed(&a_text), &self.provider.embed(&b_text));
            if sim >= self.merge_threshold {
                let b = fragments.remove(i + 1);
                let a = &mut fragments[i];
                if let FragmentContent::Text(t) = &mut a.content {
                    t.push('\n');
                    t.push_str(&b_text);
                }
                a.evidence.extend(b.evidence);
                // same i: the merged fragment may chain with the next one
            } else {
                i += 1;
            }
        }
    }

    /// Split long Text fragments at sentence boundaries where the topic
    /// shifts (consecutive-sentence similarity below `split_threshold`).
    /// Split pieces keep the parent's id with a `-s{n}` suffix, context,
    /// and geometry; their evidence is re-stamped `embedding_boundary`.
    fn split(&self, fragments: &mut Vec<KnowledgeFragment>) {
        split_text_fragments(
            fragments,
            self.provider,
            self.split_threshold,
            160,
            "-s",
            "embedding_boundary",
        );
    }
}

/// Shared semantic split pass (PR-H embedding, PR-I hybrid): long Text
/// fragments re-segment at sentence boundaries where the next sentence
/// diverges from the running segment's ACCUMULATED text — accumulation
/// strengthens the within-topic signal for weak (char-ngram) providers.
/// Pieces keep the parent's id with a `{suffix}{n}` suffix, context and
/// geometry; evidence is re-stamped with `extractor`.
fn split_text_fragments(
    fragments: &mut Vec<KnowledgeFragment>,
    provider: &dyn EmbeddingProvider,
    split_threshold: f32,
    min_chars: usize,
    suffix: &str,
    extractor: &str,
) {
    let mut out: Vec<KnowledgeFragment> = Vec::new();
    for frag in fragments.drain(..) {
        let text = match &frag.content {
            FragmentContent::Text(t) => t.clone(),
            _ => {
                out.push(frag);
                continue;
            }
        };
        // Short fragments are already one coherent unit; splitting them
        // would only fragment retrieval.
        if text.chars().count() < min_chars {
            out.push(frag);
            continue;
        }
        let units = sentences(&text);
        if units.len() < 2 {
            out.push(frag);
            continue;
        }
        let mut segments: Vec<Vec<String>> = vec![Vec::new()];
        for unit in units {
            let similar = segments
                .last()
                .and_then(|s| (!s.is_empty()).then(|| s.join(" ")))
                .is_none_or(|accumulated| {
                    cosine_similarity(&provider.embed(&accumulated), &provider.embed(&unit))
                        >= split_threshold
                });
            if similar {
                segments.last_mut().expect("segment exists").push(unit);
            } else {
                segments.push(vec![unit]);
            }
        }
        if segments.len() == 1 {
            out.push(frag);
            continue;
        }
        let base_id = frag.fragment_id.clone();
        for (si, seg) in segments.into_iter().enumerate() {
            let mut piece = frag.clone();
            piece.fragment_id = format!("{base_id}{suffix}{si}");
            piece.content = FragmentContent::Text(seg.join(" "));
            for ev in &mut piece.evidence {
                ev.extractor = extractor.into();
            }
            out.push(piece);
        }
    }
    *fragments = out;
}

/// Linguistic split pass (PR-I, the hybrid's "sentence boundaries" layer):
/// Text fragments above `max_chars` re-segment at sentence boundaries into
/// pieces that each fit the ceiling — a deterministic policy floor,
/// independent of any provider. A single sentence longer than the ceiling
/// stays whole (ponytail: sentences are the atomic unit here; a real
/// tokenizer would re-cut one — add with real embeddings). Pieces keep the
/// parent's id with a `-l{n}` suffix, context and geometry; evidence is
/// re-stamped `hybrid_boundary`.
fn split_text_at_ceiling(
    fragments: &mut Vec<KnowledgeFragment>,
    max_chars: usize,
    extractor: &str,
) {
    let mut out: Vec<KnowledgeFragment> = Vec::new();
    for frag in fragments.drain(..) {
        let text = match &frag.content {
            FragmentContent::Text(t) => t.clone(),
            _ => {
                out.push(frag);
                continue;
            }
        };
        if text.chars().count() <= max_chars {
            out.push(frag);
            continue;
        }
        let units = sentences(&text);
        if units.len() < 2 {
            out.push(frag);
            continue;
        }
        let mut pieces: Vec<Vec<String>> = vec![Vec::new()];
        for unit in units {
            let fits = match pieces.last() {
                Some(p) if !p.is_empty() => {
                    let joined: usize = p.iter().map(String::len).sum::<usize>() + p.len() - 1;
                    joined + unit.len() < max_chars
                }
                _ => true,
            };
            if fits {
                pieces.last_mut().expect("piece exists").push(unit);
            } else {
                pieces.push(vec![unit]);
            }
        }
        if pieces.len() == 1 {
            out.push(frag);
            continue;
        }
        let base_id = frag.fragment_id.clone();
        for (li, piece) in pieces.into_iter().enumerate() {
            let mut p = frag.clone();
            p.fragment_id = format!("{base_id}-l{li}");
            p.content = FragmentContent::Text(piece.join(" "));
            for ev in &mut p.evidence {
                ev.extractor = extractor.into();
            }
            out.push(p);
        }
    }
    *fragments = out;
}

/// PR-I: merge two adjacent fragments into one unit. Text+Text joins into
/// Text; every other same-unit pair (a modality transition: figure+text,
/// text+table, …) nests both into a `Mixed` composite that keeps the first
/// fragment's id, source geometry and context — children keep their own
/// modality and provenance, and `fragment_text` renders them recursively.
/// Evidence is re-stamped with the caller's extractor; when the transformer
/// score triggered the merge, the model that decided is recorded on it.
fn merge_pair(
    mut a: KnowledgeFragment,
    mut b: KnowledgeFragment,
    via_model: Option<String>,
    extractor: &str,
) -> KnowledgeFragment {
    let a_content = std::mem::replace(&mut a.content, FragmentContent::Text(String::new()));
    let b_content = std::mem::replace(&mut b.content, FragmentContent::Text(String::new()));
    match (a_content, b_content) {
        (FragmentContent::Text(mut at), FragmentContent::Text(bt)) => {
            at.push('\n');
            at.push_str(&bt);
            a.content = FragmentContent::Text(at);
        }
        (a_c, b_c) => {
            a.modality = FragmentModality::Mixed;
            a.content = FragmentContent::Mixed(vec![
                Box::new(KnowledgeFragment {
                    content: a_c,
                    ..a.clone()
                }),
                Box::new(KnowledgeFragment {
                    content: b_c,
                    ..b.clone()
                }),
            ]);
        }
    }
    a.evidence.extend(b.evidence);
    for ev in &mut a.evidence {
        ev.extractor = extractor.into();
        if let Some(model) = &via_model {
            ev.model = Some(model.clone());
        }
    }
    a
}

/// Hybrid boundary detector (PR-I, HLD §16) — the first production target:
///
/// ```text
/// structural boundary   — RuleBoundaryDetector
/// linguistic boundary   — long Text re-cut at sentence boundaries (max_chars)
/// semantic similarity   — provider cosine on rendered fragment text
/// transformer score     — optional BoundaryScorer; its P(same unit) can
///                          merge where cosine alone would not (the policy,
///                          not the score, decides — HLD §16/§17)
/// modality transition   — same-unit pairs across modalities nest into
///                          Mixed composites (figure+text, text+table, …)
/// ```
///
/// Default thresholds match `EmbeddingBoundaryDetector`'s mock-band tuning;
/// `with_thresholds` re-tunes both, `with_scorer` attaches the transformer.
/// The transformer is a pluggable implementation, not an architectural
/// dependency — a real scorer swaps in without touching the detector.
pub struct HybridBoundaryDetector<'a> {
    provider: &'a dyn EmbeddingProvider,
    scorer: Option<&'a dyn BoundaryScorer>,
    merge_threshold: f32,
    split_threshold: f32,
    max_chars: usize,
}

/// P(score says same unit) at or above which a boundary dissolves even when
/// the semantic layer disagrees (HLD §16: the boundary policy makes the
/// final decision — the scorer only proposes).
const TRANSFORMER_ACCEPT: f32 = 0.7;

impl<'a> HybridBoundaryDetector<'a> {
    pub fn new(provider: &'a dyn EmbeddingProvider) -> Self {
        HybridBoundaryDetector {
            provider,
            scorer: None,
            merge_threshold: 0.55,
            split_threshold: 0.10,
            max_chars: 800,
        }
    }

    /// Tuned thresholds: merge joins fragments at or above `merge`; a long
    /// text fragment splits where consecutive sentences fall below `split`.
    pub fn with_thresholds(provider: &'a dyn EmbeddingProvider, merge: f32, split: f32) -> Self {
        HybridBoundaryDetector {
            merge_threshold: merge,
            split_threshold: split,
            ..Self::new(provider)
        }
    }

    /// Attach the optional transformer scorer (HLD §16/§17).
    pub fn with_scorer(mut self, scorer: &'a dyn BoundaryScorer) -> Self {
        self.scorer = Some(scorer);
        self
    }
}

impl KnowledgeBoundaryDetector for HybridBoundaryDetector<'_> {
    fn name(&self) -> &str {
        "hybrid-boundary"
    }

    fn detect(&self, ast: &DocumentAst) -> Result<Vec<KnowledgeFragment>, BoundaryError> {
        let mut fragments = RuleBoundaryDetector.detect(ast)?;
        // Merge first so the semantic layer sees the natural document flow
        // (figure+caption, same-topic paragraphs); the linguistic ceiling
        // then bounds the RETRIEVAL unit size — cutting before merging would
        // let the semantic pass re-join the pieces it just cut. The semantic
        // split finally re-evaluates inside ceiling-cut pieces: they may
        // still carry topic shifts.
        self.merge(&mut fragments);
        split_text_at_ceiling(&mut fragments, self.max_chars, "hybrid_boundary");
        split_text_fragments(
            &mut fragments,
            self.provider,
            self.split_threshold,
            160,
            "-s",
            "hybrid_boundary",
        );
        finalize_neighbors(&mut fragments);
        Ok(fragments)
    }
}

impl HybridBoundaryDetector<'_> {
    /// Join adjacent fragments the policy declares one unit: rendered-text
    /// cosine at/above `merge_threshold`, OR the transformer score at/above
    /// `TRANSFORMER_ACCEPT`. Re-merged fragments may exceed `max_chars`
    /// again — coherent units are allowed to (ponytail: re-cutting would
    /// re-run the sentence pass; add if retrieval shows oversized units).
    fn merge(&self, fragments: &mut Vec<KnowledgeFragment>) {
        let mut i = 0;
        while i + 1 < fragments.len() {
            let a_text = fragment_text(&fragments[i]);
            let b_text = fragment_text(&fragments[i + 1]);
            let sim =
                cosine_similarity(&self.provider.embed(&a_text), &self.provider.embed(&b_text));
            let score = self
                .scorer
                .and_then(|s| s.score_boundary(&fragments[i], &fragments[i + 1]));
            let via_transformer = score
                .as_ref()
                .is_some_and(|sc| sc.probability >= TRANSFORMER_ACCEPT);
            if sim < self.merge_threshold && !via_transformer {
                i += 1;
                continue;
            }
            let via_model = if via_transformer {
                score.map(|sc| sc.model)
            } else {
                None
            };
            let a = fragments.remove(i);
            let b = fragments.remove(i);
            fragments.insert(i, merge_pair(a, b, via_model, "hybrid_boundary"));
            // same i: the merged unit may chain with the next one
        }
    }
}

/// Transformer boundary detector (PR-J, HLD §16 Phase 3) — the rule base
/// plus a transformer `BoundaryScorer`:
///
/// ```text
/// structural boundary   — RuleBoundaryDetector
/// linguistic boundary   — long Text re-cut at sentence boundaries (max_chars)
/// transformer score     — scorer's P(same unit) at/above `accept_threshold`
///                          dissolves the boundary (the policy, not the
///                          score, decides — HLD §16/§17)
/// modality transition   — same-unit pairs across modalities nest into
///                          Mixed composites, exactly as in the hybrid
/// ```
///
/// No embedding layer: the transformer answers "does this boundary separate
/// two semantically distinct knowledge units?" (HLD §17) and that alone
/// drives the semantic decision. A scorer returning `None` has no opinion —
/// the boundary stays, so the detector degrades to the rule detector when
/// no transformer is configured (DoD row 10: transformer optional).
pub struct TransformerBoundaryDetector<'a> {
    scorer: &'a dyn BoundaryScorer,
    accept_threshold: f32,
    max_chars: usize,
}

impl<'a> TransformerBoundaryDetector<'a> {
    pub fn new(scorer: &'a dyn BoundaryScorer) -> Self {
        Self {
            scorer,
            accept_threshold: TRANSFORMER_ACCEPT,
            max_chars: 800,
        }
    }

    /// Re-tune the policy threshold for a real model's probability scale.
    pub fn with_accept_threshold(mut self, threshold: f32) -> Self {
        self.accept_threshold = threshold;
        self
    }
}

impl KnowledgeBoundaryDetector for TransformerBoundaryDetector<'_> {
    fn name(&self) -> &str {
        "transformer-boundary"
    }

    fn detect(&self, ast: &DocumentAst) -> Result<Vec<KnowledgeFragment>, BoundaryError> {
        let mut fragments = RuleBoundaryDetector.detect(ast)?;
        // Merge first so the scorer judges the natural document flow; the
        // linguistic ceiling then bounds the RETRIEVAL unit size — the same
        // order as the hybrid detector, minus its embedding layers.
        self.merge(&mut fragments);
        split_text_at_ceiling(&mut fragments, self.max_chars, "transformer_boundary");
        finalize_neighbors(&mut fragments);
        Ok(fragments)
    }
}

impl TransformerBoundaryDetector<'_> {
    fn merge(&self, fragments: &mut Vec<KnowledgeFragment>) {
        let mut i = 0;
        while i + 1 < fragments.len() {
            let score = self.scorer.score_boundary(&fragments[i], &fragments[i + 1]);
            match score {
                Some(s) if s.probability >= self.accept_threshold => {
                    let a = fragments.remove(i);
                    let b = fragments.remove(i);
                    fragments.insert(i, merge_pair(a, b, Some(s.model), "transformer_boundary"));
                    // same i: the merged unit may chain with the next one
                }
                _ => i += 1,
            }
        }
    }
}

/// (Re)link neighboring fragment ids — shared by every detector so context
/// stays consistent after merge/split transforms.
fn finalize_neighbors(fragments: &mut [KnowledgeFragment]) {
    let ids: Vec<String> = fragments.iter().map(|f| f.fragment_id.clone()).collect();
    for (i, frag) in fragments.iter_mut().enumerate() {
        let mut neighbors = Vec::with_capacity(2);
        if i > 0 {
            neighbors.push(ids[i - 1].clone());
        }
        if i + 1 < ids.len() {
            neighbors.push(ids[i + 1].clone());
        }
        frag.context.neighboring_fragments = neighbors;
    }
}

/// Split text into sentence units on `. `/`! `/`? ` boundaries; a period
/// directly preceded by an ASCII digit (list prefixes like `1.`, decimals,
/// years) does not end a sentence. Newlines stay inside their sentence unit.
/// ponytail: no abbreviation handling — `Dr. Smith` splits in two; fine for
/// segmentation scoring, add a tokenizer when real embeddings land.
fn sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut units: Vec<String> = Vec::new();
    let mut start = 0usize;
    for i in 1..chars.len() {
        let prev = chars[i - 1];
        let c = chars[i];
        if c == ' ' && (prev == '.' || prev == '!' || prev == '?') {
            if prev == '.' && i >= 2 && chars[i - 2].is_ascii_digit() {
                continue;
            }
            units.push(chars[start..i].iter().collect());
            start = i + 1; // skip the space
        }
    }
    let rest: String = chars[start..].iter().collect();
    if !rest.trim().is_empty() {
        units.push(rest);
    }
    units
}

fn emit_block(
    node: &AstNode,
    page: u32,
    block_idx: usize,
    heading_path: &mut Vec<String>,
    out: &mut Vec<KnowledgeFragment>,
) {
    match &node.block_type {
        BlockType::Heading { .. } | BlockType::Title => {
            // Headings are structural context, not content fragments.
            heading_path.push(node.text.clone().unwrap_or_default());
        }
        BlockType::Table => {
            // The canonical AST already carries the TablePayload (attached by
            // build_table_node); fall back to conversion for hand-built ASTs.
            let payload = node
                .payload
                .clone()
                .and_then(|p| match p {
                    crate::ast::AstPayload::Table(t) => Some(t),
                    _ => None,
                })
                .or_else(|| table_payload_from_node(node));
            match payload {
                Some(table) => {
                    let confidence = node.confidence.unwrap_or(1.0);
                    out.push(KnowledgeFragment {
                        fragment_id: fragment_id(page, block_idx),
                        modality: FragmentModality::Table,
                        content: FragmentContent::Table(table),
                        context: FragmentContext {
                            heading_path: heading_path.clone(),
                            page: Some(page),
                            ..Default::default()
                        },
                        source: Some(SourceSpan {
                            document_id: None,
                            page,
                            start_offset: None,
                            end_offset: None,
                            bbox: node.bbox.clone(),
                            node_id: node.node_id.clone(),
                        }),
                        evidence: vec![evidence(node, page, confidence)],
                        confidence,
                    });
                }
                None => out.push(text_fragment(node, page, block_idx, heading_path)),
            }
        }
        BlockType::Code => {
            let confidence = node.confidence.unwrap_or(1.0);
            out.push(KnowledgeFragment {
                fragment_id: fragment_id(page, block_idx),
                modality: FragmentModality::Code,
                content: FragmentContent::Code(node.text.clone().unwrap_or_default()),
                context: FragmentContext {
                    heading_path: heading_path.clone(),
                    page: Some(page),
                    ..Default::default()
                },
                source: Some(SourceSpan {
                    document_id: None,
                    page,
                    start_offset: None,
                    end_offset: None,
                    bbox: node.bbox.clone(),
                    node_id: node.node_id.clone(),
                }),
                evidence: vec![evidence(node, page, confidence)],
                confidence,
            });
        }
        BlockType::List { .. } => {
            // Join items for the fragment; the list structure itself stays
            // canonical in the AST.
            let text = node
                .children
                .iter()
                .filter_map(|item| {
                    if item.text.as_deref().unwrap_or_default().trim().is_empty() {
                        None
                    } else {
                        Some(item.text.as_deref().unwrap_or_default().trim().to_string())
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Text(text),
                    FragmentModality::Text,
                ));
            }
        }
        // PR-F: visual modalities emit typed fragments when classification
        // attached a payload; without one (degraded analyzer) they fall back
        // to Text so the figure marker + caption are never lost.
        BlockType::Chart | BlockType::Diagram | BlockType::Formula | BlockType::Image => {
            let content = node.payload.clone().and_then(|p| match p {
                crate::ast::AstPayload::Chart(c) => Some(FragmentContent::Chart(c)),
                crate::ast::AstPayload::Diagram(d) => Some(FragmentContent::Diagram(d)),
                crate::ast::AstPayload::Formula(f) => Some(FragmentContent::Formula(f)),
                crate::ast::AstPayload::Image(i) => Some(FragmentContent::Image(i)),
                _ => None,
            });
            match content {
                Some(FragmentContent::Chart(c)) => out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Chart(c),
                    FragmentModality::Chart,
                )),
                Some(FragmentContent::Diagram(d)) => out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Diagram(d),
                    FragmentModality::Diagram,
                )),
                Some(FragmentContent::Formula(f)) => out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Formula(f),
                    FragmentModality::Formula,
                )),
                Some(FragmentContent::Image(i)) => out.push(text_fragment_with(
                    page,
                    block_idx,
                    heading_path,
                    node,
                    FragmentContent::Image(i),
                    FragmentModality::Image,
                )),
                _ => out.push(text_fragment(node, page, block_idx, heading_path)),
            }
        }
        _ => {
            // Empty container (Unknown/Section wrappers): recurse so wrapped
            // content is never silently dropped.
            if node.text.as_deref().unwrap_or_default().trim().is_empty()
                && !node.children.is_empty()
            {
                for (child_idx, child) in node.children.iter().enumerate() {
                    emit_block(child, page, child_idx, heading_path, out);
                }
            } else {
                out.push(text_fragment(node, page, block_idx, heading_path));
            }
        }
    }
}

fn text_fragment(
    node: &AstNode,
    page: u32,
    block_idx: usize,
    heading_path: &[String],
) -> KnowledgeFragment {
    text_fragment_with(
        page,
        block_idx,
        heading_path,
        node,
        FragmentContent::Text(node.text.clone().unwrap_or_default()),
        FragmentModality::Text,
    )
}

fn text_fragment_with(
    page: u32,
    block_idx: usize,
    heading_path: &[String],
    node: &AstNode,
    content: FragmentContent,
    modality: FragmentModality,
) -> KnowledgeFragment {
    let confidence = node.confidence.unwrap_or(1.0);
    KnowledgeFragment {
        fragment_id: fragment_id(page, block_idx),
        modality,
        content,
        context: FragmentContext {
            heading_path: heading_path.to_vec(),
            page: Some(page),
            ..Default::default()
        },
        source: Some(SourceSpan {
            document_id: None,
            page,
            start_offset: None,
            end_offset: None,
            bbox: node.bbox.clone(),
            node_id: node.node_id.clone(),
        }),
        evidence: vec![evidence(node, page, confidence)],
        confidence,
    }
}

/// Deterministic fragment id from position. ponytail: no document-hash
/// prefix until DocumentAst carries a document_id (PR-B) — position alone
/// is stable for a given document layout.
fn fragment_id(page: u32, block_idx: usize) -> String {
    format!("frag-p{}-b{}", page, block_idx)
}

fn evidence(node: &AstNode, page: u32, confidence: f32) -> Evidence {
    Evidence {
        document_id: None,
        page: Some(page),
        source: node
            .bbox
            .as_ref()
            .map(|b| EvidenceSource::Region { bbox: b.clone() }),
        extractor: "rule_boundary".into(),
        model: None,
        confidence,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual::DiagramAnalyzer;
    use crate::{document_model_to_ast, DocumentModel, PageModel};

    fn doc(pages: Vec<&str>) -> DocumentModel {
        let pages: Vec<PageModel> = pages
            .iter()
            .map(|t| PageModel {
                page_number: 1,
                text: t.to_string(),
                char_count: t.len(),
                source: "native".into(),
                ocr_confidence: None,
                images: vec![],
            })
            .collect();
        DocumentModel {
            page_count: pages.len() as u32,
            total_chars: pages.iter().map(|p| p.char_count).sum(),
            pages,
            ocr_stats: None,
        }
    }

    #[test]
    fn detect_structural_boundaries() {
        let dm = doc(vec![
            "1. Payment Terms\n\nPayment is due within 30 days.\n\n| Item | Qty |\n| Widget | 10 |\n| Gadget | 5 |\n\n    let x = 1;\n    let y = 2;",
        ]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();

        let kinds: Vec<FragmentModality> = fragments.iter().map(|f| f.modality.clone()).collect();
        assert!(
            kinds.contains(&FragmentModality::Text),
            "paragraph fragment"
        );
        assert!(kinds.contains(&FragmentModality::Table), "table fragment");
        assert!(kinds.contains(&FragmentModality::Code), "code fragment");
    }

    #[test]
    fn headings_become_context_not_fragments() {
        let dm = doc(vec![
            "1. Billing\n\nPayment is due within 30 days of invoice date.",
        ]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].modality, FragmentModality::Text);
        assert!(fragments[0]
            .context
            .heading_path
            .iter()
            .any(|h| h.contains("Billing")));
    }

    #[test]
    fn table_fragment_preserves_structure() {
        let dm = doc(vec!["| Name | Age |\n| Alice | 30 |\n| Bob | 25 |"]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();

        let table = fragments
            .iter()
            .find(|f| f.modality == FragmentModality::Table)
            .expect("table fragment");
        match &table.content {
            FragmentContent::Table(payload) => {
                assert_eq!(payload.headers.len(), 2);
                assert_eq!(payload.headers[0].text, "Name");
                assert_eq!(payload.rows.len(), 2);
                assert_eq!(payload.cells.len(), 4);
                assert_eq!(payload.cells[1].text, "30");
                assert_eq!(payload.cells[1].column_id, "h1");
            }
            other => panic!("expected table content, got {:?}", other),
        }
    }

    #[test]
    fn fragments_have_provenance_and_deterministic_ids() {
        let dm = doc(vec!["Paragraph one.\n\nParagraph two."]);
        let ast = document_model_to_ast(&dm);
        let first = RuleBoundaryDetector.detect(&ast).unwrap();
        let second = RuleBoundaryDetector.detect(&ast).unwrap();

        assert_eq!(first.len(), 2);
        for f in &first {
            assert!(!f.evidence.is_empty(), "evidence on {}", f.fragment_id);
            assert_eq!(f.evidence[0].page, Some(1));
            assert!(f.source.is_some(), "typed source on {}", f.fragment_id);
        }
        let ids: Vec<&str> = first.iter().map(|f| f.fragment_id.as_str()).collect();
        let ids2: Vec<&str> = second.iter().map(|f| f.fragment_id.as_str()).collect();
        assert_eq!(ids, ids2, "fragment ids must be deterministic");
        assert_eq!(
            first[0].context.neighboring_fragments,
            vec![first[1].fragment_id.clone()]
        );
    }

    #[test]
    fn fragment_ids_carry_document_hash_prefix() {
        let dm = doc(vec!["Paragraph one.\n\nParagraph two."]);
        let mut ast = document_model_to_ast(&dm);
        ast.document_id = Some("0123456789abcdef".into());
        let frags = RuleBoundaryDetector.detect(&ast).unwrap();

        assert_eq!(frags.len(), 2);
        assert_eq!(frags[0].fragment_id, "frag-01234567-p1-b0");
        assert_eq!(frags[1].fragment_id, "frag-01234567-p1-b1");
        for f in &frags {
            assert_eq!(f.context.document_id.as_deref(), Some("0123456789abcdef"));
        }
        // Neighbor links use the prefixed ids.
        assert_eq!(
            frags[0].context.neighboring_fragments,
            vec!["frag-01234567-p1-b1".to_string()]
        );
        // Deterministic across runs.
        let again = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(frags[0].fragment_id, again[0].fragment_id);
    }

    #[test]
    fn empty_document_yields_no_fragments() {
        let dm = doc(vec![]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        assert!(fragments.is_empty());
    }

    #[test]
    fn visual_node_with_payload_emits_typed_fragment() {
        // PR-F: a Diagram node carrying a payload emits a Diagram fragment
        // (HLD §11: visual structure reaches the semantic leg typed).
        let mut node = AstNode {
            block_type: BlockType::Diagram,
            text: Some("Client -> Gateway".into()),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        node.payload = crate::visual::MockDiagramAnalyzer
            .analyze(&node)
            .map(crate::ast::AstPayload::Diagram);
        let page_node = AstNode {
            block_type: BlockType::Unknown,
            text: None,
            children: vec![node],
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        let ast = DocumentAst {
            page_count: 1,
            pages: vec![page_node],
            source_type: "test".into(),
            document_id: None,
        };

        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].modality, FragmentModality::Diagram);
        assert!(matches!(fragments[0].content, FragmentContent::Diagram(_)));
    }

    #[test]
    fn visual_node_without_payload_falls_back_to_text() {
        // Degraded analyzer (no payload) must never lose content: the
        // figure marker + caption stay as a text fragment.
        let page_node = AstNode {
            block_type: BlockType::Unknown,
            text: None,
            children: vec![AstNode {
                block_type: BlockType::Figure,
                text: Some("Figure 1: fees".into()),
                children: vec![],
                bbox: None,
                confidence: None,
                ..Default::default()
            }],
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        let ast = DocumentAst {
            page_count: 1,
            pages: vec![page_node],
            source_type: "test".into(),
            document_id: None,
        };

        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].modality, FragmentModality::Text);
        match &fragments[0].content {
            FragmentContent::Text(t) => assert!(t.contains("Figure 1: fees")),
            other => panic!("expected text fallback, got {:?}", other),
        }
    }

    #[test]
    fn fragment_serde_roundtrip() {
        let dm = doc(vec!["| A | B |\n| 1 | 2 |"]);
        let ast = document_model_to_ast(&dm);
        let fragments = RuleBoundaryDetector.detect(&ast).unwrap();
        let json = serde_json::to_string(&fragments).unwrap();
        let back: Vec<KnowledgeFragment> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), fragments.len());
        assert_eq!(back[0].fragment_id, fragments[0].fragment_id);
    }

    // ------------------------------------------------------------------
    // PR-H: EmbeddingBoundaryDetector
    //
    // The detector is provider-agnostic; its mechanics are tested with a
    // synthetic provider that gives topics clean, separable vectors (the
    // mock char-ngram provider's cosine band has no topic gap — measured —
    // so it cannot exercise the split layer deterministically).
    // ------------------------------------------------------------------

    /// Topic vectors: payment/billing words → [1,0], holiday/office words
    /// → [0,1], anything else → the zero vector (cosine 0).
    struct ProbeProvider;

    impl EmbeddingProvider for ProbeProvider {
        fn name(&self) -> &str {
            "probe-topics"
        }

        fn dimensions(&self) -> usize {
            2
        }

        fn embed(&self, text: &str) -> Vec<f32> {
            let lower = text.to_lowercase();
            let is_billing = [
                "payment",
                "invoice",
                "billing",
                "processes",
                "validated",
                "settle",
                "ledger",
            ]
            .iter()
            .any(|w| lower.contains(w));
            let is_holiday = [
                "holiday", "office", "vacation", "november", "december", "winter", "closed",
            ]
            .iter()
            .any(|w| lower.contains(w));
            match (is_billing, is_holiday) {
                (true, false) => vec![1.0, 0.0],
                (false, true) => vec![0.0, 1.0],
                _ => vec![0.0, 0.0],
            }
        }
    }

    fn embedding_detector() -> EmbeddingBoundaryDetector<'static> {
        let provider: &'static ProbeProvider = Box::leak(Box::new(ProbeProvider));
        EmbeddingBoundaryDetector::new(provider)
    }

    #[test]
    fn merge_joins_similar_adjacent_text_fragments() {
        let dm = doc(vec![
            "The payment system processes invoices nightly.\n\nInvoices are processed nightly by the payment system.",
        ]);
        let ast = document_model_to_ast(&dm);
        let rule = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(rule.len(), 2, "rule emits one fragment per paragraph");
        let merged = embedding_detector().detect(&ast).unwrap();
        assert_eq!(
            merged.len(),
            1,
            "similar paragraphs merge into one fragment"
        );
        match &merged[0].content {
            FragmentContent::Text(t) => {
                assert!(t.contains("processes invoices nightly"));
                assert!(t.contains("processed nightly"));
            }
            other => panic!("expected text content, got {:?}", other),
        }
    }

    #[test]
    fn merge_keeps_dissimilar_fragments_separate() {
        let dm = doc(vec![
            "The payment system processes invoices nightly.\n\nThe company cafeteria serves lunch at noon.",
        ]);
        let ast = document_model_to_ast(&dm);
        let fragments = embedding_detector().detect(&ast).unwrap();
        assert_eq!(fragments.len(), 2, "different topics stay separate");
    }

    #[test]
    fn split_cuts_long_text_at_semantic_shift() {
        // Two topics in one long paragraph: payment processing, then a
        // hard topic shift into holiday scheduling.
        let dm = doc(vec![concat!(
            "The payment system processes invoices nightly. ",
            "Invoices are validated by the ledger team every evening. ",
            "Payments settle through the billing pipeline. ",
            "The office is closed for the winter holidays. ",
            "Team vacations must be approved by December first. ",
            "Holiday schedules are published in November. "
        )]);
        let ast = document_model_to_ast(&dm);
        let rule = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(rule.len(), 1, "rule leaves the paragraph whole");
        let split = embedding_detector().detect(&ast).unwrap();
        assert_eq!(split.len(), 2, "topic shift splits the fragment");
        assert_eq!(split[0].fragment_id, "frag-p1-b0-s0");
        assert_eq!(split[1].fragment_id, "frag-p1-b0-s1");
        match &split[0].content {
            FragmentContent::Text(t) => {
                assert!(t.contains("billing pipeline"));
                assert!(!t.contains("holidays"));
            }
            other => panic!("expected text content, got {:?}", other),
        }
        assert_eq!(
            split[0].context.neighboring_fragments,
            vec!["frag-p1-b0-s1".to_string()]
        );
        // Split pieces re-stamp their provenance.
        assert!(split.iter().all(|f| f
            .evidence
            .iter()
            .all(|e| e.extractor == "embedding_boundary")));
    }

    #[test]
    fn digit_prefixed_sentences_do_not_fragment_list_items() {
        let text = "1. First item is here. 2. Second item is there. 3. Final item is everywhere.";
        let units = sentences(text);
        assert_eq!(units.len(), 3, "one unit per list item");
        assert_eq!(units[0], "1. First item is here.");
        assert_eq!(units[1], "2. Second item is there.");
    }

    #[test]
    fn short_text_is_never_split() {
        let dm = doc(vec![
            "Short paragraph under the split gate. It stays whole.",
        ]);
        let ast = document_model_to_ast(&dm);
        let fragments = embedding_detector().detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1);
    }

    #[test]
    fn embedding_detector_is_deterministic() {
        let dm = doc(vec![concat!(
            "The payment system processes invoices nightly. ",
            "Invoices are validated by the ledger team every evening. ",
            "The office is closed for the winter holidays. ",
            "Holiday schedules are published in November. "
        )]);
        let ast = document_model_to_ast(&dm);
        let first = embedding_detector().detect(&ast).unwrap();
        let second = embedding_detector().detect(&ast).unwrap();
        let ids1: Vec<&str> = first.iter().map(|f| f.fragment_id.as_str()).collect();
        let ids2: Vec<&str> = second.iter().map(|f| f.fragment_id.as_str()).collect();
        assert_eq!(ids1, ids2);
        assert!(!ids1.is_empty());
    }

    // ------------------------------------------------------------------
    // PR-I: HybridBoundaryDetector
    // ------------------------------------------------------------------

    use crate::ast::{AstPayload, ImagePayload};
    use crate::source::{SourceSpan, VisualAssetRef};

    /// Always returns the same score — for testing the policy, not the
    /// scorer itself.
    struct ForcingScorer {
        probability: f32,
    }

    impl BoundaryScorer for ForcingScorer {
        fn score_boundary(
            &self,
            _prev: &KnowledgeFragment,
            _next: &KnowledgeFragment,
        ) -> Option<BoundaryScore> {
            Some(BoundaryScore {
                probability: self.probability,
                model: "mock-transformer".into(),
            })
        }
    }

    fn hybrid_detector() -> HybridBoundaryDetector<'static> {
        let provider: &'static ProbeProvider = Box::leak(Box::new(ProbeProvider));
        HybridBoundaryDetector::new(provider)
    }

    /// One page with an Image block (billing caption payload) followed by a
    /// Text block — the figure+caption adjacency the modality layer merges.
    fn figure_plus_text_ast(text: &str) -> DocumentAst {
        let image = AstNode {
            block_type: BlockType::Image,
            text: Some("Figure 1: payment flow".into()),
            children: vec![],
            bbox: None,
            confidence: None,
            payload: Some(AstPayload::Image(ImagePayload {
                asset: VisualAssetRef {
                    asset_id: "asset-1".into(),
                    mime_type: "image/png".into(),
                    content_hash: "deadbeef".into(),
                    source: SourceSpan {
                        document_id: None,
                        page: 1,
                        start_offset: None,
                        end_offset: None,
                        bbox: None,
                        node_id: None,
                    },
                },
                ocr_text: None,
                ocr_model: None,
                caption: Some("Figure 1: payment flow".into()),
                detected_objects: vec![],
                visual_embedding: None,
            })),
            ..Default::default()
        };
        let text_node = AstNode {
            block_type: BlockType::Unknown,
            text: Some(text.to_string()),
            children: vec![],
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        let page_node = AstNode {
            block_type: BlockType::Unknown,
            text: None,
            children: vec![image, text_node],
            bbox: None,
            confidence: None,
            ..Default::default()
        };
        DocumentAst {
            page_count: 1,
            pages: vec![page_node],
            source_type: "test".into(),
            document_id: None,
        }
    }

    #[test]
    fn hybrid_merges_figure_with_caption_text_into_mixed() {
        let ast = figure_plus_text_ast("The payment flow processes invoices nightly.");
        let fragments = hybrid_detector().detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1, "figure + same-topic text merge");
        assert_eq!(fragments[0].modality, FragmentModality::Mixed);
        assert_eq!(
            fragments[0].fragment_id, "frag-p1-b0",
            "merged unit keeps the first fragment's id"
        );
        match &fragments[0].content {
            FragmentContent::Mixed(children) => assert_eq!(children.len(), 2),
            other => panic!("expected Mixed, got {:?}", other),
        }
        let rendered = crate::chunking::fragment_text(&fragments[0]);
        assert!(rendered.contains("payment flow"), "figure caption inside");
        assert!(rendered.contains("processes invoices"), "text inside");
        assert!(fragments[0]
            .evidence
            .iter()
            .all(|e| e.extractor == "hybrid_boundary"));
    }

    #[test]
    fn hybrid_keeps_unrelated_figure_and_text_separate() {
        let ast = figure_plus_text_ast("The company cafeteria serves lunch at noon.");
        let fragments = hybrid_detector().detect(&ast).unwrap();
        assert_eq!(fragments.len(), 2, "unrelated topics stay separate");
        assert_eq!(fragments[0].modality, FragmentModality::Image);
        assert_eq!(fragments[1].modality, FragmentModality::Text);
    }

    #[test]
    fn hybrid_splits_long_text_at_sentence_ceiling() {
        // Five ~205-char billing sentences in one paragraph (1025 chars):
        // above the 800-char ceiling, the linguistic layer cuts at sentence
        // boundaries into 3+2 sentences; the semantic layer leaves the
        // pieces whole (same topic).
        let sentence = "The payment system processes invoices nightly and validates each transaction against the ledger before the settlement job runs and publishes a billing report to the finance team for review every single evening. ";
        let dm = doc(vec![&sentence.repeat(5)]);
        let ast = document_model_to_ast(&dm);
        let rule = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(rule.len(), 1, "rule leaves the paragraph whole");
        let fragments = hybrid_detector().detect(&ast).unwrap();
        assert_eq!(fragments.len(), 2, "ceiling cuts at sentence boundaries");
        assert_eq!(fragments[0].fragment_id, "frag-p1-b0-l0");
        assert_eq!(fragments[1].fragment_id, "frag-p1-b0-l1");
        for f in &fragments {
            if let FragmentContent::Text(t) = &f.content {
                assert!(
                    t.chars().count() <= 800,
                    "ceiling pieces must fit max_chars"
                );
            }
        }
        assert_eq!(
            fragments[0].context.neighboring_fragments,
            vec!["frag-p1-b0-l1".to_string()]
        );
        assert!(fragments
            .iter()
            .all(|f| f.evidence.iter().all(|e| e.extractor == "hybrid_boundary")));
    }

    #[test]
    fn hybrid_merges_similar_text_paragraphs_as_text() {
        let dm = doc(vec![
            "The payment system processes invoices nightly.\n\nInvoices are processed nightly by the payment system.",
        ]);
        let ast = document_model_to_ast(&dm);
        let fragments = hybrid_detector().detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1, "similar paragraphs merge");
        assert_eq!(
            fragments[0].modality,
            FragmentModality::Text,
            "Text+Text joins as text, not Mixed"
        );
    }

    #[test]
    fn hybrid_transformer_score_forces_merge_below_semantic_threshold() {
        // Billing vs holiday: cosine 0.0 — below the semantic threshold.
        // A strong transformer opinion (0.9 ≥ TRANSFORMER_ACCEPT) dissolves
        // the boundary; the deciding model is recorded on the evidence.
        let dm = doc(vec![
            "The payment system processes invoices nightly.\n\nThe office is closed for the winter holidays.",
        ]);
        let ast = document_model_to_ast(&dm);
        let provider: &'static ProbeProvider = Box::leak(Box::new(ProbeProvider));
        let scorer: &'static ForcingScorer =
            Box::leak(Box::new(ForcingScorer { probability: 0.9 }));
        let detector = HybridBoundaryDetector::new(provider).with_scorer(scorer);
        let fragments = detector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1, "strong score dissolves the boundary");
        assert!(fragments[0]
            .evidence
            .iter()
            .all(|e| e.extractor == "hybrid_boundary"));
        assert!(fragments[0]
            .evidence
            .iter()
            .any(|e| e.model.as_deref() == Some("mock-transformer")));
    }

    #[test]
    fn weak_transformer_score_leaves_boundary_intact() {
        let dm = doc(vec![
            "The payment system processes invoices nightly.\n\nThe office is closed for the winter holidays.",
        ]);
        let ast = document_model_to_ast(&dm);
        let provider: &'static ProbeProvider = Box::leak(Box::new(ProbeProvider));
        let scorer: &'static ForcingScorer =
            Box::leak(Box::new(ForcingScorer { probability: 0.2 }));
        let detector = HybridBoundaryDetector::new(provider).with_scorer(scorer);
        let fragments = detector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 2, "weak score leaves the boundary alone");
    }

    #[test]
    fn hybrid_detector_is_deterministic() {
        let ast = figure_plus_text_ast("The payment flow processes invoices nightly.");
        let first = hybrid_detector().detect(&ast).unwrap();
        let second = hybrid_detector().detect(&ast).unwrap();
        let ids1: Vec<&str> = first.iter().map(|f| f.fragment_id.as_str()).collect();
        let ids2: Vec<&str> = second.iter().map(|f| f.fragment_id.as_str()).collect();
        assert_eq!(ids1, ids2);
        assert!(!ids1.is_empty());
    }

    // ------------------------------------------------------------------
    // PR-J: TransformerBoundaryDetector
    // ------------------------------------------------------------------

    /// Never has an opinion — the "no transformer configured" seam.
    struct NoOpinionScorer;

    impl BoundaryScorer for NoOpinionScorer {
        fn score_boundary(
            &self,
            _prev: &KnowledgeFragment,
            _next: &KnowledgeFragment,
        ) -> Option<BoundaryScore> {
            None
        }
    }

    fn forcing_scorer(probability: f32) -> &'static ForcingScorer {
        Box::leak(Box::new(ForcingScorer { probability }))
    }

    #[test]
    fn transformer_merges_when_score_above_accept() {
        let dm = doc(vec![
            "The payment system processes invoices nightly.\n\nThe payment flow handles invoices every night.",
        ]);
        let ast = document_model_to_ast(&dm);
        let detector = TransformerBoundaryDetector::new(forcing_scorer(0.9));
        let fragments = detector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1, "strong score dissolves the boundary");
        assert_eq!(fragments[0].modality, FragmentModality::Text);
        assert!(fragments[0]
            .evidence
            .iter()
            .all(|e| e.extractor == "transformer_boundary"));
        assert!(fragments[0]
            .evidence
            .iter()
            .any(|e| e.model.as_deref() == Some("mock-transformer")));
    }

    #[test]
    fn transformer_keeps_boundary_below_accept() {
        let dm = doc(vec![
            "The payment system processes invoices nightly.\n\nThe office is closed for the winter holidays.",
        ]);
        let ast = document_model_to_ast(&dm);
        let detector = TransformerBoundaryDetector::new(forcing_scorer(0.2));
        let fragments = detector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 2, "weak score leaves the boundary alone");
    }

    #[test]
    fn transformer_no_opinion_keeps_rule_boundaries() {
        // No scorer opinion → the detector degrades to the rule detector
        // (DoD row 10: transformer optional).
        let dm = doc(vec![
            "The payment system processes invoices nightly.\n\nThe office is closed for the winter holidays.",
        ]);
        let ast = document_model_to_ast(&dm);
        let detector = TransformerBoundaryDetector::new(&NoOpinionScorer);
        let fragments = detector.detect(&ast).unwrap();
        let rule = RuleBoundaryDetector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), rule.len());
        assert_eq!(fragments[0].fragment_id, rule[0].fragment_id);
        assert_eq!(fragments[1].fragment_id, rule[1].fragment_id);
        assert!(fragments[0]
            .evidence
            .iter()
            .all(|e| e.extractor != "transformer_boundary"));
    }

    #[test]
    fn transformer_merges_figure_with_caption_text_into_mixed() {
        let ast = figure_plus_text_ast("The payment flow processes invoices nightly.");
        let detector = TransformerBoundaryDetector::new(forcing_scorer(0.9));
        let fragments = detector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 1, "scorer joins figure + caption");
        assert_eq!(fragments[0].modality, FragmentModality::Mixed);
        let rendered = crate::chunking::fragment_text(&fragments[0]);
        assert!(rendered.contains("payment flow"), "figure caption inside");
        assert!(rendered.contains("processes invoices"), "text inside");
    }

    #[test]
    fn transformer_ceiling_bounds_long_text() {
        // Same 1025-char paragraph as the hybrid ceiling test; the scorer
        // has no opinion, so only the linguistic layer cuts.
        let sentence = "The payment system processes invoices nightly and validates each transaction against the ledger before the settlement job runs and publishes a billing report to the finance team for review every single evening. ";
        let dm = doc(vec![&sentence.repeat(5)]);
        let ast = document_model_to_ast(&dm);
        let detector = TransformerBoundaryDetector::new(&NoOpinionScorer);
        let fragments = detector.detect(&ast).unwrap();
        assert_eq!(fragments.len(), 2, "ceiling cuts at sentence boundaries");
        assert_eq!(fragments[0].fragment_id, "frag-p1-b0-l0");
        assert_eq!(fragments[1].fragment_id, "frag-p1-b0-l1");
        for f in &fragments {
            if let FragmentContent::Text(t) = &f.content {
                assert!(
                    t.chars().count() <= 800,
                    "ceiling pieces must fit max_chars"
                );
            }
        }
        assert!(fragments.iter().all(|f| f
            .evidence
            .iter()
            .all(|e| e.extractor == "transformer_boundary")));
    }

    #[test]
    fn transformer_detector_is_deterministic() {
        let ast = figure_plus_text_ast("The payment flow processes invoices nightly.");
        let detector = TransformerBoundaryDetector::new(forcing_scorer(0.9));
        let first = detector.detect(&ast).unwrap();
        let second = detector.detect(&ast).unwrap();
        let ids1: Vec<&str> = first.iter().map(|f| f.fragment_id.as_str()).collect();
        let ids2: Vec<&str> = second.iter().map(|f| f.fragment_id.as_str()).collect();
        assert_eq!(ids1, ids2);
        assert!(!ids1.is_empty());
    }
}
