# ADR-0026: Answer, investigate or abstain, from accounted evidence

- Status: Accepted
- Date: 2026-11-16

## Context

A recall always returns something: the candidate budget fills with the newest memories when nothing matches. A caller that answers from whatever ranks first will answer confidently about:

- services that do not exist;
- facts that disagree;
- extractions nobody has validated;
- questions the evidence does not address.

Score margins do not help. They measure how far apart two candidates are, not whether either one answers the question.

## Decision

### A decision, not a probability

A recall policy may set `metacognition: { min_margin }`. The recall then records a decision (`answer`, `investigate` or `abstain`) with:

- the reasons, and the signals it was taken from;
- the question terms it accounted for and the framing terms it ignored;
- what the closest anchored memory left unaccounted.

The decision is a pure function of the recall's own selection. It is stored in the trace, and exact replay reproduces it. No probability is claimed.

### Coverage by accounted terms (`accounted-terms/1`)

| Memory | Covers the question when | Anchored when |
|---|---|---|
| **Structured** (subject, predicate, object) | Its whole subject is named by the question, and every question term is accounted for by its subject, predicate or object | Its subject is named, but some question term is unaccounted for |
| **Unstructured** | It contains every question term | Never |

Matching details:

- Terms are compared up to one trailing `s`.
- A short, versioned list of conversational framing terms is ignored.
- Negations and temporal qualifiers are never ignored.
- Coverage is judged per memory: several memories never combine to cover one question.
- The answer comes from the covering memory with the most specific subject, then score, then rank.

### Rules

| Situation | Decision | Reason |
|---|---|---|
| Nothing selected | Abstain | `no_evidence` |
| Nothing covering or anchored | Abstain | `not_covered` |
| Nothing covering, something anchored | Investigate | `aspect_unmatched` |
| Only unvalidated memories cover | Investigate | `only_unvalidated` |
| Covering validated memories contradict: same subject and predicate, different value, valid together | Investigate | `contradiction` |
| The margin is below `min_margin`, when set | Investigate | `narrow_margin` |
| Otherwise | Answer | — |

## Evidence

### Experiment 010

[Experiment 010](../../benchmarks/010-selective-answering/RESULTS.md) ran on held-out seeds with 1,032 questions. Framings and qualifiers were never seen in development.

| Measure | Result |
|---|---|
| Wrong answers among the 423 answered | **None** |
| Literal questions answered | All |
| Paraphrases | All investigated |
| Absent, conflicting, unvalidated-only and qualified questions answered | None |
| Margin-only at the same coverage | Wrong on 43% of its answers |

### The detector this replaces

The first version used experiment 008's IDF coverage. On development seeds it let absent services through, because framing words were as rare as names, so it was replaced before preregistration.

## Consequences

- Callers can tell three situations apart:
  - "there is nothing about this";
  - "there is something about this, but not what you asked";
  - "the evidence exists but cannot be relied on yet".
- `investigate` is the hook for later reasoning work, such as information needs or causal interventions.
- **The detector is lexical and literal.** It is safe on paraphrases, which it investigates, but not helpful on them.
  - A semantic detector must pass its own experiment before it can answer what this one investigates.
  - Contradiction detection needs structured, single-valued facts.
- The policy is opt-in. It adds a decision to a recall and never changes the ranking.
