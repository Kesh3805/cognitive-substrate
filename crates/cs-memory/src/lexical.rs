//! The shared lexical contract: one tokenizer, used for BM25 scoring and
//! for lexical candidate generation in every store, so stores cannot
//! disagree about which records share terms with a query.

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "did", "do", "does", "for", "from", "has",
    "have", "how", "in", "is", "it", "its", "of", "on", "or", "that", "the", "this", "to", "was",
    "we", "what", "when", "where", "which", "who", "why", "with",
];

/// Version of [`tokenize`]. Stored index terms carry no version of their
/// own: changing the tokenizer requires recomputing them.
pub const TOKENIZER_VERSION: u32 = 1;

/// Lowercased alphanumeric tokens, stopwords removed.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// Distinct tokens, sorted: what a record or query is indexed by.
pub fn terms(text: &str) -> Vec<String> {
    let mut t = tokenize(text);
    t.sort();
    t.dedup();
    t
}

/// Number of distinct terms two sorted term lists share.
pub fn overlap(a: &[String], b: &[String]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_are_distinct_sorted_and_stopword_free() {
        assert_eq!(
            terms("What does the API depend on? The API!"),
            ["api", "depend"]
        );
        assert_eq!(overlap(&terms("api auth db"), &terms("auth api cache")), 2);
    }
}
