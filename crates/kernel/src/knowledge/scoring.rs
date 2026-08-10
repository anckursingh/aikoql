//! Knowledge scoring helpers — cosine similarity, Jaccard, tokenization.
//!
//! These are fundamental knowledge-operations used by both the kernel's
//! IndexCoordinator and the Vector Engine crate. They live here (not in any
//! engine) because they operate on KOM types only.

use crate::knowledge::kom::*;
use std::collections::BTreeSet;

/// Cosine similarity between two equal-length non-zero vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Tokenize text into a set of lowercased alphanumeric tokens.
pub fn tokenize(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Jaccard similarity between two token sets.
pub fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    inter / union
}

/// Extract searchable text from a KnowledgeObject's properties and semantic summary.
pub fn ko_text(ko: &KnowledgeObject) -> String {
    let mut s = String::new();
    for v in ko.properties.values() {
        collect_text(v, &mut s);
    }
    if let Some(sem) = &ko.semantic {
        if let Some(sum) = &sem.summary {
            s.push(' ');
            s.push_str(sum);
        }
    }
    s
}

fn collect_text(v: &Value, out: &mut String) {
    match v {
        Value::Text(t) => {
            out.push(' ');
            out.push_str(t);
        }
        Value::List(xs) => {
            for x in xs {
                collect_text(x, out);
            }
        }
        Value::Map(m) => {
            for x in m.values() {
                collect_text(x, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    }

    #[test]
    fn jaccard_half_overlap() {
        let a = BTreeSet::from(["a".into(), "b".into()]);
        let b = BTreeSet::from(["b".into(), "c".into()]);
        assert!((jaccard(&a, &b) - 1.0 / 3.0).abs() < 1e-6);
    }
}
