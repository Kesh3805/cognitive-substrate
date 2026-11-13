//! Perception: turning raw observations into candidate memories.
//!
//! Extractors are untrusted. Whatever they return is validated by the engine
//! and stored only as `candidate` memories; an extractor can never revise
//! existing knowledge.

use async_trait::async_trait;
use cs_memory::{
    ExtractedCandidate, MemoryContent, Observation, ProceduralMemory, RelationalMemory,
    SemanticMemory, TemporalChangeKind, TemporalMemory, belief::normalize, record::CapabilityStage,
};

/// Why an extraction failed, which decides whether the job is retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    /// Transient (network, rate limit, 5xx): retry with backoff.
    Retryable(String),
    /// Will never succeed for this input (bad output, 4xx): dead-letter.
    Permanent(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(m) => write!(f, "retryable: {m}"),
            Self::Permanent(m) => write!(f, "permanent: {m}"),
        }
    }
}

#[async_trait]
pub trait Extractor: Send + Sync + std::fmt::Debug {
    /// Stable extractor name, e.g. `"rules"`.
    fn name(&self) -> &str;
    /// Bumped whenever output for the same input may change.
    fn version(&self) -> &str;
    /// Model identity, for model-backed extractors.
    fn model(&self) -> Option<&str> {
        None
    }
    async fn extract(
        &self,
        observation: &Observation,
    ) -> Result<Vec<ExtractedCandidate>, ExtractError>;
}

/// Deterministic, offline, precision-first extractor.
///
/// Pipeline:
/// 1. **Scrub** non-assertive spans: fenced code, block quotes, headers,
///    URLs.
/// 2. **Veto** sentences that are questions, start with an interrogative or
///    auxiliary, or contain hedges, conditionals, negations or TODOs.
/// 3. **Gate** entities: short subjects, never pronouns or meta-nouns
///    ("issue", "problem", "thing"…).
/// 4. **Anchor**: facts and relations need a high-signal token (identifier,
///    acronym, version, or a Title-Case term not at sentence start). This one
///    rule removes most conversational noise ("latency is high").
/// 5. **Match** one pattern per sentence, first rule wins.
#[derive(Debug, Clone, Default)]
pub struct RuleExtractor;

impl RuleExtractor {
    pub const NAME: &'static str = "rules";
    pub const VERSION: &'static str = "2";
}

const INTERROGATIVE_STARTS: &[&str] = &[
    "what", "why", "how", "when", "where", "who", "which", "is", "are", "can", "could", "should",
    "would", "do", "does", "did", "will", "shall",
];
const VETO_PHRASES: &[&str] = &[
    "maybe",
    "might",
    "perhaps",
    "probably",
    "possibly",
    "potentially",
    "i think",
    "i guess",
    "not sure",
    "could be",
    "seems",
    "apparently",
    "if",
    "unless",
    "assuming",
    "suppose",
    "whether",
    "todo",
    "fixme",
    "hopefully",
];
const NEGATIONS: &[&str] = &["not", "never", "no", "none", "without", "instead", "cannot"];
const PRONOUNS: &[&str] = &[
    "i",
    "we",
    "you",
    "he",
    "she",
    "it",
    "they",
    "this",
    "that",
    "these",
    "those",
    "there",
    "here",
    "who",
    "which",
    "what",
    "something",
    "nothing",
    "everything",
    "one",
];
const META_NOUNS: &[&str] = &[
    "issue",
    "problem",
    "reason",
    "thing",
    "fact",
    "point",
    "idea",
    "goal",
    "question",
    "answer",
    "way",
    "case",
    "result",
    "example",
    "note",
    "fix",
    "workaround",
    "error",
    "bug",
    "plan",
    "difference",
    "main thing",
    "trick",
];
const EVALUATIVE: &[&str] = &[
    "good",
    "bad",
    "slow",
    "fast",
    "broken",
    "ready",
    "done",
    "fine",
    "ok",
    "okay",
    "true",
    "false",
    "null",
    "empty",
    "great",
    "nice",
    "weird",
    "hard",
    "easy",
    "better",
    "worse",
    "high",
    "low",
    "important",
    "annoying",
];
const CAUSAL: &[&str] = &[
    "because",
    "since",
    "due to",
    "to avoid",
    "in order to",
    "so that",
];

fn words(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}

fn clean(s: &str) -> String {
    s.trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | ',' | ';' | ':' | '(' | ')'))
        .trim()
        .to_owned()
}

fn strip_article(s: &str) -> &str {
    let t = s.trim_start();
    for a in ["the ", "a ", "an ", "our ", "my ", "their "] {
        if t.len() > a.len() && t[..a.len()].eq_ignore_ascii_case(a) {
            return &t[a.len()..];
        }
    }
    t
}

/// Subjects: 1–5 words, 3–64 chars, not a pronoun or meta-noun.
fn subject(s: &str) -> Option<String> {
    let s = clean(strip_article(s));
    let n = words(&s).len();
    let lower = s.to_lowercase();
    let head = lower.split_whitespace().last().unwrap_or_default();
    if !(1..=5).contains(&n)
        || !(3..=64).contains(&s.len())
        || PRONOUNS.contains(&lower.as_str())
        || META_NOUNS.contains(&lower.as_str())
        || META_NOUNS.contains(&head)
    {
        return None;
    }
    Some(s)
}

/// Objects: 1–15 words, not a bare pronoun.
fn object(s: &str) -> Option<String> {
    let s = clean(s);
    let n = words(&s).len();
    if !(1..=15).contains(&n) || PRONOUNS.contains(&s.to_lowercase().as_str()) {
        return None;
    }
    Some(s)
}

/// A high-signal token: identifier syntax, acronym, version, or a
/// Title-Case word (callers exclude the sentence-initial word).
fn is_anchor(token: &str) -> bool {
    let t = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
    if t.len() < 2 {
        return false;
    }
    let has_lower = t.chars().any(char::is_lowercase);
    let uppers = t.chars().filter(|c| c.is_uppercase()).count();
    t.contains(['_', '/'])
        || t.contains("::")
        || (t.contains('-') && t.chars().any(char::is_alphabetic))
        || (uppers >= 2 && !has_lower)
        || (t.chars().skip(1).any(char::is_uppercase) && has_lower)
        || (t.starts_with(['v', 'V']) && t[1..].starts_with(|c: char| c.is_ascii_digit()))
        || t.chars().next().is_some_and(char::is_uppercase)
}

/// Does `phrase` (from `sentence`) carry an anchor, ignoring the
/// sentence-initial word's capitalisation?
fn anchored(sentence: &str, phrase: &str) -> bool {
    let first = sentence.split_whitespace().next().unwrap_or_default();
    phrase.split_whitespace().enumerate().any(|(i, w)| {
        let at_start =
            i == 0 && sentence.trim_start().starts_with(phrase.trim_start()) && w == first;
        if at_start {
            // A sentence-initial word counts only for identifier syntax,
            // acronyms or inner capitals, not mere capitalisation.
            let t = w.trim_matches(|c: char| !c.is_alphanumeric());
            t.contains(['_', '-', '/']) || (t.chars().filter(|c| c.is_uppercase()).count() >= 2)
        } else {
            is_anchor(w)
        }
    })
}

/// Split at the first case-insensitive occurrence of `marker` (ASCII).
fn split_on<'a>(sentence: &'a str, lower: &str, marker: &str) -> Option<(&'a str, &'a str)> {
    let at = lower.find(marker)?;
    Some((&sentence[..at], &sentence[at + marker.len()..]))
}

/// Remove fenced code blocks, quotes, headers and URLs.
fn scrub(text: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || t.starts_with('>') || t.starts_with('#') {
            continue;
        }
        let kept: Vec<&str> = line
            .split_whitespace()
            .filter(|w| !(w.starts_with("http://") || w.starts_with("https://")))
            .collect();
        out.push_str(&kept.join(" "));
        out.push('\n');
    }
    out
}

fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        // A period inside a token (v1.2, cs.core) does not end a sentence.
        let inner_dot = ch == '.' && chars.get(i + 1).is_some_and(|c| c.is_alphanumeric());
        if matches!(ch, '.' | '!' | '?' | '\n') && !inner_dot {
            if ch == '?' {
                cur.push('?');
            }
            let s = cur.trim().to_owned();
            if !s.is_empty() {
                out.push(s);
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    let s = cur.trim().to_owned();
    if !s.is_empty() {
        out.push(s);
    }
    out
}

fn vetoed(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    let tokens: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    let padded = format!(" {} ", tokens.join(" "));
    sentence.ends_with('?')
        || tokens
            .first()
            .is_some_and(|t| INTERROGATIVE_STARTS.contains(&t.as_str()))
        || VETO_PHRASES
            .iter()
            .any(|p| padded.contains(&format!(" {p} ")))
        || tokens
            .iter()
            .any(|t| NEGATIONS.contains(&t.as_str()) || t.ends_with("n't"))
        || !(3..=40).contains(&tokens.len())
}

fn candidate(content: MemoryContent, confidence: f64, rule: &str) -> ExtractedCandidate {
    ExtractedCandidate {
        content,
        confidence: Some(confidence),
        rationale: Some(format!("rule:{rule}")),
    }
}

/// Split "A, B and C" into items, only if every item is a valid subject.
fn coordination(o: &str) -> Vec<String> {
    let normalized = o.replace(", and ", ", ").replace(" and ", ", ");
    let parts: Vec<String> = normalized
        .split(',')
        .map(clean)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() > 1 && parts.iter().all(|p| subject(p).is_some()) {
        parts
    } else {
        vec![clean(o)]
    }
}

fn extract_sentence(sentence: &str) -> Vec<ExtractedCandidate> {
    let lower = sentence.to_ascii_lowercase();

    // Temporal change: "<subject> changed from <a> to <b>".
    if let Some((s, rest)) = split_on(sentence, &lower, " changed from ") {
        let rest_lower = rest.to_ascii_lowercase();
        if let Some((from, to)) = split_on(rest, &rest_lower, " to ")
            && let (Some(s), Some(a), Some(b)) = (subject(s), object(from), object(to))
            && normalize(&a) != normalize(&b)
        {
            return vec![candidate(
                MemoryContent::Temporal(TemporalMemory {
                    subject: s,
                    predicate: "is".into(),
                    value: b,
                    change_kind: TemporalChangeKind::Updated,
                }),
                0.7,
                "changed-from-to",
            )];
        }
    }

    // Decisions: explicit marker, or "decided to" with a stated rationale.
    let explicit = ["we decided to ", "decision: ", "adr: "]
        .iter()
        .any(|m| lower.contains(m));
    let reasoned = lower.contains("decided to ") && CAUSAL.iter().any(|c| lower.contains(c));
    if explicit || reasoned {
        return vec![candidate(
            MemoryContent::Semantic(SemanticMemory {
                proposition: format!("Decision: {}", clean(sentence)),
                subject: None,
                predicate: None,
                object: None,
            }),
            0.6,
            "decision",
        )];
    }

    // Relations (multi-valued), with safe coordination splitting.
    for (marker, predicate) in [
        (" depends on ", "depends on"),
        (" relies on ", "depends on"),
        (" requires ", "requires"),
        (" uses ", "uses"),
        (" is part of ", "part of"),
        (" belongs to ", "part of"),
    ] {
        if let Some((s, o)) = split_on(sentence, &lower, marker)
            && let Some(subj) = subject(s)
            && object(o).is_some()
            && (anchored(sentence, s) || anchored(sentence, o))
        {
            return coordination(strip_article(o))
                .into_iter()
                .filter_map(|o| object(strip_article(&o)))
                .map(|o| {
                    candidate(
                        MemoryContent::Relational(RelationalMemory {
                            subject: subj.clone(),
                            predicate: predicate.into(),
                            object: o,
                            directed: true,
                        }),
                        0.65,
                        "relation",
                    )
                })
                .collect();
        }
    }

    // Preferences.
    if lower.starts_with("i prefer ") {
        let o = &sentence["i prefer ".len()..];
        if let Some(o) = object(o) {
            return vec![candidate(
                MemoryContent::Semantic(SemanticMemory {
                    proposition: clean(sentence),
                    subject: Some("user".into()),
                    predicate: Some("prefers".into()),
                    object: Some(o),
                }),
                0.7,
                "preference",
            )];
        }
    }

    // Copular facts: "<subject> is|are <object>".
    for marker in [" is ", " are "] {
        if let Some((s, o)) = split_on(sentence, &lower, marker)
            && let (Some(subj), Some(obj)) = (subject(s), object(o))
        {
            let first = obj
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_lowercase();
            let progressive = first.ends_with("ing");
            let evaluative =
                words(&obj).len() == 1 && EVALUATIVE.contains(&obj.to_lowercase().as_str());
            if progressive || evaluative || !(anchored(sentence, s) || anchored(sentence, o)) {
                return Vec::new();
            }
            return vec![candidate(
                MemoryContent::Semantic(SemanticMemory {
                    proposition: clean(sentence),
                    subject: Some(subj),
                    predicate: Some("is".into()),
                    object: Some(obj),
                }),
                0.5,
                "copula",
            )];
        }
    }
    Vec::new()
}

fn is_step(line: &str) -> Option<(u32, String)> {
    let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
    let rest = &line[digits.len()..];
    let body = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    let body = clean(body);
    (!digits.is_empty() && !body.is_empty()).then(|| (digits.parse().unwrap_or(0), body))
}

/// Strictly sequential numbered steps (1, 2, …) become one procedure, named
/// by the line preceding the list.
fn extract_procedures(text: &str) -> (Vec<ExtractedCandidate>, Vec<usize>) {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut out = Vec::new();
    let mut consumed = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some((1, _)) = is_step(lines[i]) else {
            i += 1;
            continue;
        };
        let start = i;
        let mut steps = Vec::new();
        let mut expected = 1;
        while i < lines.len() {
            match is_step(lines[i]) {
                Some((n, body)) if n == expected => {
                    steps.push(body);
                    expected += 1;
                    i += 1;
                }
                _ => break,
            }
        }
        consumed.extend(start..i);
        if steps.len() < 2 {
            continue;
        }
        let name = lines[..start]
            .iter()
            .rev()
            .find(|l| !l.is_empty())
            .map(|l| clean(l))
            .filter(|n| !n.is_empty() && words(n).len() <= 12 && is_step(n).is_none())
            .unwrap_or_else(|| "procedure".into());
        if let Some(pos) = lines[..start].iter().rposition(|l| !l.is_empty()) {
            consumed.push(pos);
        }
        out.push(candidate(
            MemoryContent::Procedural(ProceduralMemory {
                capability_id: None,
                name,
                preconditions: vec![],
                steps,
                expected_outcome: None,
                stage: CapabilityStage::Observed,
            }),
            0.6,
            "numbered-steps",
        ));
    }
    (out, consumed)
}

/// Pure function used by [`RuleExtractor`]; exposed for testing.
pub fn extract_rules(text: &str) -> Vec<ExtractedCandidate> {
    let text = scrub(text);
    let (mut out, consumed) = extract_procedures(&text);
    let prose: String = text
        .lines()
        .enumerate()
        .filter(|(i, _)| !consumed.contains(i))
        .map(|(_, l)| l)
        .collect::<Vec<_>>()
        .join("\n");
    for s in sentences(&prose) {
        if vetoed(&s) {
            continue;
        }
        for c in extract_sentence(&s) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

#[async_trait]
impl Extractor for RuleExtractor {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> &str {
        Self::VERSION
    }

    async fn extract(
        &self,
        observation: &Observation,
    ) -> Result<Vec<ExtractedCandidate>, ExtractError> {
        let text = observation
            .content
            .as_deref()
            .ok_or_else(|| ExtractError::Permanent("observation content was purged".into()))?;
        Ok(extract_rules(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domains(text: &str) -> Vec<&'static str> {
        extract_rules(text)
            .iter()
            .map(|c| c.content.domain().as_str())
            .collect()
    }

    #[test]
    fn extracts_anchored_copular_fact_as_triple() {
        let out = extract_rules("The primary database is PostgreSQL.");
        let MemoryContent::Semantic(m) = &out[0].content else {
            panic!("{out:?}")
        };
        assert_eq!(m.subject.as_deref(), Some("primary database"));
        assert_eq!(m.object.as_deref(), Some("PostgreSQL"));
    }

    #[test]
    fn rejects_conversational_noise() {
        for noise in [
            "Maybe the cache is Redis.",
            "The cache is not Redis.",
            "Is the cache Redis?",
            "What is BM25 doing here",
            "It is fine.",
            "Latency is high.",
            "The deployment is slow.",
            "The issue is that `cs-core` panics.",
            "The worker is running in Docker.",
            "If the queue is empty we sleep.",
            "TODO: the API uses gRPC.",
        ] {
            assert!(
                extract_rules(noise).is_empty(),
                "{noise} -> {:?}",
                extract_rules(noise)
            );
        }
    }

    #[test]
    fn relations_changes_decisions() {
        assert_eq!(
            domains("The API depends on the auth service."),
            vec!["relational"]
        );
        assert_eq!(
            domains("Our deploy target changed from Heroku to Fly."),
            vec!["temporal"]
        );
        assert_eq!(
            domains("We decided to freeze deploys on Fridays."),
            vec!["semantic"]
        );
        assert!(domains("I decided to check the logs.").is_empty());
        assert_eq!(
            domains("I decided to pin sqlx because 0.9 breaks offline mode."),
            vec!["semantic"]
        );
        assert!(domains("The value changed from 5 to 5.").is_empty());
    }

    #[test]
    fn coordinated_objects_split_into_edges() {
        let out = extract_rules("`cs-engine` depends on cs-core, cs-memory and cs-policy.");
        assert_eq!(out.len(), 3, "{out:?}");
    }

    #[test]
    fn code_blocks_quotes_and_urls_are_ignored() {
        let text =
            "```\nThe Engine is Broken.\n```\n> The API uses SOAP.\nSee https://example.com/X-Y.";
        assert!(extract_rules(text).is_empty(), "{:?}", extract_rules(text));
    }

    #[test]
    fn numbered_steps_become_one_procedure() {
        let text =
            "Release process:\n1. Bump the version\n2. Tag the commit\n3. Publish the crate\n";
        let out = extract_rules(text);
        let MemoryContent::Procedural(p) = &out[0].content else {
            panic!("{out:?}")
        };
        assert_eq!(p.name, "Release process");
        assert_eq!(p.steps.len(), 3);
        assert_eq!(out.len(), 1, "steps must not also yield facts: {out:?}");
        // Non-sequential numbering is not a procedure.
        assert!(extract_rules("1. Alpha\n3. Gamma\n").is_empty());
    }

    #[test]
    fn deterministic() {
        let t = "The API uses Postgres. The queue is durable. We decided to ship v2.1 on Friday.";
        assert_eq!(extract_rules(t), extract_rules(t));
        assert_eq!(domains(t), vec!["relational", "semantic"]);
    }
}
