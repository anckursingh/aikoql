//! Retrieved facts: rendering, secret scrubbing, bounded context, and the
//! tenant-scoped retrieval fallback.

use crate::{json, Kernel};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fact {
    pub koid: String,
    pub statement: String,
}

pub(crate) const MAX_FACTS: usize = 8;
pub(crate) const MAX_CONTEXT_CHARS: usize = 2000;
/// One property value never takes more than this, so a long `mentions` dump
/// cannot swallow the whole budget.
const MAX_VALUE_CHARS: usize = 200;

/// The most of one passage (a row's `text`) that is shown: long enough for
/// the sentence that answers, short enough that several passages fit the
/// budget. A whole PDF page took all of it.
const MAX_TEXT_CHARS: usize = 480;

/// Spacing collapsed: PDF text keeps the page layout (a line break and
/// padding after every word), which cost a fifth of the budget per word.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A word as a search term: lowercase, edge punctuation trimmed.
fn term(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Question words that say nothing about where the answer is: they are in
/// nearly every passage, so counted as terms they pull the window anywhere.
const FILLER: &[&str] = &[
    "the", "and", "are", "was", "were", "has", "have", "had", "does", "did", "who", "whom",
    "whose", "what", "which", "when", "where", "why", "how", "many", "much", "this", "that",
    "these", "those", "with", "from", "for", "into", "about", "say", "says", "said", "there",
    "their", "they", "them", "his", "her", "its", "our", "you", "your", "can", "could", "would",
    "should", "will", "not", "any", "all", "one", "book", "text",
];

/// The question's search words: three characters or more, filler left out.
fn terms(question: &str) -> BTreeSet<String> {
    question
        .split_whitespace()
        .map(term)
        .filter(|w| w.chars().count() >= 3 && !FILLER.contains(&w.as_str()))
        .collect()
}

/// At most `MAX_TEXT_CHARS` of `text`, centred on the question: find the
/// run of words holding the most distinct question terms (the earliest on a
/// tie, so it is deterministic), then grow a window from the middle of its
/// matches outward, one word each side in turn. Each cut side is marked
/// with an ellipsis.
fn window(text: &str, terms: &BTreeSet<String>) -> String {
    let text = collapse(text);
    if text.chars().count() <= MAX_TEXT_CHARS {
        return text;
    }
    let words: Vec<&str> = text.split(' ').collect();
    let width = |w: &str| w.chars().count() + 1;
    let hit = |w: &str| terms.contains(&term(w));
    let mut best: Option<(usize, usize, usize)> = None; // (distinct hits, start, end)
    for start in 0..words.len() {
        let (mut end, mut len) = (start, 0);
        while end < words.len() && len + width(words[end]) <= MAX_TEXT_CHARS {
            len += width(words[end]);
            end += 1;
        }
        let distinct = words[start..end]
            .iter()
            .filter(|w| hit(w))
            .map(|w| term(w))
            .collect::<BTreeSet<_>>()
            .len();
        if best.is_none_or(|(h, _, _)| distinct > h) {
            best = Some((distinct, start, end));
        }
        if end == words.len() {
            break; // a later start only shrinks the run
        }
    }
    let (_, run_start, run_end) = best.unwrap_or((0, 0, words.len()));
    let matched: Vec<usize> = (run_start..run_end).filter(|&i| hit(words[i])).collect();
    let centre = match (matched.first(), matched.last()) {
        (Some(&a), Some(&b)) => (a + b) / 2,
        _ => run_start,
    };
    let (mut start, mut end, mut len) = (centre, centre + 1, width(words[centre]));
    loop {
        let grow_right = end < words.len() && len + width(words[end]) <= MAX_TEXT_CHARS;
        if grow_right {
            len += width(words[end]);
            end += 1;
        }
        let grow_left = start > 0 && len + width(words[start - 1]) <= MAX_TEXT_CHARS;
        if grow_left {
            start -= 1;
            len += width(words[start]);
        }
        if !grow_right && !grow_left {
            break;
        }
    }
    let mut out = words[start..end].join(" ");
    if start > 0 {
        out = format!("\u{2026}{out}");
    }
    if end < words.len() {
        out.push('\u{2026}');
    }
    out
}

/// Ingestion bookkeeping that tells a model nothing about the subject.
fn is_internal(key: &str) -> bool {
    matches!(key, "confidence" | "type_hint") || key.starts_with("evidence_")
}

/// Render result rows as facts for `question`. Rows without a koid or any
/// content are skipped, and so is a passage already shown (a page and its
/// chunk carry the same text); a long passage is cut to the window around
/// the question's words; every statement is secret-scrubbed; the total is
/// bounded.
pub(crate) fn facts_from_rows(rows: &serde_json::Value, question: &str) -> Vec<Fact> {
    let terms = terms(question);
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut budget = MAX_CONTEXT_CHARS;
    for row in rows.as_array().into_iter().flatten() {
        if out.len() == MAX_FACTS || budget == 0 {
            break;
        }
        let Some(koid) = row.get("koid").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some((statement, passage)) = render(row, &terms) else {
            continue;
        };
        if passage.is_some_and(|p| !seen.insert(p)) {
            continue;
        }
        let statement: String = aikoql_ingestion::redact_text(&statement)
            .chars()
            .take(budget)
            .collect();
        budget -= statement.chars().count();
        out.push(Fact {
            koid: koid.to_string(),
            statement,
        });
    }
    out
}

/// The statement, and for a passage of a document its duplicate key: the
/// document and the whole text (a page and its chunk carry the same text;
/// two documents, or two tenants, that say the same thing stay apart).
fn render(row: &serde_json::Value, terms: &BTreeSet<String>) -> Option<(String, Option<String>)> {
    let ty = row.get("type_name").and_then(|v| v.as_str()).unwrap_or("");
    let props = row
        .get("properties")?
        .as_object()
        .filter(|p| !p.is_empty())?;
    let text = |v: &serde_json::Value| {
        let t = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if t.chars().count() > MAX_VALUE_CHARS {
            format!("{}…", t.chars().take(MAX_VALUE_CHARS).collect::<String>())
        } else {
            t
        }
    };
    let label = ["name", "title", "label"]
        .iter()
        .find_map(|k| props.get(*k).and_then(|v| v.as_str()));
    if label.is_none() {
        if let Some(t) = props.get("text").and_then(|v| v.as_str()) {
            let key = props
                .get("document")
                .and_then(|d| d.as_str())
                .map(|d| format!("{d}\u{0}{}", collapse(t)));
            return Some((format!("{ty}: {}", window(t, terms)), key));
        }
    }
    let mut body: Vec<String> = props
        .iter()
        .filter(|(k, _)| !is_internal(k))
        .map(|(k, v)| format!("{k}={}", text(v)))
        .collect();
    body.sort();
    let statement = match label {
        Some(l) => format!("{ty} {l}: {}", body.join("; ")),
        None => format!("{ty}: {}", body.join("; ")),
    };
    Some((collapse(&statement), None))
}

/// Text/vector search over what the caller may see, confined to `tenant`
/// (None = unscoped, as `execute_readonly`). The kernel search is not
/// tenant-aware, so hits are over-fetched and filtered here.
pub(crate) fn retrieve_fallback(
    k: &Kernel,
    subject: &str,
    roles: &[String],
    tenant: Option<&str>,
    question: &str,
) -> Vec<Fact> {
    let ctx =
        crate::session::subject_of(&json!({"subject": subject, "roles": roles, "tenant": tenant}));
    let hits = k.find_similar(crate::SimilarityQuery {
        context: ctx.into(),
        filter: None,
        text: Some(question.to_string()),
        vector: None,
        embedding_model: None,
        k: MAX_FACTS * 4,
        fusion: crate::helpers::parse_fusion(&json!({})),
    });
    let Ok(hits) = hits else {
        return Vec::new();
    };
    let rows: Vec<serde_json::Value> = hits
        .iter()
        .filter(|h| h.score > 0.0)
        .map(|h| &h.ko)
        .filter(|ko| !ko.metadata.type_name.starts_with("aikoql:"))
        .filter(|ko| tenant.is_none_or(|t| ko.metadata.tenant.as_deref() == Some(t)))
        .map(|ko| {
            json!({
                "koid": ko.koid.to_hex(),
                "type_name": ko.metadata.type_name,
                "properties": ko.properties.iter()
                    .map(|(k, v)| (k.clone(), crate::helpers::value_to_json(v)))
                    .collect::<serde_json::Map<_, _>>(),
            })
        })
        .collect();
    facts_from_rows(&json!(rows), question)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        json, ExtensionMap, Metadata, Origin, PropertyMap, ReferentialPolicy, RememberRequest,
        Value,
    };
    use aikoql_kernel::{ManualClock, MemoryEngine, TokenTextIndex};
    use std::sync::Arc;
    use std::time::Duration;

    fn row(koid: &str, ty: &str, props: serde_json::Value) -> serde_json::Value {
        json!({"koid": koid, "type_name": ty, "properties": props})
    }

    #[test]
    fn facts_render_type_label_and_properties() {
        let f = facts_from_rows(
            &json!([row(
                "k1",
                "Person",
                json!({"name": "Ada", "city": "London"})
            )]),
            "",
        );
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].koid, "k1");
        assert_eq!(f[0].statement, "Person Ada: city=London; name=Ada");
    }

    #[test]
    fn facts_render_chunk_text() {
        let f = facts_from_rows(
            &json!([row(
                "k1",
                "DocChunk",
                json!({"text": "Refunds take 5 days."})
            )]),
            "",
        );
        assert_eq!(f[0].statement, "DocChunk: Refunds take 5 days.");
    }

    #[test]
    fn facts_drop_internal_metadata_and_cap_long_values() {
        let long = "word ".repeat(200);
        let f = facts_from_rows(
            &json!([row(
                "k1",
                "Thing",
                json!({"name": "Ada", "confidence": 0.85, "type_hint": "Person",
                   "evidence_model": "rules-v1", "evidence_document": "/x/y",
                   "mentions": long})
            )]),
            "",
        );
        let st = &f[0].statement;
        for noise in ["confidence", "type_hint", "evidence_"] {
            assert!(!st.contains(noise), "{st}");
        }
        assert!(st.starts_with("Thing Ada: "), "{st}");
        assert!(st.contains("mentions=word word"), "{st}");
        assert!(st.chars().count() < 260, "{} chars", st.chars().count());
        assert!(st.contains('…'), "a cut value is marked: {st}");
    }

    #[test]
    fn facts_are_scrubbed() {
        let f = facts_from_rows(
            &json!([row(
                "k1",
                "Note",
                json!({"body": "AKIAIOSFODNN7EXAMPLE", "region": "Asia Pacific"})
            )]),
            "",
        );
        assert!(!f[0].statement.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(f[0].statement.contains("Asia Pacific"));
    }

    #[test]
    fn facts_are_capped_to_8_and_2000_chars() {
        let big = "é".repeat(500);
        let rows: Vec<_> = (0..20)
            .map(|i| row(&format!("k{i}"), "Note", json!({"text": big})))
            .collect();
        let f = facts_from_rows(&json!(rows), "");
        assert!(f.len() <= MAX_FACTS && !f.is_empty());
        let total: usize = f.iter().map(|x| x.statement.chars().count()).sum();
        assert!(total <= MAX_CONTEXT_CHARS, "total {total}");
    }

    /// A PDF page is a ~3,600-character passage padded with the layout's
    /// line breaks: shown whole it took the entire 2,000-character budget,
    /// so the model saw one page — and only when search ranked the right
    /// one first. Long text is shown as the window around the question's
    /// words, with spacing collapsed, so several passages fit.
    #[test]
    fn long_text_is_shown_as_the_window_around_the_question() {
        let filler = |n: usize| "Indra \n  \n hymn verse ".repeat(n);
        let page = format!(
            "{} A THOUSAND heads hath Purusa, a thousand eyes, a thousand feet. {}",
            filler(80),
            filler(80)
        );
        let rows: Vec<_> = (0..4)
            .map(|i| {
                row(
                    &format!("k{i}"),
                    "DocumentChunk",
                    json!({"text": page.clone() + &i.to_string()}),
                )
            })
            .collect();
        let f = facts_from_rows(&json!(rows), "How many heads does Purusa have?");
        assert!(f.len() >= 3, "several passages fit the budget: {}", f.len());
        let st = &f[0].statement;
        assert!(
            st.contains("A THOUSAND heads hath Purusa, a thousand eyes"),
            "the window holds the question's words: {st}"
        );
        assert!(
            !st.contains("  ") && !st.contains('\n'),
            "spacing is collapsed: {st:?}"
        );
        assert!(
            st.contains("a thousand feet.") && st.starts_with("DocumentChunk: …"),
            "centred: the sentence whole, cut on both sides: {st}"
        );
        assert!(st.ends_with('…'), "a cut passage is marked: {st}");
    }

    /// A page and its chunk carry the same text: showing both spent the
    /// budget twice on one passage.
    /// "does" and "have" are in nearly every passage: counted as question
    /// terms they pulled the Purusa window onto the wrong verse on the Vedas
    /// test. Only the question's content words choose the window.
    #[test]
    fn the_window_follows_content_words_not_filler() {
        let filler = "they does have does have it does have ".repeat(30);
        let other = "cattle were born and horses ".repeat(30);
        let page =
            format!("{filler} {other} A THOUSAND heads hath Purusa, a thousand eyes. {other}");
        let f = facts_from_rows(
            &json!([row("k1", "DocumentChunk", json!({"text": page}))]),
            "How many heads does Purusa have?",
        );
        assert!(
            f[0].statement.contains("heads hath Purusa"),
            "the window is on the content words: {}",
            f[0].statement
        );
    }

    #[test]
    fn the_same_passage_is_shown_once() {
        let text = "On the instigation of the god Savitr, through Brhaspati.";
        let f = facts_from_rows(
            &json!([
                row(
                    "p1",
                    "DocumentPage",
                    json!({"text": text, "document": "d1"})
                ),
                row(
                    "c1",
                    "DocumentChunk",
                    json!({"text": format!("  {text}\n"), "document": "d1"})
                ),
                row(
                    "p2",
                    "DocumentPage",
                    json!({"text": text, "document": "d2"})
                ),
                row("n1", "Note", json!({"text": "something else"})),
            ]),
            "Savitr",
        );
        let koids: Vec<_> = f.iter().map(|x| x.koid.as_str()).collect();
        assert_eq!(koids, ["p1", "p2", "n1"], "one per document");
    }

    #[test]
    fn facts_skip_rows_without_koid() {
        let f = facts_from_rows(
            &json!([
                {"properties": {"a": "b"}},
                row("k2", "Note", json!({"a": "b"}))
            ]),
            "",
        );
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].koid, "k2");
    }

    fn mk() -> (Arc<Kernel>, String, String) {
        let k = Arc::new(
            Kernel::open(
                Arc::new(MemoryEngine::new()),
                Arc::new(ManualClock::new(10_000)),
                0xBEEF,
            )
            .unwrap(),
        );
        let m = aikoql_scheduler::IndexMaintainer::start(
            &k,
            Arc::new(aikoql_kernel::BruteForceVectorIndex::new()),
            Arc::new(TokenTextIndex::new()),
        )
        .unwrap();
        k.attach_indexes(m.clone());
        let mut ids = Vec::new();
        for tenant in ["acme", "globex"] {
            let mut props = PropertyMap::new();
            props.insert("text".into(), Value::Text("zebra policy applies".into()));
            let r = k
                .remember(RememberRequest {
                    context: crate::session::subject_of(&json!({"subject": "alice"})).into(),
                    koid: None,
                    expected_version: Some(0),
                    idempotency_key: None,
                    metadata: Metadata {
                        type_name: "Note".into(),
                        tenant: Some(tenant.into()),
                        schema_version: 1,
                        tags: vec![],
                    },
                    properties: props,
                    semantic: None,
                    relationships: vec![],
                    security: None,
                    extensions: ExtensionMap::new(),
                    origin: Origin::Human,
                    note: None,
                    referential_policy: ReferentialPolicy::default(),
                })
                .unwrap();
            ids.push(r.koid.to_hex());
        }
        m.wait_caught_up(&k, Duration::from_secs(5)).unwrap();
        (k, ids[0].clone(), ids[1].clone())
    }

    #[test]
    fn fallback_finds_a_tenant_fact_and_never_another_tenants() {
        let (k, acme, globex) = mk();
        let a = retrieve_fallback(&k, "alice", &[], Some("acme"), "zebra policy");
        assert_eq!(
            a.iter().map(|f| f.koid.clone()).collect::<Vec<_>>(),
            std::slice::from_ref(&acme)
        );
        let g = retrieve_fallback(&k, "alice", &[], Some("globex"), "zebra policy");
        assert_eq!(
            g.iter().map(|f| f.koid.clone()).collect::<Vec<_>>(),
            std::slice::from_ref(&globex)
        );
        let all = retrieve_fallback(&k, "alice", &["admin".to_string()], None, "zebra policy");
        let ids: Vec<_> = all.iter().map(|f| f.koid.clone()).collect();
        assert!(ids.contains(&acme) && ids.contains(&globex), "{ids:?}");
    }
}
