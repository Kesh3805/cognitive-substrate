//! Deterministic lexical scoring (BM25 over the candidate set).

use std::collections::{BTreeMap, BTreeSet};

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "did", "do", "does", "for", "from", "has",
    "have", "how", "in", "is", "it", "its", "of", "on", "or", "that", "the", "this", "to", "was",
    "we", "what", "when", "where", "which", "who", "why", "with",
];

/// Lowercased alphanumeric tokens, stopwords removed.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// BM25 scores of `docs` for `query`, normalized to `[0, 1]` by the maximum
/// score in the set. Returns all zeros when nothing matches.
pub fn bm25_normalized(query: &str, docs: &[&str]) -> Vec<f64> {
    let query_terms: BTreeSet<String> = tokenize(query).into_iter().collect();
    if docs.is_empty() || query_terms.is_empty() {
        return vec![0.0; docs.len()];
    }

    let tokenized: Vec<Vec<String>> = docs.iter().map(|d| tokenize(d)).collect();
    let n = tokenized.len() as f64;
    let avgdl = (tokenized.iter().map(Vec::len).sum::<usize>() as f64 / n).max(1.0);

    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for doc in &tokenized {
        let unique: BTreeSet<&str> = doc.iter().map(String::as_str).collect();
        for term in &query_terms {
            if unique.contains(term.as_str()) {
                *df.entry(term.as_str()).or_default() += 1;
            }
        }
    }

    let raw: Vec<f64> = tokenized
        .iter()
        .map(|doc| {
            let dl = doc.len() as f64;
            query_terms
                .iter()
                .map(|term| {
                    let tf = doc.iter().filter(|t| *t == term).count() as f64;
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let dfi = *df.get(term.as_str()).unwrap_or(&0) as f64;
                    let idf = (1.0 + (n - dfi + 0.5) / (dfi + 0.5)).ln();
                    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * dl / avgdl))
                })
                .sum()
        })
        .collect();

    let max = raw.iter().copied().fold(0.0_f64, f64::max);
    if max <= 0.0 {
        vec![0.0; raw.len()]
    } else {
        raw.into_iter().map(|s| s / max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_drops_stopwords_and_punctuation() {
        assert_eq!(
            tokenize("Why did we choose PostgreSQL?"),
            vec!["choose", "postgresql"]
        );
    }

    #[test]
    fn matching_doc_scores_highest() {
        let s = bm25_normalized(
            "postgres vector search",
            &[
                "we use postgres with pgvector for vector search",
                "the cat sat on the mat",
            ],
        );
        assert!((s[0] - 1.0).abs() < 1e-12);
        assert_eq!(s[1], 0.0);
    }

    #[test]
    fn no_match_is_all_zero() {
        assert_eq!(bm25_normalized("zebra", &["alpha", "beta"]), vec![0.0, 0.0]);
    }
}
