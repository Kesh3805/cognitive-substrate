# Metacognition — policy version 2, detector `accounted-terms/1`

## Request

`RecallPolicy.metacognition: { min_margin: number | null }` (default absent: no decision). `min_margin` must be in `0..=1`.

## Question terms

1. Take the shared lexical contract's terms (tokenizer v1: lowercase, alphanumeric, stopwords removed, distinct).
2. Compare each term in its **match form**: one trailing `s` is removed when the term is longer than 3 characters and does not end in `ss`.
3. Remove the **framing terms** (version 1):

   ```
   can could currently kindly know let me now please show tell would you
   ```

Negations and temporal qualifiers (`not`, `never`, `only`, `previously`, `before`, …) are not framing terms.

## Coverage of one memory

| Memory | Covers | Anchored |
|---|---|---|
| Structured (`semantic` with subject, predicate and object; `relational`; `temporal`) | Subject terms ⊆ question terms, **and** question terms ⊆ subject ∪ predicate ∪ object terms. Specificity is the number of subject terms. | Subject terms ⊆ question terms, but some question term is unaccounted for |
| Unstructured | Question terms ⊆ the memory's terms (specificity 0) | Never |

An empty question covers nothing. Memories are judged one at a time.

## Signals

Signals are computed over the recall's **selected** memories:

| Signal | Meaning |
|---|---|
| `selected` | Number of selected memories |
| `covering_active`, `covering_candidate` | Covering memories, by status |
| `anchored` | Anchored memories |
| `covering` | Covering memory ids, ordered by specificity DESC, score DESC, rank ASC. The first is the evidence to answer from. |
| `contradictions` | Pairs of covering **active** memories in a single-valued domain (`semantic`, `temporal`) with equal normalized subject and predicate, different objects and overlapping valid times |
| `top_score`, `margin` | The first covering memory's score, and its lead over the best other selected memory (its full score when it is the only one) |
| `question_terms`, `ignored_terms` | The question terms used, and the framing terms removed |
| `unaccounted` | When nothing covers: the terms that the anchored memory with the fewest unaccounted terms leaves unaccounted (ties by score) |

## Rules

Every applicable reason is listed, and the first one decides.

| # | Condition | Reason | Decision |
|---|---|---|---|
| 1 | `selected = 0` | `no_evidence` | abstain |
| 2 | nothing covering, nothing anchored | `not_covered` | abstain |
| 3 | nothing covering, `anchored > 0` | `aspect_unmatched` | investigate |
| 4 | covering, but none active | `only_unvalidated` | investigate |
| 5 | `contradictions` non-empty | `contradiction` | investigate |
| 6 | `min_margin` set and `margin < min_margin` | `narrow_margin` | investigate |
| — | none of the above | — | answer |

## Record

The decision `{ version, policy, decision, reasons, signals }` is:

- stored in the trace (`recall_traces.metacognition`);
- returned in the recall response;
- reproduced exactly by exact replay;
- named in the `RecallExecuted` event payload, by decision only.

Experiment 010 measured it ([results](../../benchmarks/010-selective-answering/RESULTS.md)).
